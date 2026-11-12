//! Built-in commands (tabs, look & feel, config) and the shell status segments.

use super::*;
use crate::util;

pub struct Core;

impl Extension for Core {
    fn id(&self) -> &'static str {
        "core"
    }

    fn status_left(&self, tab: &TabInfo, _ctx: &Ctx) -> Vec<Segment> {
        if matches!(tab.kind, TabKind::Claude) {
            return vec![];
        }
        let mut v = vec![];
        let fg = tab.fg.clone().unwrap_or_else(|| "shell".into());
        let fg = fg.trim_start_matches('-').to_string();
        v.push(Segment::text(fg, Tone::Text).icon(match tab.kind {
            TabKind::Orca => Icon::Orca,
            _ => Icon::Square,
        }));
        if let Some(cwd) = &tab.cwd {
            v.push(Segment::text(util::tildify(cwd), Tone::Dim));
            if let Some(b) = util::git_branch(cwd) {
                v.push(Segment::text(format!("⎇ {b}"), Tone::Dim));
            }
        }
        v
    }

    fn status_right(&self, _ctx: &Ctx) -> Vec<Segment> {
        let (h, m) = util::local_hm();
        vec![Segment::text(format!("{h:02}:{m:02}"), Tone::Dim)]
    }

    fn commands(&self, ctx: &Ctx) -> Vec<Command> {
        use PaletteMode::*;
        let mut v = vec![
            Command::new(All, Icon::Square, "new tab", "shell in the current directory", Action::NewTab(TabSpec::shell(ctx.active_cwd()))).keys("⌘T"),
            Command::new(All, Icon::Close, "close tab", "", Action::CloseTab(None)).keys("⌘W"),
            Command::new(All, Icon::Arrow, "next tab", "", Action::NextTab(1)).keys("⌘⇧]"),
            Command::new(All, Icon::Arrow, "previous tab", "", Action::NextTab(-1)).keys("⌘⇧["),
            Command::new(All, Icon::Gear, "settings", "look, fonts, keys, window, notifications, extensions", Action::OpenSettings).keys("⌘,"),
            Command::new(All, Icon::Gear, "edit config file", "~/.config/fluteterm/config.toml in a new tab", Action::EditConfig),
            Command::new(All, Icon::Gear, "open config in editor app", "", Action::OpenConfig),
            Command::new(All, Icon::Gear, "reload config", "", Action::ReloadConfig),
            Command::new(All, Icon::Plus, "bigger text", "", Action::FontSize(1.0)).keys("⌘+"),
            Command::new(All, Icon::Plus, "smaller text", "", Action::FontSize(-1.0)).keys("⌘-"),
            Command::new(All, Icon::Plus, "reset text size", "", Action::ResetFontSize).keys("⌘0"),
            Command::new(Themes, Icon::Square, "shape: strip", "full-width under the menu bar", Action::SetShape(Shape::Strip)),
            Command::new(Themes, Icon::Square, "shape: centred", "centred panel", Action::SetShape(Shape::Centred)),
            Command::new(Themes, Icon::Square, "shape: floating", "floating panel with a gap and a hard shadow", Action::SetShape(Shape::Floating)),
            Command::new(Themes, Icon::Dot, "motion: bayer", "ordered-dither dissolve", Action::SetPattern(Pattern::Bayer)),
            Command::new(Themes, Icon::Dot, "motion: scanlines", "", Action::SetPattern(Pattern::Scan)),
            Command::new(Themes, Icon::Dot, "motion: blocks", "", Action::SetPattern(Pattern::Blocks)),
            Command::new(Themes, Icon::Dot, "motion: noise", "", Action::SetPattern(Pattern::Noise)),
            Command::new(Themes, Icon::Dot, "motion: none", "", Action::SetPattern(Pattern::None)),
            Command::new(Themes, Icon::Ring, "toggle crt scanlines", "", Action::ToggleCrt),
            Command::new(Themes, Icon::Ring, "more opaque", "", Action::AdjustOpacity(0.05)),
            Command::new(Themes, Icon::Ring, "more transparent", "", Action::AdjustOpacity(-0.05)),
            Command::new(Themes, Icon::Ring, "more blur", "", Action::AdjustBlur(4)),
            Command::new(Themes, Icon::Ring, "less blur", "", Action::AdjustBlur(-4)),
            Command::new(All, Icon::Bell, "do not disturb: on", "", Action::SetDnd(true)),
            Command::new(All, Icon::Bell, "do not disturb: off", "", Action::SetDnd(false)),
            Command::new(All, Icon::Close, "hide", "", Action::Hide).keys("F12"),
            Command::new(All, Icon::Close, "quit fluteterm", "", Action::Quit),
        ];
        let custom = ctx.cfg.accent.is_some();
        v.push(Command::new(Themes, Icon::Ring, "accent: theme default", "use the theme's own accent", Action::SetAccent(None)).right(if custom { "" } else { "current" }, Tone::Accent));
        for (name, hex) in crate::theme::ACCENTS {
            let current = ctx.cfg.accent.as_deref().map(|a| a.eq_ignore_ascii_case(hex)).unwrap_or(false);
            v.push(Command::new(Themes, Icon::Ring, format!("accent: {name}"), *hex, Action::SetAccent(Some(hex.to_string()))).right(if current { "current" } else { "" }, Tone::Accent));
        }
        // Claude Code hooks: removable any time; installable when Claude Code exists.
        if crate::hooks::installed() {
            v.push(Command::new(All, Icon::Close, "claude: remove hooks", "delete fluteterm's entries from ~/.claude/settings.json (backed up)", Action::ClaudeHooks(false)));
            v.push(Command::new(All, Icon::Claude, "claude: reinstall hooks", "refresh fluteterm's hook entries", Action::ClaudeHooks(true)));
        } else if util::which(&ctx.cfg.claude.command).is_some() {
            v.push(Command::new(All, Icon::Claude, "claude: install hooks", "live state, notifications & usage for Claude tabs", Action::ClaudeHooks(true)));
        }
        if ctx.cfg.orca.enabled && util::which(&ctx.cfg.orca.command).is_some() {
            v.push(Command::new(All, Icon::Orca, "orca worktrees", "browse worktrees & agents", Action::OpenPalette(Orca)).keys("⌘⇧O"));
        }
        for t in crate::theme::list() {
            let current = t == ctx.cfg.theme;
            v.push(
                Command::new(Themes, Icon::Square, format!("theme: {t}"), "", Action::SetTheme(t))
                    .right(if current { "current" } else { "" }, Tone::Accent),
            );
        }
        for t in ctx.tabs {
            if Some(t.id) == ctx.active {
                continue;
            }
            let place = t.cwd.as_deref().map(util::tildify).unwrap_or_default();
            v.push(Command::new(All, Icon::Arrow, format!("go to tab: {}", t.title), place, Action::FocusTab(t.id)));
        }
        v
    }
}
