//! The application: one borderless transparent window that drops down from the
//! menu bar, a strip of tabs (each an alacritty terminal), extensions, IPC,
//! tray and notifications. Rendering lives in `draw.rs`.

mod draw;
mod palette;
mod settings;

use std::any::Any;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::str::FromStr;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use serde_json::{Value, json};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, NamedKey};
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS, WindowAttributesExtMacOS};
use winit::window::{CursorIcon, Window, WindowId};

use crate::config::{self, Config, Pattern, Shape};
use crate::ext::{self, Action, Badge, Ctx, Extension, Level, Notice, PaletteMode, TabDecor, TabInfo, TabKind, TabSpec};
use crate::ipc;
use crate::platform::{self, CGRect, ScreenInfo, tray};
use crate::render::canvas::IRect;
use crate::render::text::{CellMetrics, Fonts};
use crate::term::{self, Session, Spawn};
use crate::theme::{self, Palette};
use crate::util;
use palette::PaletteState;

pub enum UserEvent {
    Term(u64, TermEvent),
    Hotkey,
    TrayToggle,
    Menu(String),
    Ipc(ipc::Request),
    NotificationClicked(Option<u64>),
    Ext(&'static str, Box<dyn Any + Send>),
}

impl std::fmt::Debug for UserEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserEvent::Term(id, e) => write!(f, "Term({id}, {e:?})"),
            UserEvent::Hotkey => write!(f, "Hotkey"),
            UserEvent::TrayToggle => write!(f, "TrayToggle"),
            UserEvent::Menu(m) => write!(f, "Menu({m})"),
            UserEvent::Ipc(r) => write!(f, "{r:?}"),
            UserEvent::NotificationClicked(t) => write!(f, "NotificationClicked({t:?})"),
            UserEvent::Ext(id, _) => write!(f, "Ext({id})"),
        }
    }
}

/// Layout constants, in points.
pub const TABBAR_H: f64 = 34.0;
pub const STATUS_H: f64 = 28.0;
pub const GRIP_H: f64 = 8.0;
pub const SHADOW: f64 = 4.0;
pub const FLOAT_GAP: f64 = 10.0;

pub struct Tab {
    pub id: u64,
    pub session: Session,
    pub spec_kind: TabKind,
    pub spec_title: Option<String>,
    pub osc_title: Option<String>,
    pub cwd: Option<PathBuf>,
    pub fg: Option<String>,
    pub decor: TabDecor,
    pub bell: Option<Instant>,
    /// The command the tab was started with (persisted).
    pub command: Option<Vec<String>>,
    /// tmux session keeping this tab alive (tmux backend).
    pub tmux: Option<String>,
    pub pane_pid: Option<i32>,
    pub spawned: Instant,
    /// Last known selection + whether it was on the alternate screen. Programs that
    /// redraw by erasing lines (Claude Code, tmux) make the terminal drop selections;
    /// we put them back so selecting is never interrupted.
    pub sel_keep: std::cell::RefCell<Option<(Selection, bool)>>,
}

/// If the terminal dropped the selection because a line under it was erased, put it
/// back; switching between the normal and alternate screen is a real reset.
pub fn keep_selection<T>(keep: &std::cell::RefCell<Option<(Selection, bool)>>, term: &mut alacritty_terminal::Term<T>) {
    let alt = term.mode().contains(TermMode::ALT_SCREEN);
    let mut keep = keep.borrow_mut();
    match (&term.selection, keep.as_ref()) {
        (Some(s), _) => *keep = Some((s.clone(), alt)),
        (None, Some((s, was_alt))) if *was_alt == alt => term.selection = Some(s.clone()),
        (None, Some(_)) => *keep = None,
        _ => {}
    }
}

impl Tab {
    /// Keep the selection alive across line erases (see `sel_keep`).
    pub fn sync_selection(&self, term: &mut alacritty_terminal::Term<crate::term::Listener>) {
        keep_selection(&self.sel_keep, term);
    }

    /// Deliberately drop the selection (typing, new click).
    pub fn clear_selection(&self, term: &mut alacritty_terminal::Term<crate::term::Listener>) {
        term.selection = None;
        *self.sel_keep.borrow_mut() = None;
    }

    /// The launched command has exited and the tab fell back to a shell.
    fn back_at_shell(&self) -> bool {
        let shell = matches!(self.fg.as_deref().map(|s| s.trim_start_matches('-')), Some("zsh" | "bash" | "fish" | "sh" | "nu" | "login"));
        shell && self.spec_kind != TabKind::Shell && self.decor.kind.is_none()
    }
    pub fn kind(&self) -> TabKind {
        if self.back_at_shell() {
            return TabKind::Shell;
        }
        self.decor.kind.unwrap_or(self.spec_kind)
    }
    pub fn title(&self) -> String {
        if let Some(t) = &self.spec_title {
            if self.spec_kind != TabKind::Shell && !self.back_at_shell() {
                return t.clone();
            }
        }
        match self.kind() {
            TabKind::Claude => "claude".into(),
            TabKind::Orca => "orca".into(),
            _ => self
                .fg
                .clone()
                .map(|s| s.trim_start_matches('-').to_string())
                .or_else(|| self.spec_title.clone())
                .unwrap_or_else(|| "shell".into()),
        }
    }
    fn info(&self) -> TabInfo {
        TabInfo { id: self.id, kind: self.kind(), title: self.title(), cwd: self.cwd.clone(), fg: self.fg.clone() }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Vis {
    Hidden,
    Opening(Instant),
    Shown,
    Closing(Instant),
}

#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub screen: ScreenInfo,
    /// Window origin x and the y of its top edge (Cocoa coordinates, points).
    pub x: f64,
    pub top: f64,
    /// Full window size (panel + shadow margin), points.
    pub w: f64,
    pub h: f64,
    pub panel_w: f64,
    pub panel_h: f64,
    pub scale: f64,
}

pub struct Toast {
    pub notice: Notice,
    pub born: Instant,
    pub ttl: Duration,
}

#[derive(Default)]
pub struct Hits {
    pub tabs: Vec<(IRect, u64)>,
    pub plus: IRect,
    pub close: IRect,
    pub grip: IRect,
    pub term: IRect,
    pub toast_buttons: Vec<(IRect, usize, usize)>,
    pub toast_boxes: Vec<(IRect, usize)>,
    pub palette_box: IRect,
    pub palette_rows: Vec<(IRect, usize)>,
    pub palette_chips: Vec<(IRect, PaletteMode)>,
    pub status: Vec<(IRect, Action)>,
}

#[derive(Default)]
struct MouseState {
    x: f64,
    y: f64,
    left_down: bool,
    selecting: bool,
    grip_drag: Option<(f64, f64)>,
    last_click: Option<(Instant, usize, usize, u8)>,
    scroll_acc: f64,
    /// This press/drag goes to the program (it asked for the mouse), not to our selection.
    forward: bool,
}

pub struct App {
    pub cfg: Config,
    pub cfg_err: Option<String>,
    cfg_mtime: Option<std::time::SystemTime>,
    last_cfg_check: Instant,
    pub pal: Palette,
    pub fonts: Fonts,
    pub font_delta: f32,
    proxy: EventLoopProxy<UserEvent>,
    // Declared before `window` so the presenter is torn down first (fields drop in order).
    presenter: Option<crate::platform::present::Presenter>,
    pub window: Option<Rc<Window>>,
    pub geo: Option<Geometry>,
    pub vis: Vis,
    pub tabs: Vec<Tab>,
    pub active: usize,
    next_id: u64,
    pub exts: Vec<Box<dyn Extension>>,
    pub palette: Option<PaletteState>,
    pub toasts: Vec<Toast>,
    pub statuses: BTreeMap<String, String>,
    hotkey_mgr: Option<GlobalHotKeyManager>,
    registered: Vec<HotKey>,
    tray: Option<tray::Tray>,
    mods: Modifiers,
    mouse: MouseState,
    pub focused: bool,
    pub blink_epoch: Instant,
    pub dnd: bool,
    pub hits: Hits,
    pub started: Instant,
    last_tick: Instant,
    last_info: Instant,
    last_frame: Instant,
    quit: bool,
    pub cell: CellMetrics,
    pub cur_h: f64,
    pub snapshot: Option<(PathBuf, std::sync::mpsc::Sender<Value>)>,
    last_saved: String,
    restored: bool,
    /// Integrations that are enabled *and* installed on this Mac.
    pub claude_ok: bool,
    pub orca_ok: bool,
    anim_key: (u64, u64, u64, u64, u32),
    settings: Option<settings::SettingsWin>,
    pending_settings: Option<settings::Page>,
    snapshot_settings: Option<(PathBuf, std::sync::mpsc::Sender<Value>)>,
}

pub fn run() -> anyhow::Result<()> {
    // Only the app itself migrates the pre-rename config (never a CLI call such as a
    // build step), and only once the old instance has quit.
    util::migrate_legacy_config();
    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .with_activation_policy(ActivationPolicy::Accessory)
        .with_default_menu(false)
        .build()?;
    let proxy = event_loop.create_proxy();
    ipc::serve(proxy.clone())?;
    let mut app = App::new(proxy);
    event_loop.run_app(&mut app)?;
    ipc::cleanup();
    Ok(())
}

unsafe extern "C" {
    fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
}

/// The open/close animation resizes the window (and frame buffer) every frame;
/// hand the freed buffers back to the OS once it settles.
fn release_memory() {
    unsafe {
        malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
    }
}

fn ease_out_cubic(t: f64) -> f64 {
    1.0 - (1.0 - t).powi(3)
}

impl App {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        let (cfg, cfg_err) = config::load();
        let (pal, theme_err) = theme::load_with(&cfg.theme, cfg.accent.as_deref());
        let mut fonts = Fonts::new(&cfg.font.family, cfg.font.bold.as_deref());
        fonts.set_dark(!pal.light);
        let exts: Vec<Box<dyn Extension>> = vec![
            Box::new(ext::claude::Claude::new(&cfg, proxy.clone())),
            Box::new(ext::orca::Orca::new(&cfg, proxy.clone())),
            Box::new(ext::user::User::new(&cfg, proxy.clone())),
            Box::new(ext::core::Core),
        ];
        let now = Instant::now();
        Self {
            dnd: cfg.notifications.do_not_disturb,
            cfg,
            cfg_err: cfg_err.or(theme_err),
            cfg_mtime: config::mtime(),
            last_cfg_check: now,
            pal,
            fonts,
            font_delta: 0.0,
            proxy,
            window: None,
            presenter: None,
            geo: None,
            vis: Vis::Hidden,
            tabs: vec![],
            active: 0,
            next_id: 1,
            exts,
            palette: None,
            toasts: vec![],
            statuses: BTreeMap::new(),
            hotkey_mgr: None,
            registered: vec![],
            tray: None,
            mods: Modifiers::default(),
            mouse: MouseState::default(),
            focused: false,
            blink_epoch: now,
            hits: Hits::default(),
            started: now,
            last_tick: now,
            last_info: now,
            last_frame: now,
            quit: false,
            cell: CellMetrics::default(),
            cur_h: 1.0,
            snapshot: None,
            last_saved: String::new(),
            restored: false,
            claude_ok: false,
            orca_ok: false,
            anim_key: (0, 0, 0, 0, 0),
            settings: None,
            pending_settings: None,
            snapshot_settings: None,
        }
    }

    // ───────────────────────────── geometry ─────────────────────────────

    fn font_px(&self, scale: f64) -> f32 {
        ((self.cfg.font.size + self.font_delta).clamp(8.0, 40.0)) * scale as f32
    }

    fn compute_geometry(&self) -> Option<Geometry> {
        let s = platform::target_screen(self.cfg.window.screen == config::ScreenMode::Mouse)?;
        let fr = s.frame;
        let top = fr.max_y() - s.menubar;
        let avail_h = top - fr.origin.y;
        let panel_h = (avail_h * self.cfg.window.height as f64).round().max(160.0);
        let shape = self.cfg.window.shape;
        let (x, panel_w, gap) = match shape {
            Shape::Strip => (fr.origin.x, fr.size.width, 0.0),
            Shape::Centred | Shape::Floating => {
                let w = (fr.size.width * self.cfg.window.width as f64).round();
                (fr.origin.x + ((fr.size.width - w) / 2.0).round(), w, if shape == Shape::Floating { FLOAT_GAP } else { 0.0 })
            }
        };
        let w = if shape == Shape::Strip { panel_w } else { panel_w + SHADOW };
        Some(Geometry { screen: s, x, top: top - gap, w, h: panel_h + SHADOW, panel_w, panel_h, scale: s.scale })
    }

    /// Terminal area inside the panel, in points relative to the panel's top-left.
    pub fn term_rect_pt(&self, geo: &Geometry) -> (f64, f64, f64, f64) {
        let pad = self.cfg.window.padding as f64;
        let x = pad;
        let y = TABBAR_H + pad * 0.6;
        let w = geo.panel_w - 2.0 * pad;
        let h = geo.panel_h - TABBAR_H - STATUS_H - GRIP_H - pad * 1.2;
        (x, y, w.max(50.0), h.max(30.0))
    }

    fn grid_size(&mut self) -> (usize, usize) {
        let Some(geo) = self.geo else { return (80, 24) };
        let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(geo.scale);
        self.cell = self.fonts.cell_metrics(self.font_px(scale), self.cfg.font.line_height);
        let (_, _, w, h) = self.term_rect_pt(&geo);
        let cols = ((w * scale) as i32 / self.cell.w.max(1)).max(10) as usize;
        let rows = ((h * scale) as i32 / self.cell.h.max(1)).max(3) as usize;
        (cols, rows)
    }

    fn apply_geometry(&mut self) {
        let Some(geo) = self.compute_geometry() else { return };
        self.geo = Some(geo);
        let (cols, rows) = self.grid_size();
        let cell = (self.cell.w as u16, self.cell.h as u16);
        for t in &mut self.tabs {
            t.session.resize(cols, rows, cell);
        }
        if let Some(w) = &self.window {
            if !matches!(self.vis, Vis::Hidden) {
                self.cur_h = geo.h;
                platform::set_frame(w, CGRect::new(geo.x, geo.top - geo.h, geo.w, geo.h));
            }
        }
        self.request_redraw();
    }

    fn set_window_height(&mut self, h: f64) {
        let (Some(w), Some(geo)) = (&self.window, self.geo) else { return };
        let h = h.clamp(1.0, geo.h);
        if (h - self.cur_h).abs() < 0.5 {
            return;
        }
        self.cur_h = h;
        platform::set_frame(w, CGRect::new(geo.x, geo.top - h, geo.w, h));
    }

    // ───────────────────────────── visibility ─────────────────────────────

    fn visible(&self) -> bool {
        matches!(self.vis, Vis::Shown | Vis::Opening(_))
    }

    fn toggle(&mut self) {
        if self.visible() { self.hide() } else { self.show() }
    }

    fn show(&mut self) {
        if self.visible() {
            if let Some(w) = &self.window {
                platform::show(w);
            }
            return;
        }
        let new_geo = self.compute_geometry();
        let changed = match (self.geo, new_geo) {
            (Some(a), Some(b)) => {
                a.w != b.w || a.h != b.h || a.x != b.x || a.top != b.top || a.scale != b.scale
            }
            _ => true,
        };
        if changed {
            self.apply_geometry();
        }
        if self.tabs.is_empty() {
            self.new_tab(TabSpec::shell(None));
        }
        let Some(geo) = self.geo else { return };
        let Some(w) = self.window.clone() else { return };
        let start_h = if self.cfg.motion.slide && self.cfg.motion.open_ms > 0 { 2.0 } else { geo.h };
        self.cur_h = start_h;
        platform::set_frame(&w, CGRect::new(geo.x, geo.top - start_h, geo.w, start_h));
        platform::set_blur(&w, self.cfg.window.blur);
        platform::show(&w);
        self.vis = if self.cfg.motion.open_ms == 0 { Vis::Shown } else { Vis::Opening(Instant::now()) };
        self.blink_epoch = Instant::now();
        self.request_redraw();
    }

    fn hide(&mut self) {
        match self.vis {
            Vis::Hidden | Vis::Closing(_) => {}
            _ => {
                self.palette = None;
                self.mouse = MouseState::default();
                if self.cfg.motion.close_ms == 0 {
                    self.finish_hide();
                } else {
                    self.vis = Vis::Closing(Instant::now());
                    self.request_redraw();
                }
            }
        }
    }

    fn finish_hide(&mut self) {
        self.vis = Vis::Hidden;
        if let Some(p) = &mut self.presenter {
            p.trim();
        }
        release_memory();
        if let Some(w) = &self.window {
            platform::order_out(w);
        }
        if platform::app_is_active() {
            platform::hide_app();
        }
    }

    /// Advance open/close animations. Returns (progress for dissolve, done?).
    pub fn anim_progress(&mut self) -> f32 {
        let Some(geo) = self.geo else { return 1.0 };
        match self.vis {
            Vis::Opening(t0) => {
                let t = (t0.elapsed().as_secs_f64() * 1000.0 / self.cfg.motion.open_ms.max(1) as f64).min(1.0);
                if self.cfg.motion.slide {
                    let e = ease_out_cubic(t);
                    // Whole-point steps of 2 so the slide feels "pixel".
                    let h = ((geo.h * e) / 2.0).round() * 2.0;
                    self.set_window_height(h.max(2.0));
                }
                if t >= 1.0 {
                    self.set_window_height(geo.h);
                    self.vis = Vis::Shown;
                    release_memory();
                    return 1.0;
                }
                t as f32
            }
            Vis::Closing(t0) => {
                let t = (t0.elapsed().as_secs_f64() * 1000.0 / self.cfg.motion.close_ms.max(1) as f64).min(1.0);
                if t >= 1.0 {
                    self.finish_hide();
                    return 0.0;
                }
                if self.cfg.motion.slide {
                    let e = 1.0 - t * t * t;
                    let h = ((geo.h * e) / 2.0).round() * 2.0;
                    self.set_window_height(h.max(2.0));
                }
                (1.0 - t) as f32
            }
            _ => 1.0,
        }
    }

    pub fn request_redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    // ───────────────────────────── tabs ─────────────────────────────

    fn new_tab(&mut self, spec: TabSpec) -> Option<u64> {
        self.new_tab_with(spec, None, None, None)
    }

    /// tmux keeps tab processes alive while fluteterm is closed.
    pub fn use_tmux(&self) -> bool {
        match self.cfg.session.backend {
            config::Backend::Plain => false,
            config::Backend::Tmux | config::Backend::Auto => crate::session::tmux_path().is_some(),
        }
    }

    /// Create a tab. `id`/`tmux` are given when restoring; `scrollback` is replayed (plain backend).
    fn new_tab_with(&mut self, spec: TabSpec, id: Option<u64>, tmux: Option<String>, scrollback: Option<String>) -> Option<u64> {
        if self.geo.is_none() {
            self.geo = self.compute_geometry();
        }
        let (cols, rows) = self.grid_size();
        let id = id.unwrap_or_else(|| {
            let id = self.next_id;
            self.next_id += 1;
            id
        });
        self.next_id = self.next_id.max(id + 1);
        let cwd = spec.cwd.clone().or_else(|| self.active_tab().and_then(|t| t.cwd.clone()));
        let mut tmux_name = None;
        let sp = if self.use_tmux() {
            let name = tmux.unwrap_or_else(|| format!("ft-{id}"));
            let dir = cwd.clone().filter(|p| p.is_dir()).unwrap_or_else(util::home);
            let inner = spec.command.as_ref().map(|(c, a)| term::command_line(&self.cfg, c, a, spec.keep_shell));
            match crate::session::tmux_command(&name, id, &dir, inner) {
                Some(cmd) => {
                    tmux_name = Some(name);
                    Spawn { command: Some(cmd), cwd: Some(dir), keep_shell: false, direct: true, env: vec![] }
                }
                None => Spawn { command: spec.command.clone(), cwd: cwd.clone(), keep_shell: spec.keep_shell, direct: false, env: vec![] },
            }
        } else {
            Spawn { command: spec.command.clone(), cwd: cwd.clone(), keep_shell: spec.keep_shell, direct: false, env: vec![] }
        };
        match Session::spawn(id, &sp, &self.cfg, cols, rows, (self.cell.w as u16, self.cell.h as u16), self.proxy.clone()) {
            Ok(session) => {
                if let Some(text) = scrollback.filter(|s| !s.trim().is_empty()) {
                    // Replay saved history dimmed, then a marker, before the new shell prints.
                    let mut bytes = b"\x1b[2m".to_vec();
                    bytes.extend_from_slice(text.trim_end().replace('\n', "\r\n").as_bytes());
                    bytes.extend_from_slice(b"\x1b[0m\r\n\x1b[2m\xe2\x94\x80\xe2\x94\x80 restored \xe2\x94\x80\xe2\x94\x80\x1b[0m\r\n");
                    let mut parser: alacritty_terminal::vte::ansi::Processor = alacritty_terminal::vte::ansi::Processor::new();
                    let mut term = session.term.lock();
                    parser.advance(&mut *term, &bytes);
                }
                let tab = Tab {
                    id,
                    session,
                    spec_kind: spec.kind,
                    spec_title: spec.title,
                    osc_title: None,
                    cwd: sp.cwd.clone().or(cwd),
                    fg: None,
                    decor: TabDecor { kind: None, subtitle: None, badge: Badge::None },
                    bell: None,
                    command: spec.command.map(|(c, a)| std::iter::once(c).chain(a).collect()),
                    tmux: tmux_name,
                    pane_pid: None,
                    spawned: Instant::now(),
                    sel_keep: std::cell::RefCell::new(None),
                };
                let at = if self.tabs.is_empty() { 0 } else { (self.active + 1).min(self.tabs.len()) };
                self.tabs.insert(at, tab);
                self.active = at;
                self.request_redraw();
                Some(id)
            }
            Err(e) => {
                self.toast_error(format!("could not start: {e}"));
                None
            }
        }
    }

    /// Close a tab on the user's request (⌘W, middle click): also ends its tmux session.
    fn close_tab_user(&mut self, id: u64) {
        if let Some(name) = self.tab_by_id(id).and_then(|t| t.tmux.clone()) {
            crate::session::tmux_kill(&name);
        }
        self.close_tab(id);
    }

    fn close_tab(&mut self, id: u64) {
        let Some(pos) = self.tabs.iter().position(|t| t.id == id) else { return };
        self.tabs.remove(pos);
        for e in &mut self.exts {
            e.on_tab_closed(id);
        }
        if self.tabs.is_empty() {
            self.active = 0;
            self.hide();
            return;
        }
        if self.active >= self.tabs.len() || pos < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.request_redraw();
    }

    fn focus_tab(&mut self, id: u64) {
        if let Some(pos) = self.tabs.iter().position(|t| t.id == id) {
            self.active = pos;
            self.blink_epoch = Instant::now();
            self.request_redraw();
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn tab_by_id(&self, id: u64) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    fn write_active(&mut self, bytes: Vec<u8>) {
        if let Some(t) = self.tabs.get(self.active) {
            let mut term = t.session.term.lock();
            if term.grid().display_offset() != 0 {
                term.scroll_display(Scroll::Bottom);
            }
            t.clear_selection(&mut term);
            drop(term);
            t.session.write(bytes);
        }
        self.blink_epoch = Instant::now();
    }

    fn paste(&mut self, text: &str) {
        let Some(t) = self.tabs.get(self.active) else { return };
        let bracketed = t.session.term.lock().mode().contains(TermMode::BRACKETED_PASTE);
        let mut data = String::new();
        if bracketed {
            data.push_str("\x1b[200~");
            data.push_str(&text.replace('\x1b', ""));
            data.push_str("\x1b[201~");
        } else {
            data.push_str(&text.replace("\r\n", "\r").replace('\n', "\r"));
        }
        self.write_active(data.into_bytes());
    }

    fn tab_infos(&self) -> Vec<TabInfo> {
        self.tabs.iter().map(Tab::info).collect()
    }

    /// Refresh foreground process / cwd and extension decorations.
    fn refresh_tab_info(&mut self) {
        for t in &mut self.tabs {
            if let Some(name) = &t.tmux {
                // Inside tmux the PTY's foreground is the tmux client; look through to the pane.
                if t.pane_pid.is_none() && t.spawned.elapsed() > Duration::from_millis(300) {
                    t.pane_pid = crate::session::tmux_pane_pid(name);
                }
                if let Some(pid) = t.pane_pid {
                    let fg = crate::session::tty_foreground_of(pid).unwrap_or(pid);
                    t.fg = util::process_name(fg).or_else(|| util::process_name(pid));
                    if let Some(c) = util::process_cwd(fg).or_else(|| util::process_cwd(pid)) {
                        t.cwd = Some(c);
                    }
                }
            } else {
                t.fg = t.session.foreground_name();
                if let Some(c) = t.session.cwd() {
                    t.cwd = Some(c);
                }
            }
            // Decide the kind from scratch each refresh (so finished commands revert to "shell").
            t.decor = TabDecor { kind: None, subtitle: None, badge: Badge::None };
            let mut info = t.info();
            let mut decor = TabDecor { kind: None, subtitle: None, badge: Badge::None };
            for e in &self.exts {
                if let Some(d) = e.tab_decor(&info) {
                    if decor.kind.is_none() {
                        decor.kind = d.kind;
                        if let Some(k) = d.kind {
                            info.kind = k;
                        }
                    }
                    if decor.subtitle.is_none() {
                        decor.subtitle = d.subtitle;
                    }
                    if decor.badge == Badge::None {
                        decor.badge = d.badge;
                    }
                }
            }
            t.decor = decor;
        }
    }

    // ───────────────────────────── extensions ─────────────────────────────

    fn with_ctx<R>(&mut self, f: impl FnOnce(&mut Vec<Box<dyn Extension>>, &mut Ctx) -> R) -> R {
        let infos = self.tab_infos();
        let active = self.active_tab().map(|t| t.id);
        let visible = self.visible() && platform::app_is_active();
        let mut exts = std::mem::take(&mut self.exts);
        let (r, actions) = {
            let mut ctx = Ctx { tabs: &infos, active, visible, cfg: &self.cfg, now: Instant::now(), actions: vec![] };
            let r = f(&mut exts, &mut ctx);
            (r, ctx.actions)
        };
        self.exts = exts;
        for a in actions {
            self.run_action(a);
        }
        r
    }

    // ───────────────────────────── persistence ─────────────────────────────

    fn session_state(&self) -> crate::session::SavedSession {
        use crate::session::{SavedSession, SavedTab, kind_to_str};
        SavedSession {
            version: 1,
            next_id: self.next_id,
            active: self.active,
            tabs: self
                .tabs
                .iter()
                .map(|t| {
                    let mut ext = serde_json::Map::new();
                    for e in &self.exts {
                        if let Some(v) = e.save_tab(t.id) {
                            ext.insert(e.id().to_string(), v);
                        }
                    }
                    SavedTab {
                        id: t.id,
                        kind: kind_to_str(t.kind()),
                        title: t.spec_title.clone(),
                        cwd: t.cwd.clone(),
                        command: t.command.clone(),
                        tmux: t.tmux.clone(),
                        ext,
                    }
                })
                .collect(),
        }
    }

    /// Persist the layout when it changed (cheap: compares the JSON).
    fn save_session_if_changed(&mut self) {
        if !self.restored || !self.cfg.session.restore {
            return;
        }
        let state = self.session_state();
        let json = serde_json::to_string(&state).unwrap_or_default();
        if json != self.last_saved {
            crate::session::save(&state);
            self.last_saved = json;
        }
    }

    /// Full save on quit: layout + (plain backend) scrollback text.
    fn save_session_final(&mut self) {
        if !self.cfg.session.restore || !self.restored {
            return;
        }
        crate::session::save(&self.session_state());
        if self.use_tmux() {
            return;
        }
        let dir = crate::session::scrollback_dir();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let keep = self.cfg.session.scrollback_lines;
        for t in &self.tabs {
            let term = t.session.term.lock();
            let grid = term.grid();
            let cols = grid.columns();
            let top = -(grid.history_size() as i32);
            let bottom = grid.screen_lines() as i32 - 1;
            let text = term.bounds_to_string(Point::new(Line(top), Column(0)), Point::new(Line(bottom), Column(cols - 1)));
            let lines: Vec<&str> = text.lines().collect();
            let mut end = lines.len();
            while end > 0 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            let start = end.saturating_sub(keep);
            let _ = std::fs::write(dir.join(format!("{}.txt", t.id)), lines[start..end].join("\n"));
        }
    }

    /// Bring back the previous session (or start fresh).
    fn restore_session(&mut self) {
        use crate::session::{self as sess, kind_from_str};
        self.restored = true;
        let tmux = self.use_tmux();
        if tmux {
            sess::write_conf();
        }
        let saved = if self.cfg.session.restore { sess::load() } else { None };
        let live = if tmux { sess::tmux_sessions() } else { vec![] };
        let mut used: Vec<String> = vec![];
        if let Some(saved) = saved {
            self.next_id = saved.next_id.max(1);
            for st in &saved.tabs {
                let kind = kind_from_str(&st.kind);
                let name = st.tmux.clone().unwrap_or_else(|| format!("ft-{}", st.id));
                let alive = tmux && live.contains(&name);
                let spec = if alive {
                    // Re-attach: tmux ignores the command for an existing session.
                    TabSpec { kind, title: st.title.clone(), command: None, cwd: st.cwd.clone(), keep_shell: false }
                } else {
                    let claude_id = st.ext.get("claude").and_then(|c| c["session_id"].as_str()).map(String::from);
                    let command = match (kind, &st.command, claude_id) {
                        (TabKind::Claude, _, Some(id)) => Some((self.cfg.claude.command.clone(), vec!["--resume".into(), id])),
                        (TabKind::Claude, _, None) => Some((self.cfg.claude.command.clone(), vec!["--continue".into()])),
                        (_, Some(c), _) if !c.is_empty() => Some((c[0].clone(), c[1..].to_vec())),
                        _ => None,
                    };
                    let keep_shell = command.is_some() && kind == TabKind::Claude;
                    TabSpec { kind, title: st.title.clone(), command, cwd: st.cwd.clone(), keep_shell }
                };
                let scroll = if tmux { None } else { std::fs::read_to_string(sess::scrollback_dir().join(format!("{}.txt", st.id))).ok() };
                if self.new_tab_with(spec, Some(st.id), if tmux { Some(name.clone()) } else { None }, scroll).is_some() {
                    used.push(name);
                    let id = st.id;
                    for (ext_id, v) in &st.ext {
                        if let Some(e) = self.exts.iter_mut().find(|e| e.id() == ext_id) {
                            e.restore_tab(id, v);
                        }
                    }
                }
            }
            self.active = saved.active.min(self.tabs.len().saturating_sub(1));
        }
        // Adopt tmux sessions that were never saved (e.g. fluteterm crashed).
        for name in live.into_iter().filter(|n| !used.contains(n)) {
            let id = crate::session::id_from_name(&name).filter(|id| self.tab_by_id(*id).is_none());
            self.new_tab_with(TabSpec::shell(None), id, Some(name), None);
        }
        if self.tabs.is_empty() {
            self.new_tab(TabSpec::shell(None));
        }
        self.last_saved.clear();
    }

    /// Quit and end every tab's processes (the normal quit keeps them for next time).
    fn quit_and_end_sessions(&mut self) {
        for t in &self.tabs {
            if let Some(name) = &t.tmux {
                crate::session::tmux_kill(name);
            }
        }
        let _ = std::fs::remove_file(util::config_dir().join("session.json"));
        let _ = std::fs::remove_dir_all(crate::session::scrollback_dir());
        self.cfg.session.restore = false;
        self.quit = true;
    }

    fn tick(&mut self) {
        self.refresh_tab_info();
        self.with_ctx(|exts, ctx| {
            for e in exts.iter_mut() {
                e.tick(ctx);
            }
        });
        // Live-refresh the Orca palette.
        if self.palette.as_ref().map(|p| p.mode == PaletteMode::Orca).unwrap_or(false) {
            let items = self.gather_commands();
            if let Some(p) = &mut self.palette {
                p.set_items(items);
            }
        }
        // Expire toasts.
        let n = self.toasts.len();
        self.toasts.retain(|t| t.born.elapsed() < t.ttl);
        if n != self.toasts.len() {
            self.request_redraw();
        }
        self.update_tray();
        self.save_session_if_changed();
    }

    fn update_tray(&mut self) {
        let attention = self.exts.iter().any(|e| e.attention());
        let sessions: Vec<tray::MenuSession> = self
            .tabs
            .iter()
            .map(|t| {
                let badge = match t.decor.badge {
                    Badge::Spinner => " — working",
                    Badge::Attention => " — needs you",
                    Badge::Done => " — done",
                    _ => "",
                };
                let place = t.decor.subtitle.clone().or_else(|| t.cwd.as_deref().map(util::tildify)).unwrap_or_default();
                tray::MenuSession { id: t.id, label: format!("{}  {}{}", t.title(), place, badge) }
            })
            .collect();
        let agents: Vec<tray::MenuAgent> =
            self.exts.iter().filter(|e| e.id() == "orca").flat_map(|e| e.tray_lines()).map(|label| tray::MenuAgent { label }).collect();
        let hotkey = self.cfg.hotkey.clone();
        let dnd = self.dnd;
        if let Some(tr) = &mut self.tray {
            tr.set_alert(attention);
            tr.update_menu(&hotkey, &sessions, &agents, dnd, self.claude_ok, self.orca_ok);
        }
    }

    fn gather_commands(&mut self) -> Vec<ext::Command> {
        let infos = self.tab_infos();
        let active = self.active_tab().map(|t| t.id);
        let ctx = Ctx { tabs: &infos, active, visible: true, cfg: &self.cfg, now: Instant::now(), actions: vec![] };
        self.exts.iter().flat_map(|e| e.commands(&ctx)).collect()
    }

    fn open_palette(&mut self, mode: PaletteMode) {
        self.refresh_tab_info();
        let items = self.gather_commands();
        self.palette = Some(PaletteState::new(mode, items));
        self.show();
        self.request_redraw();
    }

    // ───────────────────────────── actions ─────────────────────────────

    fn toast_error(&mut self, msg: String) {
        self.push_toast(Notice {
            title: "fluteterm".into(),
            body: msg,
            tab: None,
            level: Level::Warn,
            actions: vec![],
            native: false,
            toast: true,
        });
    }

    fn push_toast(&mut self, n: Notice) {
        let ttl = if n.level == Level::Warn { Duration::from_secs(14) } else { Duration::from_secs(7) };
        self.toasts.push(Toast { notice: n, born: Instant::now(), ttl });
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        self.request_redraw();
    }

    fn notify(&mut self, n: Notice) {
        let dnd = self.dnd;
        let visible = self.visible();
        let focused = visible && platform::app_is_active();
        if visible {
            // Panel open but another app has focus: banner as well.
            if !focused && n.native && self.cfg.notifications.native && !dnd {
                platform::notify::send(n.title.clone(), None, n.body.clone(), self.cfg.notifications.sound, n.tab, self.proxy.clone());
            }
            if n.toast && self.cfg.notifications.toasts {
                self.push_toast(n);
            }
        } else if n.native && self.cfg.notifications.native && !dnd {
            platform::notify::send(n.title.clone(), None, n.body.clone(), self.cfg.notifications.sound, n.tab, self.proxy.clone());
            // Keep a toast for when the panel comes back.
            if n.level == Level::Warn && n.toast && self.cfg.notifications.toasts {
                self.push_toast(n);
            }
        } else if n.toast && self.cfg.notifications.toasts {
            self.push_toast(n);
        }
    }

    fn persist(&mut self, table: Option<&str>, key: &str, value: String) {
        config::set_value(table, key, &value);
        self.cfg_mtime = config::mtime();
    }

    pub fn run_action(&mut self, a: Action) {
        match a {
            Action::NewTab(spec) => {
                self.new_tab(spec);
                self.palette = None;
                self.show();
            }
            Action::CloseTab(id) => {
                if let Some(id) = id.or_else(|| self.active_tab().map(|t| t.id)) {
                    self.close_tab_user(id);
                }
            }
            Action::FocusTab(id) => {
                self.focus_tab(id);
                self.palette = None;
                self.show();
            }
            Action::NextTab(d) => {
                if !self.tabs.is_empty() {
                    let n = self.tabs.len() as i32;
                    self.active = ((self.active as i32 + d).rem_euclid(n)) as usize;
                    self.request_redraw();
                }
            }
            Action::SendInput { tab, data } => {
                let id = tab.or_else(|| self.active_tab().map(|t| t.id));
                if let Some(t) = id.and_then(|id| self.tab_by_id(id)) {
                    t.session.write_str(&data);
                }
            }
            Action::Notify(n) => self.notify(n),
            Action::RunDetached { program, args } => {
                let res = std::process::Command::new(&program)
                    .args(&args)
                    .env("PATH", util::augmented_path())
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
                if let Err(e) = res {
                    self.toast_error(format!("{program}: {e}"));
                } else {
                    let short = std::path::Path::new(&program).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(program);
                    self.push_toast(Notice {
                        title: "launched".into(),
                        body: format!("{short} {}", args.join(" ")),
                        tab: None,
                        level: Level::Info,
                        actions: vec![],
                        native: false,
                        toast: true,
                    });
                }
            }
            Action::SetAccent(a) => {
                match &a {
                    Some(hex) => self.persist(None, "accent", format!("\"{hex}\"")),
                    None => {
                        config::remove_value(None, "accent");
                        self.cfg_mtime = config::mtime();
                    }
                }
                self.cfg.accent = a;
                self.apply_theme();
                if let Some(s) = &self.settings {
                    s.window.request_redraw();
                }
            }
            Action::SetTheme(name) => {
                self.persist(None, "theme", format!("\"{name}\""));
                self.cfg.theme = name;
                self.apply_theme();
            }
            Action::SetShape(shape) => {
                let s = match shape {
                    Shape::Strip => "strip",
                    Shape::Centred => "centred",
                    Shape::Floating => "floating",
                };
                self.persist(Some("window"), "shape", format!("\"{s}\""));
                self.cfg.window.shape = shape;
                self.apply_geometry();
            }
            Action::SetPattern(p) => {
                let s = match p {
                    Pattern::Bayer => "bayer",
                    Pattern::Scan => "scan",
                    Pattern::Blocks => "blocks",
                    Pattern::Noise => "noise",
                    Pattern::None => "none",
                };
                self.persist(Some("motion"), "pattern", format!("\"{s}\""));
                self.cfg.motion.pattern = p;
                // Demo it.
                self.vis = Vis::Opening(Instant::now());
                self.request_redraw();
            }
            Action::AdjustOpacity(d) => {
                self.cfg.window.opacity = (self.cfg.window.opacity + d).clamp(0.2, 1.0);
                self.persist(Some("window"), "opacity", format!("{:.2}", self.cfg.window.opacity));
                self.request_redraw();
            }
            Action::AdjustBlur(d) => {
                self.cfg.window.blur = (self.cfg.window.blur as i32 + d).clamp(0, 64) as u32;
                self.persist(Some("window"), "blur", self.cfg.window.blur.to_string());
                if let Some(w) = &self.window {
                    platform::set_blur(w, self.cfg.window.blur);
                }
            }
            Action::ToggleCrt => {
                self.cfg.window.crt = !self.cfg.window.crt;
                self.persist(Some("window"), "crt", self.cfg.window.crt.to_string());
                self.request_redraw();
            }
            Action::FontSize(d) => {
                self.font_delta = (self.font_delta + d).clamp(-6.0, 20.0);
                self.fonts.clear_cache();
                self.apply_geometry();
            }
            Action::ResetFontSize => {
                self.font_delta = 0.0;
                self.fonts.clear_cache();
                self.apply_geometry();
            }
            Action::ReloadConfig => self.reload_config(true),
            Action::OpenConfig => {
                let _ = std::process::Command::new("open").arg("-t").arg(config::config_path()).spawn();
            }
            Action::EditConfig => {
                let path = config::config_path().display().to_string();
                self.run_action(Action::NewTab(TabSpec {
                    kind: TabKind::Custom('⚙'),
                    title: Some("config".into()),
                    command: Some(("sh".into(), vec!["-c".into(), "exec ${EDITOR:-nano} \"$1\"".into(), "sh".into(), path])),
                    cwd: Some(util::config_dir()),
                    keep_shell: false,
                }));
            }
            Action::OpenSettings => self.request_settings(None),
            Action::ClaudeHooks(install) => {
                let notice = |title: &str, body: String, level: Level| Notice {
                    title: title.into(),
                    body,
                    tab: None,
                    level,
                    actions: vec![],
                    native: false,
                    toast: true,
                };
                let n = if install {
                    match crate::hooks::install(false) {
                        Ok((r, _)) => notice(
                            "claude hooks installed",
                            format!("{} events → live state in tabs. Restart running Claude sessions to pick them up.", r.changed),
                            Level::Ok,
                        ),
                        Err(e) => notice("could not install hooks", e.to_string(), Level::Warn),
                    }
                } else {
                    match crate::hooks::uninstall() {
                        Ok(r) if r.changed == 0 => notice("claude hooks", "nothing to remove: fluteterm's hooks weren't installed".into(), Level::Info),
                        Ok(r) => notice(
                            "claude hooks removed",
                            format!(
                                "removed {} entries from ~/.claude/settings.json; your other hooks are untouched. Backup: {}",
                                r.changed,
                                r.backup.map(|b| util::tildify(&b)).unwrap_or_default()
                            ),
                            Level::Ok,
                        ),
                        Err(e) => notice("could not remove hooks", e.to_string(), Level::Warn),
                    }
                };
                self.push_toast(n);
                if !self.visible() {
                    self.show();
                }
                if let Some(s) = &self.settings {
                    s.window.request_redraw();
                }
            }
            Action::Show => self.show(),
            Action::Hide => self.hide(),
            Action::Toggle => self.toggle(),
            Action::OpenPalette(mode) => self.open_palette(mode),
            Action::SetDnd(on) => {
                self.dnd = on;
                self.persist(Some("notifications"), "do_not_disturb", on.to_string());
                self.update_tray();
            }
            Action::Copy(s) => util::clipboard_set(&s),
            Action::ExtCall { ext, call } => {
                self.with_ctx(|exts, ctx| {
                    if let Some(e) = exts.iter_mut().find(|e| e.id() == ext) {
                        e.on_call(&call, ctx);
                    }
                });
            }
            Action::Quit => self.quit = true,
        }
    }

    // ───────────────────────────── config ─────────────────────────────

    fn apply_theme(&mut self) {
        let (pal, err) = theme::load_with(&self.cfg.theme, self.cfg.accent.as_deref());
        if let Some(e) = err {
            self.toast_error(e);
        }
        self.fonts.set_dark(!pal.light);
        self.pal = pal;
        self.request_redraw();
    }

    fn register_hotkeys(&mut self) {
        let Some(mgr) = &self.hotkey_mgr else { return };
        let _ = mgr.unregister_all(&self.registered);
        self.registered.clear();
        let mut errors = vec![];
        for k in std::iter::once(&self.cfg.hotkey).chain(self.cfg.extra_hotkeys.iter()) {
            match HotKey::from_str(k) {
                Ok(hk) => match mgr.register(hk) {
                    Ok(()) => self.registered.push(hk),
                    Err(e) => errors.push(format!("hotkey {k}: {e}")),
                },
                Err(e) => errors.push(format!("hotkey {k}: {e}")),
            }
        }
        for e in errors {
            self.toast_error(e);
        }
    }

    /// Detect which integrations are usable (enabled + CLI found).
    pub fn detect_integrations(&mut self) {
        self.claude_ok = self.cfg.claude.enabled && util::which(&self.cfg.claude.command).is_some();
        self.orca_ok = self.cfg.orca.enabled && util::which(&self.cfg.orca.command).is_some();
    }

    fn reload_config(&mut self, force: bool) {
        let mtime = config::mtime();
        if !force && mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        let (cfg, err) = config::load();
        if let Some(e) = err {
            self.toast_error(e);
            return;
        }
        let font_changed = cfg.font.family != self.cfg.font.family || cfg.font.bold != self.cfg.font.bold;
        let hotkeys_changed = cfg.hotkey != self.cfg.hotkey || cfg.extra_hotkeys != self.cfg.extra_hotkeys;
        self.cfg = cfg;
        self.dnd = self.cfg.notifications.do_not_disturb;
        if font_changed {
            self.fonts = Fonts::new(&self.cfg.font.family, self.cfg.font.bold.as_deref());
        }
        self.apply_theme();
        if hotkeys_changed {
            self.register_hotkeys();
        }
        for e in &mut self.exts {
            e.reconfigure(&self.cfg);
        }
        self.detect_integrations();
        if let Some(w) = &self.window {
            platform::set_blur(w, self.cfg.window.blur);
        }
        self.apply_geometry();
        self.update_tray();
    }

    // ───────────────────────────── IPC ─────────────────────────────

    fn handle_ipc(&mut self, req: ipc::Request) {
        let b = &req.body;
        let cmd = b["cmd"].as_str().unwrap_or("");
        let tab_arg = |v: &Value| -> Option<u64> { v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())) };
        let reply = match cmd {
            "ping" => json!({"ok": true, "version": env!("CARGO_PKG_VERSION")}),
            "layer" => match &self.presenter {
                // Self-check of the display layer (used by tests / debugging).
                Some(p) => {
                    let (f, has_contents, scale, flipped) = p.layer_state();
                    let win = self.window.as_ref().map(|w| (w.inner_size().width, w.inner_size().height, w.scale_factor()));
                    json!({"ok": true, "frame": [f.origin.x, f.origin.y, f.size.width, f.size.height], "contents": has_contents,
                           "scale": scale, "flipped": flipped, "window_px": win.map(|w| [w.0 as f64, w.1 as f64]), "window_scale": win.map(|w| w.2)})
                }
                None => json!({"ok": false, "error": "no presenter"}),
            },
            "toggle" => {
                self.toggle();
                json!({"ok": true, "visible": self.visible()})
            }
            "show" => {
                self.show();
                json!({"ok": true})
            }
            "hide" => {
                self.hide();
                json!({"ok": true})
            }
            "notify" => {
                let tab = tab_arg(&b["tab"]);
                let level = match b["level"].as_str() {
                    Some("warn") | Some("warning") => Level::Warn,
                    Some("ok") | Some("success") => Level::Ok,
                    _ => Level::Info,
                };
                let actions = tab.map(|id| vec![("go to tab".to_string(), Action::FocusTab(id))]).unwrap_or_default();
                self.notify(Notice {
                    title: b["title"].as_str().unwrap_or("fluteterm").to_string(),
                    body: b["body"].as_str().unwrap_or("").to_string(),
                    tab,
                    level,
                    actions,
                    native: true,
                    toast: true,
                });
                json!({"ok": true})
            }
            "open" => {
                let cwd = b["cwd"].as_str().map(util::expand_tilde);
                let profile = b["profile"].as_str().unwrap_or("shell");
                let command: Option<Vec<String>> =
                    b["command"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect());
                let spec = if let Some(c) = command.filter(|c| !c.is_empty()) {
                    TabSpec {
                        kind: TabKind::Custom('▸'),
                        title: b["title"].as_str().map(String::from).or_else(|| Some(c[0].clone())),
                        command: Some((c[0].clone(), c[1..].to_vec())),
                        cwd,
                        keep_shell: b["keep_shell"].as_bool().unwrap_or(true),
                    }
                } else {
                    match profile {
                        "claude" => TabSpec {
                            kind: TabKind::Claude,
                            title: Some("claude".into()),
                            command: Some((self.cfg.claude.command.clone(), self.cfg.claude.args.clone())),
                            cwd,
                            keep_shell: true,
                        },
                        "shell" => TabSpec::shell(cwd),
                        name => match self.cfg.profiles.iter().find(|p| p.name == name) {
                            Some(p) => ext::user::User::profile_spec(p, cwd),
                            None => TabSpec::shell(cwd),
                        },
                    }
                };
                self.palette = None;
                let id = self.new_tab(spec);
                if b["show"].as_bool().unwrap_or(true) {
                    self.show();
                }
                json!({"ok": id.is_some(), "tab": id})
            }
            "send" => {
                let id = tab_arg(&b["tab"]).or_else(|| self.active_tab().map(|t| t.id));
                let text = b["text"].as_str().unwrap_or("");
                match id.and_then(|id| self.tab_by_id(id)) {
                    Some(t) => {
                        t.session.write_str(text);
                        json!({"ok": true})
                    }
                    None => json!({"ok": false, "error": "no such tab"}),
                }
            }
            "do" => {
                // Run a palette command by (fuzzy) title — scripting & testing.
                let q = b["command"].as_str().unwrap_or("").to_string();
                self.refresh_tab_info();
                let items = self.gather_commands();
                let mut p = PaletteState::new(PaletteMode::All, items);
                p.query = q.clone();
                p.refilter();
                match p.selected().cloned() {
                    Some(c) => {
                        let alt = b["alt"].as_bool().unwrap_or(false);
                        let action = if alt { c.alt_action.clone().unwrap_or(c.action.clone()) } else { c.action.clone() };
                        self.run_action(action);
                        json!({"ok": true, "ran": c.title})
                    }
                    None => json!({"ok": false, "error": format!("no command matches \"{q}\"")}),
                }
            }
            "read" => {
                let id = tab_arg(&b["tab"]).or_else(|| self.active_tab().map(|t| t.id));
                match id.and_then(|id| self.tab_by_id(id)) {
                    Some(t) => {
                        let term = t.session.term.lock();
                        let cols = term.grid().columns();
                        let rows = term.grid().screen_lines();
                        let lines: Vec<String> = (0..rows as i32)
                            .map(|r| {
                                term.bounds_to_string(Point::new(Line(r), Column(0)), Point::new(Line(r), Column(cols - 1)))
                                    .trim_end()
                                    .to_string()
                            })
                            .collect();
                        json!({"ok": true, "tab": t.id, "cols": cols, "rows": rows, "text": lines.join("\n")})
                    }
                    None => json!({"ok": false, "error": "no such tab"}),
                }
            }
            "focus" => match tab_arg(&b["tab"]) {
                Some(id) => {
                    self.run_action(Action::FocusTab(id));
                    json!({"ok": true})
                }
                None => json!({"ok": false}),
            },
            "list" => {
                let active = self.active_tab().map(|t| t.id);
                let tabs: Vec<Value> = self
                    .tabs
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "title": t.title(),
                            "kind": format!("{:?}", t.kind()).to_lowercase(),
                            "cwd": t.cwd.as_ref().map(|p| p.display().to_string()),
                            "process": t.fg,
                            "active": Some(t.id) == active,
                        })
                    })
                    .collect();
                json!({"ok": true, "visible": self.visible(), "tabs": tabs})
            }
            "event" => {
                let source = b["source"].as_str().unwrap_or("").to_string();
                let tab = tab_arg(&b["tab"]);
                let payload = b["payload"].clone();
                let handled = self.with_ctx(|exts, ctx| {
                    let mut handled = false;
                    for e in exts.iter_mut() {
                        handled |= e.on_event(&source, tab, &payload, ctx);
                    }
                    handled
                });
                self.refresh_tab_info();
                self.request_redraw();
                json!({"ok": true, "handled": handled})
            }
            "status" => {
                let key = b["key"].as_str().unwrap_or("custom").to_string();
                let text = b["text"].as_str().unwrap_or("").to_string();
                if text.is_empty() {
                    self.statuses.remove(&key);
                } else {
                    self.statuses.insert(key, text);
                }
                self.request_redraw();
                json!({"ok": true})
            }
            "accent" => {
                let v = b["color"].as_str().map(str::trim).unwrap_or("");
                if v.is_empty() || v == "default" || v == "theme" || v == "none" {
                    self.run_action(Action::SetAccent(None));
                    json!({"ok": true, "accent": null})
                } else {
                    let hex = theme::ACCENTS.iter().find(|(n, _)| *n == v).map(|(_, h)| h.to_string()).unwrap_or_else(|| v.to_string());
                    match theme::Rgb::hex(&hex) {
                        Some(c) => {
                            self.run_action(Action::SetAccent(Some(c.to_hex())));
                            json!({"ok": true, "accent": c.to_hex()})
                        }
                        None => json!({"ok": false, "error": format!("\"{v}\" is not a colour (use #rrggbb or a name)")}),
                    }
                }
            }
            "theme" => {
                if let Some(n) = b["name"].as_str() {
                    self.run_action(Action::SetTheme(n.to_string()));
                }
                json!({"ok": true})
            }
            "palette" => {
                let mode = match b["mode"].as_str() {
                    Some("agents") => PaletteMode::Agents,
                    Some("orca") => PaletteMode::Orca,
                    Some("themes") => PaletteMode::Themes,
                    _ => PaletteMode::All,
                };
                self.open_palette(mode);
                json!({"ok": true})
            }
            "reload" => {
                self.reload_config(true);
                json!({"ok": true})
            }
            "settings" => {
                let page = match b["page"].as_str() {
                    Some("fonts") => Some(settings::Page::Fonts),
                    Some("keys") => Some(settings::Page::Keys),
                    Some("window") => Some(settings::Page::Window),
                    Some("notify") => Some(settings::Page::Notify),
                    Some("claude") => Some(settings::Page::Claude),
                    Some("orca") => Some(settings::Page::Orca),
                    Some("custom") => Some(settings::Page::Custom),
                    Some("look") => Some(settings::Page::Look),
                    _ => None,
                };
                if b["close"].as_bool() == Some(true) {
                    self.close_settings();
                } else {
                    self.request_settings(page);
                }
                json!({"ok": true})
            }
            "snapshot" if b["window"].as_str() == Some("settings") => {
                let path = PathBuf::from(b["path"].as_str().unwrap_or("/tmp/fluteterm-settings.png"));
                match &self.settings {
                    Some(s) => {
                        self.snapshot_settings = Some((path, req.reply.clone()));
                        s.window.request_redraw();
                        return;
                    }
                    None => json!({"ok": false, "error": "settings window is not open"}),
                }
            }
            "snapshot" => {
                let path = PathBuf::from(b["path"].as_str().unwrap_or("/tmp/fluteterm.png"));
                self.snapshot = Some((path, req.reply.clone()));
                self.request_redraw();
                return;
            }
            "quit" => {
                self.quit = true;
                json!({"ok": true})
            }
            _ => json!({"ok": false, "error": format!("unknown cmd \"{cmd}\"")}),
        };
        let _ = req.reply.send(reply);
    }

    // ───────────────────────────── terminal events ─────────────────────────────

    fn handle_term_event(&mut self, id: u64, ev: TermEvent) {
        let is_active = self.active_tab().map(|t| t.id) == Some(id);
        match ev {
            TermEvent::Wakeup => {
                if is_active && self.visible() {
                    self.request_redraw();
                }
            }
            TermEvent::Title(t) => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                    tab.osc_title = Some(t);
                }
                self.request_redraw();
            }
            TermEvent::ResetTitle => {
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                    tab.osc_title = None;
                }
            }
            TermEvent::PtyWrite(s) => {
                if let Some(t) = self.tab_by_id(id) {
                    t.session.write_str(&s);
                }
            }
            TermEvent::ClipboardStore(_, s) => util::clipboard_set(&s),
            TermEvent::ClipboardLoad(_, f) => {
                let s = f(&util::clipboard_get());
                if let Some(t) = self.tab_by_id(id) {
                    t.session.write_str(&s);
                }
            }
            TermEvent::ColorRequest(idx, f) => {
                let c = draw::color_for_index(&self.pal, idx);
                let s = f(alacritty_terminal::vte::ansi::Rgb { r: c.r, g: c.g, b: c.b });
                if let Some(t) = self.tab_by_id(id) {
                    t.session.write_str(&s);
                }
            }
            TermEvent::TextAreaSizeRequest(f) => {
                if let Some(t) = self.tab_by_id(id) {
                    let s = f(WindowSize {
                        num_lines: t.session.rows as u16,
                        num_cols: t.session.cols as u16,
                        cell_width: self.cell.w as u16,
                        cell_height: self.cell.h as u16,
                    });
                    t.session.write_str(&s);
                }
            }
            TermEvent::Bell => {
                if let Some(t) = self.tabs.iter_mut().find(|t| t.id == id) {
                    t.bell = Some(Instant::now());
                }
                self.request_redraw();
            }
            TermEvent::ChildExit(_) | TermEvent::Exit => {
                self.close_tab(id);
            }
            _ => {}
        }
    }

    // ───────────────────────────── keyboard ─────────────────────────────

    fn on_key(&mut self, ev: KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        if self.palette.is_some() {
            self.palette_key(&ev);
            return;
        }
        let st = self.mods.state();
        let cmd = st.super_key();
        let shift = st.shift_key();
        let ctrl = st.control_key();

        if cmd {
            // Layout-independent: ⌘T / ⌘C / ⌘1 work with any input language.
            let ch = term::keys::shortcut_key(&ev);
            // User profile shortcuts first.
            for p in self.cfg.profiles.clone() {
                if let Some(k) = &p.key {
                    if shortcut_matches(k, &ch, cmd, shift, ctrl, st.alt_key()) {
                        let cwd = self.active_tab().and_then(|t| t.cwd.clone());
                        self.run_action(Action::NewTab(ext::user::User::profile_spec(&p, cwd)));
                        return;
                    }
                }
            }
            let handled = match (ch.as_str(), shift) {
                ("t", false) => {
                    let cwd = self.active_tab().and_then(|t| t.cwd.clone());
                    self.run_action(Action::NewTab(TabSpec::shell(cwd)));
                    true
                }
                ("w", false) => {
                    self.run_action(Action::CloseTab(None));
                    true
                }
                ("k", false) | ("p", true) => {
                    self.open_palette(PaletteMode::All);
                    true
                }
                ("o", true) if !self.orca_ok => true,
                ("o", true) => {
                    self.open_palette(PaletteMode::Orca);
                    true
                }
                ("a", true) => {
                    self.open_palette(PaletteMode::Agents);
                    true
                }
                ("c", true) if !self.claude_ok => {
                    self.toast_error("Claude Code isn't installed (or is turned off in settings → claude)".into());
                    true
                }
                ("c", true) => {
                    let cwd = self.active_tab().and_then(|t| t.cwd.clone());
                    self.run_action(Action::NewTab(TabSpec {
                        kind: TabKind::Claude,
                        title: Some("claude".into()),
                        command: Some((self.cfg.claude.command.clone(), self.cfg.claude.args.clone())),
                        cwd,
                        keep_shell: true,
                    }));
                    true
                }
                (",", false) => {
                    self.run_action(Action::OpenSettings);
                    true
                }
                ("c", false) => {
                    let text = self.active_tab().and_then(|t| {
                        let mut term = t.session.term.lock();
                        t.sync_selection(&mut term);
                        term.selection_to_string()
                    });
                    if let Some(s) = text.filter(|s| !s.is_empty()) {
                        util::clipboard_set(&s);
                    }
                    true
                }
                ("v", false) => {
                    let s = util::clipboard_get();
                    self.paste(&s);
                    true
                }
                ("+", _) | ("=", _) => {
                    self.run_action(Action::FontSize(1.0));
                    true
                }
                ("-", false) => {
                    self.run_action(Action::FontSize(-1.0));
                    true
                }
                ("0", false) => {
                    self.run_action(Action::ResetFontSize);
                    true
                }
                ("]", true) | ("}", _) => {
                    self.run_action(Action::NextTab(1));
                    true
                }
                ("[", true) | ("{", _) => {
                    self.run_action(Action::NextTab(-1));
                    true
                }
                ("q", false) | ("h", false) => {
                    self.hide();
                    true
                }
                ("l", false) => {
                    // Clear scrollback + screen like Terminal.app's ⌘K-ish.
                    self.write_active(vec![0x0c]);
                    true
                }
                (d, false) if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                    let n = (d.as_bytes()[0] - b'1') as usize;
                    let idx = if n == 8 { self.tabs.len().saturating_sub(1) } else { n };
                    if idx < self.tabs.len() {
                        self.active = idx;
                        self.request_redraw();
                    }
                    true
                }
                _ => false,
            };
            if handled {
                return;
            }
        }
        if ctrl && matches!(ev.logical_key, Key::Named(NamedKey::Tab)) {
            self.run_action(Action::NextTab(if shift { -1 } else { 1 }));
            return;
        }
        if shift && !cmd {
            let page = match ev.logical_key {
                Key::Named(NamedKey::PageUp) => Some(Scroll::PageUp),
                Key::Named(NamedKey::PageDown) => Some(Scroll::PageDown),
                Key::Named(NamedKey::Home) => Some(Scroll::Top),
                Key::Named(NamedKey::End) => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(s) = page {
                let alt = self.active_tab().map(|t| t.session.term.lock().mode().contains(TermMode::ALT_SCREEN)).unwrap_or(true);
                if !alt {
                    if let Some(t) = self.active_tab() {
                        t.session.term.lock().scroll_display(s);
                    }
                    self.request_redraw();
                    return;
                }
            }
        }
        let mode = self.active_tab().map(|t| *t.session.term.lock().mode()).unwrap_or(TermMode::NONE);
        if let Some(bytes) = term::keys::encode(&ev, &self.mods, mode, self.cfg.option_as_alt) {
            self.write_active(bytes);
            self.request_redraw();
        }
    }

    fn palette_key(&mut self, ev: &KeyEvent) {
        let st = self.mods.state();
        let Some(p) = &mut self.palette else { return };
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.palette = None,
            Key::Named(NamedKey::ArrowUp) => p.move_sel(-1),
            Key::Named(NamedKey::ArrowDown) => p.move_sel(1),
            Key::Named(NamedKey::PageUp) => p.move_sel(-8),
            Key::Named(NamedKey::PageDown) => p.move_sel(8),
            Key::Named(NamedKey::Tab) => {
                // Skip modes for integrations that aren't available.
                for _ in 0..4 {
                    p.cycle_mode();
                    let ok = match p.mode {
                        PaletteMode::Agents => self.claude_ok,
                        PaletteMode::Orca => self.orca_ok,
                        _ => true,
                    };
                    if ok {
                        break;
                    }
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if st.super_key() {
                    p.query.clear();
                } else {
                    p.query.pop();
                }
                p.sel = 0;
                p.scroll = 0;
                p.refilter();
            }
            Key::Named(NamedKey::Enter) => {
                let action = p.selected().map(|c| if st.super_key() { c.alt_action.clone().unwrap_or(c.action.clone()) } else { c.action.clone() });
                self.palette = None;
                if let Some(a) = action {
                    self.run_action(a);
                }
            }
            _ if st.super_key() && term::keys::shortcut_key(ev) == "k" => self.palette = None,
            _ => {
                if !st.super_key() && !st.control_key() {
                    if let Some(t) = &ev.text {
                        if t.chars().all(|c| !c.is_control()) {
                            p.query.push_str(t);
                            p.sel = 0;
                            p.scroll = 0;
                            p.refilter();
                        }
                    }
                }
            }
        }
        self.request_redraw();
    }

    // ───────────────────────────── mouse ─────────────────────────────

    fn cell_at(&self, x: f64, y: f64) -> Option<(usize, usize, Side)> {
        let t = self.active_tab()?;
        let r = self.hits.term;
        let (x, y) = (x as i32, y as i32);
        if r.w <= 0 {
            return None;
        }
        let col = ((x - r.x).max(0) / self.cell.w.max(1)) as usize;
        let row = ((y - r.y).max(0) / self.cell.h.max(1)) as usize;
        let col = col.min(t.session.cols.saturating_sub(1));
        let row = row.min(t.session.rows.saturating_sub(1));
        let within = (x - r.x).rem_euclid(self.cell.w.max(1));
        let side = if within < self.cell.w / 2 { Side::Left } else { Side::Right };
        Some((row, col, side))
    }

    /// Does the program in the active tab want mouse clicks? Inside tmux we ask tmux about
    /// the pane's program; tmux itself always enables the mouse and must not steal selections.
    fn wants_mouse(&self) -> bool {
        if self.mods.state().shift_key() {
            return false;
        }
        let Some(t) = self.active_tab() else { return false };
        if !t.session.term.lock().mode().intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        match &t.tmux {
            Some(name) => crate::session::tmux_pane_wants_mouse(name),
            None => true,
        }
    }

    fn mouse_report(&self, button: u8, pressed: bool, row: usize, col: usize, motion: bool) -> bool {
        let Some(t) = self.active_tab() else { return false };
        let mode = *t.session.term.lock().mode();
        if !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        if motion && !(mode.contains(TermMode::MOUSE_MOTION) || (mode.contains(TermMode::MOUSE_DRAG) && self.mouse.left_down)) {
            return true;
        }
        let st = self.mods.state();
        let mut b = button as u32 + if motion { 32 } else { 0 };
        if st.shift_key() {
            b += 4;
        }
        if st.alt_key() {
            b += 8;
        }
        if st.control_key() {
            b += 16;
        }
        let s = if mode.contains(TermMode::SGR_MOUSE) {
            format!("\x1b[<{};{};{}{}", b, col + 1, row + 1, if pressed { 'M' } else { 'm' })
        } else {
            let b = if pressed { b } else { 3 + if motion { 32 } else { 0 } };
            let enc = |v: usize| (32 + v.min(222) + 1) as u8 as char;
            format!("\x1b[M{}{}{}", (32 + b) as u8 as char, enc(col), enc(row))
        };
        t.session.write_str(&s);
        true
    }

    fn on_mouse_button(&mut self, state: ElementState, button: MouseButton) {
        if self.vis != Vis::Shown {
            return;
        }
        let (x, y) = (self.mouse.x as i32, self.mouse.y as i32);
        let pressed = state == ElementState::Pressed;
        if button == MouseButton::Left {
            self.mouse.left_down = pressed;
        }

        if !pressed {
            if let Some(_) = self.mouse.grip_drag.take() {
                let h = self.cfg.window.height;
                self.persist(Some("window"), "height", format!("{h:.2}"));
                self.apply_geometry();
                return;
            }
            if self.mouse.selecting {
                self.mouse.selecting = false;
                // Copy-on-select (like iTerm2); the selection itself stays visible.
                if self.cfg.copy_on_select {
                    let text = self.active_tab().and_then(|t| {
                        let mut term = t.session.term.lock();
                        t.sync_selection(&mut term);
                        term.selection_to_string()
                    });
                    if let Some(s) = text.filter(|s| !s.trim().is_empty()) {
                        util::clipboard_set(&s);
                    }
                }
                return;
            }
            if std::mem::take(&mut self.mouse.forward) {
                if let Some((row, col, _)) = self.cell_at(x as f64, y as f64) {
                    let b = match button {
                        MouseButton::Left => 0,
                        MouseButton::Middle => 1,
                        MouseButton::Right => 2,
                        _ => return,
                    };
                    self.mouse_report(b, false, row, col, false);
                }
            }
            return;
        }

        // Palette.
        if self.palette.is_some() {
            if let Some((_, m)) = self.hits.palette_chips.iter().find(|(r, _)| r.contains(x, y)).copied() {
                if let Some(p) = &mut self.palette {
                    p.mode = m;
                    p.sel = 0;
                    p.scroll = 0;
                    p.refilter();
                }
            } else if let Some((_, idx)) = self.hits.palette_rows.iter().find(|(r, _)| r.contains(x, y)).copied() {
                let cmdk = self.mods.state().super_key();
                let action = self.palette.as_ref().and_then(|p| {
                    let c = &p.items[*p.filtered.get(idx)?];
                    Some(if cmdk { c.alt_action.clone().unwrap_or(c.action.clone()) } else { c.action.clone() })
                });
                self.palette = None;
                if let Some(a) = action {
                    self.run_action(a);
                }
            } else if !self.hits.palette_box.contains(x, y) {
                self.palette = None;
            }
            self.request_redraw();
            return;
        }

        // Toast buttons / boxes.
        if let Some(&(_, ti, ai)) = self.hits.toast_buttons.iter().find(|(r, _, _)| r.contains(x, y)) {
            if let Some(t) = self.toasts.get(ti) {
                let a = t.notice.actions.get(ai).map(|(_, a)| a.clone());
                self.toasts.remove(ti);
                if let Some(a) = a {
                    self.run_action(a);
                }
            }
            self.request_redraw();
            return;
        }
        if let Some(&(_, ti)) = self.hits.toast_boxes.iter().find(|(r, _)| r.contains(x, y)) {
            if let Some(t) = self.toasts.get(ti) {
                if let Some(id) = t.notice.tab {
                    self.focus_tab(id);
                }
            }
            self.toasts.remove(ti);
            self.request_redraw();
            return;
        }

        // Clickable status-bar segments.
        if let Some(a) = self.hits.status.iter().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone()) {
            self.run_action(a);
            self.request_redraw();
            return;
        }

        // Tab bar.
        if let Some(&(_, id)) = self.hits.tabs.iter().find(|(r, _)| r.contains(x, y)) {
            if button == MouseButton::Middle {
                self.close_tab_user(id);
            } else {
                self.focus_tab(id);
            }
            return;
        }
        if self.hits.plus.contains(x, y) {
            let cwd = self.active_tab().and_then(|t| t.cwd.clone());
            self.run_action(Action::NewTab(TabSpec::shell(cwd)));
            return;
        }
        if self.hits.close.contains(x, y) {
            self.hide();
            return;
        }
        if self.hits.grip.contains(x, y) {
            self.mouse.grip_drag = Some((self.mouse.y, self.cfg.window.height as f64));
            return;
        }

        // Terminal.
        if !self.hits.term.contains(x, y) {
            return;
        }
        let Some((row, col, side)) = self.cell_at(x as f64, y as f64) else { return };
        let b = match button {
            MouseButton::Left => 0,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        // Only programs that really asked for the mouse get it (⇧ always selects).
        self.mouse.forward = self.wants_mouse();
        if self.mouse.forward && self.mouse_report(b, true, row, col, false) {
            return;
        }
        self.mouse.forward = false;
        if button == MouseButton::Right {
            let s = util::clipboard_get();
            self.paste(&s);
            return;
        }
        if button != MouseButton::Left {
            return;
        }
        // ⌘-click opens URLs.
        if self.mods.state().super_key() {
            if let Some(url) = self.url_at(row, col) {
                util::open_url(&url);
            }
            return;
        }
        let clicks = match self.mouse.last_click {
            Some((t, r, c, n)) if t.elapsed() < Duration::from_millis(400) && r == row && c == col => (n % 3) + 1,
            _ => 1,
        };
        self.mouse.last_click = Some((Instant::now(), row, col, clicks));
        let Some(t) = self.active_tab() else { return };
        let mut term = t.session.term.lock();
        let offset = term.grid().display_offset() as i32;
        let point = Point::new(Line(row as i32 - offset), Column(col));
        let ty = match clicks {
            2 => SelectionType::Semantic,
            3 => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        t.clear_selection(&mut term);
        term.selection = Some(Selection::new(ty, point, side));
        t.sync_selection(&mut term);
        drop(term);
        self.mouse.selecting = true;
        self.request_redraw();
    }

    fn on_mouse_move(&mut self, x: f64, y: f64) {
        self.mouse.x = x;
        self.mouse.y = y;
        if let Some((y0, h0)) = self.mouse.grip_drag {
            if let Some(geo) = self.geo {
                let scale = self.window.as_ref().map(|w| w.scale_factor()).unwrap_or(1.0);
                let avail = geo.screen.frame.size.height - geo.screen.menubar;
                let dh = (y - y0) / scale / avail;
                self.cfg.window.height = (h0 + dh).clamp(0.15, 0.97) as f32;
                let new_geo = self.compute_geometry();
                if let (Some(w), Some(g)) = (&self.window, new_geo) {
                    self.geo = Some(g);
                    self.cur_h = g.h;
                    platform::set_frame(w, CGRect::new(g.x, g.top - g.h, g.w, g.h));
                }
                self.request_redraw();
            }
            return;
        }
        if let Some(w) = &self.window {
            let (xi, yi) = (x as i32, y as i32);
            let icon = if self.hits.grip.contains(xi, yi) {
                CursorIcon::NsResize
            } else if self.hits.term.contains(xi, yi) && self.palette.is_none() {
                CursorIcon::Text
            } else {
                CursorIcon::Default
            };
            w.set_cursor(icon);
        }
        if self.mouse.left_down {
            if let Some((row, col, side)) = self.cell_at(x, y) {
                if self.mouse.forward {
                    self.mouse_report(0, true, row, col, true);
                    return;
                }
                if self.mouse.selecting {
                    let term_r = self.hits.term;
                    if let Some(t) = self.active_tab() {
                        let mut term = t.session.term.lock();
                        // Dragging past the top/bottom edge scrolls the history.
                        if (y as i32) < term_r.y {
                            term.scroll_display(Scroll::Delta(1));
                        } else if (y as i32) >= term_r.bottom() {
                            term.scroll_display(Scroll::Delta(-1));
                        }
                        t.sync_selection(&mut term);
                        let offset = term.grid().display_offset() as i32;
                        if let Some(sel) = &mut term.selection {
                            sel.update(Point::new(Line(row as i32 - offset), Column(col)), side);
                        }
                        t.sync_selection(&mut term);
                    }
                    self.request_redraw();
                }
            }
        } else if let Some((row, col, _)) = self.cell_at(x, y) {
            // Hover reporting only for programs we talk to directly (not through tmux).
            let direct = self.active_tab().map(|t| t.tmux.is_none()).unwrap_or(false);
            if direct && self.hits.term.contains(x as i32, y as i32) && !self.mods.state().shift_key() {
                self.mouse_report(3, true, row, col, true);
            }
        }
    }

    fn on_scroll(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
            MouseScrollDelta::PixelDelta(p) => {
                self.mouse.scroll_acc += p.y / self.cell.h.max(1) as f64;
                let whole = self.mouse.scroll_acc.trunc();
                self.mouse.scroll_acc -= whole;
                whole
            }
        };
        if lines == 0.0 {
            return;
        }
        if let Some(p) = &mut self.palette {
            p.move_sel(if lines > 0.0 { -1 } else { 1 });
            self.request_redraw();
            return;
        }
        let Some(t) = self.active_tab() else { return };
        let mode = *t.session.term.lock().mode();
        let n = lines.abs() as usize;
        if mode.intersects(TermMode::MOUSE_MODE) {
            if let Some((row, col, _)) = self.cell_at(self.mouse.x, self.mouse.y) {
                let b = if lines > 0.0 { 64 } else { 65 };
                for _ in 0..n.min(10) {
                    self.mouse_report(b, true, row, col, false);
                }
            }
        } else if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let seq: &[u8] = match (lines > 0.0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let mut v = vec![];
            for _ in 0..n.min(30) {
                v.extend_from_slice(seq);
            }
            t.session.write(v);
        } else {
            t.session.term.lock().scroll_display(Scroll::Delta(lines as i32));
            self.request_redraw();
        }
    }

    fn url_at(&self, row: usize, col: usize) -> Option<String> {
        let t = self.active_tab()?;
        let term = t.session.term.lock();
        let offset = term.grid().display_offset() as i32;
        let line = Line(row as i32 - offset);
        let cols = term.grid().columns();
        let text = term.bounds_to_string(Point::new(line, Column(0)), Point::new(line, Column(cols - 1)));
        let chars: Vec<char> = text.chars().collect();
        if col >= chars.len() {
            return None;
        }
        let is_url_char = |c: char| !c.is_whitespace() && !"<>\"'`()[]{}".contains(c);
        let mut s = col;
        while s > 0 && is_url_char(chars[s - 1]) {
            s -= 1;
        }
        let mut e = col;
        while e < chars.len() && is_url_char(chars[e]) {
            e += 1;
        }
        let word: String = chars[s..e].iter().collect();
        let word = word.trim_end_matches(['.', ',', ';', ':']);
        if word.contains("://") || word.starts_with("www.") {
            Some(word.to_string())
        } else if word.starts_with('/') || word.starts_with("~/") {
            Some(util::expand_tilde(word).display().to_string())
        } else {
            None
        }
    }
}

fn shortcut_matches(spec: &str, ch: &str, cmd: bool, shift: bool, ctrl: bool, alt: bool) -> bool {
    let spec = spec.to_lowercase();
    let parts: Vec<&str> = spec.split('+').map(str::trim).collect();
    let Some(key) = parts.last() else { return false };
    let want = |m: &str| parts.iter().any(|p| *p == m);
    *key == ch
        && want("cmd") == cmd
        && want("shift") == shift
        && (want("ctrl") || want("control")) == ctrl
        && (want("alt") || want("option") || want("opt")) == alt
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("fluteterm")
            .with_decorations(false)
            .with_transparent(true)
            .with_visible(false)
            .with_resizable(false)
            .with_has_shadow(false)
            .with_inner_size(LogicalSize::new(800.0, 400.0));
        let window = match el.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                eprintln!("fluteterm: cannot create window: {e}");
                el.exit();
                return;
            }
        };
        platform::setup_window(&window);
        let presenter = platform::present::Presenter::new(&window).expect("layer-backed window");
        self.window = Some(window);
        self.presenter = Some(presenter);

        // Global hotkey.
        match GlobalHotKeyManager::new() {
            Ok(m) => {
                self.hotkey_mgr = Some(m);
                let proxy = self.proxy.clone();
                GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                    if e.state() == HotKeyState::Pressed {
                        let _ = proxy.send_event(UserEvent::Hotkey);
                    }
                }));
                self.register_hotkeys();
            }
            Err(e) => self.toast_error(format!("global hotkeys unavailable: {e}")),
        }

        self.detect_integrations();
        // Menu bar item.
        tray::install_handlers(self.proxy.clone());
        self.tray = tray::Tray::new();
        self.update_tray();

        // Warm up: geometry + previous tabs (or a first shell) so the first F12 is instant.
        self.geo = self.compute_geometry();
        self.restore_session();
        if let Some(e) = self.cfg_err.take() {
            self.toast_error(e);
        }
        if std::env::var("FLUTETERM_SHOW_ON_START").is_ok() {
            self.show();
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, ev: UserEvent) {
        match ev {
            UserEvent::Term(id, e) => self.handle_term_event(id, e),
            UserEvent::Hotkey | UserEvent::TrayToggle => self.toggle(),
            UserEvent::Menu(id) => match id.as_str() {
                "toggle" => self.toggle(),
                "new_shell" => self.run_action(Action::NewTab(TabSpec::shell(None))),
                "new_claude" => self.run_action(Action::NewTab(TabSpec {
                    kind: TabKind::Claude,
                    title: Some("claude".into()),
                    command: Some((self.cfg.claude.command.clone(), self.cfg.claude.args.clone())),
                    cwd: None,
                    keep_shell: true,
                })),
                "orca_panel" => self.open_palette(PaletteMode::Orca),
                "palette" => self.open_palette(PaletteMode::All),
                "dnd" => {
                    let on = !self.dnd;
                    self.run_action(Action::SetDnd(on));
                }
                "config" => self.run_action(Action::OpenConfig),
                "settings" => self.request_settings(None),
                "reload" => self.reload_config(true),
                "quit" => self.quit = true,
                "quit_all" => self.quit_and_end_sessions(),
                s if s.starts_with("tab:") => {
                    if let Ok(id) = s[4..].parse() {
                        self.run_action(Action::FocusTab(id));
                    }
                }
                s if s.starts_with("orca:") => self.open_palette(PaletteMode::Orca),
                _ => {}
            },
            UserEvent::Ipc(req) => self.handle_ipc(req),
            UserEvent::NotificationClicked(tab) => {
                if let Some(id) = tab {
                    self.focus_tab(id);
                }
                self.show();
            }
            UserEvent::Ext(id, msg) => {
                self.with_ctx(|exts, ctx| {
                    if let Some(e) = exts.iter_mut().find(|e| e.id() == id) {
                        e.on_message(msg, ctx);
                    }
                });
                self.request_redraw();
            }
        }
    }

    fn window_event(&mut self, _el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.settings.as_ref().map(|s| s.window.id()) == Some(id) {
            match event {
                WindowEvent::RedrawRequested => self.render_settings(),
                WindowEvent::CursorMoved { position, .. } => self.settings_mouse_move(position.x, position.y),
                WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => self.settings_click(),
                WindowEvent::ModifiersChanged(m) => self.mods = m,
                WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => self.settings_key(&event),
                WindowEvent::CloseRequested => self.close_settings(),
                WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Resized(_) => {
                    if let Some(s) = &self.settings {
                        s.window.request_redraw();
                    }
                }
                _ => {}
            }
            return;
        }
        match event {
            WindowEvent::RedrawRequested => {
                self.render();
                self.last_frame = Instant::now();
            }
            WindowEvent::Resized(_) => self.request_redraw(),
            WindowEvent::ScaleFactorChanged { .. } => {
                self.fonts.clear_cache();
                self.apply_geometry();
            }
            WindowEvent::Focused(f) => {
                self.focused = f;
                if let Some(t) = self.active_tab() {
                    if t.session.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                        t.session.write_str(if f { "\x1b[I" } else { "\x1b[O" });
                    }
                }
                if !f && self.cfg.window.hide_on_focus_loss && self.vis == Vis::Shown {
                    self.hide();
                }
                self.request_redraw();
            }
            WindowEvent::ModifiersChanged(m) => self.mods = m,
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } => self.on_key(event),
            WindowEvent::CursorMoved { position, .. } => self.on_mouse_move(position.x, position.y),
            WindowEvent::MouseInput { state, button, .. } => self.on_mouse_button(state, button),
            WindowEvent::MouseWheel { delta, .. } => self.on_scroll(delta),
            WindowEvent::CloseRequested => self.hide(),
            _ => {}
        }
    }

    fn exiting(&mut self, _el: &ActiveEventLoop) {
        self.save_session_final();
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.quit {
            el.exit();
            return;
        }
        self.create_pending_settings(el);
        let now = Instant::now();
        if now.duration_since(self.last_tick) >= Duration::from_millis(250) {
            self.last_tick = now;
            self.tick();
        }
        if now.duration_since(self.last_cfg_check) >= Duration::from_secs(1) {
            self.last_cfg_check = now;
            self.reload_config(false);
        }
        let _ = self.last_info;
        let animating = matches!(self.vis, Vis::Opening(_) | Vis::Closing(_))
            || self.toasts.iter().any(|t| t.born.elapsed() < Duration::from_millis(300));
        let next = if animating {
            self.request_redraw();
            now + Duration::from_millis(8)
        } else if self.visible() {
            // Redraw only when something time-based actually changes on screen.
            let ms = self.started.elapsed().as_millis();
            let spin = if self.exts.iter().any(|e| e.animating()) { (ms / 160) as u64 } else { 0 };
            let blink = if self.cfg.cursor.blink && self.focused { (self.blink_epoch.elapsed().as_millis() / 530) as u64 } else { 0 };
            let caret = self.palette.as_ref().map(|p| (p.opened.elapsed().as_millis() / 530) as u64).unwrap_or(0);
            let toast = if self.toasts.is_empty() { 0 } else { (ms / 250) as u64 };
            let (h, m) = util::local_hm();
            let key = (spin, blink, caret, toast, h * 60 + m);
            if key != self.anim_key {
                self.anim_key = key;
                self.request_redraw();
            }
            now + Duration::from_millis(40)
        } else {
            now + Duration::from_millis(500)
        };
        el.set_control_flow(ControlFlow::WaitUntil(next));
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::{Config as TermConfig, Term};
    use alacritty_terminal::vte::ansi::Processor;

    fn term_with(text: &str) -> Term<VoidListener> {
        let mut term = Term::new(TermConfig::default(), &TermSize::new(40, 10), VoidListener);
        let mut p: Processor = Processor::new();
        p.advance(&mut term, text.as_bytes());
        term
    }

    fn select_line0(term: &mut Term<VoidListener>) {
        let mut s = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        s.update(Point::new(Line(0), Column(4)), Side::Right);
        term.selection = Some(s);
    }

    #[test]
    fn selection_survives_line_erase_redraws() {
        let mut term = term_with("hello world\r\nsecond line");
        let keep = std::cell::RefCell::new(None);
        select_line0(&mut term);
        keep_selection(&keep, &mut term);
        // What Claude Code / tmux do every frame: cursor home, erase line, rewrite it.
        let mut p: Processor = Processor::new();
        p.advance(&mut term, b"\x1b[H\x1b[2Khello world");
        assert!(term.selection.is_none(), "the terminal drops it on erase");
        keep_selection(&keep, &mut term);
        assert_eq!(term.selection_to_string().as_deref(), Some("hello"));
    }

    #[test]
    fn alternate_screen_switch_really_clears() {
        let mut term = term_with("hello world");
        let keep = std::cell::RefCell::new(None);
        select_line0(&mut term);
        keep_selection(&keep, &mut term);
        let mut p: Processor = Processor::new();
        p.advance(&mut term, b"\x1b[?1049h");
        keep_selection(&keep, &mut term);
        assert!(term.selection.is_none());
        assert!(keep.borrow().is_none());
    }
}
