// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! The Wayland side of the pointer: one long-lived connection (kept warm by
//! the keeby daemon) that runs selection sessions on request.

use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure},
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tiny_skia::PixmapMut;
use wayland_client::{
    globals::GlobalList,
    protocol::{wl_buffer, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_shm_pool, wl_surface},
    Connection, Dispatch, EventQueue, QueueHandle, WEnum,
};
use wayland_protocols::wp::fractional_scale::v1::client::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols_wlr::screencopy::v1::client::{zwlr_screencopy_frame_v1, zwlr_screencopy_manager_v1};
use wayland_protocols_wlr::virtual_pointer::v1::client::{zwlr_virtual_pointer_manager_v1, zwlr_virtual_pointer_v1};
use xkbcommon::xkb;

use super::config::{Click, FloatingSource, KeySource, PointerConfig};
use super::draw::{Canvas, Fonts};
use super::grab::{self, Keyboard, Watchdog};
use super::modes::{AreaSource, Engine, Rect};
use crate::input::InputMsg;

/// Idle time after which an open overlay gives up (and the keyboard back).
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a run printed and its exit status, sent back to `keeby point`.
#[derive(Default)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Outcome {
    fn error(msg: impl Into<String>) -> Self {
        Self { stderr: format!("{}\n", msg.into()), code: 1, ..Default::default() }
    }
}

pub type ReplyFn = Box<dyn FnOnce(Outcome)>;

/// A parsed `keeby point` invocation.
pub struct Request {
    pub cfg: PointerConfig,
    pub only_print: bool,
    pub output: Option<String>,
    pub restrict: Option<Rect>,
    pub drag: bool,
    pub double_click: bool,
    pub stdin: Vec<u8>,
}

#[derive(Clone, Copy)]
struct OutputGeom {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    transform: wl_output::Transform,
    scale: i32,
}

struct Session {
    engine: Engine,
    req: Request,
    stdin_areas: Vec<Rect>,
    layer: LayerSurface,
    viewport: Option<wp_viewport::WpViewport>,
    frac: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    scale120: u32,
    size: (u32, u32),
    configured: bool,
    output: Option<(wl_output::WlOutput, OutputGeom)>,
    started: bool,
    frame_pending: bool,
    dirty: bool,
    initial_area: Rect,
    keyboards: Vec<Keyboard>,
    kb_tokens: Vec<RegistrationToken>,
    grabbed: bool,
    xkb_state: Option<xkb::State>,
    vptr: Option<zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1>,
    idle_token: Option<RegistrationToken>,
    last_key: Instant,
    reply: Option<ReplyFn>,
    warnings: String,
}

pub struct PointerApp {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor: CompositorState,
    layer_shell: LayerShell,
    shm: Shm,
    pool: SlotPool,
    conn: Connection,
    qh: QueueHandle<PointerApp>,
    loop_handle: LoopHandle<'static, PointerApp>,
    seat: Option<wl_seat::WlSeat>,
    /// Kept bound for its keymap events.
    _keyboard: Option<wl_keyboard::WlKeyboard>,
    xkb_ctx: xkb::Context,
    keymap: Option<xkb::Keymap>,
    vptr_mgr: Option<zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1>,
    screencopy: Option<zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    frac_mgr: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    capture: Capture,
    fonts: Fonts,
    session: Option<Session>,
    watchdog: Watchdog,
    pills: Option<Sender<InputMsg>>,
}

impl PointerApp {
    pub fn new(
        conn: &Connection,
        globals: &GlobalList,
        qh: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, PointerApp>,
        pills: Option<Sender<InputMsg>>,
    ) -> Result<Self, String> {
        let compositor = CompositorState::bind(globals, qh).map_err(|_| "wl_compositor not available")?;
        let layer_shell = LayerShell::bind(globals, qh).map_err(|_| "Failed to get zwlr_layer_shell_v1 object.")?;
        let shm = Shm::bind(globals, qh).map_err(|_| "Failed to get wl_shm object.")?;
        let pool = SlotPool::new(1920 * 1080 * 4, &shm).map_err(|e| e.to_string())?;
        let seat: Option<wl_seat::WlSeat> = globals.bind(qh, 1..=7, ()).ok();
        let keyboard = seat.as_ref().map(|s| s.get_keyboard(qh, ()));
        Ok(Self {
            registry_state: RegistryState::new(globals),
            output_state: OutputState::new(globals, qh),
            compositor,
            layer_shell,
            capture: Capture::new(conn, shm.wl_shm().clone()),
            shm,
            pool,
            conn: conn.clone(),
            qh: qh.clone(),
            loop_handle,
            seat,
            _keyboard: keyboard,
            xkb_ctx: xkb::Context::new(xkb::CONTEXT_NO_FLAGS),
            keymap: None,
            vptr_mgr: globals.bind(qh, 1..=2, ()).ok(),
            screencopy: globals.bind(qh, 1..=1, ()).ok(),
            frac_mgr: globals.bind(qh, 1..=1, ()).ok(),
            viewporter: globals.bind(qh, 1..=1, ()).ok(),
            fonts: Fonts::new(),
            session: None,
            watchdog: Watchdog::spawn(),
            pills,
        })
    }

    pub fn tick(&self) {
        self.watchdog.tick();
    }

    pub fn busy(&self) -> bool {
        self.session.is_some()
    }

    /// The `a s d f j k l m g h b` key positions on the current layout.
    fn home_row(&self) -> Vec<String> {
        const CODES: [u32; 11] = [0x26, 0x27, 0x28, 0x29, 0x2c, 0x2d, 0x2e, 0x2f, 0x2a, 0x2b, 0x38];
        let Some(km) = &self.keymap else {
            return "asdfjklmghb".chars().map(|c| c.to_string()).collect();
        };
        let st = xkb::State::new(km);
        CODES.iter().map(|c| xkb::keysym_to_utf8(st.key_get_one_sym((*c).into()))).collect()
    }

    fn outputs(&self) -> Vec<(wl_output::WlOutput, OutputGeom, Option<String>)> {
        self.output_state
            .outputs()
            .filter_map(|o| {
                let info = self.output_state.info(&o)?;
                let (x, y) = info.logical_position?;
                let (w, h) = info.logical_size?;
                Some((o, OutputGeom { x, y, w, h, transform: info.transform, scale: info.scale_factor }, info.name))
            })
            .collect()
    }

    /// Starts a run. A run already in progress is cancelled first.
    pub fn start(&mut self, req: Request, reply: ReplyFn) {
        if self.session.is_some() {
            self.finish(false);
        }
        let mut warnings = String::new();

        let floating_detect = req.cfg.general.modes.split(',').any(|m| m == "floating")
            && req.cfg.floating.source == FloatingSource::Detect;
        if floating_detect && (!super::detect::AVAILABLE || self.screencopy.is_none()) {
            let msg = if super::detect::AVAILABLE {
                "Could not load `zwlr_screencopy_manager_v1`."
            } else {
                "keeby was built without the `detect` feature. 'detect' source not supported."
            };
            return reply(Outcome::error(msg));
        }
        if !req.only_print && self.vptr_mgr.is_none() {
            return reply(Outcome::error("Failed to get wlr_virtual_pointer_manager_v1 object."));
        }

        let engine = match Engine::new(req.cfg.clone(), self.home_row(), req.drag, req.double_click) {
            Ok(e) => e,
            Err(e) => return reply(Outcome::error(format!("{e}\nCould not load modes."))),
        };
        for spec in engine.font_specs() {
            if let Err(e) = self.fonts.load(&spec) {
                return reply(Outcome::error(e));
            }
        }

        // Pick the output up front when we can, so the first frame (and the
        // screen capture) doesn't wait on the compositor.
        let outputs = self.outputs();
        let mut initial_area = Rect { x: -1, y: -1, w: -1, h: -1 };
        let output = if let Some(name) = &req.output {
            match outputs.iter().find(|o| o.2.as_deref() == Some(name.as_str())) {
                Some(o) => {
                    // With an output given, wl-kbptr takes the area as is.
                    if let Some(r) = req.restrict {
                        initial_area = r;
                    }
                    Some((o.0.clone(), o.1))
                }
                None => return reply(Outcome::error(format!("Could not find output '{name}'."))),
            }
        } else if let Some(r) = req.restrict {
            match outputs.iter().find(|o| o.1.x <= r.x && o.1.y <= r.y && o.1.x + o.1.w > r.x && o.1.y + o.1.h > r.y) {
                Some(o) => {
                    initial_area = Rect { x: r.x - o.1.x, y: r.y - o.1.y, ..r };
                    Some((o.0.clone(), o.1))
                }
                None => return reply(Outcome::error("Could not find output containing given area.")),
            }
        } else if outputs.len() == 1 {
            Some((outputs[0].0.clone(), outputs[0].1))
        } else {
            None
        };

        // Keys: grab the keyboards (focus stays put), else take the focus.
        let keyboards = if req.cfg.general.key_source == KeySource::Evdev { grab::open_keyboards() } else { Vec::new() };
        if req.cfg.general.key_source == KeySource::Evdev && keyboards.is_empty() {
            warnings.push_str(
                "Could not open any keyboard in /dev/input, falling back to the Wayland keyboard. Is the user in the `input` group?\n",
            );
        }

        let stdin_areas = if req.cfg.floating.source == FloatingSource::Stdin && req.cfg.general.modes.contains("floating") {
            parse_stdin_areas(&req.stdin, &mut warnings)
        } else {
            Vec::new()
        };

        let surface = self.compositor.create_surface(&self.qh);
        let region = Region::new(&self.compositor).expect("region");
        surface.set_input_region(Some(region.wl_region()));
        let viewport = self.viewporter.as_ref().map(|v| v.get_viewport(&surface, &self.qh, ()));
        let frac = self.frac_mgr.as_ref().map(|m| m.get_fractional_scale(&surface, &self.qh, ()));
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            surface,
            Layer::Overlay,
            Some("selection"),
            output.as_ref().map(|o| &o.0),
        );
        layer.set_exclusive_zone(-1);
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        layer.set_keyboard_interactivity(if keyboards.is_empty() {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        layer.commit();

        let mut kb_tokens = Vec::new();
        for (i, kb) in keyboards.iter().enumerate() {
            let fd = kb.fd.try_clone().expect("dup keyboard fd");
            let tok = self
                .loop_handle
                .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, app: &mut PointerApp| {
                    app.keyboard_ready(i);
                    Ok(PostAction::Continue)
                })
                .expect("insert keyboard source");
            kb_tokens.push(tok);
        }
        let idle_token = self
            .loop_handle
            .insert_source(Timer::from_duration(IDLE_TIMEOUT), |_, _, app: &mut PointerApp| {
                match app.session.as_ref().map(|s| s.last_key.elapsed()) {
                    Some(e) if e >= IDLE_TIMEOUT => {
                        app.finish(false);
                        TimeoutAction::Drop
                    }
                    Some(e) => TimeoutAction::ToDuration(IDLE_TIMEOUT - e),
                    None => TimeoutAction::Drop,
                }
            })
            .ok();

        let xkb_state = self.keymap.as_ref().map(xkb::State::new);
        let vptr = if req.only_print {
            None
        } else {
            self.vptr_mgr.as_ref().map(|m| {
                m.create_virtual_pointer_with_output(self.seat.as_ref(), output.as_ref().map(|o| &o.0), &self.qh, ())
            })
        };

        let has_output = output.is_some();
        self.session = Some(Session {
            engine,
            req,
            stdin_areas,
            layer,
            viewport,
            frac,
            scale120: 0,
            size: (0, 0),
            configured: false,
            output,
            started: false,
            frame_pending: false,
            dirty: false,
            initial_area,
            keyboards,
            kb_tokens,
            grabbed: false,
            xkb_state,
            vptr,
            idle_token,
            last_key: Instant::now(),
            reply: Some(reply),
            warnings,
        });

        // Start capturing right away; it runs while the overlay maps.
        if floating_detect && has_output {
            self.prefetch_capture();
        }
        // The release of the keys that launched us is usually already queued.
        self.try_grab();
        let _ = self.conn.flush();
    }

    /// The area floating mode will capture if it's the first mode.
    fn first_capture_region(&self) -> Option<(wl_output::WlOutput, OutputGeom, Rect)> {
        let s = self.session.as_ref()?;
        let (o, g) = s.output.clone()?;
        if !s.req.cfg.general.modes.starts_with("floating") {
            return None;
        }
        let area = initial_area(s.initial_area, g.w, g.h).ok()?;
        Some((o, g, inset(area)))
    }

    fn prefetch_capture(&mut self) {
        if let (Some((o, g, region)), Some(mgr)) = (self.first_capture_region(), self.screencopy.clone()) {
            self.capture.request(&mgr, &o, region, g.transform);
        }
    }

    fn keyboard_ready(&mut self, i: usize) {
        let Some(s) = self.session.as_mut() else { return };
        let mut events = Vec::new();
        if s.keyboards[i].read(&mut events).is_err() {
            // Unplugged.
            if let Some(tok) = s.kb_tokens.get(i).copied() {
                self.loop_handle.remove(tok);
            }
            return;
        }
        if !s.grabbed {
            self.try_grab();
            return;
        }
        for ev in events {
            self.evdev_key(ev.code, ev.value);
            if self.session.is_none() {
                return;
            }
        }
    }

    /// Grabs every keyboard once no key is held, so the compositor sees the
    /// release of the keys that launched us.
    fn try_grab(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if s.grabbed || s.keyboards.is_empty() || s.keyboards.iter().any(|k| k.any_key_down()) {
            return;
        }
        let mut fds = Vec::new();
        let mut scratch = Vec::new();
        for kb in &s.keyboards {
            kb.set_grab(true);
            let _ = kb.read(&mut scratch); // drop what came before the grab
            fds.push(kb.fd.as_raw_fd());
        }
        s.grabbed = true;
        self.watchdog.arm(fds);
    }

    fn evdev_key(&mut self, code: u16, value: i32) {
        if let Some(p) = &self.pills {
            let key = evdev::Key::new(code);
            let msg = match value {
                1 => Some(InputMsg::Press(key)),
                0 => Some(InputMsg::Release(key)),
                _ => None,
            };
            if let Some(m) = msg {
                let _ = p.send(m);
            }
        }
        let Some(s) = self.session.as_mut() else { return };
        if s.xkb_state.is_none() {
            s.xkb_state = self.keymap.as_ref().map(xkb::State::new);
        }
        let Some(st) = s.xkb_state.as_mut() else { return };
        let kc: xkb::Keycode = (code as u32 + 8).into();
        let sym = st.key_get_one_sym(kc);
        match value {
            1 => {
                st.update_key(kc, xkb::KeyDirection::Down);
                self.handle_key(sym);
            }
            // Autorepeat, for holding an arrow in split mode or Backspace.
            2 if matches!(sym, xkb::Keysym::Left | xkb::Keysym::Right | xkb::Keysym::Up | xkb::Keysym::Down | xkb::Keysym::BackSpace) => {
                self.handle_key(sym)
            }
            0 => {
                st.update_key(kc, xkb::KeyDirection::Up);
            }
            _ => {}
        }
    }

    fn handle_key(&mut self, sym: xkb::Keysym) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.started {
            return;
        }
        s.last_key = Instant::now();
        let text = xkb::keysym_to_utf8(sym);
        let mut areas = SessionAreas { stdin: &s.stdin_areas, capture: &mut self.capture, screencopy: self.screencopy.as_ref(), output: s.output.clone() };
        let redraw = s.engine.key(sym, &text, &mut areas);
        self.flush_move();
        let s = self.session.as_mut().unwrap();
        if !s.engine.running || s.engine.has_last_mode_returned() {
            let done = s.engine.result.is_some();
            self.finish(done);
        } else if redraw {
            self.request_draw();
        }
    }

    /// Moves the pointer where bisect or split asked for while selecting.
    fn flush_move(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        let Some((x, y)) = s.engine.pending_move.take() else { return };
        if let (Some(v), Some((_, g))) = (&s.vptr, &s.output) {
            let (x, y, w, h) = apply_transform(x, y, g.w, g.h, g.transform);
            v.motion_absolute(now_ms(), x, y, w, h);
            v.frame();
            let _ = self.conn.flush();
        }
    }

    fn start_first_mode(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if s.started || !s.configured {
            return;
        }
        let Some((_, g)) = s.output.clone() else { return };
        let area = match initial_area(s.initial_area, s.size.0 as i32, s.size.1 as i32)
            .and_then(|a| if s.initial_area.w == -1 { Ok(a) } else { clamp_area(a, g.w, g.h) })
        {
            Ok(a) => a,
            Err(e) => {
                s.warnings.push_str(&e);
                s.warnings.push('\n');
                return self.finish(false);
            }
        };
        s.started = true;
        let mut areas = SessionAreas { stdin: &s.stdin_areas, capture: &mut self.capture, screencopy: self.screencopy.as_ref(), output: s.output.clone() };
        s.engine.enter_next_mode(area, &mut areas);
        self.flush_move();
        let s = self.session.as_mut().unwrap();
        if !s.engine.running || s.engine.has_last_mode_returned() {
            let done = s.engine.result.is_some();
            return self.finish(done);
        }
        self.draw();
    }

    fn request_draw(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if s.frame_pending {
            s.dirty = true;
        } else {
            self.draw();
        }
    }

    fn draw(&mut self) {
        let Some(s) = self.session.as_mut() else { return };
        if !s.configured || !s.started {
            return;
        }
        let scale120 = if s.scale120 != 0 { s.scale120 } else { s.output.as_ref().map_or(1, |o| o.1.scale) as u32 * 120 };
        let (lw, lh) = s.size;
        let (pw, ph) = (lw * scale120 / 120, lh * scale120 / 120);
        let stride = pw as i32 * 4;
        let (buffer, canvas) = match self.pool.create_buffer(pw as i32, ph as i32, stride, wl_shm::Format::Abgr8888) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("keeby: create_buffer: {e}");
                return;
            }
        };
        if let Some(pix) = PixmapMut::from_bytes(canvas, pw, ph) {
            let mut cv = Canvas::new(pix, scale120 as f64 / 120.0);
            s.engine.render(&mut cv, &mut self.fonts);
        }
        let surface = s.layer.wl_surface();
        surface.set_buffer_scale(1);
        buffer.attach_to(surface).expect("attach");
        if let Some(v) = &s.viewport {
            v.set_destination(lw as i32, lh as i32);
        }
        surface.damage_buffer(0, 0, pw as i32, ph as i32);
        surface.frame(&self.qh, surface.clone());
        surface.commit();
        s.frame_pending = true;
        s.dirty = false;
        let _ = self.conn.flush();
    }

    /// Ends the run: drops the overlay and the grab, then moves, clicks or
    /// drags.
    fn finish(&mut self, selected: bool) {
        let Some(mut s) = self.session.take() else { return };
        for tok in s.kb_tokens.drain(..) {
            self.loop_handle.remove(tok);
        }
        if let Some(tok) = s.idle_token.take() {
            self.loop_handle.remove(tok);
        }
        for kb in &s.keyboards {
            kb.set_grab(false);
        }
        self.watchdog.disarm();
        if let Some(f) = s.frac.take() {
            f.destroy();
        }
        if let Some(v) = s.viewport.take() {
            v.destroy();
        }
        let output = s.output.clone();
        drop(s.layer);
        // Make sure the overlay is gone before clicking through it.
        let _ = self.conn.roundtrip();

        let mut out = Outcome { stderr: s.warnings, ..Default::default() };
        let e = &s.engine;
        match (selected, e.result, output) {
            (true, Some(r), Some((_, g))) => {
                out.stdout = format!("{}x{}+{}+{} +{}+{} {}\n", r.w, r.h, r.x, r.y, g.x, g.y, e.click.letter());
                if let Some(v) = &s.vptr {
                    if e.click == Click::Drag {
                        self.drag(v, &g, e.drag_start, (r.x + r.w / 2, r.y + r.h / 2), e.drag_button);
                    } else {
                        self.click(v, &g, (r.x + r.w / 2, r.y + r.h / 2), e.click, e.double_click);
                    }
                }
            }
            _ => out.code = e.cfg.general.cancellation_status_code as i32,
        }
        if let Some(v) = s.vptr.take() {
            v.destroy();
        }
        let _ = self.conn.flush();
        if let Some(reply) = s.reply.take() {
            reply(out);
        }
    }

    fn motion(&self, v: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, g: &OutputGeom, (x, y): (i32, i32)) {
        let (x, y, w, h) = apply_transform(x, y, g.w, g.h, g.transform);
        v.motion_absolute(now_ms(), x, y, w, h);
        v.frame();
    }

    fn button(&self, v: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, code: u32, pressed: bool) {
        let state = if pressed { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
        v.button(now_ms(), code, state);
        v.frame();
    }

    fn click(&self, v: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, g: &OutputGeom, at: (i32, i32), click: Click, double: bool) {
        self.motion(v, g, at);
        if click != Click::None {
            for _ in 0..if double { 2 } else { 1 } {
                self.button(v, click.button_code(), true);
                self.button(v, click.button_code(), false);
                let _ = self.conn.flush();
            }
        }
        let _ = self.conn.roundtrip();
    }

    fn drag(&self, v: &zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1, g: &OutputGeom, from: (i32, i32), to: (i32, i32), button: Click) {
        // A roundtrip between steps so apps see a distinct press, move and
        // release, like wl-kbptr does.
        self.motion(v, g, from);
        let _ = self.conn.roundtrip();
        self.button(v, button.button_code(), true);
        let _ = self.conn.roundtrip();
        self.motion(v, g, to);
        let _ = self.conn.roundtrip();
        self.button(v, button.button_code(), false);
        let _ = self.conn.roundtrip();
    }
}

fn now_ms() -> u32 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u32).unwrap_or(0)
}

fn inset(mut a: Rect) -> Rect {
    // Don't capture window borders.
    a.x += 1;
    a.y += 1;
    a.w -= 2;
    a.h -= 2;
    a
}

fn initial_area(a: Rect, surface_w: i32, surface_h: i32) -> Result<Rect, String> {
    if a.w == -1 {
        Ok(Rect { x: 0, y: 0, w: surface_w, h: surface_h })
    } else {
        Ok(a)
    }
}

fn clamp_area(mut a: Rect, out_w: i32, out_h: i32) -> Result<Rect, String> {
    if a.x < 0 {
        a.w += a.x;
        a.x = 0;
    }
    if a.y < 0 {
        a.h += a.y;
        a.y = 0;
    }
    if a.w + a.x > out_w {
        a.w = out_w - a.x;
    }
    if a.h + a.y > out_h {
        a.h = out_h - a.y;
    }
    if a.w <= 0 || a.h <= 0 {
        return Err(format!("Initial area ({}x{}) is too small.", a.w, a.h));
    }
    Ok(a)
}

/// Maps output-local coordinates to the virtual pointer's untransformed
/// output space, like wl-kbptr's `_apply_transform`.
fn apply_transform(x: i32, y: i32, w: i32, h: i32, t: wl_output::Transform) -> (u32, u32, u32, u32) {
    use wl_output::Transform as T;
    let (mut x, mut y, mut w, mut h) = (x as i64, y as i64, w as i64, h as i64);
    let rot = |x: &mut i64, y: &mut i64, w: &mut i64, h: &mut i64, t: T| match t {
        T::_90 => {
            let tmp = *x;
            *x = *y;
            *y = *w - tmp;
            std::mem::swap(w, h);
        }
        T::_180 => {
            *x = *w - *x;
            *y = *h - *y;
        }
        T::_270 => {
            let tmp = *x;
            *x = *h - *y;
            *y = tmp;
            std::mem::swap(w, h);
        }
        _ => {}
    };
    match t {
        T::_90 | T::_180 | T::_270 => rot(&mut x, &mut y, &mut w, &mut h, t),
        T::Flipped => x = w - x,
        T::Flipped90 => {
            x = w - x;
            rot(&mut x, &mut y, &mut w, &mut h, T::_90);
        }
        T::Flipped180 => {
            x = w - x;
            rot(&mut x, &mut y, &mut w, &mut h, T::_180);
        }
        T::Flipped270 => {
            x = w - x;
            rot(&mut x, &mut y, &mut w, &mut h, T::_270);
        }
        _ => {}
    }
    (x.max(0) as u32, y.max(0) as u32, w as u32, h as u32)
}

fn parse_stdin_areas(stdin: &[u8], warnings: &mut String) -> Vec<Rect> {
    let mut out = Vec::new();
    for line in String::from_utf8_lossy(stdin).lines() {
        let parsed = (|| {
            let (w, rest) = line.trim().split_once('x')?;
            let mut it = rest.split('+');
            let h = it.next()?.parse().ok()?;
            let x = it.next()?.parse().ok()?;
            let y = it.next()?.parse().ok()?;
            Some(Rect { x, y, w: w.parse().ok()?, h })
        })();
        match parsed {
            Some(r) => out.push(r),
            None => warnings.push_str(&format!("Error parsing '{line}'. Skipping.\n")),
        }
    }
    out
}

// ------------------------------------------------------------------ capture

/// Screen capture for target detection, on its own event queue so it can be
/// waited on from anywhere (like wl-kbptr's blocking capture).
struct Capture {
    queue: EventQueue<CaptureState>,
    state: CaptureState,
}

#[derive(Default)]
struct CaptureState {
    shm: Option<wl_shm::WlShm>,
    /// Reused between runs when the size matches.
    buf: Option<ShmBuffer>,
    frame: Option<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1>,
    region: Rect,
    transform: u32,
    status: CaptureStatus,
    format: u32,
}

#[derive(Default, PartialEq, Eq)]
enum CaptureStatus {
    #[default]
    Idle,
    Pending,
    Ready,
    Failed,
}

struct ShmBuffer {
    wl: wl_buffer::WlBuffer,
    ptr: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
    format: u32,
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        self.wl.destroy();
        // SAFETY: `ptr`/`len` come from the mmap in `ShmBuffer::new`.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

impl ShmBuffer {
    fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<CaptureState>, w: u32, h: u32, stride: u32, format: wl_shm::Format) -> Option<Self> {
        let len = (stride * h) as usize;
        // SAFETY: memfd_create + ftruncate + mmap of a fresh anonymous file.
        unsafe {
            let fd = libc::memfd_create(c"keeby-capture".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 {
                return None;
            }
            let fd = <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd);
            if libc::ftruncate(fd.as_raw_fd(), len as i64) < 0 {
                return None;
            }
            let ptr = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd.as_raw_fd(), 0);
            if ptr == libc::MAP_FAILED {
                return None;
            }
            let pool = shm.create_pool(fd.as_fd(), len as i32, qh, ());
            let wl = pool.create_buffer(0, w as i32, h as i32, stride as i32, format, qh, ());
            pool.destroy();
            Some(Self { wl, ptr: ptr.cast(), len, width: w, height: h, stride, format: format as u32 })
        }
    }
}

impl Capture {
    fn new(conn: &Connection, shm: wl_shm::WlShm) -> Self {
        let queue = conn.new_event_queue();
        Self { queue, state: CaptureState { shm: Some(shm), ..Default::default() } }
    }

    fn request(
        &mut self,
        mgr: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
        output: &wl_output::WlOutput,
        region: Rect,
        transform: wl_output::Transform,
    ) {
        if let Some(f) = self.state.frame.take() {
            f.destroy();
        }
        let qh = self.queue.handle();
        self.state.frame = Some(mgr.capture_output_region(0, output, region.x, region.y, region.w, region.h, &qh, ()));
        self.state.region = region;
        self.state.transform = transform as u32;
        self.state.status = CaptureStatus::Pending;
    }

    /// Waits for the capture of `region` and runs detection on it.
    fn targets(&mut self, mgr: &zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1, output: &wl_output::WlOutput, region: Rect, transform: wl_output::Transform) -> Vec<Rect> {
        if self.state.status == CaptureStatus::Idle || self.state.region != region {
            self.request(mgr, output, region, transform);
        }
        while self.state.status == CaptureStatus::Pending {
            if self.queue.blocking_dispatch(&mut self.state).is_err() {
                break;
            }
        }
        let ok = self.state.status == CaptureStatus::Ready;
        self.state.status = CaptureStatus::Idle;
        if let Some(f) = self.state.frame.take() {
            f.destroy();
        }
        let Some(b) = self.state.buf.as_ref().filter(|_| ok) else {
            eprintln!("keeby: could not capture screen");
            return Vec::new();
        };
        // SAFETY: the mapping is `len` bytes and stays alive with `b`.
        let data = unsafe { std::slice::from_raw_parts_mut(b.ptr, b.len) };
        super::detect::detect(data, b.width, b.height, b.stride, b.format, self.state.transform, region)
    }
}

struct SessionAreas<'a> {
    stdin: &'a [Rect],
    capture: &'a mut Capture,
    screencopy: Option<&'a zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1>,
    output: Option<(wl_output::WlOutput, OutputGeom)>,
}

impl AreaSource for SessionAreas<'_> {
    fn areas(&mut self, source: FloatingSource, area: Rect) -> Vec<Rect> {
        match source {
            FloatingSource::Stdin => self.stdin.to_vec(),
            FloatingSource::Detect => match (self.screencopy, &self.output) {
                (Some(mgr), Some((o, g))) => self.capture.targets(mgr, o, inset(area), g.transform),
                _ => Vec::new(),
            },
        }
    }
}

impl Dispatch<zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1, ()> for CaptureState {
    fn event(
        st: &mut Self,
        frame: &zwlr_screencopy_frame_v1::ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        use zwlr_screencopy_frame_v1::Event;
        match event {
            Event::Buffer { format: WEnum::Value(format), width, height, stride } => {
                let reuse = st.buf.as_ref().is_some_and(|b| b.width == width && b.height == height && b.stride == stride && b.format == format as u32);
                if !reuse {
                    st.buf = st.shm.as_ref().and_then(|shm| ShmBuffer::new(shm, qh, width, height, stride, format));
                }
                st.format = format as u32;
                match &st.buf {
                    Some(b) => frame.copy(&b.wl),
                    None => st.status = CaptureStatus::Failed,
                }
            }
            Event::Ready { .. } => st.status = CaptureStatus::Ready,
            Event::Failed => st.status = CaptureStatus::Failed,
            _ => {}
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

// --------------------------------------------------------- wayland plumbing

impl CompositorHandler for PointerApp {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        let Some(s) = self.session.as_mut() else { return };
        s.frame_pending = false;
        if s.dirty {
            self.draw();
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, output: &wl_output::WlOutput) {
        let geom = self.outputs().into_iter().find(|o| &o.0 == output).map(|o| o.1);
        let Some(s) = self.session.as_mut() else { return };
        if s.output.is_some() {
            return;
        }
        let Some(g) = geom else { return };
        s.output = Some((output.clone(), g));
        if let (Some(mgr), None) = (&self.vptr_mgr, &s.vptr) {
            if !s.req.only_print {
                s.vptr = Some(mgr.create_virtual_pointer_with_output(self.seat.as_ref(), Some(output), &self.qh, ()));
            }
        }
        self.prefetch_capture();
        self.start_first_mode();
    }
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for PointerApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for PointerApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for PointerApp {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.finish(false);
    }

    fn configure(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let Some(s) = self.session.as_mut() else { return };
        s.size = configure.new_size;
        let first = !s.configured;
        s.configured = true;
        if s.output.is_some() {
            if s.started {
                self.draw();
            } else {
                self.start_first_mode();
            }
        } else if first {
            // A transparent pixel maps the surface so the compositor tells us
            // which output it's on.
            if let Ok((buffer, canvas)) = self.pool.create_buffer(1, 1, 4, wl_shm::Format::Abgr8888) {
                canvas.fill(0);
                let surface = s.layer.wl_surface();
                buffer.attach_to(surface).expect("attach");
                if let Some(v) = &s.viewport {
                    v.set_destination(s.size.0 as i32, s.size.1 as i32);
                }
                surface.damage_buffer(0, 0, 1, 1);
                surface.commit();
            }
            let _ = qh;
        }
    }
}

impl ProvidesRegistryState for PointerApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(PointerApp);
delegate_output!(PointerApp);
delegate_shm!(PointerApp);
delegate_layer!(PointerApp);
delegate_registry!(PointerApp);

impl Dispatch<wl_seat::WlSeat, ()> for PointerApp {
    fn event(_: &mut Self, _: &wl_seat::WlSeat, _: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for PointerApp {
    fn event(app: &mut Self, _: &wl_keyboard::WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                // SAFETY: the compositor hands us a readable keymap fd of `size` bytes.
                if let Ok(Some(km)) = unsafe {
                    xkb::Keymap::new_from_fd(&app.xkb_ctx, fd, size as usize, xkb::KEYMAP_FORMAT_TEXT_V1, xkb::KEYMAP_COMPILE_NO_FLAGS)
                } {
                    app.keymap = Some(km);
                }
            }
            // Only reaches us with key_source=wayland (the overlay has the focus).
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(st) = app.session.as_mut().and_then(|s| s.xkb_state.as_mut()) {
                    st.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::Key { key, state: WEnum::Value(wl_keyboard::KeyState::Pressed), .. } => {
                let sym = app.session.as_ref().and_then(|s| s.xkb_state.as_ref()).map(|st| st.key_get_one_sym((key + 8).into()));
                if let Some(sym) = sym {
                    app.handle_key(sym);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for PointerApp {
    fn event(app: &mut Self, _: &wp_fractional_scale_v1::WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            let Some(s) = app.session.as_mut() else { return };
            let changed = s.scale120 != 0 && s.scale120 != scale;
            s.scale120 = scale;
            if changed {
                app.request_draw();
            }
        }
    }
}

wayland_client::delegate_noop!(PointerApp: ignore zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1);
wayland_client::delegate_noop!(PointerApp: ignore zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1);
wayland_client::delegate_noop!(PointerApp: ignore zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1);
wayland_client::delegate_noop!(PointerApp: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);
wayland_client::delegate_noop!(PointerApp: ignore wp_viewporter::WpViewporter);
wayland_client::delegate_noop!(PointerApp: ignore wp_viewport::WpViewport);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transform_matches_wl_kbptr() {
        use wl_output::Transform as T;
        assert_eq!(apply_transform(10, 20, 100, 50, T::Normal), (10, 20, 100, 50));
        assert_eq!(apply_transform(10, 20, 100, 50, T::_90), (20, 90, 50, 100));
        assert_eq!(apply_transform(10, 20, 100, 50, T::_180), (90, 30, 100, 50));
        assert_eq!(apply_transform(10, 20, 100, 50, T::_270), (30, 10, 50, 100));
        assert_eq!(apply_transform(10, 20, 100, 50, T::Flipped), (90, 20, 100, 50));
    }

    #[test]
    fn stdin_areas() {
        let mut w = String::new();
        let a = parse_stdin_areas(b"10x20+30+40\nbad\n1x2+3+4\n", &mut w);
        assert_eq!(a, vec![Rect { x: 30, y: 40, w: 10, h: 20 }, Rect { x: 3, y: 4, w: 1, h: 2 }]);
        assert!(w.contains("bad"));
    }

    #[test]
    fn restrict_is_clamped() {
        assert_eq!(clamp_area(Rect { x: -10, y: 5, w: 100, h: 2000 }, 1920, 1080), Ok(Rect { x: 0, y: 5, w: 90, h: 1075 }));
        assert!(clamp_area(Rect { x: 2000, y: 0, w: 10, h: 10 }, 1920, 1080).is_err());
    }
}
