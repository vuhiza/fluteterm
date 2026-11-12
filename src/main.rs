//! fluteterm: a drop-down terminal for macOS.
//!
//! Architecture overview (see ARCHITECTURE.md for the long version):
//!
//! * `app`        winit application: window, animation, input routing, frame composition
//! * `term`       terminal sessions (alacritty_terminal + PTY) and key encoding
//! * `render`     CPU renderer: canvas, glyph cache, procedural box/block glyphs, pixel icons, UI widgets
//! * `ext`        extension system; built-ins are `claude`, `orca`, `core`, `user` (config-defined)
//! * `ipc`        unix-socket JSON protocol used by the CLI, Claude Code hooks and scripts
//! * `platform`   macOS glue: window level/spaces/blur, screens, tray icon, notifications
//! * `config` / `theme`   TOML config with live reload, palette derivation from one accent

mod app;
mod attach;
mod cli;
mod config;
mod ext;
mod hooks;
mod ipc;
mod session;
mod platform;
mod render;
mod term;
mod theme;
mod util;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    if let Err(e) = cli::run() {
        eprintln!("fluteterm: {e:#}");
        std::process::exit(1);
    }
}
