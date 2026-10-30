//! Keyboard, mouse and touchpad input, read straight from `/dev/input` (Wayland
//! doesn't let clients see each other's input). Every device is a source in
//! the caller's event loop, and so is hotplug: no threads.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};
use evdev::{AbsoluteAxisType, Device, InputEventKind, Key, PropType, RelativeAxisType};
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

/// What a device can do. One device can be several things, like a keyboard
/// with mouse keys.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caps {
    pub keyboard: bool,
    pub mouse: bool,
    pub touchpad: bool,
    /// Tablets, touchscreens and virtual absolute pointers.
    pub abs_pointer: bool,
}

impl Caps {
    pub fn of(dev: &Device) -> Self {
        let keys = dev.supported_keys();
        let has = |k: Key| keys.is_some_and(|ks| ks.contains(k));
        let rel = dev.supported_relative_axes();
        let has_rel = |a: RelativeAxisType| rel.is_some_and(|r| r.contains(a));
        let abs = dev.supported_absolute_axes();
        let has_abs = abs.is_some_and(|a| a.contains(AbsoluteAxisType::ABS_X) && a.contains(AbsoluteAxisType::ABS_Y));
        let touchpad = has_abs && has(Key::BTN_TOOL_FINGER) && !dev.properties().contains(PropType::DIRECT);
        Caps {
            // Power buttons and lid switches only report a few keys.
            keyboard: has(Key::KEY_A) || has(Key::KEY_SPACE) || has(Key::KEY_ENTER),
            mouse: has_rel(RelativeAxisType::REL_X) && has_rel(RelativeAxisType::REL_Y)
                || has_rel(RelativeAxisType::REL_WHEEL) && has(Key::BTN_LEFT),
            touchpad,
            abs_pointer: has_abs && !touchpad && (has(Key::BTN_LEFT) || has(Key::BTN_TOUCH) || has(Key::BTN_TOOL_PEN)),
        }
    }

    pub fn pointer(&self) -> bool {
        self.mouse || self.touchpad || self.abs_pointer
    }

    pub fn is_empty(&self) -> bool {
        *self == Caps::default()
    }

    pub fn names(&self) -> Vec<&'static str> {
        [(self.keyboard, "keyboard"), (self.mouse, "mouse"), (self.touchpad, "touchpad"), (self.abs_pointer, "pointer")]
            .into_iter()
            .filter_map(|(on, n)| on.then_some(n))
            .collect()
    }
}

/// Optional name whitelists from the config. Empty = every device.
#[derive(Clone, Default)]
pub struct DeviceFilter {
    pub keyboards: Vec<String>,
    pub pointers: Vec<String>,
}

impl DeviceFilter {
    pub fn from_lists(keyboards: &[String], pointers: &[String]) -> Self {
        let low = |v: &[String]| v.iter().map(|s| s.to_lowercase()).collect();
        Self { keyboards: low(keyboards), pointers: low(pointers) }
    }

    /// Narrows `caps` to what the whitelists allow for a device called `name`.
    pub fn apply(&self, name: &str, mut caps: Caps) -> Caps {
        let n = name.to_lowercase();
        let ok = |list: &[String]| list.is_empty() || list.iter().any(|s| n.contains(s));
        if !ok(&self.keyboards) {
            caps.keyboard = false;
        }
        if !ok(&self.pointers) {
            caps.mouse = false;
            caps.touchpad = false;
            caps.abs_pointer = false;
        }
        caps
    }
}

/// Every input device with what keeby makes of it, for `keeby --list`.
pub fn list_devices() -> Vec<(PathBuf, String, Caps)> {
    let mut out: Vec<_> = evdev::enumerate()
        .map(|(path, dev)| (path, dev.name().unwrap_or("(no name)").to_string(), Caps::of(&dev)))
        .collect();
    out.sort_by_key(|d| d.0.file_name().and_then(|n| n.to_str()?.strip_prefix("event")?.parse::<u32>().ok()));
    out
}

// --------------------------------------------------------------- touchpad

/// Motion counts per millimeter of finger travel (about a mouse's).
const MOTION_PER_MM: f32 = 8.0;
/// Finger travel for one scroll step.
const SCROLL_MM_PER_STEP: f32 = 4.0;
/// A touch shorter than this that barely moved is a tap.
const TAP_TIME: Duration = Duration::from_millis(180);
const TAP_MOVE_MM: f32 = 2.0;

/// Turns raw touchpad contacts into pointer events: motion with one finger,
/// scrolling with two, taps and clickpad clicks as buttons (one finger left,
/// two right, three middle, like libinput's defaults).
struct Touchpad {
    pos: (Option<i32>, Option<i32>),
    last: Option<(i32, i32)>,
    fingers: u8,
    touching: bool,
    /// Units per millimeter on each axis.
    res: (f32, f32),
    scroll_acc: (f32, f32),
    /// Start time and finger travel (mm) of the current touch.
    touch: Option<(Instant, f32)>,
    max_fingers: u8,
    held: Option<MouseBtn>,
}

impl Touchpad {
    fn new(dev: &Device) -> Self {
        let res = dev
            .get_abs_state()
            .map(|st| {
                let r = |axis: AbsoluteAxisType| {
                    let info = st[axis.0 as usize];
                    if info.resolution > 0 {
                        info.resolution as f32
                    } else {
                        // Assume a 100 mm wide pad when the driver doesn't say.
                        (info.maximum - info.minimum).max(1) as f32 / 100.0
                    }
                };
                (r(AbsoluteAxisType::ABS_X), r(AbsoluteAxisType::ABS_Y))
            })
            .unwrap_or((10.0, 10.0));
        Self {
            pos: (None, None),
            last: None,
            fingers: 0,
            touching: false,
            res,
            scroll_acc: (0.0, 0.0),
            touch: None,
            max_fingers: 0,
            held: None,
        }
    }

    fn key(&mut self, key: Key, value: i32, out: &mut Vec<InputMsg>) {
        let tool = match key {
            Key::BTN_TOOL_FINGER => Some(1),
            Key::BTN_TOOL_DOUBLETAP => Some(2),
            Key::BTN_TOOL_TRIPLETAP => Some(3),
            Key::BTN_TOOL_QUADTAP => Some(4),
            Key::BTN_TOOL_QUINTTAP => Some(5),
            _ => None,
        };
        if let Some(n) = tool {
            if value == 1 {
                self.fingers = n;
                self.max_fingers = self.max_fingers.max(n);
            } else if self.fingers == n {
                self.fingers = 0;
            }
            // The reported position jumps when the finger count changes.
            self.last = None;
            return;
        }
        match key {
            Key::BTN_TOUCH => {
                self.touching = value == 1;
                self.last = None;
                if self.touching {
                    self.touch = Some((Instant::now(), 0.0));
                    self.max_fingers = self.fingers.max(1);
                } else if let Some((start, moved)) = self.touch.take() {
                    if start.elapsed() < TAP_TIME && moved < TAP_MOVE_MM && self.held.is_none() {
                        let b = match self.max_fingers {
                            2 => MouseBtn::Right,
                            3 => MouseBtn::Middle,
                            _ => MouseBtn::Left,
                        };
                        out.push(InputMsg::MouseButton(b, true));
                        out.push(InputMsg::MouseButton(b, false));
                    }
                }
            }
            Key::BTN_LEFT | Key::BTN_RIGHT | Key::BTN_MIDDLE => {
                if value == 1 {
                    let b = match (key, self.fingers) {
                        (Key::BTN_RIGHT, _) | (_, 2) => MouseBtn::Right,
                        (Key::BTN_MIDDLE, _) | (_, 3) => MouseBtn::Middle,
                        _ => MouseBtn::Left,
                    };
                    self.held = Some(b);
                    out.push(InputMsg::MouseButton(b, true));
                } else if let Some(b) = self.held.take() {
                    out.push(InputMsg::MouseButton(b, false));
                }
            }
            _ => {}
        }
    }

    fn abs(&mut self, axis: AbsoluteAxisType, value: i32) {
        match axis {
            AbsoluteAxisType::ABS_X => self.pos.0 = Some(value),
            AbsoluteAxisType::ABS_Y => self.pos.1 = Some(value),
            _ => {}
        }
    }

    /// End of a frame: turns the position change into motion or scrolling.
    fn sync(&mut self, out: &mut Vec<InputMsg>) {
        let (Some(x), Some(y)) = self.pos else { return };
        if !self.touching {
            self.last = None;
            return;
        }
        let Some((lx, ly)) = self.last.replace((x, y)) else { return };
        let (dx_mm, dy_mm) = ((x - lx) as f32 / self.res.0, (y - ly) as f32 / self.res.1);
        if let Some(t) = self.touch.as_mut() {
            t.1 += dx_mm.hypot(dy_mm);
        }
        if self.fingers >= 2 {
            // Two-finger scroll, in the usual (not natural) direction.
            self.scroll_acc.0 += dx_mm;
            self.scroll_acc.1 += dy_mm;
            let steps = |acc: &mut f32| {
                let n = (*acc / SCROLL_MM_PER_STEP).trunc();
                *acc -= n * SCROLL_MM_PER_STEP;
                n as i32
            };
            let (h, v) = (steps(&mut self.scroll_acc.0), steps(&mut self.scroll_acc.1));
            if h != 0 || v != 0 {
                out.push(InputMsg::MouseScroll(-v, h));
            }
        } else {
            let (dx, dy) = ((dx_mm * MOTION_PER_MM).round() as i32, (dy_mm * MOTION_PER_MM).round() as i32);
            if dx != 0 || dy != 0 {
                out.push(InputMsg::MouseMotion(dx, dy));
            }
        }
    }
}

// ---------------------------------------------------------------- devices

struct Source {
    dev: Device,
    caps: Caps,
    touchpad: Option<Touchpad>,
    /// Position of an absolute pointer, current and at the last frame.
    abs: (Option<i32>, Option<i32>),
    abs_last: Option<(i32, i32)>,
}

impl Source {
    fn button(&self, key: Key) -> Option<MouseBtn> {
        if !self.caps.pointer() {
            return None;
        }
        Some(match key {
            Key::BTN_LEFT | Key::BTN_TOUCH | Key::BTN_STYLUS => MouseBtn::Left,
            Key::BTN_RIGHT | Key::BTN_STYLUS2 => MouseBtn::Right,
            Key::BTN_MIDDLE => MouseBtn::Middle,
            Key::BTN_SIDE => MouseBtn::Side,
            Key::BTN_EXTRA => MouseBtn::Extra,
            _ => return None,
        })
    }

    /// Reads what's pending. An error means the device is gone.
    fn read(&mut self, out: &mut Vec<InputMsg>) -> std::io::Result<()> {
        let events: Vec<_> = match self.dev.fetch_events() {
            Ok(ev) => ev.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) => return Err(e),
        };
        for ev in events {
            match ev.kind() {
                InputEventKind::Key(key) => {
                    if let Some(tp) = self.touchpad.as_mut() {
                        tp.key(key, ev.value(), out);
                    } else if let Some(b) = self.button(key) {
                        if ev.value() <= 1 {
                            out.push(InputMsg::MouseButton(b, ev.value() == 1));
                        }
                    } else if self.caps.keyboard {
                        match ev.value() {
                            1 => out.push(InputMsg::Press(key)),
                            0 => out.push(InputMsg::Release(key)),
                            _ => {}
                        }
                    }
                }
                InputEventKind::RelAxis(axis) if self.caps.mouse => match axis {
                    RelativeAxisType::REL_X => out.push(InputMsg::MouseMotion(ev.value(), 0)),
                    RelativeAxisType::REL_Y => out.push(InputMsg::MouseMotion(0, ev.value())),
                    RelativeAxisType::REL_WHEEL => out.push(InputMsg::MouseScroll(ev.value(), 0)),
                    RelativeAxisType::REL_HWHEEL => out.push(InputMsg::MouseScroll(0, ev.value())),
                    _ => {}
                },
                InputEventKind::AbsAxis(axis) => {
                    if let Some(tp) = self.touchpad.as_mut() {
                        tp.abs(axis, ev.value());
                    } else if self.caps.abs_pointer {
                        match axis {
                            AbsoluteAxisType::ABS_X => self.abs.0 = Some(ev.value()),
                            AbsoluteAxisType::ABS_Y => self.abs.1 = Some(ev.value()),
                            _ => {}
                        }
                    }
                }
                InputEventKind::Synchronization(_) => {
                    if let Some(tp) = self.touchpad.as_mut() {
                        tp.sync(out);
                    } else if let (Some(x), Some(y)) = self.abs {
                        if let Some((lx, ly)) = self.abs_last.replace((x, y)) {
                            if (x, y) != (lx, ly) {
                                out.push(InputMsg::MouseMotion(x - lx, y - ly));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

type Live = Rc<RefCell<HashSet<PathBuf>>>;

/// Watches every input device the filter allows, and hotplug, in `handle`'s
/// event loop; `sink` gets each event.
pub fn watch<D: 'static>(
    handle: &LoopHandle<'static, D>,
    filter: DeviceFilter,
    sink: impl FnMut(&mut D, InputMsg) + Clone + 'static,
) -> std::io::Result<()> {
    let live: Live = Default::default();
    let mut counts: HashMap<&'static str, usize> = HashMap::new();
    for (path, dev) in evdev::enumerate() {
        if let Some(caps) = add_device(handle, &filter, &live, &path, dev, sink.clone()) {
            for n in caps.names() {
                *counts.entry(n).or_default() += 1;
            }
        }
    }
    let plural = |k: &str| if k == "mouse" { "mice".to_string() } else { format!("{k}s") };
    let mut summary: Vec<String> = counts.iter().map(|(k, v)| if *v > 1 { format!("{v} {}", plural(k)) } else { format!("1 {k}") }).collect();
    summary.sort();
    eprintln!(
        "keeby: reading {} (`keeby --list` for details)",
        if summary.is_empty() { "no devices".to_string() } else { summary.join(", ") }
    );

    // Hotplug: new event nodes, and nodes whose permissions just changed.
    let mut inotify = Inotify::init()?;
    inotify.watches().add("/dev/input", WatchMask::CREATE | WatchMask::ATTRIB)?;
    // SAFETY: a dup of the inotify fd, owned by the source; `inotify` keeps
    // the original.
    let fd = unsafe { OwnedFd::from_raw_fd(libc::dup(inotify.as_raw_fd())) };
    let h2 = handle.clone();
    handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, _: &mut D| {
            let mut buf = [0u8; 4096];
            let paths: Vec<PathBuf> = match inotify.read_events(&mut buf) {
                Ok(evs) => evs
                    .filter_map(|e| e.name.map(|n| Path::new("/dev/input").join(n)))
                    .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("event")))
                    .collect(),
                Err(_) => Vec::new(),
            };
            for path in paths {
                if live.borrow().contains(&path) {
                    continue;
                }
                if let Ok(dev) = Device::open(&path) {
                    if let Some(caps) = add_device(&h2, &filter, &live, &path, dev, sink.clone()) {
                        eprintln!("keeby: added {} ({})", path.display(), caps.names().join("+"));
                    }
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

fn add_device<D: 'static>(
    handle: &LoopHandle<'static, D>,
    filter: &DeviceFilter,
    live: &Live,
    path: &Path,
    dev: Device,
    mut sink: impl FnMut(&mut D, InputMsg) + 'static,
) -> Option<Caps> {
    let caps = filter.apply(dev.name().unwrap_or(""), Caps::of(&dev));
    if caps.is_empty() {
        return None;
    }
    // SAFETY: fcntl and dup on the device's own fd; the dup is owned below.
    let fd = unsafe {
        let raw = dev.as_raw_fd();
        libc::fcntl(raw, libc::F_SETFL, libc::fcntl(raw, libc::F_GETFL) | libc::O_NONBLOCK);
        OwnedFd::from_raw_fd(libc::dup(raw))
    };
    let mut src = Source { touchpad: caps.touchpad.then(|| Touchpad::new(&dev)), dev, caps, abs: (None, None), abs_last: None };
    let (live2, path2) = (live.clone(), path.to_path_buf());
    let mut out = Vec::with_capacity(16);
    handle
        .insert_source(Generic::new(fd, Interest::READ, Mode::Level), move |_, _, data: &mut D| {
            let res = src.read(&mut out);
            for msg in out.drain(..) {
                sink(data, msg);
            }
            if res.is_err() {
                // Unplugged.
                live2.borrow_mut().remove(&path2);
                return Ok(PostAction::Remove);
            }
            Ok(PostAction::Continue)
        })
        .ok()?;
    live.borrow_mut().insert(path.to_path_buf());
    Some(caps)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad() -> Touchpad {
        Touchpad {
            pos: (None, None),
            last: None,
            fingers: 0,
            touching: false,
            res: (10.0, 10.0),
            scroll_acc: (0.0, 0.0),
            touch: None,
            max_fingers: 0,
            held: None,
        }
    }

    fn frame(tp: &mut Touchpad, x: i32, y: i32, out: &mut Vec<InputMsg>) {
        tp.abs(AbsoluteAxisType::ABS_X, x);
        tp.abs(AbsoluteAxisType::ABS_Y, y);
        tp.sync(out);
    }

    #[test]
    fn one_finger_moves() {
        let (mut tp, mut out) = (pad(), Vec::new());
        tp.key(Key::BTN_TOOL_FINGER, 1, &mut out);
        tp.key(Key::BTN_TOUCH, 1, &mut out);
        frame(&mut tp, 100, 100, &mut out);
        frame(&mut tp, 150, 80, &mut out); // 5 mm right, 2 mm up
        assert!(matches!(out[..], [InputMsg::MouseMotion(40, -16)]));
    }

    #[test]
    fn two_fingers_scroll() {
        let (mut tp, mut out) = (pad(), Vec::new());
        tp.key(Key::BTN_TOOL_DOUBLETAP, 1, &mut out);
        tp.key(Key::BTN_TOUCH, 1, &mut out);
        frame(&mut tp, 100, 100, &mut out);
        frame(&mut tp, 100, 190, &mut out); // 9 mm down: two steps
        assert!(matches!(out[..], [InputMsg::MouseScroll(-2, 0)]));
    }

    #[test]
    fn taps_click_by_finger_count() {
        let (mut tp, mut out) = (pad(), Vec::new());
        tp.key(Key::BTN_TOOL_DOUBLETAP, 1, &mut out);
        tp.key(Key::BTN_TOUCH, 1, &mut out);
        tp.key(Key::BTN_TOUCH, 0, &mut out);
        assert!(matches!(out[..], [InputMsg::MouseButton(MouseBtn::Right, true), InputMsg::MouseButton(MouseBtn::Right, false)]));
    }

    #[test]
    fn clickpad_with_two_fingers_is_right() {
        let (mut tp, mut out) = (pad(), Vec::new());
        tp.key(Key::BTN_TOOL_DOUBLETAP, 1, &mut out);
        tp.key(Key::BTN_LEFT, 1, &mut out);
        tp.key(Key::BTN_LEFT, 0, &mut out);
        assert!(matches!(out[..], [InputMsg::MouseButton(MouseBtn::Right, true), InputMsg::MouseButton(MouseBtn::Right, false)]));
    }

    #[test]
    fn whitelist_narrows_caps() {
        let f = DeviceFilter::from_lists(&["AT Translated".into()], &[]);
        let all = Caps { keyboard: true, mouse: true, ..Default::default() };
        assert_eq!(f.apply("Corne", all), Caps { mouse: true, ..Default::default() });
        assert_eq!(f.apply("AT Translated Set 2", all), all);
    }
}
