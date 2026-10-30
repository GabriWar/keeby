// SPDX-License-Identifier: GPL-3.0-only
// Ported from wl-kbptr (https://github.com/moverest/wl-kbptr) by moverest.
//! `keeby point`: move, click and drag the pointer from the keyboard (the
//! wl-kbptr port). The keeby daemon keeps a Wayland connection, fonts and
//! buffers warm and serves runs over a socket; without it, a run happens in
//! process.

mod config;
mod detect;
mod draw;
mod grab;
mod label;
mod modes;
mod session;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use wayland_client::{globals::registry_queue_init, Connection};

use crate::input::InputMsg;
pub use config::PointerConfig;
use modes::Rect;
use session::{Outcome, PointerApp, Request};

const USAGE: &str = "\
keeby point [OPTION...]

 -h, --help          show this help
 --help-config       show help on configuration
 -v, --version       show version
 -c, --config=FILE   load a wl-kbptr style configuration file
 -r, --restrict=AREA restrict to given area (wxh+x+y)
 -o, --option        set configuration option (section.key=value)
 -O, --output        specify display output to use
 -p, --only-print    only print, don't move the cursor or click
 -d, --drag          perform a click-and-drag between two selections
 -D, --double-click  double click instead of a single click
";

fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("keeby-point.sock")
}

enum Parsed {
    Run(Request),
    Done(Outcome),
}

fn parse_rect(s: &str) -> Option<Rect> {
    let (w, rest) = s.split_once('x')?;
    let mut it = rest.split('+');
    Some(Rect {
        w: w.parse().ok()?,
        h: it.next()?.parse().ok()?,
        x: it.next()?.parse().ok()?,
        y: it.next()?.parse().ok()?,
    })
}

/// Parses `keeby point` arguments, wl-kbptr's options included. Settings
/// apply in this order: theme defaults, keeby config, `-c` file, `-o`.
fn parse_args(args: &[String], stdin: Vec<u8>) -> Parsed {
    let cfg_file = crate::config::Config::load();
    let mut cfg = PointerConfig::new(&cfg_file.theme);
    for (k, v) in &cfg_file.pointer_settings {
        if let Err(e) = cfg.set(k, v) {
            eprintln!("keeby: config: {e}");
        }
    }
    let err = |m: String| Parsed::Done(Outcome { stderr: format!("{m}\n"), code: 1, ..Default::default() });
    let (mut only_print, mut output, mut restrict, mut drag, mut double) = (false, None, None, false, false);
    let (mut ini, mut opts) = (None, Vec::new());

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        // Accepts `-oX`, `-o X`, `--option=X` and `--option X`.
        let mut value = |short: &str, long: &str| -> Option<Option<String>> {
            if a == short || a == long {
                i += 1;
                return Some(args.get(i).cloned());
            }
            if let Some(v) = a.strip_prefix(&format!("{long}=")) {
                return Some(Some(v.to_string()));
            }
            if short.len() == 2 && a.starts_with(short) && a.len() > 2 {
                return Some(Some(a[2..].to_string()));
            }
            None
        };
        if let Some(v) = value("-o", "--option") {
            let Some(v) = v else { return err("Option -o needs a value.".into()) };
            opts.push(v);
        } else if let Some(v) = value("-c", "--config") {
            ini = v;
        } else if let Some(v) = value("-O", "--output") {
            output = v;
        } else if let Some(v) = value("-r", "--restrict") {
            match v.as_deref().and_then(parse_rect) {
                Some(r) => restrict = Some(r),
                None => return err("Could not parse --restrict argument.".into()),
            }
        } else {
            match a {
                "-h" | "--help" => return Parsed::Done(Outcome { stdout: USAGE.into(), ..Default::default() }),
                "--help-config" => return Parsed::Done(Outcome { stdout: config::HELP.into(), ..Default::default() }),
                "-v" | "--version" => {
                    let feat = if detect::AVAILABLE { " (opencv)" } else { "" };
                    return Parsed::Done(Outcome { stdout: format!("keeby {}{feat}\n", env!("CARGO_PKG_VERSION")), ..Default::default() });
                }
                "-p" | "--only-print" => only_print = true,
                "-d" | "--drag" => drag = true,
                "-D" | "--double-click" => double = true,
                // Combined short flags like `-dp`.
                s if s.starts_with('-') && !s.starts_with("--") && s[1..].chars().all(|c| "pdD".contains(c)) => {
                    only_print |= s.contains('p');
                    drag |= s.contains('d');
                    double |= s.contains('D');
                }
                _ => return err("Unknown argument.".into()),
            }
        }
        i += 1;
    }

    if let Some(path) = ini {
        if let Err(e) = cfg.load_ini(&path) {
            return err(format!("{e}\nFailed to read configuration file."));
        }
    }
    for o in &opts {
        if let Err(e) = cfg.set_option(o) {
            return err(e);
        }
    }
    Parsed::Run(Request { cfg, only_print, output, restrict, drag, double_click: double, stdin })
}

/// Floating mode's `stdin` source reads `wxh+x+y` areas, one per line.
fn read_stdin_if_needed(req: &Request) -> Vec<u8> {
    let mut buf = Vec::new();
    let floating = req.cfg.general.modes.split(',').any(|m| m == "floating");
    if floating && req.cfg.floating.source == config::FloatingSource::Stdin {
        let _ = std::io::stdin().read_to_end(&mut buf);
    }
    buf
}

/// Wire format: request is `args\0...\0\0` followed by stdin bytes; reply is
/// `code\n` then stdout, `\0`, stderr.
fn encode_reply(o: &Outcome) -> Vec<u8> {
    let mut v = format!("{}\n", o.code).into_bytes();
    v.extend_from_slice(o.stdout.as_bytes());
    v.push(0);
    v.extend_from_slice(o.stderr.as_bytes());
    v
}

fn print_outcome(o: &Outcome) -> i32 {
    print!("{}", o.stdout);
    eprint!("{}", o.stderr);
    let _ = std::io::stdout().flush();
    o.code
}

/// Entry point of `keeby point`. Returns the exit status.
pub fn run_cli(args: &[String]) -> i32 {
    // Answer help and bad arguments without touching the daemon.
    let req = match parse_args(args, Vec::new()) {
        Parsed::Done(o) => return print_outcome(&o),
        Parsed::Run(r) => r,
    };
    let stdin = read_stdin_if_needed(&req);

    if let Ok(mut s) = UnixStream::connect(socket_path()) {
        let mut msg = Vec::new();
        for a in args {
            msg.extend_from_slice(a.as_bytes());
            msg.push(0);
        }
        msg.push(0);
        msg.extend_from_slice(&stdin);
        if s.write_all(&msg).is_ok() && s.shutdown(std::net::Shutdown::Write).is_ok() {
            let mut reply = Vec::new();
            if s.read_to_end(&mut reply).is_ok() {
                if let Some(o) = decode_reply(&reply) {
                    return print_outcome(&o);
                }
            }
        }
        eprintln!("keeby: daemon didn't answer, running standalone");
    }

    // No daemon: same code, in this process.
    let req = Request { stdin, ..req };
    let outcome = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out2 = outcome.clone();
    match run_loop(None, None, Some((req, Box::new(move |o| *out2.borrow_mut() = Some(o))))) {
        Ok(()) => print_outcome(&outcome.borrow_mut().take().unwrap_or_default()),
        Err(e) => print_outcome(&Outcome { stderr: format!("{e}\n"), code: 1, ..Default::default() }),
    }
}

fn decode_reply(b: &[u8]) -> Option<Outcome> {
    let nl = b.iter().position(|c| *c == b'\n')?;
    let code = std::str::from_utf8(&b[..nl]).ok()?.parse().ok()?;
    let rest = &b[nl + 1..];
    let z = rest.iter().position(|c| *c == 0)?;
    Some(Outcome {
        code,
        stdout: String::from_utf8_lossy(&rest[..z]).into_owned(),
        stderr: String::from_utf8_lossy(&rest[z + 1..]).into_owned(),
    })
}

fn handle_client(app: &mut PointerApp, mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut msg = Vec::new();
    if stream.read_to_end(&mut msg).is_err() {
        return;
    }
    let end = msg.windows(2).position(|w| w == [0, 0]).map(|p| p + 1).unwrap_or(0);
    let args: Vec<String> = msg[..end]
        .split(|c| *c == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    let stdin = msg.get(end + 1..).unwrap_or_default().to_vec();
    if args.first().map(String::as_str) == Some("--overlay") {
        let show = match args.get(1).map(String::as_str) {
            Some("on") => Some(true),
            Some("off") => Some(false),
            _ => None,
        };
        let ok = app.send_overlay(show);
        let _ = stream.write_all(&encode_reply(&Outcome { code: if ok { 0 } else { 1 }, ..Default::default() }));
        return;
    }
    let reply = move |o: Outcome| {
        let mut stream = stream;
        let _ = stream.write_all(&encode_reply(&o));
    };
    match parse_args(&args, stdin) {
        Parsed::Done(o) => reply(o),
        Parsed::Run(req) => app.start(req, Box::new(reply)),
    }
}

/// Runs the pointer loop. As a daemon it serves `keeby point` over a socket
/// until the process ends; with `once`, it runs that request and returns.
fn run_loop(
    listener: Option<UnixListener>,
    pills: Option<Sender<InputMsg>>,
    once: Option<(Request, session::ReplyFn)>,
) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("Failed to connect to Wayland compositor: {e}"))?;
    let (globals, mut queue) = registry_queue_init::<PointerApp>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let mut event_loop: EventLoop<'static, PointerApp> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let handle = event_loop.handle();
    let mut app = PointerApp::new(&conn, &globals, &qh, handle.clone(), pills)?;

    // Outputs (and their xdg-output details) and the keymap, before the
    // first run.
    for _ in 0..2 {
        queue.roundtrip(&mut app).map_err(|e| e.to_string())?;
    }
    WaylandSource::new(conn.clone(), queue).insert(handle.clone()).map_err(|e| e.to_string())?;

    let daemon = listener.is_some();
    if let Some(l) = listener {
        l.set_nonblocking(true).map_err(|e| e.to_string())?;
        handle
            .insert_source(Generic::new(l, Interest::READ, Mode::Level), |_, l, app: &mut PointerApp| {
                while let Ok((stream, _)) = l.accept() {
                    let _ = stream.set_nonblocking(false);
                    handle_client(app, stream);
                }
                Ok(PostAction::Continue)
            })
            .map_err(|e| e.to_string())?;
    }
    if let Some((req, reply)) = once {
        app.start(req, reply);
    }

    loop {
        event_loop.dispatch(Some(Duration::from_millis(500)), &mut app).map_err(|e| e.to_string())?;
        app.tick();
        if !daemon && !app.busy() {
            return Ok(());
        }
    }
}

/// `keeby overlay [toggle|on|off]`: asks the running keeby to hide or show
/// its overlay. Exits 1 when keeby isn't running.
pub fn overlay_cli(cmd: Option<&str>) -> i32 {
    let cmd = cmd.unwrap_or("toggle");
    if !matches!(cmd, "toggle" | "on" | "off") {
        eprintln!("usage: keeby overlay [toggle|on|off]");
        return 1;
    }
    let Ok(mut s) = UnixStream::connect(socket_path()) else {
        eprintln!("keeby isn't running");
        return 1;
    };
    let msg = format!("--overlay\0{cmd}\0\0");
    let mut reply = Vec::new();
    if s.write_all(msg.as_bytes()).is_err() || s.shutdown(std::net::Shutdown::Write).is_err() || s.read_to_end(&mut reply).is_err() {
        return 1;
    }
    decode_reply(&reply).map_or(1, |o| o.code)
}

/// Starts the pointer service inside the keeby daemon, on its own thread.
/// Keys typed into the overlay are forwarded to `pills`.
pub fn spawn_daemon(pills: Sender<InputMsg>) {
    let path = socket_path();
    // Only replace a socket nobody answers on.
    if UnixStream::connect(&path).is_ok() {
        eprintln!("keeby: pointer service already running at {}", path.display());
        return;
    }
    let _ = std::fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("keeby: pointer service disabled: {e}");
            return;
        }
    };
    std::thread::Builder::new()
        .name("pointer".into())
        .spawn(move || {
            if let Err(e) = run_loop(Some(listener), Some(pills), None) {
                eprintln!("keeby: pointer service stopped: {e}");
            }
        })
        .expect("spawn pointer thread");
}
