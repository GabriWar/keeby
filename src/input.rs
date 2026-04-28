use std::sync::mpsc::Sender;
use std::thread;

use evdev::{Device, EventType, InputEventKind, Key, RelativeAxisType};

#[derive(Debug, Clone, Copy)]
pub enum InputMsg {
    Press(Key),
    Release(Key),
    MouseMotion(i32, i32),
    MouseButton(MouseBtn, bool),
    MouseScroll(i32, i32), // (vertical, horizontal); +up/right, -down/left
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Middle,
    Right,
    Side,
    Extra,
}

pub fn spawn(tx: Sender<InputMsg>) {
    for (path, dev) in evdev::enumerate() {
        let kind = device_kind(&dev);
        if kind == DevKind::Other {
            continue;
        }
        let tx = tx.clone();
        let path_dbg = path.clone();
        thread::Builder::new()
            .name(format!("evdev-{}", path.display()))
            .spawn(move || {
                if let Err(e) = run_device(dev, kind, tx) {
                    eprintln!("evdev {} ended: {e}", path_dbg.display());
                }
            })
            .expect("spawn evdev thread");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DevKind {
    Keyboard,
    Mouse,
    Other,
}

fn device_kind(dev: &Device) -> DevKind {
    // Mouse: has REL_X + a mouse button.
    let has_rel_x = dev
        .supported_relative_axes()
        .map(|r| r.contains(RelativeAxisType::REL_X))
        .unwrap_or(false);
    let keys = dev.supported_keys();
    let has_btn_left = keys.map(|k| k.contains(Key::BTN_LEFT)).unwrap_or(false);
    if has_rel_x && has_btn_left {
        return DevKind::Mouse;
    }
    // Keyboard: alpha keys + a modifier.
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
