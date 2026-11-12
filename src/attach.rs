//! `fluteterm orca-attach <handle>`: a live view of an Orca terminal inside a
//! fluteterm tab. The screen is mirrored with `orca terminal read --screen`
//! (~4 fps) and keystrokes are forwarded with `orca terminal send`.
//! `ctrl-]` detaches; the agent keeps running in Orca.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::util;

struct RawMode(libc::termios);

impl RawMode {
    fn enter() -> Option<Self> {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return None;
            }
            let orig = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Some(Self(orig))
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

fn term_size() -> (usize, usize) {
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return (ws.ws_col as usize, ws.ws_row as usize);
        }
    }
    (80, 24)
}

fn read_screen(orca: &str, handle: &str) -> Result<(Vec<String>, String), String> {
    let out = Command::new(orca)
        .args(["terminal", "read", "--terminal", handle, "--screen", "--json"])
        .env("PATH", util::augmented_path())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|_| String::from_utf8_lossy(&out.stdout).trim().to_string())?;
    if v["ok"].as_bool() == Some(false) {
        return Err(v["error"]["message"].as_str().or_else(|| v["error"].as_str()).unwrap_or("read failed").to_string());
    }
    let t = &v["result"]["terminal"];
    let lines = t["tail"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
    Ok((lines, t["status"].as_str().unwrap_or("running").to_string()))
}

pub fn run(handle: &str, title: &str) -> anyhow::Result<()> {
    let orca = util::which("orca").map(|p| p.display().to_string()).unwrap_or_else(|| "orca".into());
    let _raw = RawMode::enter();
    let mut out = std::io::stdout();
    // Alternate screen, hidden cursor.
    write!(out, "\x1b[?1049h\x1b[?25l")?;
    out.flush()?;

    let stop = Arc::new(AtomicBool::new(false));
    let pending: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    // Keyboard → pending buffer (ctrl-] detaches).
    {
        let stop = stop.clone();
        let pending = pending.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let mut stdin = std::io::stdin();
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if buf[..n].contains(&0x1d) {
                    stop.store(true, Ordering::SeqCst);
                    break;
                }
                pending.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
    }
    // Pending buffer → orca terminal send (batched).
    {
        let stop = stop.clone();
        let pending = pending.clone();
        let orca = orca.clone();
        let handle = handle.to_string();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let data: Vec<u8> = std::mem::take(&mut *pending.lock().unwrap());
                if !data.is_empty() {
                    let text = String::from_utf8_lossy(&data).replace('\0', "");
                    let _ = Command::new(&orca)
                        .args(["terminal", "send", "--terminal", &handle, "--text", &text, "--json"])
                        .env("PATH", util::augmented_path())
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                } else {
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
        });
    }

    let mut last: Vec<String> = vec![];
    let mut last_size = (0, 0);
    let mut errors = 0;
    let mut status_msg = String::new();
    while !stop.load(Ordering::SeqCst) {
        let (cols, rows) = term_size();
        match read_screen(&orca, handle) {
            Ok((lines, status)) => {
                errors = 0;
                if status != "running" {
                    status_msg = format!("terminal {status}");
                    stop.store(true, Ordering::SeqCst);
                }
                if lines != last || (cols, rows) != last_size {
                    let mut frame = String::from("\x1b[H");
                    let header = format!(" ◆ orca · {title} — live view · typing goes to the agent · ctrl-] detach ");
                    let header: String = header.chars().take(cols).collect();
                    frame.push_str(&format!("\x1b[7m{header:<cols$}\x1b[0m\r\n"));
                    let body = rows.saturating_sub(1);
                    let start = lines.len().saturating_sub(body);
                    for i in 0..body {
                        frame.push_str("\x1b[2K");
                        if let Some(l) = lines.get(start + i) {
                            let l: String = l.chars().take(cols).collect();
                            frame.push_str(&l);
                        }
                        if i + 1 < body {
                            frame.push_str("\r\n");
                        }
                    }
                    out.write_all(frame.as_bytes())?;
                    out.flush()?;
                    last = lines;
                    last_size = (cols, rows);
                }
            }
            Err(e) => {
                errors += 1;
                if errors >= 5 {
                    status_msg = format!("lost the Orca terminal: {e}");
                    break;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    write!(out, "\x1b[?25h\x1b[?1049l")?;
    out.flush()?;
    drop(_raw);
    if !status_msg.is_empty() {
        println!("fluteterm: {status_msg}");
    } else {
        println!("detached from Orca terminal {handle} (the agent keeps running in Orca)");
    }
    Ok(())
}
