// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! Exclusive keyboard grab straight from `/dev/input`, so the overlay can read
//! keys without the compositor moving the keyboard focus (which would close
//! open menus).

use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const EV_KEY: u16 = 0x01;
const KEY_A: usize = 30;
const KEY_Z: usize = 44;
const KEY_MAX: usize = 0x2ff;
const KEY_BYTES: usize = KEY_MAX / 8 + 1;

// ioctl request numbers for evdev (see linux/input.h).
const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'E' as u64) << 8) | nr
}
const EVIOCGRAB: u64 = ioc(1, 0x90, std::mem::size_of::<libc::c_int>() as u64);
const EVIOCGKEY: u64 = ioc(2, 0x18, KEY_BYTES as u64);
const EVIOCGBIT_KEY: u64 = ioc(2, 0x20 + EV_KEY as u64, KEY_BYTES as u64);

#[repr(C)]
struct InputEvent {
    time: libc::timeval,
    kind: u16,
    code: u16,
    value: i32,
}

pub struct KeyEvent {
    pub code: u16,
    /// 1 press, 0 release, 2 autorepeat.
    pub value: i32,
}

pub struct Keyboard {
    pub fd: OwnedFd,
}

fn test_bit(bits: &[u8], n: usize) -> bool {
    bits[n / 8] & (1 << (n % 8)) != 0
}

/// Opens every input device that looks like a keyboard (has A and Z).
pub fn open_keyboards() -> Vec<Keyboard> {
    let Ok(dir) = std::fs::read_dir("/dev/input") else { return Vec::new() };
    let mut out = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("event") {
            continue;
        }
        let path = std::ffi::CString::new(entry.path().as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: plain open(2) of a device node; the fd is owned below.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            continue;
        }
        // SAFETY: `fd` was just opened and isn't owned by anything else.
        let fd = unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let mut bits = [0u8; KEY_BYTES];
        // SAFETY: EVIOCGBIT fills at most `KEY_BYTES` bytes.
        let ok = unsafe { libc::ioctl(fd.as_raw_fd(), EVIOCGBIT_KEY as _, bits.as_mut_ptr()) } >= 0;
        if ok && test_bit(&bits, KEY_A) && test_bit(&bits, KEY_Z) {
            out.push(Keyboard { fd });
        }
    }
    out
}

impl Keyboard {
    pub fn any_key_down(&self) -> bool {
        let mut bits = [0u8; KEY_BYTES];
        // SAFETY: EVIOCGKEY fills at most `KEY_BYTES` bytes.
        let ok = unsafe { libc::ioctl(self.fd.as_raw_fd(), EVIOCGKEY as _, bits.as_mut_ptr()) } >= 0;
        ok && bits.iter().any(|b| *b != 0)
    }

    pub fn set_grab(&self, on: bool) -> bool {
        set_grab(self.fd.as_raw_fd(), on)
    }

    /// Reads every pending key event.
    pub fn read(&self, out: &mut Vec<KeyEvent>) -> std::io::Result<()> {
        let mut buf: [InputEvent; 32] = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: reading whole `input_event` structs into `buf`.
            let n = unsafe {
                libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf))
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                return if err.kind() == std::io::ErrorKind::WouldBlock { Ok(()) } else { Err(err) };
            }
            if n == 0 {
                return Ok(());
            }
            let count = n as usize / std::mem::size_of::<InputEvent>();
            out.extend(buf[..count].iter().filter(|e| e.kind == EV_KEY).map(|e| KeyEvent { code: e.code, value: e.value }));
        }
    }
}

fn set_grab(fd: RawFd, on: bool) -> bool {
    // SAFETY: EVIOCGRAB takes an int by value.
    unsafe { libc::ioctl(fd, EVIOCGRAB as _, on as libc::c_int) >= 0 }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Releases the grab from a separate thread if the pointer loop stops
/// ticking, so a hang can never leave the user without a keyboard.
#[derive(Clone)]
pub struct Watchdog {
    heartbeat: Arc<AtomicU64>,
    grabbed: Arc<Mutex<Vec<RawFd>>>,
    armed: Arc<AtomicBool>,
}

impl Watchdog {
    pub fn spawn() -> Self {
        let w = Self {
            heartbeat: Arc::new(AtomicU64::new(now_ms())),
            grabbed: Arc::new(Mutex::new(Vec::new())),
            armed: Arc::new(AtomicBool::new(false)),
        };
        let wd = w.clone();
        std::thread::Builder::new()
            .name("pointer-watchdog".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(500));
                if wd.armed.load(Ordering::Acquire)
                    && now_ms().saturating_sub(wd.heartbeat.load(Ordering::Acquire)) > 3000
                {
                    for fd in wd.grabbed.lock().unwrap().drain(..) {
                        set_grab(fd, false);
                    }
                    wd.armed.store(false, Ordering::Release);
                    eprintln!("keeby: pointer stopped responding, keyboard released");
                }
            })
            .expect("spawn watchdog");
        w
    }

    pub fn tick(&self) {
        self.heartbeat.store(now_ms(), Ordering::Release);
    }

    pub fn arm(&self, fds: Vec<RawFd>) {
        self.tick();
        *self.grabbed.lock().unwrap() = fds;
        self.armed.store(true, Ordering::Release);
    }

    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
        self.grabbed.lock().unwrap().clear();
    }
}
