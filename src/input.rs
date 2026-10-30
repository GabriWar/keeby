use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use evdev::{Device, EventType, InputEventKind, Key, RelativeAxisType};
use inotify::{Inotify, WatchMask};

#[derive(Debug, Clone, Copy)]
pub enum InputMsg {
    Press(Key),
    Release(Key),
    MouseMotion(i32, i32),
    MouseButton(MouseBtn, bool),
    MouseScroll(i32, i32),
    /// Show (`Some(true)`), hide (`Some(false)`) or toggle (`None`) the
    /// overlay, from `keeby overlay`.
    SetOverlay(Option<bool>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
    Side,
    Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevKind {
    Keyboard,
    Mouse,
    Other,
}

/// Filter built from the user's config. Empty whitelists = accept all detected.
#[derive(Clone, Default)]
pub struct DeviceFilter {
    pub keyboard_substrings: Vec<String>,
    pub mouse_substrings: Vec<String>,
}

impl DeviceFilter {
    pub fn from_lists(keyboards: &[String], mice: &[String]) -> Self {
        Self {
            keyboard_substrings: keyboards.iter().map(|s| s.to_lowercase()).collect(),
            mouse_substrings: mice.iter().map(|s| s.to_lowercase()).collect(),
        }
    }
    fn allows(&self, kind: DevKind, name: Option<&str>) -> bool {
        let n = name.unwrap_or("").to_lowercase();
        match kind {
            DevKind::Keyboard => {
                self.keyboard_substrings.is_empty()
                    || self.keyboard_substrings.iter().any(|s| n.contains(s))
            }
            DevKind::Mouse => {
                self.mouse_substrings.is_empty()
                    || self.mouse_substrings.iter().any(|s| n.contains(s))
            }
            DevKind::Other => false,
        }
    }
}

type Tracked = Arc<Mutex<std::collections::HashSet<PathBuf>>>;

pub fn spawn(tx: Sender<InputMsg>, filter: DeviceFilter) {
    let tracked: Tracked = Arc::new(Mutex::new(Default::default()));
    let filter = Arc::new(filter);

    eprintln!("keeby: scanning input devices");
    for (path, dev) in evdev::enumerate() {
        try_spawn_device(&path, dev, &tx, &tracked, &filter, true);
    }

    let tx_watch = tx.clone();
    let tracked_watch = tracked.clone();
    let filter_watch = filter.clone();
    thread::Builder::new()
        .name("evdev-hotplug".into())
        .spawn(move || {
            if let Err(e) = watch_hotplug(tx_watch, tracked_watch, filter_watch) {
                eprintln!("keeby: hotplug watcher exited: {e}");
            }
        })
        .expect("spawn hotplug watcher");
}

fn try_spawn_device(
    path: &Path,
    dev: Device,
    tx: &Sender<InputMsg>,
    tracked: &Tracked,
    filter: &Arc<DeviceFilter>,
    log: bool,
) {
    let kind = device_kind(&dev);
    let name = dev.name().map(|s| s.to_string());
    let name_str = name.as_deref().unwrap_or("?");

    if kind == DevKind::Other {
        return;
    }
    if !filter.allows(kind, name.as_deref()) {
        if log {
            eprintln!("  skip {} [{:?}] {} (filtered)", path.display(), kind, name_str);
        }
        return;
    }
    {
        let mut set = tracked.lock().unwrap();
        if !set.insert(path.to_path_buf()) {
            return;
        }
    }
    if log {
        eprintln!("  use  {} [{:?}] {}", path.display(), kind, name_str);
    }

    let tx = tx.clone();
    let tracked = tracked.clone();
    let path_owned = path.to_path_buf();
    thread::Builder::new()
        .name(format!("evdev-{}", path.display()))
        .spawn(move || {
            let _ = run_device(dev, kind, tx);
            tracked.lock().unwrap().remove(&path_owned);
        })
        .expect("spawn evdev thread");
}

/// Loose classifier — only requires the bare minimum a real keyboard/mouse reports.
fn device_kind(dev: &Device) -> DevKind {
    let has_rel_x = dev
        .supported_relative_axes()
        .map(|r| r.contains(RelativeAxisType::REL_X))
        .unwrap_or(false);
    let keys = dev.supported_keys();
    let has_btn_left = keys.map(|k| k.contains(Key::BTN_LEFT)).unwrap_or(false);

    // Mouse: relative-X + a real mouse button. Excludes plain touchpads (those
    // also have BTN_TOUCH but we still want to accept since they have BTN_LEFT).
    if has_rel_x && has_btn_left {
        return DevKind::Mouse;
    }
    // Keyboard: at minimum has KEY_SPACE or KEY_A. This catches:
    //   - laptop integrated keyboards
    //   - external USB keyboards (incl. one half of a split mech)
    //   - bluetooth keyboards advertising a partial keymap
    if let Some(keys) = keys {
        let looks_like_keyboard = keys.contains(Key::KEY_SPACE)
            || keys.contains(Key::KEY_A)
            || keys.contains(Key::KEY_ENTER);
        // Avoid matching power-button / lid-switch nodes that only advertise
        // KEY_POWER / KEY_SLEEP without any letters.
        if looks_like_keyboard {
            return DevKind::Keyboard;
        }
    }
    DevKind::Other
}

/// Diagnostic: enumerate every device, return (path, name, kind) tuples.
pub fn list_devices() -> Vec<(PathBuf, String, DevKind)> {
    let mut out = Vec::new();
    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("(no name)").to_string();
        let kind = device_kind(&dev);
        out.push((path, name, kind));
    }
    out
}

fn run_device(mut dev: Device, kind: DevKind, tx: Sender<InputMsg>) -> std::io::Result<()> {
    loop {
        for ev in dev.fetch_events()? {
            match ev.event_type() {
                EventType::KEY => {
                    if let InputEventKind::Key(key) = ev.kind() {
                        match kind {
                            DevKind::Keyboard => {
                                let msg = match ev.value() {
                                    1 => Some(InputMsg::Press(key)),
                                    0 => Some(InputMsg::Release(key)),
                                    _ => None,
                                };
                                if let Some(m) = msg {
                                    if tx.send(m).is_err() {
                                        return Ok(());
                                    }
                                }
                            }
                            DevKind::Mouse => {
                                let pressed = ev.value() == 1;
                                let released = ev.value() == 0;
                                let btn = match key {
                                    Key::BTN_LEFT => Some(MouseBtn::Left),
                                    Key::BTN_RIGHT => Some(MouseBtn::Right),
                                    Key::BTN_MIDDLE => Some(MouseBtn::Middle),
                                    Key::BTN_SIDE => Some(MouseBtn::Side),
                                    Key::BTN_EXTRA => Some(MouseBtn::Extra),
                                    _ => None,
                                };
                                if let Some(b) = btn {
                                    if pressed || released {
                                        if tx.send(InputMsg::MouseButton(b, pressed)).is_err() {
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                            DevKind::Other => {}
                        }
                    }
                }
                EventType::RELATIVE if kind == DevKind::Mouse => {
                    if let InputEventKind::RelAxis(axis) = ev.kind() {
                        let val = ev.value();
                        let msg = match axis {
                            RelativeAxisType::REL_X => Some(InputMsg::MouseMotion(val, 0)),
                            RelativeAxisType::REL_Y => Some(InputMsg::MouseMotion(0, val)),
                            RelativeAxisType::REL_WHEEL => Some(InputMsg::MouseScroll(val, 0)),
                            RelativeAxisType::REL_HWHEEL => Some(InputMsg::MouseScroll(0, val)),
                            _ => None,
                        };
                        if let Some(m) = msg {
                            if tx.send(m).is_err() {
                                return Ok(());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn watch_hotplug(
    tx: Sender<InputMsg>,
    tracked: Tracked,
    filter: Arc<DeviceFilter>,
) -> std::io::Result<()> {
    let mut inotify = Inotify::init()?;
    inotify
        .watches()
        .add("/dev/input", WatchMask::CREATE | WatchMask::ATTRIB)?;
    let mut buf = [0u8; 4096];
    loop {
        let events = inotify.read_events_blocking(&mut buf)?;
        for event in events {
            let Some(name) = event.name else { continue };
            let name_str = name.to_string_lossy();
            if !name_str.starts_with("event") {
                continue;
            }
            let path = PathBuf::from("/dev/input").join(name.to_owned());
            for _ in 0..3 {
                if let Ok(dev) = Device::open(&path) {
                    try_spawn_device(&path, dev, &tx, &tracked, &filter, true);
                    break;
                }
                thread::sleep(Duration::from_millis(120));
            }
        }
    }
}
