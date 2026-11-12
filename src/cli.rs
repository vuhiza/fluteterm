//! Command-line interface. The same binary is the app, its remote control and
//! the Claude Code hook.

use std::io::{IsTerminal, Read};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::{app, config, ipc, theme, util};

const HELP: &str = "\
fluteterm — a drop-down terminal for macOS

USAGE
  fluteterm                    start (in the background), or toggle if running
  fluteterm run                run in the foreground
  fluteterm toggle|show|hide   control the panel
  fluteterm open [PROFILE] [--cwd DIR] [-- CMD ARGS…]
                             new tab: shell (default), claude, or a [[profile]] name
  fluteterm send [--tab ID] TEXT
  fluteterm notify TITLE [BODY] [--tab ID] [--level info|ok|warn]
  fluteterm status KEY [TEXT]  set/clear a custom status-bar segment
  fluteterm palette [all|agents|orca|themes]
  fluteterm settings [PAGE]    open settings (look fonts keys window notify claude orca custom)
  fluteterm list               tabs as JSON
  fluteterm do COMMAND [--alt]   run a command-palette entry by name (⌘⏎ variant with --alt)
  fluteterm read [--tab ID]    print a tab's visible screen text
  fluteterm snapshot [FILE]    save the current frame as PNG (panel must be open)
  fluteterm theme NAME         switch theme (see `fluteterm themes`)
  fluteterm themes             list themes
  fluteterm accent COLOR       #rrggbb, a name (amber, rose, teal…), or `default`
  fluteterm reload             reload config
  fluteterm quit
  fluteterm config             print the config path (creates it on first run)

INTEGRATIONS
  fluteterm install claude     add Claude Code hooks (~/.claude/settings.json, backed up)
  fluteterm uninstall claude   remove them
  fluteterm install login      start fluteterm at login (LaunchAgent)
  fluteterm uninstall login
  fluteterm hook claude        (used by the hooks; reads hook JSON on stdin)
  fluteterm orca-attach HANDLE live view of an Orca terminal (ctrl-] detaches)

Config: ~/.config/fluteterm/config.toml   Socket: ~/.config/fluteterm/fluteterm.sock
";

fn send(body: Value) -> anyhow::Result<Value> {
    let v = ipc::request(&body).context("fluteterm is not running (start it with `fluteterm`)")?;
    if v["ok"].as_bool() == Some(false) {
        bail!("{}", v["error"].as_str().unwrap_or("request failed"));
    }
    Ok(v)
}

fn flag(args: &mut Vec<String>, name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    if i + 1 < args.len() {
        let v = args.remove(i + 1);
        args.remove(i);
        Some(v)
    } else {
        args.remove(i);
        None
    }
}

pub fn run() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = if args.is_empty() { String::new() } else { args.remove(0) };
    match cmd.as_str() {
        "" => start(),
        "run" => app::run(),
        "-h" | "--help" | "help" => {
            print!("{HELP}");
            Ok(())
        }
        "-V" | "--version" | "version" => {
            println!("fluteterm {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        "toggle" | "show" | "hide" | "reload" | "quit" => {
            send(json!({"cmd": cmd}))?;
            Ok(())
        }
        "settings" => {
            let close = args.iter().any(|a| a == "--close");
            let page = args.iter().find(|a| !a.starts_with("--")).cloned();
            send(json!({"cmd": "settings", "page": page, "close": close}))?;
            Ok(())
        }
        "snapshot" => {
            let settings = args.iter().any(|a| a == "--settings");
            let args: Vec<String> = args.into_iter().filter(|a| a != "--settings").collect();
            let path = args.first().cloned().unwrap_or_else(|| "/tmp/fluteterm.png".into());
            let path = std::fs::canonicalize(".").map(|d| d.join(&path)).unwrap_or_else(|_| path.clone().into());
            let window = if settings { "settings" } else { "panel" };
            let v = send(json!({"cmd": "snapshot", "path": path, "window": window}))?;
            println!("{}", v["path"].as_str().unwrap_or(""));
            Ok(())
        }
        "gen-icon" => {
            let out = args.first().context("usage: fluteterm gen-icon OUT.png [SIZE]")?;
            let size: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1024);
            gen_icon(out, size)
        }
        "ping" => {
            println!("{}", send(json!({"cmd": "ping"}))?);
            Ok(())
        }
        "orca-attach" => {
            let title = flag(&mut args, "--title").unwrap_or_else(|| "agent".into());
            let handle = args.first().context("usage: fluteterm orca-attach HANDLE [--title T]")?;
            crate::attach::run(handle, &title)
        }
        "do" => {
            let alt = args.iter().any(|a| a == "--alt");
            let q: Vec<String> = args.into_iter().filter(|a| a != "--alt").collect();
            let v = send(json!({"cmd": "do", "command": q.join(" "), "alt": alt}))?;
            println!("ran: {}", v["ran"].as_str().unwrap_or(""));
            Ok(())
        }
        "read" => {
            let tab = flag(&mut args, "--tab");
            let v = send(json!({"cmd": "read", "tab": tab}))?;
            println!("{}", v["text"].as_str().unwrap_or(""));
            Ok(())
        }
        "list" => {
            println!("{}", serde_json::to_string_pretty(&send(json!({"cmd": "list"}))?)?);
            Ok(())
        }
        "palette" => {
            let mode = args.first().cloned().unwrap_or_else(|| "all".into());
            send(json!({"cmd": "palette", "mode": mode}))?;
            Ok(())
        }
        "theme" => {
            let name = args.first().context("usage: fluteterm theme NAME")?;
            send(json!({"cmd": "theme", "name": name}))?;
            Ok(())
        }
        "accent" => {
            let color = args.first().cloned().unwrap_or_else(|| "default".into());
            let v = send(json!({"cmd": "accent", "color": color}))?;
            println!("accent: {}", v["accent"].as_str().unwrap_or("theme default"));
            Ok(())
        }
        "themes" => {
            for t in theme::list() {
                println!("{t}");
            }
            Ok(())
        }
        "config" => {
            let _ = config::load();
            println!("{}", config::config_path().display());
            Ok(())
        }
        "open" => {
            let cwd = flag(&mut args, "--cwd").or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()));
            let (before, after) = match args.iter().position(|a| a == "--") {
                Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
                None => (args.clone(), vec![]),
            };
            let profile = before.first().cloned().unwrap_or_else(|| "shell".into());
            let mut body = json!({"cmd": "open", "profile": profile, "cwd": cwd});
            if !after.is_empty() {
                body["command"] = json!(after);
            }
            let v = send(body)?;
            if let Some(id) = v["tab"].as_u64() {
                println!("{id}");
            }
            Ok(())
        }
        "send" => {
            let tab = flag(&mut args, "--tab");
            let text = args.join(" ");
            let text = text.replace("\\n", "\n").replace("\\r", "\r");
            send(json!({"cmd": "send", "tab": tab, "text": text}))?;
            Ok(())
        }
        "notify" => {
            let tab = flag(&mut args, "--tab").or_else(|| std::env::var("FLUTETERM_TAB_ID").or_else(|_| std::env::var("DRPDOWN_TAB_ID")).ok());
            let level = flag(&mut args, "--level").unwrap_or_else(|| "info".into());
            let title = args.first().cloned().unwrap_or_else(|| "fluteterm".into());
            let body = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            send(json!({"cmd": "notify", "title": title, "body": body, "tab": tab, "level": level}))?;
            Ok(())
        }
        "status" => {
            let key = args.first().context("usage: fluteterm status KEY [TEXT]")?.clone();
            let text = args.get(1..).map(|a| a.join(" ")).unwrap_or_default();
            send(json!({"cmd": "status", "key": key, "text": text}))?;
            Ok(())
        }
        "hook" => hook(args.first().map(String::as_str).unwrap_or("")),
        "install" => match args.first().map(String::as_str) {
            Some("claude") => install_claude(args.iter().any(|a| a == "--print")),
            Some("login") => install_login(),
            _ => bail!("usage: fluteterm install claude|login"),
        },
        "uninstall" => match args.first().map(String::as_str) {
            Some("claude") => uninstall_claude(),
            Some("login") => uninstall_login(),
            _ => bail!("usage: fluteterm uninstall claude|login"),
        },
        other => bail!("unknown command `{other}` (see `fluteterm --help`)"),
    }
}

/// `fluteterm` with no arguments: toggle a running instance, or start one in the background.
fn start() -> anyhow::Result<()> {
    if ipc::is_running() {
        send(json!({"cmd": "toggle"}))?;
        return Ok(());
    }
    if !std::io::stdout().is_terminal() || std::env::var("FLUTETERM_FOREGROUND").is_ok() {
        return app::run();
    }
    // Resolve symlinks (e.g. Homebrew's bin/fluteterm) so the app runs from inside its
    // .app bundle and gets the bundle's identity (notification name and icon).
    let exe = std::env::current_exe().and_then(std::fs::canonicalize)?;
    let _ = std::fs::create_dir_all(util::config_dir());
    let log = std::fs::OpenOptions::new().create(true).append(true).open(util::log_path())?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("run").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    // Wait until it answers so `fluteterm && fluteterm show` works.
    for _ in 0..50 {
        if ipc::is_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("fluteterm is running — press F12 (or fn+F12) to drop it down.");
    println!("Tip: `fluteterm install claude` wires up Claude Code; `fluteterm install login` starts it at login.");
    Ok(())
}

/// Claude Code hook entry point: forward the hook JSON to the running app.
/// Must be fast and must never fail loudly — Claude Code shows hook errors.
fn hook(source: &str) -> anyhow::Result<()> {
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    // Tabs started before the rename still carry the old variable name.
    let Ok(tab) = std::env::var("FLUTETERM_TAB_ID").or_else(|_| std::env::var("DRPDOWN_TAB_ID")) else { return Ok(()) };
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let _ = ipc::request(&json!({"cmd": "event", "source": source, "tab": tab, "payload": payload}));
    Ok(())
}

fn install_claude(print_only: bool) -> anyhow::Result<()> {
    let (r, json) = crate::hooks::install(print_only)?;
    if let Some(json) = json {
        println!("{json}");
        return Ok(());
    }
    if let Some(b) = r.backup {
        println!("backup: {}", b.display());
    }
    println!("installed fluteterm hooks for: {}", crate::hooks::EVENTS.iter().map(|e| e.0).collect::<Vec<_>>().join(", "));
    println!("hook command: {}", r.command);
    println!("Hooks only act inside fluteterm tabs (they need $FLUTETERM_TAB_ID); elsewhere they exit immediately.");
    println!("Restart running Claude Code sessions to pick them up.");
    Ok(())
}

fn uninstall_claude() -> anyhow::Result<()> {
    let r = crate::hooks::uninstall()?;
    if r.changed == 0 {
        println!("no fluteterm hooks were installed — nothing changed");
    } else {
        println!("removed {} fluteterm hook entries (backup: {})", r.changed, r.backup.map(|b| b.display().to_string()).unwrap_or_default());
    }
    Ok(())
}

fn launch_agent_path() -> PathBuf {
    util::home().join("Library/LaunchAgents/dev.fluteterm.plist")
}

fn install_login() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?.display().to_string();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>dev.fluteterm</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        log = util::log_path().display()
    );
    let path = launch_agent_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&path, plist)?;
    println!("wrote {}", path.display());
    println!("fluteterm will start at login. To start it now via launchd: launchctl load -w {}", path.display());
    Ok(())
}

fn uninstall_login() -> anyhow::Result<()> {
    let path = launch_agent_path();
    let _ = std::process::Command::new("launchctl").arg("unload").arg(&path).status();
    std::fs::remove_file(&path).ok();
    println!("removed {}", path.display());
    Ok(())
}

/// App icon: the fluteterm mark in pixels on a graphite squircle-ish tile with a hard shadow.
fn gen_icon(out: &str, size: u32) -> anyhow::Result<()> {
    const MARK: [&str; 8] = ["........", "########", ".######.", "..####..", "...##...", "........", "###.####", "........"];
    let s = size as usize;
    let mut px = vec![0u8; s * s * 4];
    let put = |px: &mut Vec<u8>, x: usize, y: usize, c: [u8; 4]| {
        let i = (y * s + x) * 4;
        px[i..i + 4].copy_from_slice(&c);
    };
    let margin = s / 10;
    let tile = s - 2 * margin;
    let notch = s / 32;
    let shadow = s / 40;
    let inside = |x: usize, y: usize, ox: usize, oy: usize| -> bool {
        if x < ox || y < oy || x >= ox + tile || y >= oy + tile {
            return false;
        }
        let (dx, dy) = (x - ox, y - oy);
        let edge = |d: usize| d < notch || d >= tile - notch;
        !(edge(dx) && edge(dy))
    };
    for y in 0..s {
        for x in 0..s {
            if inside(x, y, margin, margin) {
                put(&mut px, x, y, [0x16, 0x13, 0x13, 255]);
            } else if inside(x, y, margin + shadow, margin + shadow) {
                put(&mut px, x, y, [0, 0, 0, 160]);
            }
        }
    }
    // 1px-ish line border.
    let line = (s / 256).max(1);
    for y in margin..margin + tile {
        for x in margin..margin + tile {
            let (dx, dy) = (x - margin, y - margin);
            if inside(x, y, margin, margin) && (dx < line || dy < line || dx >= tile - line || dy >= tile - line) {
                put(&mut px, x, y, [0x3a, 0x33, 0x33, 255]);
            }
        }
    }
    let cell = tile / 12;
    let ox = margin + (tile - 8 * cell) / 2;
    let oy = margin + (tile - 8 * cell) / 2;
    for (my, row) in MARK.iter().enumerate() {
        for (mx, ch) in row.bytes().enumerate() {
            if ch != b'#' {
                continue;
            }
            let color = if my >= 6 { [0xa8, 0xa2, 0x9c, 255] } else { [0xf4, 0xf0, 0xee, 255] };
            for y in 0..cell {
                for x in 0..cell {
                    put(&mut px, ox + mx * cell + x, oy + my * cell + y, color);
                }
            }
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(out)?);
    let mut enc = png::Encoder::new(file, size, size);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&px)?;
    Ok(())
}
