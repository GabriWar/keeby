mod config;
mod gui;
mod input;
mod labels;
mod mouse;
mod pointer;
mod preview;
mod render;

use std::time::{Duration, Instant};

use calloop::channel::Event as ChanEvent;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use evdev::Key;
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
    shm::{
        slot::{Buffer, SlotPool},
        Shm, ShmHandler,
    },
};
use tiny_skia::PixmapMut;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, QueueHandle,
};

use crate::config::{Anchor as CfgAnchor, Config};
use crate::input::{DeviceFilter, InputMsg};
use crate::labels::parse_key_name;
use crate::mouse::{MouseRenderer, MouseState};
use crate::render::{Renderer, State as KeyState, HEIGHT, WIDTH};

/// One overlay layer surface with two buffers it draws into in turn.
struct Surface {
    layer: LayerSurface,
    configured: bool,
    size: (u32, u32),
    buffers: [Option<Buffer>; 2],
    /// A frame callback is pending: draw on it instead of right away.
    frame_pending: bool,
    dirty: bool,
}

impl Surface {
    fn new(layer: LayerSurface, size: (u32, u32)) -> Self {
        Self { layer, configured: false, size, buffers: [None, None], frame_pending: false, dirty: false }
    }

    /// Draws into a free buffer (or a new one) and commits it. Reusing two
    /// buffers saves creating a `wl_buffer` every frame.
    fn draw(&mut self, pool: &mut SlotPool, qh: &QueueHandle<App>, animate: bool, paint: impl FnOnce(&mut PixmapMut)) {
        if !self.configured {
            return;
        }
        let (w, h) = self.size;
        let stride = w as i32 * 4;
        let free = self
            .buffers
            .iter()
            .position(|b| b.as_ref().is_some_and(|b| b.height() == h as i32 && b.stride() == stride && b.canvas(pool).is_some()));
        let slot = match free {
            Some(i) => i,
            None => {
                let i = self.buffers.iter().position(Option::is_none).unwrap_or(0);
                match pool.create_buffer(w as i32, h as i32, stride, wl_shm::Format::Abgr8888) {
                    Ok((b, _)) => self.buffers[i] = Some(b),
                    Err(e) => {
                        eprintln!("keeby: create_buffer: {e}");
                        return;
                    }
                }
                i
            }
        };
        let buffer = self.buffers[slot].as_ref().unwrap();
        let Some(canvas) = buffer.canvas(pool) else { return };
        if let Some(mut pix) = PixmapMut::from_bytes(canvas, w, h) {
            paint(&mut pix);
        }
        let surface = self.layer.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        if animate {
            surface.frame(qh, surface.clone());
            self.frame_pending = true;
        }
        buffer.attach_to(surface).expect("attach buffer");
        surface.commit();
        self.dirty = false;
    }
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<Self>,
    loop_handle: LoopHandle<'static, App>,

    keys_surface: Surface,
    keys: KeyState,
    keys_renderer: Renderer,
    /// Wakes the keys overlay when the oldest pill starts fading.
    fade_timer: Option<RegistrationToken>,

    mouse_surface: Option<Surface>,
    mouse: MouseState,
    mouse_renderer: Option<MouseRenderer>,
    last_mouse_activity: Instant,
    auto_hide_mouse: Option<Duration>,
    mouse_hidden: bool,

    privacy_key: Option<Key>,
    privacy_mode: bool,
    /// `keeby overlay off` hides the pills and the mouse widget; the pointer
    /// service keeps running.
    overlay_hidden: bool,
    exit: bool,
}

impl App {
    fn draw_keys(&mut self) {
        if self.keys_surface.frame_pending {
            self.keys_surface.dirty = true;
            return;
        }
        let animate = self.keys.has_animation();
        let (keys, renderer) = (&mut self.keys, &mut self.keys_renderer);
        self.keys_surface.draw(&mut self.pool, &self.qh, animate, |pix| renderer.render(pix, keys));
        if !animate {
            self.schedule_fade();
        }
    }

    /// Pills fade on frame callbacks; this wakes the overlay when the oldest
    /// one should start fading, so nothing lingers while no key is pressed.
    fn schedule_fade(&mut self) {
        if let Some(tok) = self.fade_timer.take() {
            self.loop_handle.remove(tok);
        }
        let Some(at) = self.keys.next_fade() else { return };
        self.fade_timer = self
            .loop_handle
            .insert_source(Timer::from_deadline(at), |_, _, app: &mut App| {
                app.fade_timer = None;
                app.keys.prune();
                app.draw_keys();
                TimeoutAction::Drop
            })
            .ok();
    }

    fn draw_mouse(&mut self) {
        let (Some(s), Some(renderer)) = (self.mouse_surface.as_mut(), self.mouse_renderer.as_ref()) else { return };
        if s.frame_pending {
            s.dirty = true;
            return;
        }
        let (state, hidden) = (&self.mouse, self.mouse_hidden);
        s.draw(&mut self.pool, &self.qh, !hidden, |pix| {
            if hidden {
                pix.fill(tiny_skia::Color::TRANSPARENT);
            } else {
                renderer.render(pix, state);
            }
        });
    }

    fn mouse_activity(&mut self) {
        self.last_mouse_activity = Instant::now();
        self.mouse_hidden = false;
        self.draw_mouse();
    }

    fn clear(&mut self) {
        self.keys.stream.clear();
        self.mouse = MouseState::new();
        self.draw_keys();
        self.draw_mouse();
    }

    fn on_input(&mut self, msg: InputMsg) {
        if let InputMsg::SetOverlay(show) = msg {
            self.overlay_hidden = !show.unwrap_or(self.overlay_hidden);
            if self.overlay_hidden {
                self.clear();
            }
            return;
        }
        // The privacy key toggles capture before anything is recorded.
        if let (Some(pk), InputMsg::Press(k)) = (self.privacy_key, msg) {
            if k == pk {
                self.privacy_mode = !self.privacy_mode;
                if self.privacy_mode {
                    self.clear();
                }
                return;
            }
        }
        if self.privacy_mode || self.overlay_hidden {
            return;
        }
        match msg {
            InputMsg::Press(k) => {
                if self.keys.press(k) {
                    self.draw_keys();
                }
            }
            InputMsg::Release(k) => self.keys.release(k),
            InputMsg::MouseMotion(dx, dy) => {
                self.mouse.add_motion(dx, dy);
                self.mouse_activity();
            }
            InputMsg::MouseButton(b, pressed) => {
                self.mouse.set_button(b, pressed);
                self.mouse_activity();
            }
            InputMsg::MouseScroll(v, h) => {
                self.mouse.add_scroll(v, h);
                self.mouse_activity();
            }
            InputMsg::SetOverlay(_) => {}
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surf: &wl_surface::WlSurface, _: u32) {
        if self.keys_surface.layer.wl_surface() == surf {
            self.keys_surface.frame_pending = false;
            self.keys.prune();
            self.draw_keys();
        } else if let Some(ms) = self.mouse_surface.as_mut().filter(|m| m.layer.wl_surface() == surf) {
            ms.frame_pending = false;
            self.mouse.step();
            if ms.dirty || self.mouse.has_animation() {
                self.draw_mouse();
            }
        }
    }
    fn surface_enter(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
    fn surface_leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: &wl_output::WlOutput) {}
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.exit = true;
    }

    fn configure(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface, configure: LayerSurfaceConfigure, _: u32) {
        let apply = |s: &mut Surface| {
            let (w, h) = configure.new_size;
            if w != 0 && h != 0 {
                s.size = (w, h);
            }
            s.configured = true;
        };
        if &self.keys_surface.layer == layer {
            apply(&mut self.keys_surface);
            self.draw_keys();
        } else if let Some(ms) = self.mouse_surface.as_mut().filter(|m| &m.layer == layer) {
            apply(ms);
            self.draw_mouse();
        }
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_layer!(App);
delegate_registry!(App);

fn anchor_to_wlr(a: CfgAnchor) -> Anchor {
    match a {
        CfgAnchor::TopLeft => Anchor::TOP | Anchor::LEFT,
        CfgAnchor::Top => Anchor::TOP,
        CfgAnchor::TopRight => Anchor::TOP | Anchor::RIGHT,
        CfgAnchor::Left => Anchor::LEFT,
        CfgAnchor::Center => Anchor::empty(),
        CfgAnchor::Right => Anchor::RIGHT,
        CfgAnchor::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
        CfgAnchor::Bottom => Anchor::BOTTOM,
        CfgAnchor::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
    }
}

fn anchor_margins(a: CfgAnchor, mx: i32, my: i32) -> (i32, i32, i32, i32) {
    match a {
        CfgAnchor::TopLeft => (my, 0, 0, mx),
        CfgAnchor::Top => (my, 0, 0, 0),
        CfgAnchor::TopRight => (my, mx, 0, 0),
        CfgAnchor::Left => (0, 0, 0, mx),
        CfgAnchor::Center => (0, 0, 0, 0),
        CfgAnchor::Right => (0, mx, 0, 0),
        CfgAnchor::BottomLeft => (0, 0, my, mx),
        CfgAnchor::Bottom => (0, 0, my, 0),
        CfgAnchor::BottomRight => (0, mx, my, 0),
    }
}

fn print_help() {
    println!("keeby — your keyboard and mouse on screen, and the pointer from the keyboard\n");
    println!("USAGE:");
    println!("    keeby                          run the overlay and the pointer service");
    println!("    keeby point [OPTION...]        move, click and drag the pointer from the keyboard");
    println!("                                   (`keeby point --help` for its options)");
    println!("    keeby overlay [toggle|on|off]  hide or show the overlay of the running keeby");
    println!("    keeby config [--no-open]       edit the settings in the browser");
    println!("    keeby --list                   input devices and monitors keeby can see");
    println!("    keeby --help                   this message\n");
    println!("CONFIG:  ~/.config/keeby/config.conf");
    println!("REPO:    https://github.com/GabriWar/keeby");
}

fn run_list() {
    println!("=== INPUT DEVICES ===");
    for (path, name, caps) in input::list_devices() {
        let kind = if caps.is_empty() { "-".to_string() } else { caps.names().join("+") };
        println!("  {:<20} {:<18} {}", path.display(), kind, name);
    }
    println!("\n=== MONITORS ===");
    match list_monitors() {
        Ok(list) if !list.is_empty() => list.iter().for_each(|n| println!("  {n}")),
        Ok(_) => println!("  (none — no Wayland session?)"),
        Err(e) => println!("  error: {e}"),
    }
    println!(
        "\nTo use only some devices, put name parts in ~/.config/keeby/config.conf:\n  keyboards = AT Translated, Corne\n  mice      = Logitech, ELAN\n  monitor   = eDP-1"
    );
}

/// Just the outputs, to list them or pick one by name before creating
/// surfaces.
struct OutputProbe {
    registry_state: RegistryState,
    output_state: OutputState,
}
impl ProvidesRegistryState for OutputProbe {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}
impl OutputHandler for OutputProbe {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}
delegate_output!(OutputProbe);
delegate_registry!(OutputProbe);

fn list_monitors() -> Result<Vec<String>, String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<OutputProbe>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut probe = OutputProbe { registry_state: RegistryState::new(&globals), output_state: OutputState::new(&globals, &qh) };
    queue.roundtrip(&mut probe).map_err(|e| e.to_string())?;
    queue.roundtrip(&mut probe).map_err(|e| e.to_string())?;
    Ok(probe.output_state.outputs().filter_map(|o| probe.output_state.info(&o)?.name).collect())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("point" | "kbptr") => std::process::exit(pointer::run_cli(&args[1..])),
        Some("overlay") => std::process::exit(pointer::overlay_cli(args.get(1).map(String::as_str))),
        Some("config") => std::process::exit(gui::run(!args.iter().any(|a| a == "--no-open"))),
        Some("-h" | "--help" | "help") => return print_help(),
        Some("-l" | "--list" | "list") => return run_list(),
        _ => {}
    }

    let cfg = Config::load();
    let conn = Connection::connect_to_env().expect("connect wayland");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh: QueueHandle<App> = queue.handle();

    // Pick the monitor by name before creating surfaces on it.
    let target_output = cfg.monitor.as_deref().and_then(|name| {
        let mut pq = conn.new_event_queue::<OutputProbe>();
        let pqh = pq.handle();
        let mut probe = OutputProbe { registry_state: RegistryState::new(&globals), output_state: OutputState::new(&globals, &pqh) };
        pq.roundtrip(&mut probe).ok()?;
        pq.roundtrip(&mut probe).ok()?;
        let found = probe.output_state.outputs().find(|o| probe.output_state.info(o).and_then(|i| i.name).as_deref() == Some(name));
        if found.is_none() {
            eprintln!("keeby: monitor '{name}' not found, using the focused one");
        }
        found
    });

    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell not available");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm not available");

    let make_layer = |name: &str, anchor: Anchor, size: (u32, u32), (top, right, bottom, left): (i32, i32, i32, i32)| {
        let surface = compositor.create_surface(&qh);
        let region = Region::new(&compositor).expect("region");
        surface.set_input_region(Some(region.wl_region()));
        let layer = layer_shell.create_layer_surface(&qh, surface, Layer::Overlay, Some(name), target_output.as_ref());
        layer.set_anchor(anchor);
        layer.set_size(size.0, size.1);
        layer.set_margin(top, right, bottom, left);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.commit();
        Surface::new(layer, size)
    };

    let keys_margins = anchor_margins(cfg.keys_anchor, cfg.keys_margin_x, cfg.margin_bottom);
    let keys_surface = make_layer("keeby", anchor_to_wlr(cfg.keys_anchor), (WIDTH, HEIGHT), keys_margins);
    let mouse_renderer = cfg.mouse_enabled.then(|| MouseRenderer::new(&cfg));
    let mouse_surface = mouse_renderer.as_ref().map(|r| {
        let margins = anchor_margins(cfg.mouse_anchor, cfg.mouse_margin_x, cfg.mouse_margin_y);
        make_layer("keeby-mouse", anchor_to_wlr(cfg.mouse_anchor), r.surface_size(), margins)
    });
    let mouse_bytes = mouse_surface.as_ref().map_or(0, |s| s.size.0 * s.size.1 * 4) as usize;
    let pool = SlotPool::new((WIDTH * HEIGHT * 4) as usize * 2 + mouse_bytes * 2, &shm).expect("slot pool");

    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().expect("event loop");
    let handle = event_loop.handle();

    let privacy_key = cfg.privacy_key.as_deref().and_then(parse_key_name);
    if let (Some(name), None) = (cfg.privacy_key.as_deref(), privacy_key) {
        eprintln!("keeby: privacy_key '{name}' not recognized — disabled");
    }
    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        qh: qh.clone(),
        loop_handle: handle.clone(),
        keys_surface,
        keys: KeyState::new(&cfg),
        keys_renderer: Renderer::new(&cfg),
        fade_timer: None,
        mouse_surface,
        mouse: MouseState::new(),
        mouse_renderer,
        last_mouse_activity: Instant::now(),
        auto_hide_mouse: cfg.auto_hide_mouse.then(|| Duration::from_secs_f32(cfg.auto_hide_mouse_secs)),
        mouse_hidden: false,
        privacy_key,
        privacy_mode: false,
        overlay_hidden: false,
        exit: false,
    };

    queue.roundtrip(&mut app).expect("roundtrip");
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).expect("insert wayland source");

    let filter = DeviceFilter::from_lists(&cfg.keyboard_names, &cfg.mouse_names);
    if let Err(e) = input::watch(&handle, filter, |app: &mut App, msg| app.on_input(msg)) {
        eprintln!("keeby: can't watch input devices: {e}");
    }

    // The pointer service runs on its own thread with its own Wayland
    // connection; keys typed into it come back here as pills.
    let (pills_tx, pills_rx) = calloop::channel::channel::<InputMsg>();
    handle
        .insert_source(pills_rx, |ev, _, app: &mut App| {
            if let ChanEvent::Msg(msg) = ev {
                app.on_input(msg);
            }
        })
        .expect("insert pointer channel");
    pointer::spawn_daemon(pills_tx);

    if let Some(after) = app.auto_hide_mouse {
        handle
            .insert_source(Timer::from_duration(after), move |_, _, app: &mut App| {
                let idle = app.last_mouse_activity.elapsed();
                if idle >= after && !app.mouse_hidden {
                    app.mouse_hidden = true;
                    app.draw_mouse();
                }
                TimeoutAction::ToDuration(after.saturating_sub(idle).max(Duration::from_millis(250)))
            })
            .expect("insert auto-hide timer");
    }

    while !app.exit {
        event_loop.dispatch(None, &mut app).expect("event loop dispatch");
    }
}
