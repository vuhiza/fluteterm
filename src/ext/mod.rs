//! The extension system.
//!
//! An extension is a plain Rust type implementing [`Extension`]. It never
//! touches the window or terminals directly; instead it
//!
//! * **observes**: IPC events (`on_event`), its own background messages
//!   (`on_message`), and a periodic `tick` with a read-only view of the tabs;
//! * **contributes**: palette commands, tab decorations (icon, badge,
//!   subtitle), status-bar segments, tray lines and an "attention" flag;
//! * **acts** by pushing [`Action`]s into the context; the app executes them.
//!
//! Built-ins: `claude` (Claude Code hooks → tab state, notifications, usage),
//! `orca` (Orca CLI poller → worktrees/agents, notifications, launch actions),
//! `user` (profiles/commands/modules from config.toml), `core` (everything else).
//! External tools integrate without code through the IPC socket (see ipc.rs).

pub mod claude;
pub mod claude_usage;
pub mod core;
pub mod orca;
pub mod user;

use std::any::Any;
use std::path::PathBuf;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Config, Pattern, Shape};
use crate::render::icons::Icon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind {
    Shell,
    Claude,
    Orca,
    Custom(char),
}

#[derive(Clone, Debug)]
pub struct TabSpec {
    pub kind: TabKind,
    pub title: Option<String>,
    pub command: Option<(String, Vec<String>)>,
    pub cwd: Option<PathBuf>,
    pub keep_shell: bool,
}

impl TabSpec {
    pub fn shell(cwd: Option<PathBuf>) -> Self {
        Self { kind: TabKind::Shell, title: None, command: None, cwd, keep_shell: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Info,
    Ok,
    Warn,
}

#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    pub body: String,
    pub tab: Option<u64>,
    pub level: Level,
    /// Toast buttons. The first is the primary (accent) action.
    pub actions: Vec<(String, Action)>,
    /// Allow a Notification Center banner when the panel is hidden.
    pub native: bool,
    /// Allow an in-panel toast.
    pub toast: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaletteMode {
    All,
    Agents,
    Orca,
    Themes,
}

#[derive(Clone, Debug)]
pub enum Action {
    NewTab(TabSpec),
    CloseTab(Option<u64>),
    FocusTab(u64),
    NextTab(i32),
    SendInput { tab: Option<u64>, data: String },
    Notify(Notice),
    RunDetached { program: String, args: Vec<String> },
    SetTheme(String),
    /// Some("#rrggbb") overrides the theme accent; None restores it.
    SetAccent(Option<String>),
    SetShape(Shape),
    SetPattern(Pattern),
    AdjustOpacity(f32),
    AdjustBlur(i32),
    ToggleCrt,
    FontSize(f32),
    ResetFontSize,
    ReloadConfig,
    OpenConfig,
    EditConfig,
    OpenSettings,
    /// true = install Claude Code hooks, false = remove them.
    ClaudeHooks(bool),
    Show,
    Hide,
    Toggle,
    OpenPalette(PaletteMode),
    SetDnd(bool),
    Copy(String),
    /// Ask an extension to do something it owns (e.g. run a CLI in the background).
    ExtCall { ext: &'static str, call: String },
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Text,
    Dim,
    Faint,
    Accent,
    Ok,
    Warn,
    Err,
}

#[derive(Clone, Debug)]
pub struct Segment {
    pub icon: Option<Icon>,
    pub text: String,
    pub tone: Tone,
    /// (fraction, number of cells)
    pub meter: Option<(f32, u8)>,
    pub spinner: bool,
    /// Clicking the segment runs this.
    pub action: Option<Action>,
}

impl Segment {
    pub fn text(t: impl Into<String>, tone: Tone) -> Self {
        Self { icon: None, text: t.into(), tone, meter: None, spinner: false, action: None }
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn meter(mut self, frac: f32, cells: u8) -> Self {
        self.meter = Some((frac, cells));
        self
    }
    pub fn on_click(mut self, a: Action) -> Self {
        self.action = Some(a);
        self
    }
    pub fn spinning(mut self) -> Self {
        self.spinner = true;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Badge {
    None,
    Spinner,
    Attention,
    Done,
    Dots(u8),
}

#[derive(Clone, Debug)]
pub struct TabDecor {
    pub kind: Option<TabKind>,
    pub subtitle: Option<String>,
    pub badge: Badge,
}

#[derive(Clone, Debug)]
pub struct TabInfo {
    pub id: u64,
    pub kind: TabKind,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub fg: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Command {
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    pub keys: String,
    pub mode: PaletteMode,
    pub right: String,
    pub right_tone: Tone,
    pub action: Action,
    /// ⌘⏎ variant.
    pub alt_action: Option<Action>,
}

impl Command {
    pub fn new(mode: PaletteMode, icon: Icon, title: impl Into<String>, detail: impl Into<String>, action: Action) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            icon,
            keys: String::new(),
            mode,
            right: String::new(),
            right_tone: Tone::Faint,
            action,
            alt_action: None,
        }
    }
    pub fn keys(mut self, k: &str) -> Self {
        self.keys = k.into();
        self
    }
    pub fn right(mut self, r: impl Into<String>, tone: Tone) -> Self {
        self.right = r.into();
        self.right_tone = tone;
        self
    }
    pub fn alt(mut self, a: Action) -> Self {
        self.alt_action = Some(a);
        self
    }
}

pub struct Ctx<'a> {
    pub tabs: &'a [TabInfo],
    pub active: Option<u64>,
    pub visible: bool,
    pub cfg: &'a Config,
    pub now: Instant,
    pub actions: Vec<Action>,
}

impl Ctx<'_> {
    pub fn push(&mut self, a: Action) {
        self.actions.push(a);
    }
    pub fn active_tab(&self) -> Option<&TabInfo> {
        self.active.and_then(|id| self.tabs.iter().find(|t| t.id == id))
    }
    pub fn active_cwd(&self) -> Option<PathBuf> {
        self.active_tab().and_then(|t| t.cwd.clone())
    }
    /// Is the user currently looking at this tab?
    pub fn watching(&self, tab: Option<u64>) -> bool {
        self.visible && tab.is_some() && tab == self.active
    }
}

pub trait Extension {
    fn id(&self) -> &'static str;
    /// IPC `{"cmd":"event","source":..}`. Return true if handled.
    fn on_event(&mut self, _source: &str, _tab: Option<u64>, _payload: &Value, _ctx: &mut Ctx) -> bool {
        false
    }
    /// Messages posted by this extension's own background threads.
    fn on_message(&mut self, _msg: Box<dyn Any + Send>, _ctx: &mut Ctx) {}
    fn tick(&mut self, _ctx: &mut Ctx) {}
    fn commands(&self, _ctx: &Ctx) -> Vec<Command> {
        vec![]
    }
    fn tab_decor(&self, _tab: &TabInfo) -> Option<TabDecor> {
        None
    }
    fn status_left(&self, _tab: &TabInfo, _ctx: &Ctx) -> Vec<Segment> {
        vec![]
    }
    fn status_right(&self, _ctx: &Ctx) -> Vec<Segment> {
        vec![]
    }
    fn tray_lines(&self) -> Vec<String> {
        vec![]
    }
    fn attention(&self) -> bool {
        false
    }
    /// True while something animated (spinner) is shown.
    fn animating(&self) -> bool {
        false
    }
    fn on_tab_closed(&mut self, _id: u64) {}
    /// State to persist with a tab (restored with `restore_tab` on next start).
    fn save_tab(&self, _tab: u64) -> Option<Value> {
        None
    }
    fn restore_tab(&mut self, _tab: u64, _state: &Value) {}
    /// Handle an `Action::ExtCall` addressed to this extension.
    fn on_call(&mut self, _call: &str, _ctx: &mut Ctx) {}
    fn reconfigure(&mut self, _cfg: &Config) {}
}

/// Helper so background threads can post typed messages back to their extension.
pub fn post(proxy: &winit::event_loop::EventLoopProxy<crate::app::UserEvent>, ext: &'static str, msg: impl Any + Send) {
    let _ = proxy.send_event(crate::app::UserEvent::Ext(ext, Box::new(msg)));
}
