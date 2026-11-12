//! Configuration: `~/.config/fluteterm/config.toml`, live-reloaded.

use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::util;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Strip,
    #[serde(alias = "center", alias = "centered")]
    Centred,
    Floating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Pattern {
    #[default]
    Bayer,
    Scan,
    Blocks,
    Noise,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OptionAsAlt {
    #[default]
    Left,
    Right,
    Both,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScreenMode {
    #[default]
    Mouse,
    Primary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Global toggle hotkey, e.g. "F12", "Alt+Space", "Cmd+Shift+Backquote".
    pub hotkey: String,
    /// Additional global hotkeys that also toggle the panel.
    pub extra_hotkeys: Vec<String>,
    pub theme: String,
    /// Optional accent override ("#rrggbb"); None = the theme's own accent.
    pub accent: Option<String>,
    pub font: FontConfig,
    pub window: WindowConfig,
    pub motion: MotionConfig,
    pub shell: ShellConfig,
    pub option_as_alt: OptionAsAlt,
    /// Copy the selection to the clipboard as soon as the mouse is released.
    pub copy_on_select: bool,
    pub cursor: CursorConfig,
    pub scrollback: usize,
    pub notifications: NotifyConfig,
    pub claude: ClaudeConfig,
    pub orca: OrcaConfig,
    pub session: SessionConfig,
    #[serde(rename = "profile")]
    pub profiles: Vec<Profile>,
    #[serde(rename = "module")]
    pub modules: Vec<Module>,
    #[serde(rename = "command")]
    pub commands: Vec<UserCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FontConfig {
    /// "JetBrains Mono" (bundled) or a path to a .ttf/.otf file.
    pub family: String,
    pub bold: Option<String>,
    pub size: f32,
    pub line_height: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    /// Fraction of the screen height (0.2 - 0.95).
    pub height: f32,
    /// Fraction of the screen width for `centred` / `floating`.
    pub width: f32,
    pub shape: Shape,
    pub screen: ScreenMode,
    pub opacity: f32,
    /// Background blur radius (native macOS window blur). 0 disables.
    pub blur: u32,
    pub crt: bool,
    pub hide_on_focus_loss: bool,
    pub padding: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MotionConfig {
    pub pattern: Pattern,
    /// Size of one dither "pixel", in points.
    pub cell: u32,
    pub open_ms: u32,
    pub close_ms: u32,
    /// Slide the panel down while it dissolves in.
    pub slide: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShellConfig {
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Working directory for the first tab (default: home).
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorConfig {
    pub style: CursorStyle,
    pub blink: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyConfig {
    /// macOS Notification Center banners (when the panel is hidden or the tab is in background).
    pub native: bool,
    /// In-panel pixel toasts.
    pub toasts: bool,
    pub sound: bool,
    pub do_not_disturb: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub enabled: bool,
    pub command: String,
    pub args: Vec<String>,
    /// Notify when a Claude turn finishes.
    pub notify_done: bool,
    /// Notify when Claude needs permission / input.
    pub notify_attention: bool,
    /// Live sessions, 5-hour usage window and today's tokens in the status bar.
    pub status_usage: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrcaConfig {
    pub enabled: bool,
    pub command: String,
    /// Poll interval while the panel is visible (seconds).
    pub poll_secs: u64,
    /// Poll interval while hidden (seconds) — still needed for notifications.
    pub idle_poll_secs: u64,
    /// Agent used by "launch agent" actions.
    pub agent: String,
    pub notify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    /// tmux if installed, else plain.
    #[default]
    Auto,
    Tmux,
    Plain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// Bring tabs back on start.
    pub restore: bool,
    /// tmux keeps processes running while fluteterm is closed; plain restores layout + scrollback.
    pub backend: Backend,
    /// Lines of scrollback saved per tab (plain backend).
    pub scrollback_lines: usize,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { restore: true, backend: Backend::Auto, scrollback_lines: 2000 }
    }
}

/// A launchable tab type, e.g. `[[profile]] name = "htop" command = "htop"`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    /// Single character shown as the tab icon.
    pub icon: Option<String>,
    /// Optional in-panel shortcut, e.g. "cmd+shift+h".
    pub key: Option<String>,
}

/// A custom status-bar segment fed by a shell command.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Module {
    pub name: String,
    pub command: String,
    pub interval: u64,
}

/// A custom command-palette entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserCommand {
    pub name: String,
    /// Shell command line.
    pub run: String,
    /// Run in a new tab (true) or detached in the background (false).
    pub tab: bool,
    pub detail: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            hotkey: "F12".into(),
            extra_hotkeys: vec![],
            theme: "graphite".into(),
            accent: None,
            font: FontConfig::default(),
            window: WindowConfig::default(),
            motion: MotionConfig::default(),
            shell: ShellConfig::default(),
            option_as_alt: OptionAsAlt::Left,
            copy_on_select: true,
            cursor: CursorConfig::default(),
            scrollback: 10_000,
            notifications: NotifyConfig::default(),
            claude: ClaudeConfig::default(),
            orca: OrcaConfig::default(),
            session: SessionConfig::default(),
            profiles: vec![],
            modules: vec![],
            commands: vec![],
        }
    }
}

impl Default for FontConfig {
    fn default() -> Self {
        Self { family: "JetBrains Mono".into(), bold: None, size: 14.0, line_height: 1.2 }
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            height: 0.5,
            width: 0.76,
            shape: Shape::Strip,
            screen: ScreenMode::Mouse,
            opacity: 0.86,
            blur: 14,
            crt: false,
            hide_on_focus_loss: false,
            padding: 12.0,
        }
    }
}

impl Default for MotionConfig {
    fn default() -> Self {
        Self { pattern: Pattern::Bayer, cell: 2, open_ms: 220, close_ms: 160, slide: true }
    }
}

impl Default for CursorConfig {
    fn default() -> Self {
        Self { style: CursorStyle::Block, blink: true }
    }
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self { native: true, toasts: true, sound: false, do_not_disturb: false }
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self { enabled: true, command: "claude".into(), args: vec![], notify_done: true, notify_attention: true, status_usage: true }
    }
}

impl Default for OrcaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            command: "orca".into(),
            poll_secs: 5,
            idle_poll_secs: 20,
            agent: "claude".into(),
            notify: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    util::config_dir().join("config.toml")
}

pub fn themes_dir() -> PathBuf {
    util::config_dir().join("themes")
}

/// Load config, writing the documented default file on first run.
pub fn load() -> (Config, Option<String>) {
    let path = config_path();
    if !path.exists() {
        let _ = std::fs::create_dir_all(util::config_dir());
        let _ = std::fs::write(&path, DEFAULT_CONFIG);
        let _ = std::fs::create_dir_all(themes_dir());
        let _ = std::fs::write(themes_dir().join("example.toml"), crate::theme::EXAMPLE_THEME);
    }
    match std::fs::read_to_string(&path) {
        Ok(s) => match toml::from_str::<Config>(&s) {
            Ok(mut c) => {
                c.sanitize();
                (c, None)
            }
            Err(e) => (Config::default(), Some(format!("config.toml: {e}"))),
        },
        Err(e) => (Config::default(), Some(format!("config.toml: {e}"))),
    }
}

pub fn mtime() -> Option<SystemTime> {
    let cfg = std::fs::metadata(config_path()).and_then(|m| m.modified()).ok();
    // Theme edits also trigger a reload.
    let themes = std::fs::read_dir(themes_dir())
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max();
    match (cfg, themes) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

impl Config {
    fn sanitize(&mut self) {
        self.window.height = self.window.height.clamp(0.15, 0.97);
        self.window.width = self.window.width.clamp(0.3, 1.0);
        self.window.opacity = self.window.opacity.clamp(0.2, 1.0);
        self.window.blur = self.window.blur.min(64);
        self.font.size = self.font.size.clamp(8.0, 40.0);
        self.font.line_height = self.font.line_height.clamp(1.0, 2.0);
        self.motion.cell = self.motion.cell.clamp(1, 8);
        self.motion.open_ms = self.motion.open_ms.min(2000);
        self.motion.close_ms = self.motion.close_ms.min(2000);
    }
}

/// Persist a single key without destroying the user's comments/formatting.
/// `table` = None for top-level keys.
/// Remove a key (top-level when `table` is None), keeping everything else.
pub fn remove_value(table: Option<&str>, key: &str) {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let mut section: Option<String> = None;
    let mut out = vec![];
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            section = Some(t.trim_matches(|c| c == '[' || c == ']').trim().to_string());
        } else if section.as_deref() == table {
            let rest = t.strip_prefix(key).map(str::trim_start);
            if rest.map(|r| r.starts_with('=')).unwrap_or(false) {
                continue;
            }
        }
        out.push(l);
    }
    let _ = std::fs::write(path, out.join("\n") + "\n");
}

pub fn set_value(table: Option<&str>, key: &str, value: &str) {
    let path = config_path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let is_key_line = |l: &str| {
        let t = l.trim_start();
        t.starts_with(key) && t[key.len()..].trim_start().starts_with('=')
    };
    // Section boundaries.
    let mut section: Option<String> = None;
    let mut insert_at: Option<usize> = None;
    let mut found = None;
    let mut first_table = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            if first_table.is_none() {
                first_table = Some(i);
            }
            let name = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            section = Some(name.clone());
            if table == Some(name.as_str()) {
                insert_at = Some(i + 1);
            }
            continue;
        }
        if section.as_deref() == table && is_key_line(l) {
            found = Some(i);
            break;
        }
    }
    let new_line = format!("{key} = {value}");
    match (found, table) {
        (Some(i), _) => lines[i] = new_line,
        (None, None) => lines.insert(first_table.unwrap_or(lines.len()), new_line),
        (None, Some(t)) => match insert_at {
            Some(i) => lines.insert(i, new_line),
            None => {
                lines.push(String::new());
                lines.push(format!("[{t}]"));
                lines.push(new_line);
            }
        },
    }
    let mut out = lines.join("\n");
    out.push('\n');
    let _ = std::fs::write(path, out);
}

pub const DEFAULT_CONFIG: &str = r##"# fluteterm configuration — edits are applied live.
# Docs: README.md in the fluteterm repo. Every key is optional.

# Global hotkey that drops the terminal down / pulls it up.
# Examples: "F12", "Alt+Space", "Cmd+Backquote", "Ctrl+Shift+KeyT".
# On Mac keyboards F12 may be a media key: press fn+F12, or enable
# "Use F1, F2, etc. keys as standard function keys" in System Settings → Keyboard.
hotkey = "F12"
extra_hotkeys = []

# graphite · clay · moss · glacier · orchid · blood · paper (light), or a file in ~/.config/fluteterm/themes/
theme = "graphite"
# accent = "#e0a030"       # optional: your own accent colour on top of the theme

option_as_alt = "left"     # left | right | both | none  (Option key sends Meta/Alt)
copy_on_select = true      # selecting text copies it (⌘C works too)
scrollback = 10000

[font]
family = "JetBrains Mono"  # bundled; or a path to a .ttf / .otf file
size = 14
line_height = 1.2

[window]
height = 0.5               # fraction of the screen
width = 0.76               # for centred / floating shapes
shape = "strip"            # strip | centred | floating
screen = "mouse"           # mouse (follow the pointer) | primary
opacity = 0.86
blur = 14                  # native background blur radius, 0 = off
crt = false                # faint scanlines
hide_on_focus_loss = false
padding = 12

[motion]
pattern = "bayer"          # bayer | scan | blocks | noise | none
cell = 2                   # dither pixel size in points
open_ms = 220
close_ms = 160
slide = true

[shell]
# program = "/bin/zsh"
# args = ["-l"]
# cwd = "~/Projects"

[cursor]
style = "block"            # block | beam | underline
blink = true

[notifications]
native = true              # macOS Notification Center
toasts = true              # in-panel pixel toasts
sound = false
do_not_disturb = false

[claude]
enabled = true
command = "claude"
args = []
notify_done = true
notify_attention = true
status_usage = true        # live sessions + 5h window + today's tokens in the status bar

[session]
restore = true             # bring all tabs back when fluteterm starts
backend = "auto"           # auto | tmux (processes keep running while closed) | plain (layout + scrollback)
scrollback_lines = 2000

[orca]
enabled = true
command = "orca"
poll_secs = 5
idle_poll_secs = 20
agent = "claude"
notify = true

# ── Extensions without code ───────────────────────────────────────────────
# Tab profiles appear in the command palette (⌘K) and can have shortcuts.
# [[profile]]
# name = "htop"
# command = "htop"
# icon = "▲"
# key = "cmd+shift+h"

# Status-bar modules: any command that prints one line.
# [[module]]
# name = "battery"
# command = "pmset -g batt | grep -Eo '[0-9]+%'"
# interval = 60

# Palette commands.
# [[command]]
# name = "deploy staging"
# run = "make deploy-staging"
# tab = true
"##;
