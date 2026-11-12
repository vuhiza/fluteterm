//! Command palette state (⌘K, ⌘⇧O): fuzzy filtering over extension commands.

use std::time::Instant;

use crate::ext::{Command, PaletteMode};

pub struct PaletteState {
    pub mode: PaletteMode,
    pub query: String,
    pub sel: usize,
    pub scroll: usize,
    pub items: Vec<Command>,
    pub filtered: Vec<usize>,
    pub opened: Instant,
}

impl PaletteState {
    pub fn new(mode: PaletteMode, items: Vec<Command>) -> Self {
        let mut s = Self { mode, query: String::new(), sel: 0, scroll: 0, items, filtered: vec![], opened: Instant::now() };
        s.refilter();
        s
    }

    pub fn set_items(&mut self, items: Vec<Command>) {
        let keep = self.selected().map(|c| c.title.clone());
        self.items = items;
        self.refilter();
        if let Some(t) = keep {
            if let Some(pos) = self.filtered.iter().position(|&i| self.items[i].title == t) {
                self.sel = pos;
            }
        }
    }

    pub fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        let mut scored: Vec<(i32, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, c)| match self.mode {
                PaletteMode::All => c.mode != PaletteMode::Orca || !q.is_empty(),
                m => c.mode == m,
            })
            .filter_map(|(i, c)| {
                if q.is_empty() {
                    return Some((0, i));
                }
                let hay = format!("{} {}", c.title, c.detail).to_lowercase();
                score(&q, &hay).map(|s| (s + if c.title.to_lowercase().starts_with(&q) { 50 } else { 0 }, i))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        if self.sel >= self.filtered.len() {
            self.sel = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn selected(&self) -> Option<&Command> {
        self.filtered.get(self.sel).map(|&i| &self.items[i])
    }

    pub fn move_sel(&mut self, d: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as i32;
        self.sel = ((self.sel as i32 + d).rem_euclid(n)) as usize;
    }

    pub fn ensure_visible(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if self.sel < self.scroll {
            self.scroll = self.sel;
        } else if self.sel >= self.scroll + rows {
            self.scroll = self.sel + 1 - rows;
        }
    }

    pub fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            PaletteMode::All => PaletteMode::Agents,
            PaletteMode::Agents => PaletteMode::Orca,
            PaletteMode::Orca => PaletteMode::Themes,
            PaletteMode::Themes => PaletteMode::All,
        };
        self.sel = 0;
        self.scroll = 0;
        self.refilter();
    }
}

/// Subsequence match with bonuses for contiguous runs and word starts.
fn score(q: &str, hay: &str) -> Option<i32> {
    let h: Vec<char> = hay.chars().collect();
    let mut score = 0;
    let mut hi = 0;
    let mut prev_match: Option<usize> = None;
    for qc in q.chars() {
        if qc == ' ' {
            continue;
        }
        let mut found = None;
        while hi < h.len() {
            if h[hi] == qc {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let i = found?;
        score += 1;
        if prev_match == Some(i.wrapping_sub(1)) {
            score += 5;
        }
        if i == 0 || !h[i - 1].is_alphanumeric() {
            score += 3;
        }
        prev_match = Some(i);
        hi = i + 1;
    }
    Some(score - (h.len() as i32 / 20))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ext::{Action, Command};
    use crate::render::icons::Icon;

    fn cmd(title: &str, mode: PaletteMode) -> Command {
        Command::new(mode, Icon::Square, title, "", Action::Hide)
    }

    #[test]
    fn fuzzy_prefers_prefix_and_contiguous() {
        let items = vec![cmd("close tab", PaletteMode::All), cmd("claude --resume", PaletteMode::Agents), cmd("theme: clay", PaletteMode::Themes)];
        let mut p = PaletteState::new(PaletteMode::All, items);
        p.query = "cla".into();
        p.refilter();
        assert_eq!(p.selected().unwrap().title, "claude --resume");
        p.query = "zzz".into();
        p.refilter();
        assert!(p.selected().is_none());
    }

    #[test]
    fn modes_filter() {
        let items = vec![cmd("a", PaletteMode::All), cmd("b", PaletteMode::Orca)];
        let mut p = PaletteState::new(PaletteMode::Orca, items);
        assert_eq!(p.filtered.len(), 1);
        p.cycle_mode();
        assert_eq!(p.mode, PaletteMode::Themes);
    }
}
