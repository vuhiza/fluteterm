//! Newline-delimited JSON over a unix socket at ~/.config/fluteterm/fluteterm.sock.
//!
//! Every request is one JSON object with a "cmd" field; the reply is one JSON
//! line. This is the extension surface for anything outside the process:
//! the `fluteterm` CLI, Claude Code hooks, Orca automations, shell scripts.
//!
//! Commands (see README "IPC"):
//!   {"cmd":"toggle"} {"cmd":"show"} {"cmd":"hide"} {"cmd":"ping"}
//!   {"cmd":"notify","title":"..","body":"..","tab":3}
//!   {"cmd":"open","profile":"claude","cwd":"/path","command":["htop"]}
//!   {"cmd":"send","tab":3,"text":"ls\n"}
//!   {"cmd":"list"}
//!   {"cmd":"event","source":"claude","tab":3,"payload":{...}}
//!   {"cmd":"status","key":"build","text":"✓ green"}   (custom status segment; empty text clears)
//!   {"cmd":"theme","name":"clay"}
//!   {"cmd":"reload"}

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::{Value, json};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;
use crate::util;

pub struct Request {
    pub body: Value,
    pub reply: mpsc::Sender<Value>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Request({})", self.body)
    }
}

static SOCKET_INODE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Remove the socket on exit — but only if it is still ours (a newer instance
/// may already have replaced it).
pub fn cleanup() {
    use std::os::unix::fs::MetadataExt;
    let path = util::socket_path();
    let ours = SOCKET_INODE.load(std::sync::atomic::Ordering::SeqCst);
    if let Ok(meta) = std::fs::metadata(&path) {
        if ours != 0 && meta.ino() == ours {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Start the server thread. Fails if another instance is already serving.
pub fn serve(proxy: EventLoopProxy<UserEvent>) -> anyhow::Result<()> {
    let path = util::socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            anyhow::bail!("another fluteterm instance is running");
        }
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    if let Ok(meta) = std::fs::metadata(&path) {
        use std::os::unix::fs::MetadataExt;
        SOCKET_INODE.store(meta.ino(), std::sync::atomic::Ordering::SeqCst);
    }
    std::thread::Builder::new().name("fluteterm-ipc".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let proxy = proxy.clone();
            std::thread::spawn(move || handle(stream, proxy));
        }
    })?;
    Ok(())
}

fn handle(stream: UnixStream, proxy: EventLoopProxy<UserEvent>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(body) => {
                let (tx, rx) = mpsc::channel();
                if proxy.send_event(UserEvent::Ipc(Request { body, reply: tx })).is_err() {
                    json!({"ok": false, "error": "app is shutting down"})
                } else {
                    rx.recv_timeout(Duration::from_secs(3)).unwrap_or(json!({"ok": false, "error": "timeout"}))
                }
            }
            Err(e) => json!({"ok": false, "error": format!("bad json: {e}")}),
        };
        let mut s = reply.to_string();
        s.push('\n');
        if writer.write_all(s.as_bytes()).is_err() {
            break;
        }
    }
}

/// Client: send one request and wait for the reply.
pub fn request(body: &Value) -> anyhow::Result<Value> {
    let mut stream = UnixStream::connect(util::socket_path())?;
    stream.set_read_timeout(Some(Duration::from_secs(4)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut s = body.to_string();
    s.push('\n');
    stream.write_all(s.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

pub fn is_running() -> bool {
    request(&json!({"cmd": "ping"})).is_ok()
}
