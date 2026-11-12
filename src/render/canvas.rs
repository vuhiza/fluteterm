//! A premultiplied-ARGB software canvas with the handful of primitives the
//! design language needs: alpha fills, hairlines, notched rects, hard shadows,
//! ordered dithering and coverage blending for text.

use crate::theme::Rgb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };

    pub fn rgb(c: Rgb) -> Self {
        Self { r: c.r, g: c.g, b: c.b, a: 255 }
    }

    pub fn rgba(c: Rgb, a: f32) -> Self {
        Self { r: c.r, g: c.g, b: c.b, a: (a.clamp(0.0, 1.0) * 255.0).round() as u8 }
    }

    pub fn with_alpha(self, a: f32) -> Self {
        Self { a: ((self.a as f32) * a.clamp(0.0, 1.0)).round() as u8, ..self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl IRect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }
    pub fn intersect(&self, o: &IRect) -> IRect {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.right().min(o.right());
        let y1 = self.bottom().min(o.bottom());
        IRect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
    }
    pub fn inset(&self, d: i32) -> IRect {
        IRect::new(self.x + d, self.y + d, (self.w - 2 * d).max(0), (self.h - 2 * d).max(0))
    }
}

/// Bayer 4x4 thresholds (0..16).
pub const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub w: i32,
    pub h: i32,
    /// Physical pixels per point.
    pub scale: f32,
    pub clip: IRect,
}

#[inline]
fn blend_px(dst: u32, c: Color, cov: u32) -> u32 {
    // cov: 0..=255 extra coverage multiplier
    let a = (c.a as u32 * cov + 127) / 255;
    if a == 0 {
        return dst;
    }
    if a >= 255 {
        return 0xff00_0000 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
    }
    let inv = 255 - a;
    let da = dst >> 24;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let oa = a + (da * inv + 127) / 255;
    let or = (c.r as u32 * a + dr * inv + 127) / 255;
    let og = (c.g as u32 * a + dg * inv + 127) / 255;
    let ob = (c.b as u32 * a + db * inv + 127) / 255;
    oa.min(255) << 24 | or.min(255) << 16 | og.min(255) << 8 | ob.min(255)
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u32], w: i32, h: i32, scale: f32) -> Self {
        Self { buf, w, h, scale, clip: IRect::new(0, 0, w, h) }
    }

    /// Points → physical pixels.
    #[inline]
    pub fn px(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    /// One logical pixel ("the pixel grid" of the illusion), at least 1 physical px.
    #[inline]
    pub fn unit(&self) -> i32 {
        (self.scale.round() as i32).max(1)
    }

    pub fn clear(&mut self) {
        self.buf.fill(0);
    }

    pub fn set_clip(&mut self, r: IRect) -> IRect {
        let old = self.clip;
        self.clip = r.intersect(&IRect::new(0, 0, self.w, self.h));
        old
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: Color, cov: u8) {
        if !self.clip.contains(x, y) {
            return;
        }
        let i = (y * self.w + x) as usize;
        self.buf[i] = blend_px(self.buf[i], c, cov as u32);
    }

    pub fn fill(&mut self, r: IRect, c: Color) {
        if c.a == 0 {
            return;
        }
        let r = r.intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        if c.a == 255 {
            let v = 0xff00_0000 | (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32;
            for y in r.y..r.bottom() {
                let row = (y * self.w) as usize;
                self.buf[row + r.x as usize..row + r.right() as usize].fill(v);
            }
            return;
        }
        for y in r.y..r.bottom() {
            let row = (y * self.w) as usize;
            for p in &mut self.buf[row + r.x as usize..row + r.right() as usize] {
                *p = blend_px(*p, c, 255);
            }
        }
    }

    /// Replace (not blend) a region, e.g. to punch transparent holes.
    pub fn set(&mut self, r: IRect, c: Color) {
        let r = r.intersect(&self.clip);
        let a = c.a as u32;
        let v = a << 24 | (c.r as u32 * a / 255) << 16 | (c.g as u32 * a / 255) << 8 | (c.b as u32 * a / 255);
        for y in r.y..r.bottom() {
            let row = (y * self.w) as usize;
            self.buf[row + r.x as usize..row + r.right() as usize].fill(v);
        }
    }

    pub fn hline(&mut self, x: i32, y: i32, w: i32, t: i32, c: Color) {
        self.fill(IRect::new(x, y, w, t), c);
    }

    pub fn vline(&mut self, x: i32, y: i32, h: i32, t: i32, c: Color) {
        self.fill(IRect::new(x, y, t, h), c);
    }

    pub fn stroke(&mut self, r: IRect, t: i32, c: Color) {
        self.hline(r.x, r.y, r.w, t, c);
        self.hline(r.x, r.bottom() - t, r.w, t, c);
        self.vline(r.x, r.y + t, r.h - 2 * t, t, c);
        self.vline(r.right() - t, r.y + t, r.h - 2 * t, t, c);
    }

    /// Filled rect with pixel-notched corners (corner cut of `n` px).
    pub fn notched(&mut self, r: IRect, n: i32, c: Color) {
        if r.h <= 2 * n || r.w <= 2 * n {
            self.fill(r, c);
            return;
        }
        self.fill(IRect::new(r.x + n, r.y, r.w - 2 * n, n), c);
        self.fill(IRect::new(r.x, r.y + n, r.w, r.h - 2 * n), c);
        self.fill(IRect::new(r.x + n, r.bottom() - n, r.w - 2 * n, n), c);
    }

    /// Outline with notched corners.
    pub fn notched_stroke(&mut self, r: IRect, n: i32, t: i32, c: Color) {
        self.hline(r.x + n, r.y, r.w - 2 * n, t, c);
        self.hline(r.x + n, r.bottom() - t, r.w - 2 * n, t, c);
        self.vline(r.x, r.y + n, r.h - 2 * n, t, c);
        self.vline(r.right() - t, r.y + n, r.h - 2 * n, t, c);
    }

    /// Hard offset shadow (no blur), drawn behind `r`.
    pub fn hard_shadow(&mut self, r: IRect, off: i32, c: Color) {
        self.fill(IRect::new(r.x + off, r.bottom(), r.w, off), c);
        self.fill(IRect::new(r.right(), r.y + off, off, r.h), c);
    }

    /// Ordered-dither fill: `level` in 0..=16 is the density; `cell` is the dither pixel size.
    pub fn dither(&mut self, r: IRect, c: Color, level: u8, cell: i32) {
        let r = r.intersect(&self.clip);
        let cell = cell.max(1);
        for y in r.y..r.bottom() {
            let by = ((y / cell) & 3) as usize;
            for x in r.x..r.right() {
                let bx = ((x / cell) & 3) as usize;
                if BAYER4[by][bx] < level {
                    let i = (y * self.w + x) as usize;
                    self.buf[i] = blend_px(self.buf[i], c, 255);
                }
            }
        }
    }

    /// Draw an 8-bit coverage bitmap at (x, y) tinted with `c`.
    pub fn coverage(&mut self, x: i32, y: i32, w: usize, h: usize, cov: &[u8], c: Color) {
        let r = IRect::new(x, y, w as i32, h as i32).intersect(&self.clip);
        if r.w <= 0 || r.h <= 0 {
            return;
        }
        for yy in r.y..r.bottom() {
            let sy = (yy - y) as usize;
            let row = (yy * self.w) as usize;
            for xx in r.x..r.right() {
                let sx = (xx - x) as usize;
                let a = cov[sy * w + sx];
                if a != 0 {
                    let i = row + xx as usize;
                    self.buf[i] = blend_px(self.buf[i], c, a as u32);
                }
            }
        }
    }

    /// Antialiased filled circle.
    pub fn circle(&mut self, cx: f32, cy: f32, rad: f32, c: Color) {
        let x0 = (cx - rad - 1.0).floor() as i32;
        let x1 = (cx + rad + 1.0).ceil() as i32;
        let y0 = (cy - rad - 1.0).floor() as i32;
        let y1 = (cy + rad + 1.0).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (rad - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// Antialiased quarter-ring (for rounded box-drawing corners).
    /// `quadrant`: which quadrant of the circle around (cx, cy) to draw: 0=TL 1=TR 2=BR 3=BL.
    pub fn arc(&mut self, cx: f32, cy: f32, rad: f32, thick: f32, quadrant: u8, c: Color, clip: IRect) {
        let old = self.set_clip(clip.intersect(&self.clip));
        let x0 = (cx - rad - thick).floor() as i32;
        let x1 = (cx + rad + thick).ceil() as i32;
        let y0 = (cy - rad - thick).floor() as i32;
        let y1 = (cy + rad + thick).ceil() as i32;
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let ok = match quadrant {
                    0 => dx <= 0.5 && dy <= 0.5,
                    1 => dx >= -0.5 && dy <= 0.5,
                    2 => dx >= -0.5 && dy >= -0.5,
                    _ => dx <= 0.5 && dy >= -0.5,
                };
                if !ok {
                    continue;
                }
                let d = (dx * dx + dy * dy).sqrt();
                let cov = (thick / 2.0 - (d - rad).abs() + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, (cov * 255.0) as u8);
                }
            }
        }
        self.clip = old;
    }

    /// Antialiased line segment of thickness `t`.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, t: f32, c: Color) {
        let minx = x0.min(x1) - t;
        let maxx = x0.max(x1) + t;
        let miny = y0.min(y1) - t;
        let maxy = y0.max(y1) + t;
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        for y in miny.floor() as i32..maxy.ceil() as i32 {
            for x in minx.floor() as i32..maxx.ceil() as i32 {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let u = (((px - x0) * dx + (py - y0) * dy) / len2).clamp(0.0, 1.0);
                let qx = x0 + u * dx - px;
                let qy = y0 + u * dy - py;
                let d = (qx * qx + qy * qy).sqrt();
                let cov = (t / 2.0 - d + 0.5).clamp(0.0, 1.0);
                if cov > 0.0 {
                    self.blend(x, y, c, (cov * 255.0) as u8);
                }
            }
        }
    }

    /// Darken every other logical pixel row (CRT scanlines).
    pub fn scanlines(&mut self, r: IRect, strength: f32) {
        let u = self.unit();
        let r = r.intersect(&self.clip);
        let k = ((1.0 - strength.clamp(0.0, 1.0)) * 256.0) as u32;
        for y in r.y..r.bottom() {
            if (y / u) % 2 == 0 {
                continue;
            }
            let row = (y * self.w) as usize;
            for p in &mut self.buf[row + r.x as usize..row + r.right() as usize] {
                let v = *p;
                let a = v & 0xff00_0000;
                let rr = ((v >> 16 & 0xff) * k) >> 8;
                let gg = ((v >> 8 & 0xff) * k) >> 8;
                let bb = ((v & 0xff) * k) >> 8;
                *p = a | rr << 16 | gg << 8 | bb;
            }
        }
    }

    /// Zero pixels whose (cell-quantised) threshold says "not yet visible".
    /// `progress` 0..=1. `threshold(cx, cy) -> 0..1`.
    pub fn dissolve(&mut self, r: IRect, cell: i32, progress: f32, threshold: impl Fn(i32, i32) -> f32) {
        if progress >= 1.0 {
            return;
        }
        let r = r.intersect(&IRect::new(0, 0, self.w, self.h));
        let cell = cell.max(1);
        for y in r.y..r.bottom() {
            let cy = (y - r.y) / cell;
            let row = (y * self.w) as usize;
            let mut x = r.x;
            while x < r.right() {
                let cx = (x - r.x) / cell;
                let run_end = (r.x + (cx + 1) * cell).min(r.right());
                if threshold(cx, cy) >= progress {
                    self.buf[row + x as usize..row + run_end as usize].fill(0);
                }
                x = run_end;
            }
        }
    }
}
