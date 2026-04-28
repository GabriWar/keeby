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
enum DevKind {
    Keyboard,
    Mouse,
    Other,
}

/// Set of currently-watched device paths so hotplug events don't double-spawn.
type Tracked = Arc<Mutex<std::collections::HashSet<PathBuf>>>;

pub fn spawn(tx: Sender<InputMsg>) {
    let tracked: Tracked = Arc::new(Mutex::new(Default::default()));

    // Enumerate already-attached devices.
    for (path, dev) in evdev::enumerate() {
        try_spawn_device(&path, dev, &tx, &tracked);
    }

    // Watch /dev/input for new devices (replugs, USB hotplug, virtual KVM).
    let tx_watch = tx.clone();
    let tracked_watch = tracked.clone();
    thread::Builder::new()
        .name("evdev-hotplug".into())
        .spawn(move || {
            if let Err(e) = watch_hotplug(tx_watch, tracked_watch) {
                eprintln!("keeby: hotplug watcher exited: {e}");
            }
        })
        .expect("spawn hotplug watcher");
}

fn try_spawn_device(path: &Path, dev: Device, tx: &Sender<InputMsg>, tracked: &Tracked) {
    let kind = device_kind(&dev);
    if kind == DevKind::Other {
        return;
    }
    {
        let mut set = tracked.lock().unwrap();
        if !set.insert(path.to_path_buf()) {
            return; // already watching
        }
    }
    let tx = tx.clone();
    let tracked = tracked.clone();
    let path_owned = path.to_path_buf();
    thread::Builder::new()
        .name(format!("evdev-{}", path.display()))
        .spawn(move || {
            let _ = run_device(dev, kind, tx);
            // On exit (replug, EOF, perm-change): drop from tracked set so the
            // next hotplug for the same path is allowed to re-spawn.
            tracked.lock().unwrap().remove(&path_owned);
        })
        .expect("spawn evdev thread");
}

fn device_kind(dev: &Device) -> DevKind {
    let has_rel_x = dev
        .supported_relative_axes()
        .map(|r| r.contains(RelativeAxisType::REL_X))
        .unwrap_or(false);
    let keys = dev.supported_keys();
    let has_btn_left = keys.map(|k| k.contains(Key::BTN_LEFT)).unwrap_or(false);
    if has_rel_x && has_btn_left {
        return DevKind::Mouse;
    }
    if let Some(keys) = keys {
        if keys.contains(Key::KEY_A) && keys.contains(Key::KEY_Z) && keys.contains(Key::KEY_LEFTCTRL) {
            return DevKind::Keyboard;
        }
    }
    DevKind::Other
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

fn watch_hotplug(tx: Sender<InputMsg>, tracked: Tracked) -> std::io::Result<()> {
    let mut inotify = Inotify::init()?;
    // CREATE: new event* file appears (USB plug-in).
    // ATTRIB: udev usually sets perms in a separate step after CREATE — wait for it.
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
            // Re-check briefly because udev often sets perms after the inode appears.
            for _ in 0..3 {
                if let Ok(dev) = Device::open(&path) {
                    try_spawn_device(&path, dev, &tx, &tracked);
                    break;
                }
                thread::sleep(Duration::from_millis(120));
            }
        }
    }
}
