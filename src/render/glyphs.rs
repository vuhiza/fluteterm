//! Procedurally drawn glyphs: box drawing (seamless across cells), block
//! elements, shades (rendered as ordered dither — part of the pixel illusion),
//! braille, and a few symbols Claude Code uses heavily.

use super::canvas::{Canvas, Color, IRect};

/// Box-drawing table for U+2500..=U+257F: [up, right, down, left], 0 none, 1 light, 2 heavy, 3 double.
/// 9 = special (rounded corners / diagonals).
#[rustfmt::skip]
const BOX: [[u8; 4]; 128] = [
    [0,1,0,1],[0,2,0,2],[1,0,1,0],[2,0,2,0],[0,1,0,1],[0,2,0,2],[1,0,1,0],[2,0,2,0], // 2500-2507
    [0,1,0,1],[0,2,0,2],[1,0,1,0],[2,0,2,0],[0,1,1,0],[0,2,1,0],[0,1,2,0],[0,2,2,0], // 2508-250F
    [0,0,1,1],[0,0,1,2],[0,0,2,1],[0,0,2,2],[1,1,0,0],[1,2,0,0],[2,1,0,0],[2,2,0,0], // 2510-2517
    [1,0,0,1],[1,0,0,2],[2,0,0,1],[2,0,0,2],[1,1,1,0],[1,2,1,0],[2,1,1,0],[1,1,2,0], // 2518-251F
    [2,1,2,0],[2,2,1,0],[1,2,2,0],[2,2,2,0],[1,0,1,1],[1,0,1,2],[2,0,1,1],[1,0,2,1], // 2520-2527
    [2,0,2,1],[2,0,1,2],[1,0,2,2],[2,0,2,2],[0,1,1,1],[0,1,1,2],[0,2,1,1],[0,2,1,2], // 2528-252F
    [0,1,2,1],[0,1,2,2],[0,2,2,1],[0,2,2,2],[1,1,0,1],[1,1,0,2],[1,2,0,1],[1,2,0,2], // 2530-2537
    [2,1,0,1],[2,1,0,2],[2,2,0,1],[2,2,0,2],[1,1,1,1],[1,1,1,2],[1,2,1,1],[1,2,1,2], // 2538-253F
    [2,1,1,1],[1,1,2,1],[2,1,2,1],[2,1,1,2],[2,2,1,1],[1,1,2,2],[1,2,2,1],[2,2,1,2], // 2540-2547
    [1,2,2,2],[2,1,2,2],[2,2,2,1],[2,2,2,2],[0,1,0,1],[0,2,0,2],[1,0,1,0],[2,0,2,0], // 2548-254F
    [0,3,0,3],[3,0,3,0],[0,3,1,0],[0,1,3,0],[0,3,3,0],[0,0,1,3],[0,0,3,1],[0,0,3,3], // 2550-2557
    [1,3,0,0],[3,1,0,0],[3,3,0,0],[1,0,0,3],[3,0,0,1],[3,0,0,3],[1,3,1,0],[3,1,3,0], // 2558-255F
    [3,3,3,0],[1,0,1,3],[3,0,3,1],[3,0,3,3],[0,3,1,3],[0,1,3,1],[0,3,3,3],[1,3,0,3], // 2560-2567
    [3,1,0,1],[3,3,0,3],[1,3,1,3],[3,1,3,1],[3,3,3,3],[9,9,9,9],[9,9,9,9],[9,9,9,9], // 2568-256F
    [9,9,9,9],[9,9,9,9],[9,9,9,9],[9,9,9,9],[0,0,0,1],[1,0,0,0],[0,1,0,0],[0,0,1,0], // 2570-2577
    [0,0,0,2],[2,0,0,0],[0,2,0,0],[0,0,2,0],[0,2,0,1],[1,0,2,0],[0,1,0,2],[2,0,1,0], // 2578-257F
];

/// Returns true if the character was drawn procedurally.
pub fn draw(cv: &mut Canvas, ch: char, cell: IRect, c: Color) -> bool {
    let cp = ch as u32;
    match cp {
        0x2500..=0x257F => {
            box_char(cv, cp, cell, c);
            true
        }
        0x2580..=0x259F => {
            block(cv, cp, cell, c);
            true
        }
        0x2800..=0x28FF => {
            braille(cv, cp - 0x2800, cell, c);
            true
        }
        // ⏺ (Claude's bullet) and ● — a crisp disc.
        0x23FA | 0x25CF => {
            let r = cell.w.min(cell.h) as f32 * 0.32;
            cv.circle(cell.x as f32 + cell.w as f32 / 2.0, cell.y as f32 + cell.h as f32 / 2.0, r, c);
            true
        }
        // ⎿ (Claude's result connector): left vertical + bottom stroke.
        0x23BF => {
            let t = cv.unit();
            let x = cell.x + cell.w / 3;
            let y = cell.y + cell.h / 2;
            cv.vline(x, cell.y, y - cell.y + t, t, c);
            cv.hline(x, y, cell.right() - x, t, c);
            true
        }
        // ⏵ ⏴ ⏶ ⏷ ▶ ◀ ▲ ▼ ► ◄ — crisp triangles (Claude Code uses ⏵⏵).
        0x23F4..=0x23F7 | 0x25B6 | 0x25C0 | 0x25B2 | 0x25BC | 0x25BA | 0x25C4 => {
            let small = (0x23F4..=0x23F7).contains(&cp);
            let s = if small { cell.w as f32 * 0.62 } else { cell.w as f32 * 0.8 };
            let cx = cell.x as f32 + cell.w as f32 / 2.0;
            let cy = cell.y as f32 + cell.h as f32 / 2.0;
            let h = s / 2.0;
            let dir = match cp {
                0x23F5 | 0x25B6 | 0x25BA => 0, // right
                0x23F4 | 0x25C0 | 0x25C4 => 1, // left
                0x23F6 | 0x25B2 => 2,          // up
                _ => 3,                        // down
            };
            let y0 = (cy - h).floor() as i32;
            let y1 = (cy + h).ceil() as i32;
            let x0 = (cx - h).floor() as i32;
            let x1 = (cx + h).ceil() as i32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let fx = (x as f32 + 0.5 - cx) / h; // -1..1
                    let fy = (y as f32 + 0.5 - cy) / h;
                    let inside = match dir {
                        0 => fx >= -1.0 && fy.abs() <= (1.0 - fx) / 2.0,
                        1 => fx <= 1.0 && fy.abs() <= (fx + 1.0) / 2.0,
                        2 => fy <= 1.0 && fx.abs() <= (fy + 1.0) / 2.0,
                        _ => fy >= -1.0 && fx.abs() <= (1.0 - fy) / 2.0,
                    };
                    if inside {
                        cv.blend(x, y, c, 255);
                    }
                }
            }
            true
        }
        // Claude Code's spinner stars: ✢ ✳ ✶ ✻ ✽ (drawn so no fallback font is needed).
        0x2722 | 0x2733 | 0x2736 | 0x273B | 0x273D => {
            let (spokes, heavy) = match cp {
                0x2722 => (4, false),
                0x2733 => (8, false),
                0x2736 => (6, true),
                0x273B => (6, false),
                _ => (8, true),
            };
            let cx = cell.x as f32 + cell.w as f32 / 2.0;
            let cy = cell.y as f32 + cell.h as f32 / 2.0;
            let r = cell.w as f32 * 0.46;
            let t = cv.unit() as f32 * if heavy { 1.6 } else { 1.1 };
            for i in 0..spokes {
                let a = std::f32::consts::PI * 2.0 * i as f32 / spokes as f32 - std::f32::consts::FRAC_PI_2;
                cv.line(cx, cy, cx + a.cos() * r, cy + a.sin() * r, t, c);
            }
            true
        }
        // ▪ ■ — square bullets, snapped to the pixel grid.
        0x25AA => {
            let u = cv.unit();
            let s = if cp == 0x25A0 { (cell.w * 2 / 3).max(3 * u) } else { (cell.w / 3).max(2 * u) };
            let s = s / u * u;
            cv.fill(IRect::new(cell.x + (cell.w - s) / 2, cell.y + (cell.h - s) / 2, s, s), c);
            true
        }
        _ => false,
    }
}

fn box_char(cv: &mut Canvas, cp: u32, cell: IRect, c: Color) {
    let idx = (cp - 0x2500) as usize;
    let t = cv.unit();
    let heavy = t * 2;
    let cx = cell.x + (cell.w - t) / 2;
    let cy = cell.y + (cell.h - t) / 2;
    let segs = BOX[idx];
    if segs[0] == 9 {
        special(cv, cp, cell, c, t);
        return;
    }
    // Dashed variants: draw as light/heavy but with gaps.
    let dashed = matches!(cp, 0x2504..=0x250B | 0x254C..=0x254F);
    let thick = |w: u8| if w == 2 { heavy } else { t };
    let gap = (t * 2).max(2);
    // Horizontal halves.
    for (side, w) in [(3usize, segs[3]), (1, segs[1])] {
        if w == 0 {
            continue;
        }
        let th = thick(w);
        let y = cell.y + (cell.h - th) / 2;
        let (x0, x1) = if side == 3 { (cell.x, cx + t) } else { (cx, cell.right()) };
        if w == 3 {
            cv.hline(x0, cy - gap, x1 - x0, t, c);
            cv.hline(x0, cy + gap, x1 - x0, t, c);
        } else if dashed {
            let n = 2;
            let seg = (x1 - x0) / n;
            for i in 0..n {
                cv.hline(x0 + i * seg, y, seg - seg / 3, th, c);
            }
        } else {
            cv.hline(x0, y, x1 - x0, th, c);
        }
    }
    for (side, w) in [(0usize, segs[0]), (2, segs[2])] {
        if w == 0 {
            continue;
        }
        let th = thick(w);
        let x = cell.x + (cell.w - th) / 2;
        let (y0, y1) = if side == 0 { (cell.y, cy + t) } else { (cy, cell.bottom()) };
        if w == 3 {
            cv.vline(cx - gap, y0, y1 - y0, t, c);
            cv.vline(cx + gap, y0, y1 - y0, t, c);
        } else if dashed {
            let n = 2;
            let seg = (y1 - y0) / n;
            for i in 0..n {
                cv.vline(x, y0 + i * seg, seg - seg / 3, th, c);
            }
        } else {
            cv.vline(x, y0, y1 - y0, th, c);
        }
    }
}

fn special(cv: &mut Canvas, cp: u32, cell: IRect, c: Color, t: i32) {
    let fx = cell.x as f32;
    let fy = cell.y as f32;
    let w = cell.w as f32;
    let h = cell.h as f32;
    let tf = t as f32;
    let mx = fx + ((cell.w - t) / 2) as f32 + tf / 2.0;
    let my = fy + ((cell.h - t) / 2) as f32 + tf / 2.0;
    let r = (w / 2.0).min(h / 2.0);
    match cp {
        // ╭ ╮ ╯ ╰
        0x256D..=0x2570 => {
            let (dir_x, dir_y): (f32, f32) = match cp {
                0x256D => (1.0, 1.0),   // right + down
                0x256E => (-1.0, 1.0),  // left + down
                0x256F => (-1.0, -1.0), // left + up
                _ => (1.0, -1.0),       // right + up
            };
            let ccx = mx + dir_x * r;
            let ccy = my + dir_y * r;
            let quadrant = match (dir_x > 0.0, dir_y > 0.0) {
                (true, true) => 0,
                (false, true) => 1,
                (false, false) => 2,
                (true, false) => 3,
            };
            cv.arc(ccx, ccy, r, tf, quadrant, c, cell);
            // Straight continuations to the cell edges.
            let ti = t;
            let xi = cell.x + (cell.w - t) / 2;
            let yi = cell.y + (cell.h - t) / 2;
            if dir_x > 0.0 {
                let x0 = ccx.round() as i32;
                cv.hline(x0, yi, cell.right() - x0, ti, c);
            } else {
                let x1 = ccx.round() as i32;
                cv.hline(cell.x, yi, x1 - cell.x, ti, c);
            }
            if dir_y > 0.0 {
                let y0 = ccy.round() as i32;
                cv.vline(xi, y0, cell.bottom() - y0, ti, c);
            } else {
                let y1 = ccy.round() as i32;
                cv.vline(xi, cell.y, y1 - cell.y, ti, c);
            }
        }
        0x2571 => cv.line(fx + w, fy, fx, fy + h, tf, c),
        0x2572 => cv.line(fx, fy, fx + w, fy + h, tf, c),
        _ => {
            cv.line(fx + w, fy, fx, fy + h, tf, c);
            cv.line(fx, fy, fx + w, fy + h, tf, c);
        }
    }
}

fn block(cv: &mut Canvas, cp: u32, cell: IRect, c: Color) {
    let IRect { x, y, w, h } = cell;
    let eighth_h = |n: i32| (h * n + 4) / 8;
    let eighth_w = |n: i32| (w * n + 4) / 8;
    let (hw, hh) = (w / 2, h / 2);
    let quad = |cv: &mut Canvas, q: u8| {
        // bit0 UL, bit1 UR, bit2 LL, bit3 LR
        if q & 1 != 0 {
            cv.fill(IRect::new(x, y, hw, hh), c);
        }
        if q & 2 != 0 {
            cv.fill(IRect::new(x + hw, y, w - hw, hh), c);
        }
        if q & 4 != 0 {
            cv.fill(IRect::new(x, y + hh, hw, h - hh), c);
        }
        if q & 8 != 0 {
            cv.fill(IRect::new(x + hw, y + hh, w - hw, h - hh), c);
        }
    };
    match cp {
        0x2580 => cv.fill(IRect::new(x, y, w, hh), c),
        0x2581..=0x2588 => {
            let n = (cp - 0x2580) as i32;
            let bh = eighth_h(n);
            cv.fill(IRect::new(x, y + h - bh, w, bh), c);
        }
        0x2589..=0x258F => {
            let n = 8 - (cp - 0x2588) as i32;
            cv.fill(IRect::new(x, y, eighth_w(n), h), c);
        }
        0x2590 => cv.fill(IRect::new(x + hw, y, w - hw, h), c),
        // Shades → ordered dither on the logical pixel grid.
        0x2591 => cv.dither(cell, c, 4, cv.unit()),
        0x2592 => cv.dither(cell, c, 8, cv.unit()),
        0x2593 => cv.dither(cell, c, 12, cv.unit()),
        0x2594 => cv.fill(IRect::new(x, y, w, eighth_h(1)), c),
        0x2595 => {
            let bw = eighth_w(1);
            cv.fill(IRect::new(x + w - bw, y, bw, h), c)
        }
        0x2596 => quad(cv, 4),
        0x2597 => quad(cv, 8),
        0x2598 => quad(cv, 1),
        0x2599 => quad(cv, 1 | 4 | 8),
        0x259A => quad(cv, 1 | 8),
        0x259B => quad(cv, 1 | 2 | 4),
        0x259C => quad(cv, 1 | 2 | 8),
        0x259D => quad(cv, 2),
        0x259E => quad(cv, 2 | 4),
        _ => quad(cv, 2 | 4 | 8),
    }
}

fn braille(cv: &mut Canvas, bits: u32, cell: IRect, c: Color) {
    // Dot positions: (col, row)
    const DOTS: [(i32, i32); 8] = [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)];
    let u = cv.unit();
    let dot = (cell.w / 4).max(u) / u * u;
    let dot = dot.max(u);
    let colx = [cell.x + cell.w / 4 - dot / 2, cell.x + cell.w * 3 / 4 - dot / 2];
    for (i, (col, row)) in DOTS.iter().enumerate() {
        if bits & (1 << i) != 0 {
            let yy = cell.y + (cell.h * (2 * row + 1)) / 8 - dot / 2;
            cv.fill(IRect::new(colx[*col as usize], yy, dot, dot), c);
        }
    }
}
