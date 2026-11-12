//! Tiny pixel-art icons drawn on the logical pixel grid (each "pixel" is one
//! point, i.e. 2×2 physical pixels on Retina). Part of the pixel illusion: the
//! icons are crisp pixel art, the text next to them is smooth.

use super::canvas::{Canvas, Color, IRect};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Square,
    Claude,
    Orca,
    Warn,
    Plus,
    Close,
    Check,
    Dot,
    Ring,
    Bolt,
    Gear,
    Search,
    Folder,
    Arrow,
    Bell,
    Spinner(u8),
}

#[rustfmt::skip]
const fn bmp(icon: Icon) -> &'static [&'static str; 7] {
    match icon {
        Icon::Square => &[
            ".......",
            ".#####.",
            ".#####.",
            ".#####.",
            ".#####.",
            ".#####.",
            ".......",
        ],
        Icon::Claude => &[
            "...#...",
            ".#.#.#.",
            "..###..",
            "#######",
            "..###..",
            ".#.#.#.",
            "...#...",
        ],
        Icon::Orca => &[
            "...#...",
            "..###..",
            ".#####.",
            "#######",
            ".#####.",
            "..###..",
            "...#...",
        ],
        Icon::Warn => &[
            "...#...",
            "..#.#..",
            "..#.#..",
            ".#.#.#.",
            ".#...#.",
            "#..#..#",
            "#######",
        ],
        Icon::Plus => &[
            ".......",
            "...#...",
            "...#...",
            ".#####.",
            "...#...",
            "...#...",
            ".......",
        ],
        Icon::Close => &[
            ".......",
            ".#...#.",
            "..#.#..",
            "...#...",
            "..#.#..",
            ".#...#.",
            ".......",
        ],
        Icon::Check => &[
            ".......",
            "......#",
            ".....#.",
            "#...#..",
            ".#.#...",
            "..#....",
            ".......",
        ],
        Icon::Dot => &[
            ".......",
            ".......",
            "..###..",
            "..###..",
            "..###..",
            ".......",
            ".......",
        ],
        Icon::Ring => &[
            ".......",
            "..###..",
            ".#...#.",
            ".#...#.",
            ".#...#.",
            "..###..",
            ".......",
        ],
        Icon::Bolt => &[
            "....##.",
            "...##..",
            "..##...",
            ".#####.",
            "...##..",
            "..##...",
            ".##....",
        ],
        Icon::Gear => &[
            "...#...",
            ".#####.",
            ".##.##.",
            "###.###",
            ".##.##.",
            ".#####.",
            "...#...",
        ],
        Icon::Search => &[
            ".###...",
            "#...#..",
            "#...#..",
            "#...#..",
            ".####..",
            "....##.",
            ".....##",
        ],
        Icon::Folder => &[
            ".......",
            "###....",
            "#######",
            "#.....#",
            "#.....#",
            "#######",
            ".......",
        ],
        Icon::Arrow => &[
            ".......",
            "...#...",
            "....#..",
            "#######",
            "....#..",
            "...#...",
            ".......",
        ],
        Icon::Bell => &[
            "...#...",
            "..###..",
            ".#####.",
            ".#####.",
            "#######",
            ".......",
            "...#...",
        ],
        Icon::Spinner(f) => match f % 4 {
            0 => &[".......", ".......", ".......", ".......", "###....", "###....", "###...."],
            1 => &["###....", "###....", "###....", ".......", ".......", ".......", "......."],
            2 => &["....###", "....###", "....###", ".......", ".......", ".......", "......."],
            _ => &[".......", ".......", ".......", ".......", "....###", "....###", "....###"],
        },
    }
}

/// Size of an icon in physical pixels for the given canvas.
pub fn size(cv: &Canvas) -> i32 {
    7 * cv.unit()
}

/// Draw an icon with its top-left at (x, y). Returns its width.
pub fn draw(cv: &mut Canvas, icon: Icon, x: i32, y: i32, c: Color) -> i32 {
    let u = cv.unit();
    let rows = bmp(icon);
    for (ry, row) in rows.iter().enumerate() {
        for (rx, ch) in row.bytes().enumerate() {
            if ch == b'#' {
                cv.fill(IRect::new(x + rx as i32 * u, y + ry as i32 * u, u, u), c);
            }
        }
    }
    7 * u
}

/// Draw an icon vertically centred in a box of height `h` starting at `top`.
pub fn draw_centered(cv: &mut Canvas, icon: Icon, x: i32, top: i32, h: i32, c: Color) -> i32 {
    let s = size(cv);
    draw(cv, icon, x, top + (h - s) / 2, c)
}
