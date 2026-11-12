//! Themes: graphite surfaces + one accent, derived exactly like the design board
//! (see design/DESIGN.md). Custom themes live in ~/.config/fluteterm/themes/*.toml.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn hex(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches('#');
        let v = match s.len() {
            6 => u32::from_str_radix(s, 16).ok()?,
            3 => {
                let v = u32::from_str_radix(s, 16).ok()?;
                let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
                (r * 17) << 16 | (g * 17) << 8 | (b * 17)
            }
            _ => return None,
        };
        Some(Self::new((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    pub fn mix(self, o: Rgb, t: f32) -> Rgb {
        let l = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8;
        Rgb::new(l(self.r, o.r), l(self.g, o.g), l(self.b, o.b))
    }

    pub fn luma(self) -> f32 {
        (0.2126 * self.r as f32 + 0.7152 * self.g as f32 + 0.0722 * self.b as f32) / 255.0
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

#[derive(Debug, Clone)]
pub struct Palette {
    pub name: String,
    pub light: bool,
    pub bg: Rgb,
    pub panel: Rgb,
    pub surface: Rgb,
    pub surface_hi: Rgb,
    pub line: Rgb,
    pub text: Rgb,
    pub dim: Rgb,
    pub faint: Rgb,
    pub accent: Rgb,
    pub on_accent: Rgb,
    pub ok: Rgb,
    pub warn: Rgb,
    pub err: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
    pub ansi: [Rgb; 16],
}

pub const PRESETS: &[(&str, &str)] = &[
    ("graphite", "#a8a29c"),
    ("clay", "#d9643a"),
    ("moss", "#7fb05a"),
    ("glacier", "#5aa3c8"),
    ("orchid", "#a67be0"),
    ("blood", "#c4161c"),
    ("paper", "#3a3533"),
];

fn h(s: &str) -> Rgb {
    Rgb::hex(s).unwrap()
}

pub fn derive(name: &str, accent: Rgb, light: bool, tint: f32) -> Palette {
    let on_accent = if accent.luma() > 0.6 { h("#0b0909") } else { h("#ffffff") };
    if light {
        let text = h("#141010");
        let ansi = [
            h("#3a3333"), h("#b02a22"), h("#2e8b3a"), h("#9a6a00"),
            h("#2f5f94"), h("#7a3f82"), h("#2b7a76"), h("#d8d0ca"),
            h("#6a6060"), h("#d0352d"), h("#3c9e4a"), h("#b88200"),
            h("#4277b0"), h("#9a5aa0"), h("#3b908b"), h("#f4f0ed"),
        ];
        return Palette {
            name: name.into(),
            light,
            bg: h("#f4f0ed").mix(accent, 0.04 * tint),
            panel: h("#ebe6e2").mix(accent, 0.06 * tint),
            surface: h("#ddd6d1").mix(accent, 0.08 * tint),
            surface_hi: h("#d0c8c2").mix(accent, 0.12 * tint),
            line: h("#c4bbb5").mix(accent, 0.10 * tint),
            text,
            dim: h("#4a4242"),
            faint: h("#776e6b"),
            accent,
            on_accent,
            ok: h("#2e8b3a"),
            warn: h("#b86a00"),
            err: h("#c4161c"),
            cursor: text,
            selection: accent.mix(h("#ebe6e2"), 0.55),
            ansi,
        };
    }
    let text = h("#f4f0ee").mix(accent, 0.04);
    let ansi = [
        h("#2a2424"), h("#d0352d"), h("#78b86a"), h("#d9a03a"),
        h("#5a7fa8"), h("#9a5aa0"), h("#4f9a96"), h("#bdb5b3"),
        h("#6e6463"), h("#ff5a4c"), h("#a2d692"), h("#f0c46a"),
        h("#86a6c8"), h("#c486c8"), h("#7cc2bc"), h("#f4f0ee"),
    ];
    Palette {
        name: name.into(),
        light,
        bg: h("#080707").mix(accent, 0.035 * tint),
        panel: h("#0e0c0c").mix(accent, 0.055 * tint),
        surface: h("#161313").mix(accent, 0.08 * tint),
        surface_hi: h("#1f1b1b").mix(accent, 0.12 * tint),
        line: h("#3a3333").mix(accent, 0.14 * tint),
        text,
        dim: h("#bdb5b3").mix(accent, 0.10),
        faint: h("#8f8583").mix(accent, 0.10),
        accent,
        on_accent,
        ok: h("#78c878"),
        warn: h("#ffaa28"),
        err: h("#ff5a4c"),
        cursor: text,
        selection: accent.mix(h("#0e0c0c"), 0.55),
        ansi,
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ThemeFile {
    theme: ThemeHeader,
    colors: BTreeMap<String, String>,
    ansi: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(default)]
struct ThemeHeader {
    name: Option<String>,
    mode: String,
    accent: String,
    tint: f32,
}

impl Default for ThemeHeader {
    fn default() -> Self {
        Self { name: None, mode: "dark".into(), accent: "#a8a29c".into(), tint: 1.0 }
    }
}

const ANSI_NAMES: [&str; 16] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    "bright_black", "bright_red", "bright_green", "bright_yellow",
    "bright_blue", "bright_magenta", "bright_cyan", "bright_white",
];

/// Resolve a theme by name: user file first, then built-in preset. Also accepts "#rrggbb".
/// Curated accent colours offered by settings and the palette (all optional).
pub const ACCENTS: &[(&str, &str)] = &[
    ("amber", "#e0a030"),
    ("ember", "#d9643a"),
    ("rose", "#e0607e"),
    ("crimson", "#c4161c"),
    ("violet", "#a67be0"),
    ("indigo", "#7a86e8"),
    ("sky", "#5aa3c8"),
    ("teal", "#3fb8a8"),
    ("mint", "#78c8a0"),
    ("lime", "#a8d050"),
    ("sand", "#c8b48a"),
    ("ash", "#a8a29c"),
];

/// Load a theme, optionally overriding its accent. Greys keep the theme's own
/// derivation; everything accent-coloured follows the override.
pub fn load_with(name: &str, accent: Option<&str>) -> (Palette, Option<String>) {
    let (mut p, err) = load(name);
    let Some(hex) = accent.filter(|s| !s.trim().is_empty()) else { return (p, err) };
    match Rgb::hex(hex) {
        Some(a) => {
            p.accent = a;
            p.on_accent = if a.luma() > 0.6 { h("#0b0909") } else { h("#ffffff") };
            p.selection = a.mix(p.panel, 0.55);
            (p, err)
        }
        None => (p, Some(format!("accent \"{hex}\" is not a #rrggbb colour"))),
    }
}

pub fn load(name: &str) -> (Palette, Option<String>) {
    let path = crate::config::themes_dir().join(format!("{name}.toml"));
    if path.exists() {
        match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|s| {
            toml::from_str::<ThemeFile>(&s).map_err(|e| e.to_string())
        }) {
            Ok(tf) => return (from_file(name, tf), None),
            Err(e) => {
                return (preset("graphite"), Some(format!("theme {name}: {e}")));
            }
        }
    }
    if let Some(c) = Rgb::hex(name) {
        return (derive(name, c, false, 1.0), None);
    }
    if PRESETS.iter().any(|(n, _)| *n == name) {
        return (preset(name), None);
    }
    (preset("graphite"), Some(format!("unknown theme \"{name}\"")))
}

pub fn preset(name: &str) -> Palette {
    let accent = PRESETS.iter().find(|(n, _)| *n == name).map(|(_, a)| *a).unwrap_or("#a8a29c");
    derive(name, h(accent), name == "paper", 1.0)
}

fn from_file(name: &str, tf: ThemeFile) -> Palette {
    let accent = Rgb::hex(&tf.theme.accent).unwrap_or(h("#a8a29c"));
    let mut p = derive(
        tf.theme.name.as_deref().unwrap_or(name),
        accent,
        tf.theme.mode.eq_ignore_ascii_case("light"),
        tf.theme.tint,
    );
    for (k, v) in &tf.colors {
        let Some(c) = Rgb::hex(v) else { continue };
        match k.as_str() {
            "bg" => p.bg = c,
            "panel" => p.panel = c,
            "surface" => p.surface = c,
            "surface_hi" | "surfaceHi" => p.surface_hi = c,
            "line" => p.line = c,
            "text" => p.text = c,
            "dim" => p.dim = c,
            "faint" => p.faint = c,
            "accent" => p.accent = c,
            "on_accent" | "onAccent" => p.on_accent = c,
            "ok" => p.ok = c,
            "warn" => p.warn = c,
            "err" => p.err = c,
            "cursor" => p.cursor = c,
            "selection" => p.selection = c,
            _ => {}
        }
    }
    for (k, v) in &tf.ansi {
        if let (Some(i), Some(c)) = (ANSI_NAMES.iter().position(|n| n == k), Rgb::hex(v)) {
            p.ansi[i] = c;
        }
    }
    p
}

/// All theme names: presets + user files.
pub fn list() -> Vec<String> {
    let mut v: Vec<String> = PRESETS.iter().map(|(n, _)| n.to_string()).collect();
    if let Ok(rd) = std::fs::read_dir(crate::config::themes_dir()) {
        let mut user: Vec<String> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension()? != "toml" {
                    return None;
                }
                Some(p.file_stem()?.to_string_lossy().into_owned())
            })
            .filter(|n| n != "example")
            .collect();
        user.sort();
        v.extend(user);
    }
    v
}

pub const EXAMPLE_THEME: &str = r##"# Copy this file to <name>.toml and set `theme = "<name>"` in config.toml.
# A theme is one accent colour; every other tone is derived from it.

[theme]
name   = "example"
mode   = "dark"        # or "light"
accent = "#d9643a"
tint   = 1.0           # how much of the accent bleeds into the greys

# Optional overrides of derived tones:
[colors]
# bg = "#0a0808"
# panel = "#100c0b"
# text = "#f4f0ee"
# dim = "#bdb5b3"
# faint = "#8f8583"
# ok = "#78c878"
# warn = "#ffaa28"
# err = "#ff5a4c"

# Optional: the 16 ANSI colours.
[ansi]
# red = "#d0352d"
# green = "#78b86a"
# bright_blue = "#86a6c8"
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_mix() {
        assert_eq!(Rgb::hex("#a8a29c"), Some(Rgb::new(0xa8, 0xa2, 0x9c)));
        assert_eq!(Rgb::hex("fff"), Some(Rgb::new(255, 255, 255)));
        assert_eq!(Rgb::hex("nope"), None);
        assert_eq!(Rgb::new(0, 0, 0).mix(Rgb::new(255, 255, 255), 0.5), Rgb::new(128, 128, 128));
    }

    #[test]
    fn presets_are_readable() {
        for (name, _) in PRESETS {
            let p = preset(name);
            // Secondary text must keep contrast against the panel (design rule).
            let d = (p.faint.luma() - p.panel.luma()).abs();
            assert!(d > 0.3, "{name}: faint too close to panel ({d})");
        }
    }
}
