//! Font loading, glyph cache and text drawing. Text is always smooth (antialiased);
//! the "pixel" look lives in the procedural glyphs and widgets, not in the font.

use std::collections::HashMap;

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _};
use fontdue::{Font, FontSettings};
use unicode_width::UnicodeWidthChar;

use super::canvas::{Canvas, Color, IRect};
use super::glyphs;

static JB_REGULAR: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");
static JB_BOLD: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Bold.ttf");
static JB_ITALIC: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Italic.ttf");

const FALLBACKS: &[(&str, u32)] = &[
    ("/System/Library/Fonts/Menlo.ttc", 0),
    ("/System/Library/Fonts/Apple Symbols.ttf", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/System/Library/Fonts/CJKSymbolsFallback.ttc", 0),
];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Style {
    Regular,
    Bold,
    Italic,
}

pub struct Glyph {
    pub w: usize,
    pub h: usize,
    pub left: i32,
    /// Offset from the baseline to the top of the bitmap (positive = above).
    pub top: i32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CellMetrics {
    pub w: i32,
    pub h: i32,
    pub baseline: i32,
    pub px: f32,
}

pub struct Fonts {
    regular: Font,
    bold: Font,
    italic: Font,
    fallbacks: Vec<Fallback>,
    cache: HashMap<(char, Style, u32, u8), Option<Glyph>>,
    lut: [u8; 256],
}

fn load_font(bytes: &[u8], index: u32) -> Option<Font> {
    Font::from_bytes(bytes, FontSettings { collection_index: index, scale: 40.0, ..Default::default() }).ok()
}

impl Fonts {
    pub fn new(family: &str, bold: Option<&str>) -> Self {
        let custom = |path: &str| -> Option<Font> {
            let p = crate::util::expand_tilde(path);
            let bytes = std::fs::read(p).ok()?;
            load_font(&bytes, 0)
        };
        let builtin = family.eq_ignore_ascii_case("jetbrains mono") || family.is_empty();
        let regular = if builtin { None } else { custom(family) };
        let bold_f = bold.and_then(custom);
        let regular_ok = regular.is_some();
        Self {
            regular: regular.unwrap_or_else(|| load_font(JB_REGULAR, 0).unwrap()),
            bold: bold_f.unwrap_or_else(|| {
                if regular_ok { custom(family).unwrap() } else { load_font(JB_BOLD, 0).unwrap() }
            }),
            italic: if regular_ok { custom(family).unwrap() } else { load_font(JB_ITALIC, 0).unwrap() },
            fallbacks: FALLBACKS.iter().map(|(p, i)| Fallback { path: p, index: *i, state: None }).collect(),
            cache: HashMap::new(),
            lut: std::array::from_fn(|i| i as u8),
        }
    }

    /// Light-on-dark text looks thin with linear coverage; apply a gentle gamma.
    pub fn set_dark(&mut self, dark: bool) {
        let g = if dark { 0.78 } else { 1.0 };
        self.lut = std::array::from_fn(|i| ((i as f32 / 255.0).powf(g) * 255.0).round() as u8);
        self.cache.clear();
    }

    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    pub fn cell_metrics(&self, px: f32, line_height: f32) -> CellMetrics {
        let m = self.regular.metrics('M', px);
        let w = m.advance_width.round().max(1.0) as i32;
        let lm = self.regular.horizontal_line_metrics(px);
        let (asc, desc) = lm.map(|l| (l.ascent, l.descent)).unwrap_or((px * 0.8, -px * 0.2));
        let content = asc - desc;
        let h = (px * line_height).round().max(content.ceil()) as i32;
        let baseline = ((h as f32 - content) / 2.0 + asc).round() as i32;
        CellMetrics { w, h, baseline, px }
    }

    fn font(&self, s: Style) -> &Font {
        match s {
            Style::Regular => &self.regular,
            Style::Bold => &self.bold,
            Style::Italic => &self.italic,
        }
    }

    fn glyph(&mut self, ch: char, style: Style, px: f32, cell_w: i32, cells: u8) -> Option<&Glyph> {
        let key = (ch, style, (px * 4.0) as u32, cells);
        if !self.cache.contains_key(&key) {
            let g = self.rasterize(ch, style, px, cell_w, cells);
            self.cache.insert(key, g);
        }
        self.cache.get(&key).and_then(|g| g.as_ref())
    }

    fn rasterize(&mut self, ch: char, style: Style, px: f32, cell_w: i32, cells: u8) -> Option<Glyph> {
        let lut = self.lut;
        let room = (cell_w * cells as i32) as f32 * 1.05;
        let primary = self.font(style);
        let font: Option<&Font> = if primary.lookup_glyph_index(ch) != 0 {
            Some(primary)
        } else if self.regular.lookup_glyph_index(ch) != 0 {
            Some(&self.regular)
        } else {
            None
        };
        if let Some(font) = font {
            // Scale down glyphs that are wider than their cells.
            let adv = font.metrics(ch, px).advance_width;
            let px = if adv > room && adv > 0.0 { px * room / adv } else { px };
            let (m, data) = font.rasterize(ch, px);
            return Some(Glyph {
                w: m.width,
                h: m.height,
                left: m.xmin,
                top: m.ymin + m.height as i32,
                data: data.into_iter().map(|c| lut[c as usize]).collect(),
            });
        }
        // System fallbacks: memory-mapped and rasterised lazily with ab_glyph, so a
        // 20 MB font costs only the pages we touch.
        for fb in &mut self.fallbacks {
            let Some(f) = fb.get() else { continue };
            let id = f.glyph_id(ch);
            if id.0 == 0 {
                continue;
            }
            let upem = f.units_per_em().unwrap_or(1000.0);
            let mut scale = PxScale::from(px * f.height_unscaled() / upem);
            let adv = f.as_scaled(scale).h_advance(id);
            if adv > room && adv > 0.0 {
                scale = PxScale::from(scale.y * room / adv);
            }
            let og = f.outline_glyph(id.with_scale(scale))?;
            let b = og.px_bounds();
            let (w, h) = (b.width().ceil().max(0.0) as usize, b.height().ceil().max(0.0) as usize);
            let mut data = vec![0u8; w * h];
            og.draw(|x, y, c| {
                let (x, y) = (x as usize, y as usize);
                if x < w && y < h {
                    data[y * w + x] = lut[(c.clamp(0.0, 1.0) * 255.0) as usize];
                }
            });
            return Some(Glyph { w, h, left: b.min.x.floor() as i32, top: -b.min.y.floor() as i32, data });
        }
        None
    }

    /// Draw one character into a cell. `cell` is the cell box (for procedural glyphs),
    /// `baseline` the absolute baseline y.
    pub fn draw_char(
        &mut self,
        cv: &mut Canvas,
        ch: char,
        cell: IRect,
        baseline: i32,
        px: f32,
        style: Style,
        color: Color,
        cells: u8,
    ) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        if glyphs::draw(cv, ch, cell, color) {
            return;
        }
        let cell_w = cell.w / cells.max(1) as i32;
        if let Some(g) = self.glyph(ch, style, px, cell_w, cells) {
            cv.coverage(cell.x + g.left, baseline - g.top, g.w, g.h, &g.data, color);
        } else {
            // Tofu: a small outlined box in the pixel style.
            let u = cv.unit();
            let r = IRect::new(cell.x + cell.w / 5, cell.y + cell.h / 4, cell.w * 3 / 5, cell.h / 2);
            cv.stroke(r, u, color.with_alpha(0.5));
        }
    }

    /// Draw a UI string on a monospace grid of the given size. Returns the advance in px.
    pub fn draw_text(&mut self, cv: &mut Canvas, x: i32, top: i32, text: &str, px: f32, style: Style, color: Color) -> i32 {
        let m = self.cell_metrics(px, 1.25);
        let mut pen = x;
        for ch in text.chars() {
            let cells = ch.width().unwrap_or(1).max(1) as i32;
            let cell = IRect::new(pen, top, m.w * cells, m.h);
            self.draw_char(cv, ch, cell, top + m.baseline, px, style, color, cells as u8);
            pen += m.w * cells;
        }
        pen - x
    }

    /// Width of a UI string in px at the given size.
    pub fn text_width(&self, text: &str, px: f32) -> i32 {
        let w = self.cell_metrics(px, 1.25).w;
        text.chars().map(|c| c.width().unwrap_or(1).max(1) as i32).sum::<i32>() * w
    }

    pub fn line_height(&self, px: f32) -> i32 {
        self.cell_metrics(px, 1.25).h
    }
}

/// Truncate a string to `max` display columns, adding an ellipsis.
pub fn truncate(s: &str, max: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(1);
        if w + cw > max {
            if max > 0 {
                while out.chars().count() > 0 && w + 1 > max {
                    let c = out.pop().unwrap();
                    w -= c.width().unwrap_or(1);
                }
                out.push('…');
            }
            return out;
        }
        out.push(ch);
        w += cw;
    }
    out
}

struct Fallback {
    path: &'static str,
    index: u32,
    /// None = not tried yet; Some(None) = unavailable.
    state: Option<Option<FontRef<'static>>>,
}

impl Fallback {
    fn get(&mut self) -> Option<&FontRef<'static>> {
        if self.state.is_none() {
            self.state = Some(mmap(self.path).and_then(|d| FontRef::try_from_slice_and_index(d, self.index).ok()));
        }
        self.state.as_ref().unwrap().as_ref()
    }
}

/// Read-only memory map, leaked for the life of the process (fonts are shared, never unloaded).
fn mmap(path: &str) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len() as usize;
    if len == 0 {
        return None;
    }
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0);
        if p == libc::MAP_FAILED {
            return None;
        }
        Some(std::slice::from_raw_parts(p as *const u8, len))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world", 6), "hello…");
    }

    #[test]
    fn cell_metrics_sane() {
        let f = Fonts::new("JetBrains Mono", None);
        let m = f.cell_metrics(28.0, 1.3);
        assert!(m.w > 10 && m.h >= 36 && m.baseline < m.h);
    }
}
