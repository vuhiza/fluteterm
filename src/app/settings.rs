//! The settings window (⌘,): a separate pixel-styled window, as on the design
//! board. Every control writes `config.toml` (comments preserved) and reloads
//! it, so changes apply live to the panel behind.

use std::rc::Rc;

use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey, PhysicalKey};
use winit::window::Window;

use super::App;
use crate::config::{self, CursorStyle, OptionAsAlt, Pattern, ScreenMode, Shape};
use crate::ext::Action;
use crate::platform::{self, CGRect};
use crate::render::canvas::{Canvas, Color, IRect};
use crate::render::icons::{self, Icon};
use crate::render::text::{Style, truncate};
use crate::theme::{self, Rgb};
use crate::util;

pub const WIN_W: f64 = 900.0;
pub const WIN_H: f64 = 800.0;
const SHADOW: f64 = 6.0;
const TITLE_H: f64 = 38.0;
const NAV_W: f64 = 196.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Look,
    Fonts,
    Keys,
    Window,
    Notify,
    Claude,
    Orca,
    Custom,
}

const NAV: &[(&str, &[(Page, Icon, &str)])] = &[
    ("appearance", &[(Page::Look, Icon::Ring, "look"), (Page::Fonts, Icon::Square, "fonts")]),
    ("behaviour", &[(Page::Keys, Icon::Arrow, "keys"), (Page::Window, Icon::Square, "window"), (Page::Notify, Icon::Bell, "notify")]),
    ("extensions", &[(Page::Claude, Icon::Claude, "claude"), (Page::Orca, Icon::Orca, "orca"), (Page::Custom, Icon::Plus, "custom")]),
];

#[derive(Clone, Debug)]
pub enum SAct {
    Page(Page),
    /// Persist `[table] key = value` and reload.
    Set(Option<&'static str>, &'static str, String),
    Record,
    /// Some("#rrggbb") or None (theme default).
    Accent(Option<String>),
    HexEdit,
    Run(Action),
    Close,
    Drag,
}

pub struct SettingsWin {
    // Declared before `window` so it is dropped first (fields drop in order).
    presenter: crate::platform::present::Presenter,
    pub window: Rc<Window>,
    pub page: Page,
    pub recording: bool,
    /// Text being typed into the custom accent field.
    pub hex_edit: Option<String>,
    hits: Vec<(IRect, SAct)>,
    mouse: (f64, f64),
}

fn q(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// winit key → global-hotkey accelerator string ("Cmd+Shift+KeyT", "F12", "Alt+Space").
fn accelerator(ev: &KeyEvent, mods: winit::keyboard::ModifiersState) -> Option<String> {
    let PhysicalKey::Code(code) = ev.physical_key else { return None };
    let name = format!("{code:?}");
    if name.contains("Shift") || name.contains("Control") || name.contains("Alt") || name.contains("Super") || name.contains("Meta") || name == "Fn" {
        return None;
    }
    let mut parts = vec![];
    if mods.super_key() {
        parts.push("Cmd");
    }
    if mods.control_key() {
        parts.push("Ctrl");
    }
    if mods.alt_key() {
        parts.push("Alt");
    }
    if mods.shift_key() {
        parts.push("Shift");
    }
    let mut s = parts.join("+");
    if !s.is_empty() {
        s.push('+');
    }
    s.push_str(&name);
    Some(s)
}

impl App {
    pub fn request_settings(&mut self, page: Option<Page>) {
        if let Some(s) = &mut self.settings {
            if let Some(p) = page {
                s.page = p;
            }
            platform::show(&s.window);
            s.window.request_redraw();
        } else {
            self.pending_settings = Some(page.unwrap_or(Page::Look));
        }
    }

    /// Called from `about_to_wait` (window creation needs the event loop).
    pub fn create_pending_settings(&mut self, el: &ActiveEventLoop) {
        let Some(page) = self.pending_settings.take() else { return };
        let attrs = Window::default_attributes()
            .with_title("fluteterm settings")
            .with_decorations(false)
            .with_transparent(true)
            .with_visible(false)
            .with_resizable(false)
            .with_inner_size(LogicalSize::new(WIN_W, WIN_H));
        #[allow(unused_imports)]
        use winit::platform::macos::WindowAttributesExtMacOS;
        let attrs = attrs.with_has_shadow(false);
        let Ok(window) = el.create_window(attrs) else { return };
        let window = Rc::new(window);
        platform::setup_window_level(&window, 26, true);
        let Some(presenter) = crate::platform::present::Presenter::new(&window) else { return };
        if let Some(scr) = platform::target_screen(true) {
            let fr = scr.frame;
            let x = fr.origin.x + ((fr.size.width - WIN_W) / 2.0).round();
            let top = fr.max_y() - scr.menubar - 90.0;
            platform::set_frame(&window, CGRect::new(x, top - WIN_H, WIN_W, WIN_H));
        }
        platform::set_blur(&window, 24);
        platform::show(&window);
        window.request_redraw();
        self.settings = Some(SettingsWin { window, presenter, page, recording: false, hex_edit: None, hits: vec![], mouse: (0.0, 0.0) });
    }

    pub fn close_settings(&mut self) {
        if let Some(s) = self.settings.take() {
            platform::order_out(&s.window);
            if s.recording {
                self.register_hotkeys();
            }
        }
        if self.visible() {
            if let Some(w) = &self.window {
                platform::show(w);
            }
        }
    }

    pub fn render_settings(&mut self) {
        let Some(mut s) = self.settings.take() else { return };
        let size = s.window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));
        let scale = s.window.scale_factor() as f32;
        {
            let (page, recording, hex) = (s.page, s.recording, s.hex_edit.clone());
            let buf = s.presenter.frame(w as usize, h as usize);
            let hits = {
                let mut cv = Canvas::new(buf, w as i32, h as i32, scale);
                cv.clear();
                self.draw_settings(&mut cv, page, recording, hex)
            };
            if let Some((path, reply)) = self.snapshot_settings.take() {
                let r = super::draw::save_png(&path, buf, w, h);
                let _ = reply.send(match r {
                    Ok(()) => serde_json::json!({"ok": true, "path": path.display().to_string()}),
                    Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                });
            }
            s.presenter.present();
            s.hits = hits;
        }
        self.settings = Some(s);
    }

    pub fn settings_mouse_move(&mut self, x: f64, y: f64) {
        if let Some(s) = &mut self.settings {
            s.mouse = (x, y);
        }
    }

    pub fn settings_click(&mut self) {
        let Some(s) = &self.settings else { return };
        let (x, y) = (s.mouse.0 as i32, s.mouse.1 as i32);
        let Some(act) = s.hits.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, a)| a.clone()) else { return };
        self.settings_act(act);
    }

    fn settings_act(&mut self, act: SAct) {
        match act {
            SAct::Page(p) => {
                if let Some(s) = &mut self.settings {
                    s.page = p;
                }
            }
            SAct::Set(table, key, value) => {
                config::set_value(table, key, &value);
                self.reload_config(true);
            }
            SAct::Record => {
                if let Some(s) = &mut self.settings {
                    s.recording = true;
                }
                // Free the current hotkey so it can be pressed while recording.
                if let Some(m) = &self.hotkey_mgr {
                    let _ = m.unregister_all(&self.registered);
                }
            }
            SAct::Accent(a) => {
                if let Some(s) = &mut self.settings {
                    s.hex_edit = None;
                }
                self.run_action(Action::SetAccent(a));
            }
            SAct::HexEdit => {
                let cur = self.cfg.accent.clone().unwrap_or_default().trim_start_matches('#').to_string();
                if let Some(s) = &mut self.settings {
                    s.hex_edit = Some(cur);
                }
            }
            SAct::Run(a) => self.run_action(a),
            SAct::Close => {
                self.close_settings();
                return;
            }
            SAct::Drag => {
                if let Some(s) = &self.settings {
                    let _ = s.window.drag_window();
                }
            }
        }
        if let Some(s) = &self.settings {
            s.window.request_redraw();
        }
    }

    pub fn settings_key(&mut self, ev: &KeyEvent) {
        if ev.state != ElementState::Pressed {
            return;
        }
        let mods = self.mods.state();
        // Typing a custom accent.
        if let Some(mut text) = self.settings.as_ref().and_then(|s| s.hex_edit.clone()) {
            match &ev.logical_key {
                Key::Named(NamedKey::Escape) => {
                    if let Some(s) = &mut self.settings {
                        s.hex_edit = None;
                    }
                }
                Key::Named(NamedKey::Enter) => {
                    let valid = text.len() == 6 || text.len() == 3;
                    if let Some(s) = &mut self.settings {
                        s.hex_edit = if valid { None } else { Some(text.clone()) };
                    }
                    if valid {
                        if let Some(c) = Rgb::hex(&text) {
                            self.run_action(Action::SetAccent(Some(c.to_hex())));
                        }
                    }
                }
                Key::Named(NamedKey::Backspace) => {
                    text.pop();
                    if let Some(s) = &mut self.settings {
                        s.hex_edit = Some(text);
                    }
                }
                _ => {
                    if let Some(t) = &ev.text {
                        for ch in t.chars() {
                            if ch.is_ascii_hexdigit() && text.len() < 6 {
                                text.push(ch.to_ascii_lowercase());
                            }
                        }
                        if let Some(s) = &mut self.settings {
                            s.hex_edit = Some(text);
                        }
                    }
                }
            }
            if let Some(s) = &self.settings {
                s.window.request_redraw();
            }
            return;
        }
        let recording = self.settings.as_ref().map(|s| s.recording).unwrap_or(false);
        if recording {
            if matches!(ev.logical_key, Key::Named(NamedKey::Escape)) {
                if let Some(s) = &mut self.settings {
                    s.recording = false;
                }
                self.register_hotkeys();
            } else if let Some(acc) = accelerator(ev, mods) {
                if let Some(s) = &mut self.settings {
                    s.recording = false;
                }
                config::set_value(None, "hotkey", &q(&acc));
                self.reload_config(true);
                self.register_hotkeys();
            }
            if let Some(s) = &self.settings {
                s.window.request_redraw();
            }
            return;
        }
        let close = matches!(ev.logical_key, Key::Named(NamedKey::Escape))
            || (mods.super_key() && matches!(crate::term::keys::shortcut_key(ev).as_str(), "w" | ","));
        if close {
            self.close_settings();
            return;
        }
        // ↑/↓ move between pages.
        let order: Vec<Page> = NAV.iter().flat_map(|(_, items)| items.iter().map(|i| i.0)).collect();
        let delta = match ev.logical_key {
            Key::Named(NamedKey::ArrowDown) => 1,
            Key::Named(NamedKey::ArrowUp) => -1,
            _ => 0,
        };
        if delta != 0 {
            if let Some(s) = &mut self.settings {
                let i = order.iter().position(|p| *p == s.page).unwrap_or(0) as i32;
                s.page = order[((i + delta).rem_euclid(order.len() as i32)) as usize];
                s.window.request_redraw();
            }
        }
    }

    // ───────────────────────────── drawing ─────────────────────────────

    fn draw_settings(&mut self, cv: &mut Canvas, page: Page, recording: bool, hex_edit: Option<String>) -> Vec<(IRect, SAct)> {
        let pal = self.pal.clone();
        let u = cv.unit();
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let mut hits: Vec<(IRect, SAct)> = vec![];
        let sh = cv.px(SHADOW as f32);
        let card = IRect::new(0, 0, cv.w - sh, cv.h - sh);
        cv.hard_shadow(card, sh, Color::rgba(Rgb::new(0, 0, 0), 0.55));
        cv.fill(card, Color::rgba(pal.panel, 0.97));
        cv.stroke(card, u, Color::rgb(pal.line));

        // Title bar.
        let th = cv.px(TITLE_H as f32);
        let title = IRect::new(card.x, card.y, card.w, th);
        hits.push((title, SAct::Drag));
        cv.fill(IRect::new(card.x + u, card.y + u, card.w - 2 * u, th - u), Color::rgba(pal.bg, 0.5));
        cv.hline(card.x, card.y + th, card.w, u, Color::rgb(pal.line));
        let ty = card.y + (th - lh) / 2;
        let pad = cv.px(14.0);
        cv.fill(IRect::new(card.x + pad, card.y + (th - 6 * u) / 2, 6 * u, 6 * u), Color::rgb(pal.text));
        let mut tx = card.x + pad + 6 * u + cv.px(10.0);
        tx += self.fonts.draw_text(cv, tx, ty, "settings", upx, Style::Bold, Color::rgb(pal.text));
        let mem = util::rss_mb().map(|m| format!(" · {m} MB")).unwrap_or_default();
        self.fonts.draw_text(cv, tx + cv.px(10.0), ty, &format!("fluteterm {}{mem}", env!("CARGO_PKG_VERSION")), upx, Style::Regular, Color::rgb(pal.faint));
        let cw = th;
        let close = IRect::new(card.right() - cw, card.y, cw, th);
        icons::draw_centered(cv, Icon::Close, close.x + (cw - icons::size(cv)) / 2, card.y, th, Color::rgb(pal.dim));
        hits.push((close, SAct::Close));

        // Nav.
        let nav = IRect::new(card.x, card.y + th + u, cv.px(NAV_W as f32), card.h - th - u);
        cv.vline(nav.right(), nav.y, nav.h, u, Color::rgb(pal.line));
        let mut ny = nav.y + cv.px(10.0);
        let row_h = lh + cv.px(10.0);
        for (group, items) in NAV {
            ny += self.s_label(cv, nav.x + cv.px(14.0), ny, group, &pal) + cv.px(4.0);
            for (p, icon, label) in items.iter() {
                let r = IRect::new(nav.x + cv.px(8.0), ny, nav.w - cv.px(16.0), row_h);
                let sel = *p == page;
                if sel {
                    cv.notched(r, u, Color::rgb(pal.accent));
                }
                let fg = if sel { pal.on_accent } else { pal.dim };
                icons::draw_centered(cv, *icon, r.x + cv.px(10.0), r.y, r.h, Color::rgb(if sel { pal.on_accent } else { pal.accent }));
                self.fonts.draw_text(cv, r.x + cv.px(10.0) + icons::size(cv) + cv.px(10.0), r.y + (r.h - lh) / 2, label, upx, if sel { Style::Bold } else { Style::Regular }, Color::rgb(fg));
                hits.push((r, SAct::Page(*p)));
                ny += row_h + cv.px(2.0);
            }
            ny += cv.px(8.0);
        }
        // Footer in nav.
        let foot = "live · config.toml";
        self.fonts.draw_text(cv, nav.x + cv.px(14.0), nav.bottom() - lh - cv.px(12.0), foot, upx * 0.9, Style::Regular, Color::rgb(pal.faint));

        // Main.
        let mx = nav.right() + cv.px(22.0);
        let mw = card.right() - mx - cv.px(22.0);
        let mut y = nav.y + cv.px(14.0);
        let old = cv.set_clip(IRect::new(nav.right() + u, nav.y, card.right() - nav.right() - 2 * u, nav.h - u));
        let cfg = self.cfg.clone();
        match page {
            Page::Look => {
                y = self.s_heading(cv, mx, y, mw, "palette", &pal);
                y = self.s_presets(cv, &mut hits, mx, y, mw, &cfg.theme, &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "accent · optional", &pal);
                y = self.s_accents(cv, &mut hits, mx, y, mw, cfg.accent.as_deref(), hex_edit.as_deref(), &cfg.theme, &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "panel", &pal);
                let op = cfg.window.opacity;
                let filled = (((op - 0.3) / 0.07).round() as usize).min(10);
                y = self.s_meter(cv, &mut hits, mx, y, mw, "panel opacity", &format!("{}%", (op * 100.0).round()), 10, filled, &pal, |i| {
                    SAct::Set(Some("window"), "opacity", format!("{:.2}", 0.3 + 0.07 * (i + 1) as f32))
                });
                let blur = cfg.window.blur;
                y = self.s_meter(cv, &mut hits, mx, y, mw, "wallpaper blur", &if blur == 0 { "off".into() } else { format!("{blur}px") }, 11, (blur / 4) as usize + if blur > 0 { 1 } else { 0 }, &pal, |i| {
                    SAct::Set(Some("window"), "blur", (i * 4).to_string())
                });
                let shapes = [(Shape::Strip, "strip"), (Shape::Centred, "centred"), (Shape::Floating, "floating")];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "shape", shapes.iter().map(|(s, n)| (n.to_string(), cfg.window.shape == *s, SAct::Set(Some("window"), "shape", q(n)))).collect(), &pal);
                let pats = [(Pattern::Bayer, "bayer"), (Pattern::Scan, "scan"), (Pattern::Blocks, "blocks"), (Pattern::Noise, "noise"), (Pattern::None, "none")];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "motion", pats.iter().map(|(p, n)| (n.to_string(), cfg.motion.pattern == *p, SAct::Set(Some("motion"), "pattern", q(n)))).collect(), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "crt scanlines", "faint dark rows over the panel", cfg.window.crt, SAct::Set(Some("window"), "crt", (!cfg.window.crt).to_string()), &pal);
                let _ = y;
            }
            Page::Fonts => {
                y = self.s_heading(cv, mx, y, mw, "terminal font", &pal);
                let fams = [
                    ("JetBrains Mono", "JetBrains Mono"),
                    ("SF Mono", "/System/Library/Fonts/SFNSMono.ttf"),
                    ("Menlo", "/System/Library/Fonts/Menlo.ttc"),
                    ("Monaco", "/System/Library/Fonts/Monaco.ttf"),
                ];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "family", fams.iter().map(|(n, v)| (n.to_string(), cfg.font.family == *v, SAct::Set(Some("font"), "family", q(v)))).collect(), &pal);
                let size = cfg.font.size.round() as usize;
                y = self.s_meter(cv, &mut hits, mx, y, mw, "size", &format!("{size}pt"), 13, size.saturating_sub(9).min(13), &pal, |i| SAct::Set(Some("font"), "size", (10 + i).to_string()));
                let lhs = [1.1f32, 1.2, 1.3, 1.4, 1.5];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "line height", lhs.iter().map(|v| (format!("{v:.1}"), (cfg.font.line_height - v).abs() < 0.01, SAct::Set(Some("font"), "line_height", format!("{v:.1}")))).collect(), &pal);
                y = self.s_note(cv, mx, y + cv.px(4.0), mw, "Any .ttf / .otf works: set `family` to its path in config.toml. ⌘+ / ⌘- change size for the session.", &pal);
                let _ = y;
            }
            Page::Keys => {
                y = self.s_heading(cv, mx, y, mw, "global hotkey", &pal);
                y = self.s_hotkey(cv, &mut hits, mx, y, mw, &cfg.hotkey, recording, &pal);
                y = self.s_note(cv, mx, y, mw, "F12 on a laptop keyboard may need fn. ⌘Space is taken by Spotlight unless you disable it in System Settings.", &pal);
                let opts = [(OptionAsAlt::Left, "left"), (OptionAsAlt::Right, "right"), (OptionAsAlt::Both, "both"), (OptionAsAlt::None, "none")];
                y = self.s_chips(cv, &mut hits, mx, y + cv.px(4.0), mw, "option as meta", opts.iter().map(|(o, n)| (n.to_string(), cfg.option_as_alt == *o, SAct::Set(None, "option_as_alt", q(n)))).collect(), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "copy on select", "selected text goes to the clipboard right away (⌘C also works)", cfg.copy_on_select, SAct::Set(None, "copy_on_select", (!cfg.copy_on_select).to_string()), &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "in the panel", &pal);
                for (k, what) in [
                    ("⌘T / ⌘W", "new / close tab"),
                    ("⌘1…9  ⌘⇧[ ]", "switch tabs"),
                    ("⌘K", "command palette"),
                    ("⌘⇧C", "new claude session here"),
                    ("⌘⇧O / ⌘⇧A", "orca worktrees / agent commands"),
                    ("⌘,", "settings"),
                    ("⌘+ ⌘- ⌘0", "text size"),
                ] {
                    self.fonts.draw_text(cv, mx, y, k, upx, Style::Bold, Color::rgb(pal.text));
                    self.fonts.draw_text(cv, mx + cv.px(170.0), y, what, upx, Style::Regular, Color::rgb(pal.dim));
                    y += lh + cv.px(6.0);
                }
            }
            Page::Window => {
                y = self.s_heading(cv, mx, y, mw, "size & place", &pal);
                let hgt = cfg.window.height;
                y = self.s_meter(cv, &mut hits, mx, y, mw, "drop height", &format!("{}%", (hgt * 100.0).round()), 12, (((hgt - 0.25) / 0.06).round() as usize + 1).min(12), &pal, |i| {
                    SAct::Set(Some("window"), "height", format!("{:.2}", 0.25 + 0.06 * i as f32))
                });
                let wd = cfg.window.width;
                y = self.s_meter(cv, &mut hits, mx, y, mw, "width (centred / floating)", &format!("{}%", (wd * 100.0).round()), 10, (((wd - 0.5) / 0.055).round() as usize + 1).min(10), &pal, |i| {
                    SAct::Set(Some("window"), "width", format!("{:.2}", 0.5 + 0.055 * i as f32))
                });
                y = self.s_chips(cv, &mut hits, mx, y, mw, "screen", vec![
                    ("follow mouse".into(), cfg.window.screen == ScreenMode::Mouse, SAct::Set(Some("window"), "screen", q("mouse"))),
                    ("primary".into(), cfg.window.screen == ScreenMode::Primary, SAct::Set(Some("window"), "screen", q("primary"))),
                ], &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "hide when focus is lost", "like Yakuake: click elsewhere to pull it up", cfg.window.hide_on_focus_loss, SAct::Set(Some("window"), "hide_on_focus_loss", (!cfg.window.hide_on_focus_loss).to_string()), &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "sessions", &pal);
                let s = &cfg.session;
                y = self.s_check(cv, &mut hits, mx, y, mw, "restore tabs on start", "directories, titles, Claude conversations", s.restore, SAct::Set(Some("session"), "restore", (!s.restore).to_string()), &pal);
                let has_tmux = crate::session::tmux_path().is_some();
                y = self.s_chips(cv, &mut hits, mx, y, mw, if has_tmux { "keep processes running while closed" } else { "keep processes running (needs tmux: brew install tmux)" }, vec![
                    ("tmux · keep running".into(), s.backend != config::Backend::Plain && has_tmux, SAct::Set(Some("session"), "backend", q("tmux"))),
                    ("plain · layout + scrollback".into(), s.backend == config::Backend::Plain || !has_tmux, SAct::Set(Some("session"), "backend", q("plain"))),
                ], &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "motion & cursor", &pal);
                let speeds = [(0u32, "instant"), (140, "fast"), (220, "normal"), (400, "slow")];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "drop speed", speeds.iter().map(|(ms, n)| (n.to_string(), cfg.motion.open_ms == *ms, SAct::Set(Some("motion"), "open_ms", ms.to_string()))).collect(), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "slide while dissolving", "the panel slides down in whole-pixel steps", cfg.motion.slide, SAct::Set(Some("motion"), "slide", (!cfg.motion.slide).to_string()), &pal);
                let cs = [(CursorStyle::Block, "block"), (CursorStyle::Beam, "beam"), (CursorStyle::Underline, "underline")];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "cursor", cs.iter().map(|(c, n)| (n.to_string(), cfg.cursor.style == *c, SAct::Set(Some("cursor"), "style", q(n)))).collect(), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "blinking cursor", "", cfg.cursor.blink, SAct::Set(Some("cursor"), "blink", (!cfg.cursor.blink).to_string()), &pal);
                let _ = y;
            }
            Page::Notify => {
                y = self.s_heading(cv, mx, y, mw, "notifications", &pal);
                let n = &cfg.notifications;
                y = self.s_check(cv, &mut hits, mx, y, mw, "Notification Center banners", "when the panel is hidden; click one to jump to the tab", n.native, SAct::Set(Some("notifications"), "native", (!n.native).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "in-panel toasts", "pixel toasts with actions (allow / deny / go to tab)", n.toasts, SAct::Set(Some("notifications"), "toasts", (!n.toasts).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "sound", "play the default sound with banners", n.sound, SAct::Set(Some("notifications"), "sound", (!n.sound).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "do not disturb", "no banners; toasts still appear in the panel", n.do_not_disturb, SAct::Set(Some("notifications"), "do_not_disturb", (!n.do_not_disturb).to_string()), &pal);
                let _ = y;
            }
            Page::Claude => {
                y = self.s_heading(cv, mx, y, mw, "claude code", &pal);
                let c = &cfg.claude;
                y = self.s_check(cv, &mut hits, mx, y, mw, "enabled", "tab badges, status bar, palette commands", c.enabled, SAct::Set(Some("claude"), "enabled", (!c.enabled).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "notify when a turn finishes", "", c.notify_done, SAct::Set(Some("claude"), "notify_done", (!c.notify_done).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "notify when claude needs you", "permission prompts, with allow / deny buttons", c.notify_attention, SAct::Set(Some("claude"), "notify_attention", (!c.notify_attention).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "usage in the status bar", "live sessions · 5-hour window · today's tokens (all sessions on this Mac)", c.status_usage, SAct::Set(Some("claude"), "status_usage", (!c.status_usage).to_string()), &pal);
                y = self.s_heading(cv, mx, y + cv.px(6.0), mw, "hooks", &pal);
                let n_hooks = crate::hooks::count();
                let installed = n_hooks > 0;
                y = self.s_status(
                    cv,
                    mx,
                    y,
                    mw,
                    &if installed { format!("installed: {n_hooks} entries in ~/.claude/settings.json") } else { "not installed: no live Claude state in tabs".to_string() },
                    installed,
                    &pal,
                );
                let mut bx = mx;
                bx += self.s_button(cv, &mut hits, bx, y, if installed { "reinstall hooks" } else { "install hooks" }, true, SAct::Run(Action::ClaudeHooks(true)), &pal) + cv.px(8.0);
                if installed {
                    self.s_button_danger(cv, &mut hits, bx, y, "remove hooks", SAct::Run(Action::ClaudeHooks(false)), &pal);
                }
                y += lh + cv.px(18.0);
                y = self.s_note(cv, mx, y, mw, "Removing only deletes fluteterm's own entries (other hooks stay) and backs the file up first.", &pal);
                y += lh + cv.px(18.0);
                y = self.s_note(cv, mx, y, mw, &format!("command: {} {}", c.command, c.args.join(" ")), &pal);
                let _ = y;
            }
            Page::Orca => {
                y = self.s_heading(cv, mx, y, mw, "orca", &pal);
                let o = &cfg.orca;
                let found = util::which(&o.command);
                y = self.s_status(cv, mx, y, mw, &found.as_ref().map(|p| format!("cli: {}", util::tildify(p))).unwrap_or_else(|| "orca CLI not found".into()), found.is_some(), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "enabled", "worktrees & agents in the status bar, ⌘⇧O and tabs", o.enabled, SAct::Set(Some("orca"), "enabled", (!o.enabled).to_string()), &pal);
                y = self.s_check(cv, &mut hits, mx, y, mw, "notify on agent changes", "finished / waiting for you", o.notify, SAct::Set(Some("orca"), "notify", (!o.notify).to_string()), &pal);
                let agents = ["claude", "codex", "opencode", "gemini"];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "launch agent", agents.iter().map(|a| (a.to_string(), o.agent == *a, SAct::Set(Some("orca"), "agent", q(a)))).collect(), &pal);
                let polls = [3u64, 5, 10, 30];
                y = self.s_chips(cv, &mut hits, mx, y, mw, "refresh (visible)", polls.iter().map(|s| (format!("{s}s"), o.poll_secs == *s, SAct::Set(Some("orca"), "poll_secs", s.to_string()))).collect(), &pal);
                let _ = y;
            }
            Page::Custom => {
                y = self.s_heading(cv, mx, y, mw, "code-free extensions", &pal);
                for (label, n) in [("profiles  [[profile]]", cfg.profiles.len()), ("status modules  [[module]]", cfg.modules.len()), ("palette commands  [[command]]", cfg.commands.len())] {
                    self.fonts.draw_text(cv, mx, y, label, upx, Style::Regular, Color::rgb(pal.text));
                    self.fonts.draw_text(cv, mx + mw - cv.px(40.0), y, &n.to_string(), upx, Style::Bold, Color::rgb(pal.accent));
                    y += lh + cv.px(8.0);
                }
                y += cv.px(6.0);
                let mut bx = mx;
                bx += self.s_button(cv, &mut hits, bx, y, "edit config.toml", true, SAct::Run(Action::EditConfig), &pal) + cv.px(8.0);
                bx += self.s_button(cv, &mut hits, bx, y, "open in editor app", false, SAct::Run(Action::OpenConfig), &pal) + cv.px(8.0);
                self.s_button(cv, &mut hits, bx, y, "themes folder", false, SAct::Run(Action::RunDetached { program: "open".into(), args: vec![config::themes_dir().display().to_string()] }), &pal);
                y += lh + cv.px(22.0);
                y = self.s_note(cv, mx, y, mw, "Examples are in config.toml. Scripts can also talk to fluteterm over its socket: fluteterm notify / status / open / send.", &pal);
                let _ = y;
            }
        }
        cv.clip = old;
        hits
    }

    fn s_label(&mut self, cv: &mut Canvas, x: i32, y: i32, text: &str, pal: &theme::Palette) -> i32 {
        let px = 13.5 * cv.scale * 0.8;
        let u = cv.unit();
        let lh = self.fonts.line_height(px);
        cv.fill(IRect::new(x, y + (lh - 3 * u) / 2, 3 * u, 3 * u), Color::rgb(pal.faint));
        let cw = self.fonts.text_width("M", px) + u;
        for (i, ch) in text.to_uppercase().chars().enumerate() {
            self.fonts.draw_text(cv, x + 6 * u + i as i32 * cw, y, &ch.to_string(), px, Style::Bold, Color::rgb(pal.faint));
        }
        lh
    }

    fn s_heading(&mut self, cv: &mut Canvas, x: i32, y: i32, w: i32, text: &str, pal: &theme::Palette) -> i32 {
        let h = self.s_label(cv, x, y, text, pal);
        cv.hline(x, y + h + cv.px(5.0), w, cv.unit(), Color::rgb(pal.line));
        y + h + cv.px(14.0)
    }

    fn s_note(&mut self, cv: &mut Canvas, x: i32, y: i32, w: i32, text: &str, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let cols = (w / self.fonts.text_width("M", upx).max(1)) as usize;
        let mut yy = y;
        let mut line = String::new();
        for word in text.split_whitespace() {
            if line.chars().count() + word.chars().count() + 1 > cols && !line.is_empty() {
                self.fonts.draw_text(cv, x, yy, &line, upx, Style::Regular, Color::rgb(pal.faint));
                yy += lh;
                line.clear();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if !line.is_empty() {
            self.fonts.draw_text(cv, x, yy, &line, upx, Style::Regular, Color::rgb(pal.faint));
            yy += lh;
        }
        yy + cv.px(10.0)
    }

    fn s_status(&mut self, cv: &mut Canvas, x: i32, y: i32, _w: i32, text: &str, good: bool, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let (icon, c) = if good { (Icon::Check, pal.ok) } else { (Icon::Warn, pal.warn) };
        icons::draw_centered(cv, icon, x, y, lh, Color::rgb(c));
        self.fonts.draw_text(cv, x + icons::size(cv) + cv.px(8.0), y, text, upx, Style::Regular, Color::rgb(pal.dim));
        y + lh + cv.px(12.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn s_meter(
        &mut self,
        cv: &mut Canvas,
        hits: &mut Vec<(IRect, SAct)>,
        x: i32,
        y: i32,
        w: i32,
        label: &str,
        value: &str,
        n: usize,
        filled: usize,
        pal: &theme::Palette,
        act: impl Fn(usize) -> SAct,
    ) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        self.fonts.draw_text(cv, x, y, label, upx, Style::Regular, Color::rgb(pal.text));
        let vw = self.fonts.text_width(value, upx);
        self.fonts.draw_text(cv, x + w - vw, y, value, upx, Style::Regular, Color::rgb(pal.dim));
        let cy = y + lh + cv.px(6.0);
        let gap = cv.px(3.0);
        let cw = ((w - gap * (n as i32 - 1)) / n as i32).min(cv.px(30.0));
        let ch = cv.px(12.0);
        for i in 0..n {
            let r = IRect::new(x + i as i32 * (cw + gap), cy, cw, ch);
            cv.fill(r, Color::rgb(if i < filled { pal.accent } else { pal.surface_hi }));
            hits.push((IRect::new(r.x, r.y - cv.px(4.0), cw + gap, ch + cv.px(8.0)), act(i)));
        }
        cy + ch + cv.px(16.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn s_chips(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, _w: i32, label: &str, opts: Vec<(String, bool, SAct)>, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        self.fonts.draw_text(cv, x, y, label, upx, Style::Regular, Color::rgb(pal.text));
        let mut cx = x;
        let cy = y + lh + cv.px(6.0);
        for (text, on, act) in opts {
            let cw = self.fonts.text_width(&text, upx) + cv.px(18.0);
            let r = IRect::new(cx, cy, cw, lh + cv.px(8.0));
            if on {
                cv.notched(r, u, Color::rgb(pal.accent));
                self.fonts.draw_text(cv, cx + cv.px(9.0), cy + cv.px(4.0), &text, upx, Style::Bold, Color::rgb(pal.on_accent));
            } else {
                cv.notched(r, u, Color::rgb(pal.surface));
                cv.notched_stroke(r, u, u, Color::rgb(pal.line));
                self.fonts.draw_text(cv, cx + cv.px(9.0), cy + cv.px(4.0), &text, upx, Style::Regular, Color::rgb(pal.dim));
            }
            hits.push((r, act));
            cx += cw + cv.px(6.0);
        }
        cy + lh + cv.px(8.0) + cv.px(16.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn s_check(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, w: i32, label: &str, sub: &str, on: bool, act: SAct, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let b = cv.px(11.0);
        let br = IRect::new(x, y + (lh - b) / 2, b, b);
        if on {
            cv.fill(br, Color::rgb(pal.text));
            cv.fill(br.inset(2 * u), Color::rgb(pal.panel));
            cv.fill(br.inset(3 * u), Color::rgb(pal.accent));
        } else {
            cv.stroke(br, u, Color::rgb(pal.dim));
        }
        let tx = x + b + cv.px(10.0);
        self.fonts.draw_text(cv, tx, y, label, upx, Style::Regular, Color::rgb(pal.text));
        let mut h = lh;
        if !sub.is_empty() {
            let cols = ((w - (tx - x)) / self.fonts.text_width("M", upx).max(1)) as usize;
            self.fonts.draw_text(cv, tx, y + lh, &truncate(sub, cols), upx, Style::Regular, Color::rgb(pal.faint));
            h += lh;
        }
        hits.push((IRect::new(x, y - cv.px(3.0), w, h + cv.px(6.0)), act));
        y + h + cv.px(12.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn s_button(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, text: &str, primary: bool, act: SAct, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let w = self.fonts.text_width(text, upx) + cv.px(22.0);
        let r = IRect::new(x, y, w, lh + cv.px(10.0));
        if primary {
            cv.notched(r, u, Color::rgb(pal.accent));
            self.fonts.draw_text(cv, x + cv.px(11.0), y + cv.px(5.0), text, upx, Style::Bold, Color::rgb(pal.on_accent));
        } else {
            cv.notched(r, u, Color::rgb(pal.surface));
            cv.notched_stroke(r, u, u, Color::rgb(pal.line));
            self.fonts.draw_text(cv, x + cv.px(11.0), y + cv.px(5.0), text, upx, Style::Regular, Color::rgb(pal.text));
        }
        hits.push((r, act));
        w
    }

    /// Outlined in the error colour: for actions that remove something.
    fn s_button_danger(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, text: &str, act: SAct, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let w = self.fonts.text_width(text, upx) + cv.px(22.0);
        let r = IRect::new(x, y, w, lh + cv.px(10.0));
        cv.notched(r, u, Color::rgb(pal.surface));
        cv.notched_stroke(r, u, u, Color::rgb(pal.err));
        self.fonts.draw_text(cv, x + cv.px(11.0), y + cv.px(5.0), text, upx, Style::Regular, Color::rgb(pal.err));
        hits.push((r, act));
        w
    }

    #[allow(clippy::too_many_arguments)]
    fn s_hotkey(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, w: i32, current: &str, recording: bool, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let fw = (w / 2).min(cv.px(260.0));
        let r = IRect::new(x, y, fw, lh + cv.px(14.0));
        cv.fill(r, Color::rgb(pal.bg));
        cv.stroke(r, u, Color::rgb(if recording { pal.accent } else { pal.line }));
        let shown = if recording { "press the new shortcut…  (esc cancels)".to_string() } else { current.replace("Key", "").replace('+', " + ") };
        self.fonts.draw_text(cv, x + cv.px(10.0), y + cv.px(7.0), &shown, upx, if recording { Style::Regular } else { Style::Bold }, Color::rgb(if recording { pal.accent } else { pal.text }));
        hits.push((r, SAct::Record));
        let bx = x + fw + cv.px(10.0);
        self.s_button(cv, hits, bx, y + cv.px(2.0), if recording { "recording…" } else { "record" }, !recording, SAct::Record, pal);
        y + r.h + cv.px(10.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn s_accents(
        &mut self,
        cv: &mut Canvas,
        hits: &mut Vec<(IRect, SAct)>,
        x: i32,
        y: i32,
        w: i32,
        current: Option<&str>,
        hex_edit: Option<&str>,
        theme_name: &str,
        pal: &theme::Palette,
    ) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let s = cv.px(26.0);
        let gap = cv.px(7.0);
        // "theme" tile: the theme's own accent.
        let own = theme::load(theme_name).0.accent;
        let mut cx = x;
        let mut tile = |cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, cx: i32, color: Rgb, on: bool, act: SAct| {
            let r = IRect::new(cx, y, s, s);
            if on {
                cv.stroke(IRect::new(r.x - 2 * u, r.y - 2 * u, r.w + 4 * u, r.h + 4 * u), u, Color::rgb(pal.text));
            }
            cv.fill(r, Color::rgb(color));
            cv.stroke(r, u, Color::rgba(Rgb::new(0, 0, 0), 0.35));
            hits.push((r, act));
        };
        tile(cv, hits, cx, own, current.is_none(), SAct::Accent(None));
        // A pixel "↺" mark on the theme tile so it reads as "default".
        cv.fill(IRect::new(cx + s / 2 - u, y + s / 2 - u, 2 * u, 2 * u), Color::rgb(if own.luma() > 0.6 { Rgb::new(0, 0, 0) } else { Rgb::new(255, 255, 255) }));
        cx += s + gap * 2;
        for (_, hex) in theme::ACCENTS {
            if cx + s > x + w {
                break;
            }
            let c = Rgb::hex(hex).unwrap();
            let on = current.map(|a| a.eq_ignore_ascii_case(hex)).unwrap_or(false);
            tile(cv, hits, cx, c, on, SAct::Accent(Some(hex.to_string())));
            cx += s + gap;
        }
        let mut yy = y + s + cv.px(12.0);
        // Custom hex field.
        let fw = cv.px(180.0);
        let r = IRect::new(x, yy, fw, lh + cv.px(12.0));
        let editing = hex_edit.is_some();
        cv.fill(r, Color::rgb(pal.bg));
        cv.stroke(r, u, Color::rgb(if editing { pal.accent } else { pal.line }));
        let custom = current.filter(|a| !theme::ACCENTS.iter().any(|(_, h)| h.eq_ignore_ascii_case(a)));
        let (text, tc) = match (hex_edit, custom) {
            (Some(t), _) => (format!("#{t}"), pal.text),
            (None, Some(c)) => (c.to_string(), pal.text),
            (None, None) => ("#rrggbb".to_string(), pal.faint),
        };
        let chip = cv.px(12.0);
        let preview = hex_edit.and_then(Rgb::hex).or_else(|| custom.and_then(Rgb::hex));
        if let Some(c) = preview {
            cv.fill(IRect::new(r.x + cv.px(9.0), r.y + (r.h - chip) / 2, chip, chip), Color::rgb(c));
        }
        let tx = r.x + cv.px(9.0) + chip + cv.px(8.0);
        let adv = self.fonts.draw_text(cv, tx, r.y + cv.px(6.0), &text, upx, Style::Regular, Color::rgb(tc));
        if editing {
            cv.fill(IRect::new(tx + adv, r.y + cv.px(6.0), cv.unit() * 2, lh), Color::rgb(pal.accent));
        }
        hits.push((r, SAct::HexEdit));
        let note = if editing { "type hex · ⏎ apply · esc cancel" } else { "click to type a custom colour" };
        self.fonts.draw_text(cv, r.right() + cv.px(12.0), r.y + cv.px(6.0), note, upx, Style::Regular, Color::rgb(pal.faint));
        yy = r.bottom() + cv.px(16.0);
        yy
    }

    fn s_presets(&mut self, cv: &mut Canvas, hits: &mut Vec<(IRect, SAct)>, x: i32, y: i32, w: i32, current: &str, pal: &theme::Palette) -> i32 {
        let upx = 13.5 * cv.scale;
        let lh = self.fonts.line_height(upx);
        let u = cv.unit();
        let names = theme::list();
        let cols = 4;
        let gap = cv.px(8.0);
        let tw = (w - gap * (cols - 1)) / cols;
        let th = lh + cv.px(30.0);
        for (i, name) in names.iter().enumerate() {
            let (cx, cy) = ((i as i32 % cols) * (tw + gap), (i as i32 / cols) * (th + gap));
            let r = IRect::new(x + cx, y + cy, tw, th);
            let p = theme::load(name).0;
            let on = name == current;
            cv.fill(r, Color::rgb(if on { pal.surface_hi } else { pal.surface }));
            cv.stroke(r, u, Color::rgb(if on { pal.text } else { pal.line }));
            let sw = IRect::new(r.x + cv.px(8.0), r.y + cv.px(8.0), r.w - cv.px(16.0), cv.px(12.0));
            cv.fill(sw, Color::rgb(if p.light { p.panel } else { p.accent }));
            cv.fill(IRect::new(sw.right() - sw.h, sw.y, sw.h, sw.h), Color::rgb(p.panel));
            let label = truncate(name, ((r.w - cv.px(16.0)) / self.fonts.text_width("M", upx).max(1)) as usize);
            let lw = self.fonts.text_width(&label, upx);
            self.fonts.draw_text(cv, r.x + (r.w - lw) / 2, sw.bottom() + cv.px(5.0), &label, upx, if on { Style::Bold } else { Style::Regular }, Color::rgb(if on { pal.text } else { pal.dim }));
            hits.push((r, SAct::Set(None, "theme", q(name))));
        }
        let rows = (names.len() as i32 + cols - 1) / cols;
        y + rows * (th + gap) + cv.px(8.0)
    }
}
