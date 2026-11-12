//! Small helpers: paths, processes, clipboard, time.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/.config/fluteterm`, the home of config.toml, themes/ and the IPC socket.
pub fn config_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FLUTETERM_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    home().join(".config").join("fluteterm")
}

/// One-time move of the pre-rename config folder (`~/.config/drpdown`).
pub fn migrate_legacy_config() {
    if std::env::var("FLUTETERM_CONFIG_DIR").is_ok() {
        return;
    }
    let old = home().join(".config").join("drpdown");
    let new = config_dir();
    if !old.is_dir() || new.exists() {
        return;
    }
    // Never move it from under a still-running old instance.
    if std::os::unix::net::UnixStream::connect(old.join("drpdown.sock")).is_ok() {
        return;
    }
    if std::fs::rename(&old, &new).is_ok() {
        // Stale runtime files from the old name.
        let _ = std::fs::remove_file(new.join("drpdown.sock"));
        let _ = std::fs::rename(new.join("drpdown.log"), new.join("fluteterm.log"));
    }
}

pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("FLUTETERM_SOCKET") {
        return PathBuf::from(p);
    }
    config_dir().join("fluteterm.sock")
}

pub fn log_path() -> PathBuf {
    config_dir().join("fluteterm.log")
}

/// Replace the home directory prefix with `~`.
pub fn tildify(p: &Path) -> String {
    let h = home();
    if let Ok(rest) = p.strip_prefix(&h) {
        if rest.as_os_str().is_empty() {
            return "~".into();
        }
        return format!("~/{}", rest.display());
    }
    p.display().to_string()
}

pub fn expand_tilde(s: &str) -> PathBuf {
    if s == "~" {
        return home();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return home().join(rest);
    }
    PathBuf::from(s)
}

/// The user's login shell.
pub fn user_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null() && !(*pw).pw_shell.is_null() {
            if let Ok(s) = std::ffi::CStr::from_ptr((*pw).pw_shell).to_str() {
                return s.to_string();
            }
        }
    }
    "/bin/zsh".into()
}

/// Foreground process group of a PTY master.
pub fn foreground_pid(fd: i32) -> Option<i32> {
    let p = unsafe { libc::tcgetpgrp(fd) };
    (p > 0).then_some(p)
}

pub fn process_name(pid: i32) -> Option<String> {
    let mut buf = [0u8; 256];
    let n = unsafe { libc::proc_name(pid, buf.as_mut_ptr() as *mut _, buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    Some(String::from_utf8_lossy(&buf[..n as usize]).into_owned())
}

pub fn process_cwd(pid: i32) -> Option<PathBuf> {
    unsafe {
        let mut info: libc::proc_vnodepathinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
        let n = libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        );
        if n != size {
            return None;
        }
        let raw = &info.pvi_cdir.vip_path;
        let bytes: Vec<u8> = raw
            .iter()
            .flat_map(|row| row.iter())
            .map(|&c| c as u8)
            .take_while(|&c| c != 0)
            .collect();
        if bytes.is_empty() {
            return None;
        }
        Some(PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
    }
}

/// Current git branch for a directory (walks up; supports worktrees' `.git` files).
pub fn git_branch(dir: &Path) -> Option<String> {
    let mut cur = Some(dir);
    while let Some(d) = cur {
        let git = d.join(".git");
        if git.is_dir() {
            return read_head(&git.join("HEAD"));
        }
        if git.is_file() {
            let s = std::fs::read_to_string(&git).ok()?;
            let gitdir = s.trim().strip_prefix("gitdir:")?.trim();
            let gd = if Path::new(gitdir).is_absolute() { PathBuf::from(gitdir) } else { d.join(gitdir) };
            return read_head(&gd.join("HEAD"));
        }
        cur = d.parent();
    }
    None
}

fn read_head(p: &Path) -> Option<String> {
    let s = std::fs::read_to_string(p).ok()?;
    let s = s.trim();
    if let Some(r) = s.strip_prefix("ref: refs/heads/") {
        Some(r.to_string())
    } else {
        Some(s.chars().take(7).collect())
    }
}

pub fn clipboard_set(text: &str) {
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

pub fn clipboard_get() -> String {
    Command::new("pbpaste")
        .env("LANG", "en_US.UTF-8")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

pub fn open_url(url: &str) {
    let _ = Command::new("open").arg(url).spawn();
}

/// Run a command with a timeout; returns stdout on success.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .env("PATH", augmented_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if start.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => return None,
        }
    }
}

/// Run a shell command line (via the user's shell) and return stdout.
pub fn run_shell_with_timeout(cmdline: &str, timeout: Duration) -> Option<String> {
    let sh = user_shell();
    run_with_timeout(&sh, &["-l", "-c", cmdline], timeout)
}

/// GUI apps launched by launchd get a minimal PATH; add the usual suspects so
/// `orca`, `claude`, homebrew tools etc. resolve.
pub fn augmented_path() -> String {
    let mut parts: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let h = home();
    for extra in [
        h.join(".local/bin").display().to_string(),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        "/Applications/Orca.app/Contents/Resources/bin".into(),
        h.join(".cargo/bin").display().to_string(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ] {
        if !parts.contains(&extra) {
            parts.push(extra);
        }
    }
    parts.join(":")
}

/// Find an executable on the augmented PATH.
pub fn which(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let p = expand_tilde(program);
        return p.exists().then_some(p);
    }
    augmented_path().split(':').map(|d| Path::new(d).join(program)).find(|p| p.is_file())
}

/// Local wall-clock time as (hour, minute).
pub fn local_hm() -> (u32, u32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour as u32, tm.tm_min as u32)
    }
}

/// Resident memory of this process in MB.
pub fn rss_mb() -> Option<u64> {
    unsafe {
        let mut info: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
        let n = libc::proc_pidinfo(libc::getpid(), libc::PROC_PIDTASKINFO, 0, &mut info as *mut _ as *mut libc::c_void, size);
        (n == size).then(|| info.pti_resident_size / (1024 * 1024))
    }
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Human short duration: 41s, 2m 14s, 1h 3m.
pub fn short_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{}h {}m", s / 3600, (s % 3600) / 60)
    }
}

pub fn short_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}
