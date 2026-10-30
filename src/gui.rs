//! `keeby config`: edits ~/.config/keeby/config.conf from a page in the
//! browser. A tiny HTTP server on localhost serves the page and reads and
//! writes the file; a random token keeps other pages from talking to it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

const PAGE: &str = include_str!("../assets/config.html");

fn token() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn respond(s: &mut TcpStream, status: &str, kind: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = s.write_all(head.as_bytes());
    let _ = s.write_all(body);
}

/// Stops the running keeby (not this process) and starts a fresh one, so the
/// overlay picks up the new settings. The pointer reads the file on every run
/// already.
fn restart_daemon() -> Result<(), String> {
    let me = std::process::id();
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir("/proc").map_err(|e| e.to_string())?.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
        if pid == me {
            continue;
        }
        let Ok(cmd) = std::fs::read(entry.path().join("cmdline")) else { continue };
        let args: Vec<&[u8]> = cmd.split(|c| *c == 0).filter(|a| !a.is_empty()).collect();
        // The daemon is `keeby` with no subcommand.
        if args.len() == 1 && args[0].rsplit(|c| *c == b'/').next() == Some(b"keeby") {
            // SAFETY: plain kill(2).
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    std::process::Command::new("setsid")
        .arg("-f")
        .arg(&exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start keeby: {e}"))?;
    Ok(())
}

/// Serves the page until it says it's done. Returns the exit status.
pub fn run(open_browser: bool) -> i32 {
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("keeby: can't start the config page: {e}");
            return 1;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let tok = token();
    let url = format!("http://127.0.0.1:{port}/?t={tok}");
    let path = crate::config::config_path();
    if !path.exists() {
        crate::config::Config::load(); // writes the starter config
    }
    println!("keeby config: {url}\n(close the page with Done, or Ctrl+C here)");
    if open_browser {
        let _ = std::process::Command::new("xdg-open")
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        let mut reader = BufReader::new(match s.try_clone() {
            Ok(c) => c,
            Err(_) => continue,
        });
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                break;
            }
            if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len.min(1 << 20)];
        if reader.read_exact(&mut body).is_err() {
            continue;
        }
        let (route, query) = target.split_once('?').unwrap_or((target, ""));
        let authed = query.split('&').any(|kv| kv == format!("t={tok}"));
        if !authed {
            respond(&mut s, "403 Forbidden", "text/plain", b"open the link keeby config printed");
            continue;
        }
        match (method, route) {
            ("GET", "/") => respond(&mut s, "200 OK", "text/html; charset=utf-8", PAGE.replace("__TOKEN__", &tok).as_bytes()),
            ("GET", "/api/config") => {
                let text = std::fs::read(&path).unwrap_or_default();
                respond(&mut s, "200 OK", "text/plain; charset=utf-8", &text);
            }
            ("POST", "/api/config") => {
                // Write next to it and rename, so a crash can't leave half a file.
                let tmp = path.with_extension("conf.tmp");
                match std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &path)) {
                    Ok(()) => respond(&mut s, "200 OK", "text/plain", b"saved"),
                    Err(e) => respond(&mut s, "500 Internal Server Error", "text/plain", e.to_string().as_bytes()),
                }
            }
            ("POST", "/api/restart") => match restart_daemon() {
                Ok(()) => respond(&mut s, "200 OK", "text/plain", b"restarted"),
                Err(e) => respond(&mut s, "500 Internal Server Error", "text/plain", e.as_bytes()),
            },
            ("POST", "/api/quit") => {
                respond(&mut s, "200 OK", "text/plain", b"bye");
                return 0;
            }
            _ => respond(&mut s, "404 Not Found", "text/plain", b"not found"),
        }
    }
    0
}
