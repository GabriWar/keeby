mod config;
mod input;
mod labels;
mod mouse;
mod pointer;
mod render;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use calloop::channel::Event as ChanEvent;
use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, Region},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
        WaylandSurface,
    },
    shm::{slot::SlotPool, Shm, ShmHandler},
};
use tiny_skia::Pixmap;
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
    Connection, Proxy, QueueHandle,
};

use crate::config::{Anchor as CfgAnchor, Config};
use crate::input::{DeviceFilter, InputMsg};
use crate::labels::parse_key_name;
use crate::mouse::{MouseRenderer, MouseState};
use crate::render::{Renderer, State as KeyState, HEIGHT, WIDTH};
use evdev::Key;

// ============================================================================
// Output probe: dedicated tiny dispatch target for the initial output roundtrip.
// We need wl_output `name` events processed before we can pick a monitor by name.
// ============================================================================
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

// ============================================================================
// Main app
// ============================================================================
struct Surface {
    layer: LayerSurface,
    configured: bool,
    width: u32,
    height: u32,
    pixmap: Pixmap,
    hidden: bool,
}

struct App {
    registry_state: RegistryState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    qh: QueueHandle<Self>,

    keys_surface: Surface,
    keys: KeyState,
    keys_renderer: Renderer,
    last_keys_activity: Instant,

    mouse_surface: Option<Surface>,
    mouse: MouseState,
    mouse_renderer: Option<MouseRenderer>,
    mouse_pending_frame: bool,
    mouse_dirty: bool,
    last_mouse_activity: Instant,

    cfg_auto_hide_keys: bool,
    cfg_auto_hide_keys_secs: f32,
    cfg_auto_hide_mouse: bool,
    cfg_auto_hide_mouse_secs: f32,

    privacy_key: Option<Key>,
    privacy_mode: bool,

    exit: bool,
}

impl App {
    fn draw_keys(&mut self) {
        let s = &mut self.keys_surface;
        if !s.configured {
            return;
        }
        let stride = s.width as i32 * 4;
        let (buffer, canvas) = match self.pool.create_buffer(
            s.width as i32,
            s.height as i32,
            stride,
            wl_shm::Format::Abgr8888,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("create_buffer (keys): {e}");
                return;
            }
        };
        self.keys_renderer.render(&mut s.pixmap, &mut self.keys);
        let src = s.pixmap.data();
        canvas[..src.len()].copy_from_slice(src);
        let surface = s.layer.wl_surface();
        surface.damage_buffer(0, 0, s.width as i32, s.height as i32);
        if self.keys.has_animation() {
            surface.frame(&self.qh, surface.clone());
        }
        buffer.attach_to(surface).expect("attach buffer");
        surface.commit();
        s.hidden = false;
    }

    fn draw_mouse(&mut self) {
        let (Some(s), Some(renderer)) = (self.mouse_surface.as_mut(), self.mouse_renderer.as_ref())
        else {
            return;
        };
        if !s.configured {
            return;
        }
        let stride = s.width as i32 * 4;
        let (buffer, canvas) = match self.pool.create_buffer(
            s.width as i32,
            s.height as i32,
            stride,
            wl_shm::Format::Abgr8888,
        ) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("create_buffer (mouse): {e}");
                return;
            }
        };
        renderer.render(&mut s.pixmap, &self.mouse);
        let src2 = s.pixmap.data();
        canvas[..src2.len()].copy_from_slice(src2);
        let surface = s.layer.wl_surface();
        surface.damage_buffer(0, 0, s.width as i32, s.height as i32);
        surface.frame(&self.qh, surface.clone());
        buffer.attach_to(surface).expect("attach buffer");
        surface.commit();
        self.mouse_pending_frame = true;
        self.mouse_dirty = false;
        s.hidden = false;
    }

    fn hide_keys(&mut self) {
        if self.keys_surface.hidden {
            return;
        }
        let surface = self.keys_surface.layer.wl_surface();
        surface.attach(None, 0, 0);
        surface.commit();
        self.keys_surface.hidden = true;
    }

    fn hide_mouse(&mut self) {
        let Some(ms) = self.mouse_surface.as_mut() else {
            return;
        };
        if ms.hidden {
            return;
        }
        let surface = ms.layer.wl_surface();
        surface.attach(None, 0, 0);
        surface.commit();
        ms.hidden = true;
    }

    fn check_auto_hide(&mut self) {
        // Null-buffer attach causes Hyprland to drop the layer from its list and
        // re-attaching doesn't always restore it cleanly. Instead we rely on the
        // natural fade: once `stream` empties, render() paints fully transparent,
        // so the surface stays mapped but invisible. No CPU cost beyond the one
        // final transparent frame.
        let _ = self.last_keys_activity;
        let _ = self.last_mouse_activity;
    }

    fn is_keys_surface(&self, surf: &wl_surface::WlSurface) -> bool {
        self.keys_surface.layer.wl_surface().id() == surf.id()
    }
    fn is_mouse_surface(&self, surf: &wl_surface::WlSurface) -> bool {
        self.mouse_surface
            .as_ref()
            .map(|s| s.layer.wl_surface().id() == surf.id())
            .unwrap_or(false)
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: i32) {}
    fn transform_changed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: wl_output::Transform) {}
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, surf: &wl_surface::WlSurface, _: u32) {
        if self.is_keys_surface(surf) {
            self.keys.step_animation();
            let pruned = self.keys.prune();
            if pruned || self.keys.has_animation() {
                self.draw_keys();
            }
        } else if self.is_mouse_surface(surf) {
            self.mouse_pending_frame = false;
            self.mouse.step();
            if self.mouse_dirty || self.mouse.has_animation() {
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

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let surf_id = layer.wl_surface().id();
        let (w, h) = configure.new_size;
        if surf_id == self.keys_surface.layer.wl_surface().id() {
            let s = &mut self.keys_surface;
            if w != 0 {
                s.width = w;
            }
            if h != 0 {
                s.height = h;
            }
            if s.pixmap.width() != s.width || s.pixmap.height() != s.height {
                s.pixmap = Pixmap::new(s.width, s.height).expect("pixmap");
            }
            s.configured = true;
            self.draw_keys();
        } else if let Some(ms) = self.mouse_surface.as_mut() {
            if surf_id == ms.layer.wl_surface().id() {
                if w != 0 {
                    ms.width = w;
                }
                if h != 0 {
                    ms.height = h;
                }
                if ms.pixmap.width() != ms.width || ms.pixmap.height() != ms.height {
                    ms.pixmap = Pixmap::new(ms.width, ms.height).expect("pixmap");
                }
                ms.configured = true;
                self.draw_mouse();
            }
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
    println!("keeby — Wayland keyboard and mouse overlay, and keyboard-driven pointer\n");
    println!("USAGE:");
    println!("    keeby            run the overlay and the pointer service (default)");
    println!("    keeby point ...  move, click and drag the pointer from the keyboard");
    println!("                     (`keeby point --help` for its options)");
    println!("    keeby --list     list keyboards, mice, and monitors, then exit");
    println!("    keeby --help     show this message\n");
    println!("CONFIG:  ~/.config/keeby/config.conf");
    println!("REPO:    https://github.com/GabriWar/keeby");
}

fn run_list() {
    use crate::input::{list_devices, DevKind};
    println!("=== KEYBOARDS ===");
    let devs = list_devices();
    let mut found_kbd = false;
    for (path, name, kind) in &devs {
        if matches!(kind, DevKind::Keyboard) {
            println!("  {}\t{}", path.display(), name);
            found_kbd = true;
        }
    }
    if !found_kbd { println!("  (none detected)"); }

    println!("\n=== MICE ===");
    let mut found_mouse = false;
    for (path, name, kind) in &devs {
        if matches!(kind, DevKind::Mouse) {
            println!("  {}\t{}", path.display(), name);
            found_mouse = true;
        }
    }
    if !found_mouse { println!("  (none detected)"); }

    println!("\n=== MONITORS ===");
    match list_monitors() {
        Ok(list) if !list.is_empty() => {
            for name in list {
                println!("  {name}");
            }
        }
        Ok(_) => println!("  (none — no Wayland session?)"),
        Err(e) => println!("  error: {e}"),
    }

    println!(
        "\nTo whitelist devices, paste names into ~/.config/keeby/config.conf:\n  keyboards = AT Translated, Corne\n  mice      = Logitech, Razer\n  monitor   = eDP-1"
    );
}

fn list_monitors() -> Result<Vec<String>, String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, _eq) = registry_queue_init::<OutputProbe>(&conn).map_err(|e| e.to_string())?;
    let mut probe_eq = conn.new_event_queue::<OutputProbe>();
    let probe_qh = probe_eq.handle();
    let mut probe = OutputProbe {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &probe_qh),
    };
    probe_eq.roundtrip(&mut probe).map_err(|e| e.to_string())?;
    probe_eq.roundtrip(&mut probe).map_err(|e| e.to_string())?;
    let mut names = Vec::new();
    for o in probe.output_state.outputs() {
        if let Some(info) = probe.output_state.info(&o) {
            if let Some(n) = info.name {
                names.push(n);
            }
        }
    }
    Ok(names)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.first().map(String::as_str), Some("point" | "kbptr")) {
        std::process::exit(pointer::run_cli(&args[1..]));
    }
    if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
        print_help();
        return;
    }
    if args.iter().any(|a| a == "-l" || a == "--list" || a == "list") {
        run_list();
        return;
    }

    let cfg = Config::load();

    let conn = Connection::connect_to_env().expect("connect wayland");
    let (globals, event_queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh: QueueHandle<App> = event_queue.handle();

    // ----- Probe outputs on a separate queue so we can pick a monitor by name. -----
    let target_output = {
        let mut probe_eq = conn.new_event_queue::<OutputProbe>();
        let probe_qh = probe_eq.handle();
        let mut probe = OutputProbe {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &probe_qh),
        };
        // Two roundtrips: first to get the wl_output proxies, second to receive name/info.
        probe_eq.roundtrip(&mut probe).expect("probe roundtrip 1");
        probe_eq.roundtrip(&mut probe).expect("probe roundtrip 2");

        if let Some(name) = cfg.monitor.as_deref() {
            let found = probe.output_state.outputs().find(|o| {
                probe
                    .output_state
                    .info(o)
                    .and_then(|i| i.name)
                    .as_deref()
                    == Some(name)
            });
            if found.is_none() {
                eprintln!(
                    "keeby: monitor '{name}' not found. available: {}",
                    probe
                        .output_state
                        .outputs()
                        .filter_map(|o| probe.output_state.info(&o).and_then(|i| i.name))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            found
        } else {
            None
        }
    };

    let compositor_state = CompositorState::bind(&globals, &qh).expect("compositor not available");
    let layer_shell = LayerShell::bind(&globals, &qh).expect("wlr-layer-shell not available");
    let shm = Shm::bind(&globals, &qh).expect("shm not available");

    // Keys surface.
    let keys_surf = compositor_state.create_surface(&qh);
    let region = Region::new(&compositor_state).expect("region");
    keys_surf.set_input_region(Some(region.wl_region()));
    let keys_layer = layer_shell.create_layer_surface(
        &qh,
        keys_surf,
        Layer::Overlay,
        Some("keeby"),
        target_output.as_ref(),
    );
    keys_layer.set_anchor(Anchor::BOTTOM);
    keys_layer.set_size(WIDTH, HEIGHT);
    keys_layer.set_margin(0, 0, cfg.margin_bottom, 0);
    keys_layer.set_keyboard_interactivity(KeyboardInteractivity::None);
    keys_layer.set_exclusive_zone(-1);
    keys_layer.commit();
    let keys_pixmap = Pixmap::new(WIDTH, HEIGHT).expect("pixmap");

    // Mouse surface (optional).
    let mouse_renderer = if cfg.mouse_enabled {
        Some(MouseRenderer::new(&cfg))
    } else {
        None
    };
    let mouse_surface = if let Some(r) = mouse_renderer.as_ref() {
        let (mw, mh) = r.surface_size();
        let surf = compositor_state.create_surface(&qh);
        let region2 = Region::new(&compositor_state).expect("region2");
        surf.set_input_region(Some(region2.wl_region()));
        let layer = layer_shell.create_layer_surface(
            &qh,
            surf,
            Layer::Overlay,
            Some("keeby-mouse"),
            target_output.as_ref(),
        );
        layer.set_anchor(anchor_to_wlr(cfg.mouse_anchor));
        layer.set_size(mw, mh);
        let (t, ri, b, l) = anchor_margins(cfg.mouse_anchor, cfg.mouse_margin_x, cfg.mouse_margin_y);
        layer.set_margin(t, ri, b, l);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_exclusive_zone(-1);
        layer.commit();
        let pixmap = Pixmap::new(mw, mh).expect("mouse pixmap");
        Some(Surface {
            layer,
            configured: false,
            width: mw,
            height: mh,
            pixmap,
            hidden: false,
        })
    } else {
        None
    };

    let mouse_bytes = mouse_surface
        .as_ref()
        .map(|s| s.width * s.height * 4)
        .unwrap_or(0) as usize;
    let pool_size = (WIDTH * HEIGHT * 4) as usize * 2 + mouse_bytes * 2 + 4096;
    let pool = SlotPool::new(pool_size, &shm).expect("slot pool");

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        qh: qh.clone(),
        keys_surface: Surface {
            layer: keys_layer,
            configured: false,
            width: WIDTH,
            height: HEIGHT,
            pixmap: keys_pixmap,
            hidden: false,
        },
        keys: KeyState::new(&cfg),
        keys_renderer: Renderer::new(&cfg),
        last_keys_activity: Instant::now(),
        mouse_surface,
        mouse: MouseState::new(),
        mouse_renderer,
        mouse_pending_frame: false,
        mouse_dirty: false,
        last_mouse_activity: Instant::now(),
        cfg_auto_hide_keys: cfg.auto_hide_keys,
        cfg_auto_hide_keys_secs: cfg.auto_hide_keys_secs,
        cfg_auto_hide_mouse: cfg.auto_hide_mouse,
        cfg_auto_hide_mouse_secs: cfg.auto_hide_mouse_secs,
        privacy_key: cfg.privacy_key.as_deref().and_then(parse_key_name),
        privacy_mode: false,
        exit: false,
    };
    if let Some(name) = cfg.privacy_key.as_deref() {
        if app.privacy_key.is_none() {
            eprintln!("keeby: privacy_key '{name}' not recognized — disabled");
        }
    }

    let mut event_loop: EventLoop<App> = EventLoop::try_new().expect("event loop");
    let loop_handle = event_loop.handle();

    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .expect("insert wayland source");

    let (mpsc_tx, mpsc_rx) = mpsc::channel::<InputMsg>();
    pointer::spawn_daemon(mpsc_tx.clone());
    let filter = DeviceFilter::from_lists(&cfg.keyboard_names, &cfg.mouse_names);
    input::spawn(mpsc_tx, filter);

    let (loop_tx, loop_chan) = calloop::channel::channel::<InputMsg>();
    std::thread::spawn(move || {
        while let Ok(msg) = mpsc_rx.recv() {
            if loop_tx.send(msg).is_err() {
                break;
            }
        }
    });

    loop_handle
        .insert_source(loop_chan, |ev, _, app: &mut App| {
            if let ChanEvent::Msg(msg) = ev {
                // Privacy panic key: intercept BEFORE any state mutation.
                if let (Some(pk), InputMsg::Press(k)) = (app.privacy_key, &msg) {
                    if *k == pk {
                        app.privacy_mode = !app.privacy_mode;
                        if app.privacy_mode {
                            app.keys.stream.clear();
                            app.mouse = MouseState::new();
                            app.draw_keys();
                            app.draw_mouse();
                        }
                        return;
                    }
                }
                if app.privacy_mode {
                    return;
                }

                let mut redraw_keys = false;
                let mut redraw_mouse = false;
                match msg {
                    InputMsg::Press(k) => {
                        if app.keys.press(k) {
                            redraw_keys = true;
                            app.last_keys_activity = Instant::now();
                        }
                    }
                    InputMsg::Release(k) => {
                        app.keys.release(k);
                    }
                    InputMsg::MouseMotion(dx, dy) => {
                        app.mouse.add_motion(dx, dy);
                        if dx.abs() + dy.abs() > 1 {
                            app.last_mouse_activity = Instant::now();
                        }
                        redraw_mouse = true;
                    }
                    InputMsg::MouseButton(b, pressed) => {
                        app.mouse.set_button(b, pressed);
                        app.last_mouse_activity = Instant::now();
                        redraw_mouse = true;
                    }
                    InputMsg::MouseScroll(v, h) => {
                        app.mouse.add_scroll(v, h);
                        app.last_mouse_activity = Instant::now();
                        redraw_mouse = true;
                    }
                }
                if redraw_keys {
                    app.draw_keys();
                }
                if redraw_mouse {
                    app.mouse_dirty = true;
                    if !app.mouse_pending_frame {
                        app.draw_mouse();
                    }
                }
            }
        })
        .expect("insert channel source");

    // Auto-hide poller (cheap, every 500ms).
    loop_handle
        .insert_source(Timer::immediate(), |_deadline, _, app: &mut App| {
            app.check_auto_hide();
            TimeoutAction::ToDuration(Duration::from_millis(500))
        })
        .expect("insert auto-hide timer");

    while !app.exit {
        event_loop
            .dispatch(Some(Duration::from_millis(33)), &mut app)
            .expect("event loop dispatch");
    }
}
