//! Frame composition. Everything is drawn in physical pixels on a
//! premultiplied canvas; decorations snap to the logical pixel grid
//! (`cv.unit()`), text is smooth.

use std::num::NonZeroU32;
use std::time::{Duration, Instant};

use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor};

use super::{App, GRIP_H, Geometry, Hits, SHADOW, STATUS_H, TABBAR_H, Vis};
use crate::config::{CursorStyle, Pattern, Shape};
use crate::ext::{Badge, Level, PaletteMode, Segment, TabKind, Tone};
use crate::render::canvas::{BAYER4, Canvas, Color, IRect};
use crate::render::icons::{self, Icon};
use crate::render::text::{Style, truncate};
use crate::theme::{Palette, Rgb};

const BLACK: Rgb = Rgb::new(0, 0, 0);

fn tone(p: &Palette, t: Tone) -> Rgb {
    match t {
        Tone::Text => p.text,
        Tone::Dim => p.dim,
        Tone::Faint => p.faint,
        Tone::Accent => p.accent,
        Tone::Ok => p.ok,
        Tone::Warn => p.warn,
        Tone::Err => p.err,
    }
}

pub fn color_for_index(p: &Palette, idx: usize) -> Rgb {
    match idx {
        0..=15 => p.ansi[idx],
        16..=231 => {
            let i = idx - 16;
            let lv = |v: usize| if v == 0 { 0u8 } else { (55 + v * 40) as u8 };
            Rgb::new(lv(i / 36), lv((i / 6) % 6), lv(i % 6))
        }
        232..=255 => {
            let g = (8 + (idx - 232) * 10) as u8;
            Rgb::new(g, g, g)
        }
        256 => p.text,
        257 => p.panel,
        258 => p.cursor,
        259..=266 => p.ansi[idx - 259].mix(p.panel, 0.35),
        267 => p.text,
        _ => p.dim,
    }
}

fn named(p: &Palette, n: NamedColor) -> Rgb {
    color_for_index(p, n as usize)
}

/// Resolve a terminal colour. Returns None for the default background (transparent).
fn resolve(c: AnsiColor, colors: &Colors, p: &Palette, is_bg: bool) -> Option<Rgb> {
    let conv = |r: alacritty_terminal::vte::ansi::Rgb| Rgb::new(r.r, r.g, r.b);
    match c {
        AnsiColor::Spec(rgb) => Some(conv(rgb)),
        AnsiColor::Indexed(i) => Some(colors[i as usize].map(conv).unwrap_or_else(|| color_for_index(p, i as usize))),
        AnsiColor::Named(n) => {
            if is_bg && n == NamedColor::Background && colors[n].is_none() {
                return None;
            }
            Some(colors[n].map(conv).unwrap_or_else(|| named(p, n)))
        }
    }
}

fn bayer(cx: i32, cy: i32) -> f32 {
    (BAYER4[(cy & 3) as usize][(cx & 3) as usize] as f32 + 0.5) / 16.0
}

fn hash01(x: i32, y: i32) -> f32 {
    let mut h = (x as u32).wrapping_mul(374_761_393) ^ (y as u32).wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    ((h ^ (h >> 16)) & 0xffff) as f32 / 65535.0
}

/// Word-wrap to at most `max_lines` lines of `cols` columns.
fn wrap(s: &str, cols: usize, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.chars().count() + 1 + word.chars().count() <= cols {
            cur.push(' ');
            cur.push_str(word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        if let Some(l) = lines.last_mut() {
            *l = truncate(&format!("{l} …"), cols);
        }
    }
    lines.into_iter().map(|l| truncate(&l, cols)).collect()
}

impl App {
    fn ui_px(&self, scale: f32) -> f32 {
        13.5 * scale
    }

    fn spinner_frame(&self) -> u8 {
        ((self.started.elapsed().as_millis() / 160) % 4) as u8
    }

    pub(super) fn render(&mut self) {
        let progress = self.anim_progress();
        if self.vis == Vis::Hidden {
            return;
        }
        let (Some(window), Some(geo)) = (self.window.clone(), self.geo) else { return };
        // The buffer always has the full panel size; during the slide the (shorter)
        // window clips it. Same-size buffers are recycled, so animating never allocates.
        let scale = window.scale_factor() as f32;
        let visible_h = window.inner_size().height.max(1) as i32;
        let (w, h) = (((geo.w * scale as f64).round() as u32).max(1), ((geo.h * scale as f64).round() as u32).max(1));
        let Some(mut surface) = self.surface.take() else { return };
        if surface.resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap()).is_ok() {
            if let Ok(mut buf) = surface.buffer_mut() {
                {
                    let mut cv = Canvas::new(&mut buf, w as i32, h as i32, scale);
                    cv.clear();
                    let off = (visible_h - h as i32).min(0);
                    self.draw_frame(&mut cv, &geo, off, progress);
                }
                if let Some((path, reply)) = self.snapshot.take() {
                    let r = save_png(&path, &buf, w, h);
                    let _ = reply.send(match r {
                        Ok(()) => serde_json::json!({"ok": true, "path": path.display().to_string(), "width": w, "height": h}),
                        Err(e) => serde_json::json!({"ok": false, "error": e.to_string()}),
                    });
                }
                let _ = buf.present();
            }
        }
        self.surface = Some(surface);
    }

    fn draw_frame(&mut self, cv: &mut Canvas, geo: &Geometry, off: i32, progress: f32) {
        let pal = self.pal.clone();
        let u = cv.unit();
        let shape = self.cfg.window.shape;
        let panel = IRect::new(0, off, cv.px(geo.panel_w as f32), cv.px(geo.panel_h as f32));
        let line = Color::rgb(pal.line);

        // Hard shadow + panel body.
        let sh = cv.px(SHADOW as f32);
        if shape == Shape::Strip {
            cv.fill(IRect::new(panel.x, panel.bottom(), panel.w, sh), Color::rgba(BLACK, 0.30));
        } else {
            cv.hard_shadow(panel, sh, Color::rgba(BLACK, 0.55));
        }
        // The buffer was just cleared: write the panel directly (no per-pixel blending).
        cv.set(panel, Color::rgba(pal.panel, self.cfg.window.opacity));
        if shape == Shape::Strip {
            cv.hline(panel.x, panel.bottom() - u, panel.w, u, line);
        } else {
            cv.stroke(panel, u, line);
        }

        let mut hits = Hits::default();
        let tabbar = IRect::new(panel.x, panel.y, panel.w, cv.px(TABBAR_H as f32));
        let status = IRect::new(panel.x, panel.bottom() - cv.px((STATUS_H + GRIP_H) as f32), panel.w, cv.px(STATUS_H as f32));
        let grip = IRect::new(panel.x, status.bottom(), panel.w, panel.bottom() - status.bottom());
        let (tx, ty, tw, th) = self.term_rect_pt(geo);
        let term = IRect::new(panel.x + cv.px(tx as f32), panel.y + cv.px(ty as f32), cv.px(tw as f32), cv.px(th as f32));
        hits.term = term;
        hits.grip = grip;

        self.draw_tabbar(cv, tabbar, &pal, &mut hits);
        self.draw_terminal(cv, term, &pal);
        self.draw_status(cv, status, &pal, &mut hits);

        // Grip: a dashed pixel handle.
        let gw = cv.px(40.0);
        let gx = grip.x + (grip.w - gw) / 2;
        let gy = grip.y + (grip.h - u) / 2;
        let mut x = gx;
        while x < gx + gw {
            cv.fill(IRect::new(x, gy, u, u), Color::rgb(pal.faint));
            x += 2 * u;
        }

        self.draw_toasts(cv, panel, term, &pal, &mut hits);
        if self.palette.is_some() {
            self.draw_palette(cv, panel, &pal, &mut hits);
        }
        self.hits = hits;

        if self.cfg.window.crt {
            cv.scanlines(panel, 0.18);
        }

        // Pixel dissolve during open/close.
        if progress < 1.0 && self.cfg.motion.pattern != Pattern::None {
            let cell = (self.cfg.motion.cell as i32 * u).max(1);
            let full = IRect::new(0, 0, cv.w, cv.h);
            let rows = (cv.h / cell).max(1) as f32;
            match self.cfg.motion.pattern {
                Pattern::Bayer => cv.dissolve(full, cell, progress, bayer),
                Pattern::Blocks => cv.dissolve(full, cell, progress, |x, y| bayer(x / 3, y / 3)),
                Pattern::Noise => cv.dissolve(full, cell, progress, hash01),
                Pattern::Scan => cv.dissolve(full, cell, progress, move |x, y| (y as f32 / rows) * 0.75 + bayer(x, y) * 0.25),
                Pattern::None => {}
            }
        }
    }

    // ───────────────────────────── tab bar ─────────────────────────────

    fn draw_tabbar(&mut self, cv: &mut Canvas, r: IRect, pal: &Palette, hits: &mut Hits) {
        let u = cv.unit();
        let upx = self.ui_px(cv.scale);
        cv.fill(r, Color::rgba(pal.bg, 0.55));
        cv.hline(r.x, r.bottom() - u, r.w, u, Color::rgb(pal.line));
        let lh = self.fonts.line_height(upx);
        let ttop = r.y + (r.h - lh) / 2;
        let pad = cv.px(12.0);
        let gap = cv.px(8.0);
        let isz = icons::size(cv);
        let cw = self.fonts.text_width("M", upx).max(1);

        // Right side: hints + close.
        let close_w = cv.px(34.0);
        let hint = if self.orca_ok { "⌘K commands  ⌘⇧O orca" } else { "⌘K commands" };
        let hint_w = self.fonts.text_width(hint, upx) + pad;
        let plus_w = cv.px(32.0);
        let show_hint = r.w > cv.px(900.0);
        let avail = r.w - close_w - plus_w - if show_hint { hint_w } else { 0 };

        struct T {
            id: u64,
            icon: Option<Icon>,
            glyph: Option<char>,
            title: String,
            sub: String,
            badge: Badge,
            key: String,
            bell: bool,
        }
        let frame = self.spinner_frame();
        let mut items: Vec<T> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let (icon, glyph) = match t.kind() {
                    TabKind::Shell => (Some(Icon::Square), None),
                    TabKind::Claude => (Some(Icon::Claude), None),
                    TabKind::Orca => (Some(Icon::Orca), None),
                    TabKind::Custom(c) => (None, Some(c)),
                };
                T {
                    id: t.id,
                    icon,
                    glyph,
                    title: t.title(),
                    sub: t.decor.subtitle.clone().unwrap_or_default(),
                    badge: t.decor.badge,
                    key: if i < 9 { format!("⌘{}", i + 1) } else { String::new() },
                    bell: t.bell.map(|b| b.elapsed() < Duration::from_millis(700)).unwrap_or(false),
                }
            })
            .collect();

        let width_of = |t: &T, fonts: &crate::render::text::Fonts| -> i32 {
            let mut w = pad + isz + gap + fonts.text_width(&t.title, upx);
            if !t.sub.is_empty() {
                w += gap + fonts.text_width(&t.sub, upx);
            }
            if t.badge != Badge::None {
                w += gap + isz;
            }
            if !t.key.is_empty() {
                w += gap + fonts.text_width(&t.key, upx);
            }
            w + pad
        };
        // Shrink: drop key hints, then subtitles, then truncate titles.
        let total = |items: &Vec<T>, fonts: &crate::render::text::Fonts| items.iter().map(|t| width_of(t, fonts)).sum::<i32>();
        if total(&items, &self.fonts) > avail {
            items.iter_mut().for_each(|t| t.key.clear());
        }
        if total(&items, &self.fonts) > avail {
            for t in items.iter_mut() {
                t.sub = truncate(&t.sub, 10);
            }
        }
        if total(&items, &self.fonts) > avail {
            items.iter_mut().for_each(|t| t.sub.clear());
        }
        if total(&items, &self.fonts) > avail && !items.is_empty() {
            let per = (avail / items.len() as i32 - 2 * pad - isz - gap * 2 - isz).max(cw * 3);
            let cols = (per / cw).max(3) as usize;
            for t in items.iter_mut() {
                t.title = truncate(&t.title, cols);
            }
        }

        let mut x = r.x;
        let active_id = self.active_tab().map(|t| t.id);
        for t in &items {
            let w = width_of(t, &self.fonts);
            let rect = IRect::new(x, r.y, w, r.h - u);
            let active = Some(t.id) == active_id;
            if active {
                cv.fill(rect, Color::rgba(pal.surface_hi, 0.85));
                cv.fill(IRect::new(x, r.bottom() - 2 * u, w, 2 * u), Color::rgb(pal.accent));
            } else if t.bell {
                cv.fill(IRect::new(x, r.bottom() - 2 * u, w, 2 * u), Color::rgb(pal.warn));
            }
            let mut cx = x + pad;
            let icol = match t.badge {
                Badge::Attention => pal.warn,
                _ if active => pal.accent,
                _ => pal.dim,
            };
            if let Some(i) = t.icon {
                icons::draw_centered(cv, i, cx, r.y, r.h, Color::rgb(icol));
            } else if let Some(g) = t.glyph {
                self.fonts.draw_text(cv, cx, ttop, &g.to_string(), upx, Style::Regular, Color::rgb(icol));
            }
            cx += isz + gap;
            let tc = if active { pal.text } else { pal.dim };
            cx += self.fonts.draw_text(cv, cx, ttop, &t.title, upx, if active { Style::Bold } else { Style::Regular }, Color::rgb(tc));
            if !t.sub.is_empty() {
                cx += gap;
                cx += self.fonts.draw_text(cv, cx, ttop, &t.sub, upx, Style::Regular, Color::rgb(pal.faint));
            }
            match t.badge {
                Badge::None => {}
                Badge::Spinner => {
                    cx += gap;
                    icons::draw_centered(cv, Icon::Spinner(frame), cx, r.y, r.h, Color::rgb(pal.accent));
                    cx += isz;
                }
                Badge::Attention => {
                    cx += gap;
                    icons::draw_centered(cv, Icon::Warn, cx, r.y, r.h, Color::rgb(pal.warn));
                    cx += isz;
                }
                Badge::Done => {
                    cx += gap;
                    icons::draw_centered(cv, Icon::Check, cx, r.y, r.h, Color::rgb(pal.ok));
                    cx += isz;
                }
                Badge::Dots(n) => {
                    cx += gap;
                    let s = 2 * u;
                    let y = r.y + (r.h - s) / 2;
                    for i in 0..n as i32 {
                        cv.fill(IRect::new(cx + i * (s + u), y, s, s), Color::rgb(pal.accent));
                    }
                    cx += isz;
                }
            }
            if !t.key.is_empty() {
                cx += gap;
                self.fonts.draw_text(cv, cx, ttop, &t.key, upx, Style::Regular, Color::rgb(pal.faint));
            }
            cv.vline(x + w - u, r.y, r.h - u, u, Color::rgb(pal.line));
            hits.tabs.push((rect, t.id));
            x += w;
        }
        // "+"
        let plus = IRect::new(x, r.y, plus_w, r.h - u);
        icons::draw_centered(cv, Icon::Plus, x + (plus_w - isz) / 2, r.y, r.h, Color::rgb(pal.faint));
        hits.plus = plus;

        // Hints and close.
        let close = IRect::new(r.right() - close_w, r.y, close_w, r.h - u);
        cv.vline(close.x, r.y, r.h - u, u, Color::rgb(pal.line));
        icons::draw_centered(cv, Icon::Close, close.x + (close_w - isz) / 2, r.y, r.h, Color::rgb(pal.dim));
        hits.close = close;
        if show_hint {
            let hx = close.x - hint_w;
            if hx > x + plus_w {
                self.fonts.draw_text(cv, hx, ttop, hint, upx, Style::Regular, Color::rgb(pal.faint));
            }
        }
    }

    // ───────────────────────────── terminal ─────────────────────────────

    fn draw_terminal(&mut self, cv: &mut Canvas, r: IRect, pal: &Palette) {
        let Some(tab) = self.tabs.get(self.active) else { return };
        let cell = self.cell;
        let term_lock = tab.session.term.clone();
        let mut term = term_lock.lock();
        tab.sync_selection(&mut term);
        let content = term.renderable_content();
        let colors = content.colors;
        let sel = content.selection;
        let offset = content.display_offset as i32;
        let cursor = content.cursor;
        let mode = content.mode;
        let rows = tab.session.rows as i32;
        let cols = tab.session.cols as i32;
        let old_clip = cv.set_clip(r);
        let u = cv.unit();
        let px = cell.px;

        struct C {
            row: i32,
            col: i32,
            ch: char,
            fg: Rgb,
            bg: Option<Rgb>,
            flags: Flags,
        }
        let mut cells: Vec<C> = Vec::with_capacity((rows * cols) as usize);
        for ic in content.display_iter {
            let row = ic.point.line.0 + offset;
            let col = ic.point.column.0 as i32;
            if row < 0 || row >= rows || col >= cols {
                continue;
            }
            let c = ic.cell;
            let mut fg = resolve(c.fg, colors, pal, false).unwrap_or(pal.text);
            let mut bg = resolve(c.bg, colors, pal, true);
            let flags = c.flags;
            if flags.contains(Flags::INVERSE) {
                let f = bg.unwrap_or(pal.panel);
                bg = Some(fg);
                fg = f;
            }
            if flags.contains(Flags::DIM) {
                fg = fg.mix(bg.unwrap_or(pal.panel), 0.4);
            }
            if let Some(s) = &sel {
                if s.contains(Point::new(Line(row - offset), Column(col as usize))) {
                    bg = Some(pal.selection);
                    fg = pal.text;
                }
            }
            cells.push(C { row, col, ch: c.c, fg, bg, flags });
        }

        // Backgrounds.
        for c in &cells {
            if let Some(bg) = c.bg {
                let w = if c.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
                cv.fill(IRect::new(r.x + c.col * cell.w, r.y + c.row * cell.h, cell.w * w, cell.h), Color::rgb(bg));
            }
        }

        // Cursor geometry.
        let crow = cursor.point.line.0 + offset;
        let ccol = cursor.point.column.0 as i32;
        let blink_on = !self.cfg.cursor.blink || !self.focused || (self.blink_epoch.elapsed().as_millis() / 530) % 2 == 0;
        let show_cursor = cursor.shape != CursorShape::Hidden
            && mode.contains(alacritty_terminal::term::TermMode::SHOW_CURSOR)
            && crow >= 0
            && crow < rows
            && blink_on;
        let shape = match (cursor.shape, self.cfg.cursor.style) {
            (CursorShape::Block, CursorStyle::Beam) => CursorShape::Beam,
            (CursorShape::Block, CursorStyle::Underline) => CursorShape::Underline,
            (s, _) => s,
        };
        let crect = IRect::new(r.x + ccol * cell.w, r.y + crow * cell.h, cell.w, cell.h);
        let block_cursor = show_cursor && self.focused && matches!(shape, CursorShape::Block);
        if block_cursor {
            cv.fill(crect, Color::rgb(pal.cursor));
        }

        // Glyphs + decorations.
        for c in &cells {
            if c.flags.contains(Flags::WIDE_CHAR_SPACER) || c.flags.contains(Flags::HIDDEN) {
                continue;
            }
            let wide = c.flags.contains(Flags::WIDE_CHAR);
            let x = r.x + c.col * cell.w;
            let y = r.y + c.row * cell.h;
            let box_ = IRect::new(x, y, cell.w * if wide { 2 } else { 1 }, cell.h);
            let style = if c.flags.contains(Flags::BOLD) {
                Style::Bold
            } else if c.flags.contains(Flags::ITALIC) {
                Style::Italic
            } else {
                Style::Regular
            };
            let fg = if block_cursor && c.row == crow && c.col == ccol { c.bg.unwrap_or(pal.panel) } else { c.fg };
            let color = Color::rgb(fg);
            if c.ch != ' ' {
                self.fonts.draw_char(cv, c.ch, box_, y + cell.baseline, px, style, color, if wide { 2 } else { 1 });
            }
            let uy = y + cell.baseline + 2 * u;
            if c.flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE) {
                cv.hline(x, uy.min(y + cell.h - u), box_.w, u, color);
                if c.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    cv.hline(x, (uy + 2 * u).min(y + cell.h - u), box_.w, u, color);
                }
            }
            if c.flags.intersects(Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
                // Pixel zig-zag / dots.
                let mut xx = x;
                let mut up = false;
                while xx < x + box_.w {
                    let yy = if c.flags.contains(Flags::UNDERCURL) && up { uy - u } else { uy };
                    cv.fill(IRect::new(xx, yy.min(y + cell.h - u), u, u), color);
                    xx += if c.flags.contains(Flags::UNDERCURL) { u } else { 2 * u };
                    up = !up;
                }
            }
            if c.flags.contains(Flags::STRIKEOUT) {
                cv.hline(x, y + cell.h / 2, box_.w, u, color);
            }
        }

        if show_cursor && !block_cursor {
            let c = Color::rgb(pal.cursor);
            match (shape, self.focused) {
                (CursorShape::Beam, true) => cv.fill(IRect::new(crect.x, crect.y, 2 * u, crect.h), c),
                (CursorShape::Underline, true) => cv.fill(IRect::new(crect.x, crect.bottom() - 2 * u, crect.w, 2 * u), c),
                _ => cv.stroke(crect, u, c),
            }
        }

        // Scrollback indicator.
        if offset > 0 {
            let label = format!("↑ {offset}");
            let upx = self.ui_px(cv.scale);
            let w = self.fonts.text_width(&label, upx) + cv.px(16.0);
            let h = self.fonts.line_height(upx) + cv.px(6.0);
            let b = IRect::new(r.right() - w - cv.px(4.0), r.y + cv.px(4.0), w, h);
            cv.notched(b, u, Color::rgb(pal.accent));
            self.fonts.draw_text(cv, b.x + cv.px(8.0), b.y + cv.px(3.0), &label, upx, Style::Bold, Color::rgb(pal.on_accent));
        }
        drop(term);
        cv.clip = old_clip;
    }

    // ───────────────────────────── status bar ─────────────────────────────

    fn seg_width(&self, cv: &Canvas, s: &Segment, upx: f32) -> i32 {
        let pad = cv.px(10.0);
        let mut w = pad * 2 + self.fonts.text_width(&s.text, upx);
        if s.icon.is_some() {
            w += icons::size(cv) + cv.px(6.0);
        }
        if s.spinner {
            w += icons::size(cv) + cv.px(6.0);
        }
        if let Some((_, n)) = s.meter {
            w += n as i32 * (cv.px(5.0) + cv.unit()) + cv.px(6.0);
        }
        w
    }

    fn draw_seg(&mut self, cv: &mut Canvas, x: i32, r: IRect, s: &Segment, pal: &Palette, upx: f32, ttop: i32) -> i32 {
        let pad = cv.px(10.0);
        let u = cv.unit();
        let mut cx = x + pad;
        let col = tone(pal, s.tone);
        if let Some(i) = s.icon {
            let ic = if s.tone == Tone::Warn { pal.warn } else { pal.accent };
            cx += icons::draw_centered(cv, i, cx, r.y, r.h, Color::rgb(ic)) + cv.px(6.0);
        }
        if s.spinner {
            cx += icons::draw_centered(cv, Icon::Spinner(self.spinner_frame()), cx, r.y, r.h, Color::rgb(pal.accent)) + cv.px(6.0);
        }
        if let Some((frac, n)) = s.meter {
            let cw = cv.px(5.0);
            let ch = cv.px(10.0);
            let filled = (frac.clamp(0.0, 1.0) * n as f32).round() as i32;
            let y = r.y + (r.h - ch) / 2;
            for i in 0..n as i32 {
                let c = if i < filled { if s.tone == Tone::Warn { pal.warn } else { pal.accent } } else { pal.surface_hi };
                cv.fill(IRect::new(cx + i * (cw + u), y, cw, ch), Color::rgb(c));
            }
            cx += n as i32 * (cw + u) + cv.px(6.0);
        }
        cx += self.fonts.draw_text(cv, cx, ttop, &s.text, upx, Style::Regular, Color::rgb(col));
        cx + pad - x
    }

    fn dotted_sep(cv: &mut Canvas, x: i32, r: IRect, c: Color) {
        let u = cv.unit();
        let mut y = r.y + cv.px(7.0);
        while y < r.bottom() - cv.px(7.0) {
            cv.fill(IRect::new(x, y, u, u), c);
            y += 2 * u;
        }
    }

    fn draw_status(&mut self, cv: &mut Canvas, r: IRect, pal: &Palette, hits: &mut Hits) {
        let u = cv.unit();
        let upx = self.ui_px(cv.scale);
        cv.fill(r, Color::rgba(pal.bg, 0.6));
        cv.hline(r.x, r.y, r.w, u, Color::rgb(pal.line));
        let ttop = r.y + (r.h - self.fonts.line_height(upx)) / 2;

        let infos = self.tab_infos();
        let active = self.active_tab().map(|t| t.id);
        let ctx = crate::ext::Ctx { tabs: &infos, active, visible: true, cfg: &self.cfg, now: Instant::now(), actions: vec![] };
        let mut left: Vec<Segment> = vec![];
        if let Some(info) = infos.iter().find(|t| Some(t.id) == active) {
            for e in &self.exts {
                left.extend(e.status_left(info, &ctx));
            }
        }
        let mut right: Vec<Segment> = self.statuses.values().map(|t| Segment::text(t.clone(), Tone::Dim)).collect();
        for e in &self.exts {
            right.extend(e.status_right(&ctx));
        }
        drop(ctx);

        let sep = Color::rgb(pal.line);
        // Right first (it has priority for space).
        let mut rx = r.right() - cv.px(4.0);
        let mut right_start = rx;
        for (i, s) in right.iter().enumerate().rev() {
            let w = self.seg_width(cv, s, upx);
            if rx - w < r.x + r.w / 3 {
                break;
            }
            rx -= w;
            self.draw_seg(cv, rx, r, s, pal, upx, ttop);
            if let Some(a) = &s.action {
                hits.status.push((IRect::new(rx, r.y, w, r.h), a.clone()));
            }
            if i > 0 {
                Self::dotted_sep(cv, rx, r, sep);
            }
            right_start = rx;
        }
        let mut x = r.x + cv.px(4.0);
        let old = cv.set_clip(IRect::new(r.x, r.y, right_start - r.x - cv.px(8.0), r.h));
        for (i, s) in left.iter().enumerate() {
            if i > 0 {
                Self::dotted_sep(cv, x, r, sep);
            }
            let w = self.draw_seg(cv, x, r, s, pal, upx, ttop);
            if let Some(a) = &s.action {
                hits.status.push((IRect::new(x, r.y, w, r.h), a.clone()));
            }
            x += w;
        }
        cv.clip = old;
    }

    // ───────────────────────────── toasts ─────────────────────────────

    fn draw_toasts(&mut self, cv: &mut Canvas, panel: IRect, term: IRect, pal: &Palette, hits: &mut Hits) {
        if self.toasts.is_empty() {
            return;
        }
        let u = cv.unit();
        let upx = self.ui_px(cv.scale);
        let lh = self.fonts.line_height(upx);
        let cw = self.fonts.text_width("M", upx).max(1);
        let w = cv.px(420.0).min(panel.w / 2).max(cv.px(260.0));
        let pad = cv.px(10.0);
        let mut y = term.y + cv.px(4.0);
        let toasts: Vec<(usize, crate::ext::Notice, f32, f32)> = self
            .toasts
            .iter()
            .enumerate()
            .rev()
            .map(|(i, t)| {
                let enter = (t.born.elapsed().as_secs_f32() / 0.22).min(1.0);
                let left = 1.0 - t.born.elapsed().as_secs_f32() / t.ttl.as_secs_f32();
                (i, t.notice.clone(), enter, left.clamp(0.0, 1.0))
            })
            .collect();
        for (i, n, enter, left) in toasts {
            let body = wrap(&n.body, ((w - 2 * pad) / cw) as usize, 3);
            let head_h = lh + cv.px(10.0);
            let body_h = if body.is_empty() { 0 } else { body.len() as i32 * lh + cv.px(10.0) };
            let act_h = if n.actions.is_empty() { 0 } else { lh + cv.px(16.0) };
            let h = head_h + body_h + act_h + 2 * u;
            // Stepped slide-in from the right (6 whole steps).
            let step = (enter * 6.0).floor() / 6.0;
            let xoff = ((1.0 - step) * (w + cv.px(24.0)) as f32) as i32;
            let b = IRect::new(panel.right() - cv.px(14.0) - w + xoff, y, w, h);
            if b.bottom() > panel.bottom() - cv.px((STATUS_H + GRIP_H) as f32) {
                break;
            }
            cv.hard_shadow(b, cv.px(4.0), Color::rgba(BLACK, 0.55));
            cv.fill(b, Color::rgba(pal.panel, 0.98));
            cv.stroke(b, u, Color::rgb(pal.line));
            let (icon, ic) = match n.level {
                Level::Warn => (Icon::Warn, pal.warn),
                Level::Ok => (Icon::Check, pal.ok),
                Level::Info => (Icon::Bell, pal.accent),
            };
            let hy = b.y + cv.px(5.0);
            icons::draw_centered(cv, icon, b.x + pad, hy, lh, Color::rgb(ic));
            let tx = b.x + pad + icons::size(cv) + cv.px(8.0);
            let title = truncate(&n.title, ((b.right() - pad - tx) / cw).max(1) as usize);
            self.fonts.draw_text(cv, tx, hy, &title, upx, Style::Bold, Color::rgb(pal.text));
            cv.hline(b.x, b.y + head_h, b.w, u, Color::rgb(pal.line));
            let mut by = b.y + head_h + cv.px(5.0);
            for l in &body {
                self.fonts.draw_text(cv, b.x + pad, by, l, upx, Style::Regular, Color::rgb(pal.dim));
                by += lh;
            }
            if !n.actions.is_empty() {
                let ay = b.y + head_h + body_h + cv.px(2.0);
                let mut ax = b.x + pad;
                for (ai, (label, _)) in n.actions.iter().enumerate() {
                    let bw = self.fonts.text_width(label, upx) + cv.px(16.0);
                    let br = IRect::new(ax, ay, bw, lh + cv.px(6.0));
                    if ai == 0 {
                        cv.notched(br, u, Color::rgb(pal.accent));
                        self.fonts.draw_text(cv, ax + cv.px(8.0), ay + cv.px(3.0), label, upx, Style::Bold, Color::rgb(pal.on_accent));
                    } else {
                        cv.notched(br, u, Color::rgb(pal.surface));
                        cv.notched_stroke(br, u, u, Color::rgb(pal.line));
                        self.fonts.draw_text(cv, ax + cv.px(8.0), ay + cv.px(3.0), label, upx, Style::Regular, Color::rgb(pal.text));
                    }
                    hits.toast_buttons.push((br, i, ai));
                    ax += bw + cv.px(6.0);
                }
            }
            // Draining accent line.
            let bar_w = (b.w as f32 * left) as i32;
            cv.fill(IRect::new(b.x, b.bottom() - 2 * u, bar_w, 2 * u), Color::rgb(if n.level == Level::Warn { pal.warn } else { pal.accent }));
            hits.toast_boxes.push((b, i));
            y = b.bottom() + cv.px(10.0);
        }
    }

    // ───────────────────────────── palette ─────────────────────────────

    fn draw_palette(&mut self, cv: &mut Canvas, panel: IRect, pal: &Palette, hits: &mut Hits) {
        let Some(mut p) = self.palette.take() else { return };
        let u = cv.unit();
        let upx = self.ui_px(cv.scale);
        let lh = self.fonts.line_height(upx);
        let cw = self.fonts.text_width("M", upx).max(1);

        // Dither the content behind (pixel-style dimming).
        let below = IRect::new(panel.x, panel.y + cv.px(TABBAR_H as f32), panel.w, panel.h - cv.px(TABBAR_H as f32));
        cv.dither(below, Color::rgba(pal.bg, 0.92), 8, u);

        let w = cv.px(780.0).min(panel.w - cv.px(60.0));
        let x = panel.x + (panel.w - w) / 2;
        let y = panel.y + cv.px((TABBAR_H + 14.0) as f32);
        let row_h = lh + cv.px(10.0);
        let head_h = lh + cv.px(14.0);
        let input_h = lh + cv.px(16.0);
        let foot_h = lh + cv.px(12.0);
        let max_h = panel.bottom() - cv.px((STATUS_H + GRIP_H + 14.0) as f32) - y;
        let rows_fit = ((max_h - head_h - input_h - foot_h - cv.px(16.0)) / row_h).clamp(1, 12) as usize;
        let shown = p.filtered.len().min(rows_fit).max(1);
        p.ensure_visible(rows_fit);
        let h = head_h + input_h + cv.px(8.0) + shown as i32 * row_h + foot_h + cv.px(8.0);
        let b = IRect::new(x, y, w, h);
        hits.palette_box = b;
        cv.hard_shadow(b, cv.px(4.0), Color::rgba(BLACK, 0.6));
        cv.fill(b, Color::rgba(pal.panel, 0.99));
        cv.stroke(b, u, Color::rgb(pal.line));

        // Header.
        let pad = cv.px(12.0);
        let hy = b.y + (head_h - lh) / 2;
        let (hicon, htitle) = match p.mode {
            PaletteMode::All => (Icon::Square, "commands"),
            PaletteMode::Agents => (Icon::Claude, "agents"),
            PaletteMode::Orca => (Icon::Orca, "orca"),
            PaletteMode::Themes => (Icon::Ring, "look & feel"),
        };
        icons::draw_centered(cv, hicon, b.x + pad, b.y, head_h, Color::rgb(pal.accent));
        let mut hx = b.x + pad + icons::size(cv) + cv.px(8.0);
        hx += self.fonts.draw_text(cv, hx, hy, htitle, upx, Style::Bold, Color::rgb(pal.text));
        self.fonts.draw_text(cv, hx + cv.px(8.0), hy, &p.filtered.len().to_string(), upx, Style::Regular, Color::rgb(pal.faint));
        // Mode chips (right-aligned).
        let (claude_ok, orca_ok) = (self.claude_ok, self.orca_ok);
        let chips: Vec<(PaletteMode, &str)> = [(PaletteMode::All, "ALL"), (PaletteMode::Agents, "AGENTS"), (PaletteMode::Orca, "ORCA"), (PaletteMode::Themes, "LOOK")]
            .into_iter()
            .filter(|(m, _)| match m {
                PaletteMode::Agents => claude_ok,
                PaletteMode::Orca => orca_ok,
                _ => true,
            })
            .collect();
        let small = upx * 0.85;
        let slh = self.fonts.line_height(small);
        let mut cx = b.right() - pad;
        for (m, label) in chips.iter().rev() {
            let cwid = self.fonts.text_width(label, small) + cv.px(12.0);
            cx -= cwid;
            let cr = IRect::new(cx, b.y + (head_h - slh - cv.px(4.0)) / 2, cwid, slh + cv.px(4.0));
            if *m == p.mode {
                cv.notched(cr, u, Color::rgb(pal.accent));
                self.fonts.draw_text(cv, cx + cv.px(6.0), cr.y + cv.px(2.0), label, small, Style::Bold, Color::rgb(pal.on_accent));
            } else {
                cv.notched_stroke(cr, u, u, Color::rgb(pal.line));
                self.fonts.draw_text(cv, cx + cv.px(6.0), cr.y + cv.px(2.0), label, small, Style::Regular, Color::rgb(pal.dim));
            }
            hits.palette_chips.push((cr, *m));
            cx -= cv.px(4.0);
        }
        cv.hline(b.x, b.y + head_h, b.w, u, Color::rgb(pal.line));

        // Input.
        let ir = IRect::new(b.x + pad, b.y + head_h + cv.px(8.0), b.w - 2 * pad, input_h);
        cv.fill(ir, Color::rgb(pal.bg));
        cv.stroke(ir, u, Color::rgb(pal.line));
        let iy = ir.y + (ir.h - lh) / 2;
        let mut ix = ir.x + cv.px(10.0);
        if p.query.is_empty() {
            let ph = match p.mode {
                PaletteMode::Orca => "worktree… ⏎ shell there · ⌘⏎ claude there",
                PaletteMode::Agents => "claude, resume, /compact…",
                PaletteMode::Themes => "theme, shape, motion…",
                PaletteMode::All => "type to filter…",
            };
            self.fonts.draw_text(cv, ix + cw + cv.px(4.0), iy, ph, upx, Style::Regular, Color::rgb(pal.faint));
        } else {
            ix += self.fonts.draw_text(cv, ix, iy, &p.query, upx, Style::Regular, Color::rgb(pal.text));
        }
        if (p.opened.elapsed().as_millis() / 530) % 2 == 0 {
            cv.fill(IRect::new(ix, iy, cw, lh), Color::rgb(pal.accent));
        }

        // Rows.
        let mut ry = ir.bottom() + cv.px(8.0);
        let isz = icons::size(cv);
        if p.filtered.is_empty() {
            self.fonts.draw_text(cv, b.x + pad + cv.px(8.0), ry + (row_h - lh) / 2, "nothing matches", upx, Style::Regular, Color::rgb(pal.faint));
        }
        for vis_i in 0..shown {
            let fi = p.scroll + vis_i;
            let Some(&idx) = p.filtered.get(fi) else { break };
            let c = &p.items[idx];
            let rr = IRect::new(b.x + cv.px(6.0), ry, b.w - cv.px(12.0), row_h);
            let sel = fi == p.sel;
            if sel {
                cv.notched(rr, u, Color::rgb(pal.accent));
            }
            let (tc, dc, fc) = if sel {
                (pal.on_accent, pal.on_accent.mix(pal.accent, 0.3), pal.on_accent.mix(pal.accent, 0.35))
            } else {
                (pal.text, pal.dim, pal.faint)
            };
            let ty = ry + (row_h - lh) / 2;
            icons::draw_centered(cv, c.icon, rr.x + cv.px(8.0), ry, row_h, Color::rgb(if sel { pal.on_accent } else { pal.accent }));
            let mut tx = rr.x + cv.px(8.0) + isz + cv.px(10.0);
            // Right column: right text + keys.
            let mut right_w = 0;
            if !c.keys.is_empty() {
                right_w += self.fonts.text_width(&c.keys, upx) + cv.px(12.0);
            }
            if !c.right.is_empty() {
                right_w += self.fonts.text_width(&c.right, upx) + cv.px(12.0);
            }
            let max_text = rr.right() - right_w - tx - cv.px(8.0);
            let title_cols = ((max_text * 2 / 3) / cw).max(4) as usize;
            let title = truncate(&c.title, title_cols);
            tx += self.fonts.draw_text(cv, tx, ty, &title, upx, Style::Bold, Color::rgb(tc));
            let rem = (rr.right() - right_w - tx - cv.px(18.0)) / cw;
            if rem > 3 && !c.detail.is_empty() {
                self.fonts.draw_text(cv, tx + cv.px(10.0), ty, &truncate(&c.detail, rem as usize), upx, Style::Regular, Color::rgb(dc));
            }
            let mut rx = rr.right() - cv.px(10.0);
            if !c.keys.is_empty() {
                rx -= self.fonts.text_width(&c.keys, upx);
                self.fonts.draw_text(cv, rx, ty, &c.keys, upx, Style::Regular, Color::rgb(fc));
                rx -= cv.px(12.0);
            }
            if !c.right.is_empty() {
                rx -= self.fonts.text_width(&c.right, upx);
                let rc = if sel { pal.on_accent } else { tone(pal, c.right_tone) };
                self.fonts.draw_text(cv, rx, ty, &c.right, upx, Style::Regular, Color::rgb(rc));
            }
            hits.palette_rows.push((rr, fi));
            ry += row_h;
        }

        // Footer.
        let fy = b.bottom() - foot_h + (foot_h - lh) / 2;
        cv.hline(b.x, b.bottom() - foot_h, b.w, u, Color::rgb(pal.line));
        let hint = if p.mode == PaletteMode::Orca {
            "⏎ shell in worktree · ⌘⏎ claude in worktree · ⇥ mode · esc close"
        } else {
            "⏎ run · ⌘⏎ alternate · ↑↓ select · ⇥ mode · esc close"
        };
        self.fonts.draw_text(cv, b.x + pad, fy, hint, upx, Style::Regular, Color::rgb(pal.faint));
        self.palette = Some(p);
    }
}

/// Write the (premultiplied ARGB) frame as a PNG. Used by `fluteterm snapshot`.
pub(super) fn save_png(path: &std::path::Path, buf: &[u32], w: u32, h: u32) -> anyhow::Result<()> {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for &p in buf.iter().take((w * h) as usize) {
        let a = (p >> 24) & 0xff;
        let un = |c: u32| if a == 0 { 0 } else { ((c * 255 + a / 2) / a).min(255) as u8 };
        rgba.extend_from_slice(&[un((p >> 16) & 0xff), un((p >> 8) & 0xff), un(p & 0xff), a as u8]);
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
