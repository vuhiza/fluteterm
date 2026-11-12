//! Claude Code integration.
//!
//! `fluteterm install claude` adds hooks to ~/.claude/settings.json that run
//! `fluteterm hook claude`. Inside a fluteterm tab the hook forwards the hook JSON
//! (plus the tab id from $FLUTETERM_TAB_ID) over IPC; this extension turns the
//! stream into per-tab state: thinking / running a tool / needs you / done,
//! plus model and context usage parsed from the transcript.

use std::any::Any;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use winit::event_loop::EventLoopProxy;

use super::*;
use crate::app::UserEvent;
use crate::util;
use super::claude_usage::{BLOCK_SECS, Scanner, UsageSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Thinking,
    Tool,
    Attention,
    Done,
}

struct ClaudeTab {
    state: State,
    since: Instant,
    turn_started: Option<Instant>,
    last_turn: Option<Duration>,
    tool: Option<String>,
    message: Option<String>,
    model: Option<String>,
    ctx_tokens: Option<u64>,
    output_tokens: u64,
    transcript: Option<PathBuf>,
    cwd: Option<PathBuf>,
    session_id: Option<String>,
}

impl ClaudeTab {
    fn new() -> Self {
        Self {
            state: State::Idle,
            since: Instant::now(),
            turn_started: None,
            last_turn: None,
            tool: None,
            message: None,
            model: None,
            ctx_tokens: None,
            output_tokens: 0,
            transcript: None,
            cwd: None,
            session_id: None,
        }
    }
    fn set(&mut self, s: State) {
        if self.state != s {
            self.state = s;
            self.since = Instant::now();
        }
    }
    fn place(&self) -> String {
        self.cwd
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".into())
    }
}

struct Usage {
    tab: u64,
    model: Option<String>,
    ctx: Option<u64>,
    output: u64,
}

pub struct Claude {
    proxy: EventLoopProxy<UserEvent>,
    tabs: HashMap<u64, ClaudeTab>,
    enabled: bool,
    notify_done: bool,
    notify_attention: bool,
    command: String,
    args: Vec<String>,
    status_usage: bool,
    /// The claude CLI exists on this Mac.
    installed: bool,
    usage: Option<UsageSummary>,
    poke: Arc<AtomicBool>,
}

impl Claude {
    pub fn new(cfg: &Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut s = Self {
            proxy,
            tabs: HashMap::new(),
            enabled: true,
            notify_done: true,
            notify_attention: true,
            command: "claude".into(),
            args: vec![],
            status_usage: true,
            installed: false,
            usage: None,
            poke: Arc::new(AtomicBool::new(true)),
        };
        s.reconfigure(cfg);
        // Usage across every Claude Code session on this Mac, refreshed every
        // 30 s (or right after a turn ends).
        let proxy = s.proxy.clone();
        let poke = s.poke.clone();
        std::thread::Builder::new()
            .name("fluteterm-claude-usage".into())
            .spawn(move || {
                let mut scanner = Scanner::default();
                loop {
                    let summary = scanner.scan();
                    super::post(&proxy, "claude", summary);
                    for _ in 0..300 {
                        if poke.swap(false, Ordering::Relaxed) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })
            .ok();
        s
    }

    fn refresh_usage(&self, tab: u64) {
        let Some(path) = self.tabs.get(&tab).and_then(|t| t.transcript.clone()) else { return };
        let proxy = self.proxy.clone();
        std::thread::spawn(move || {
            if let Some((model, ctx, output)) = read_usage(&path) {
                super::post(&proxy, "claude", Usage { tab, model, ctx, output });
            }
        });
    }

    fn spec(&self, cwd: Option<PathBuf>, extra: &[&str]) -> TabSpec {
        let mut args = self.args.clone();
        args.extend(extra.iter().map(|s| s.to_string()));
        TabSpec {
            kind: TabKind::Claude,
            title: Some("claude".into()),
            command: Some((self.command.clone(), args)),
            cwd,
            keep_shell: true,
        }
    }
}

/// Parse the tail of a Claude Code transcript (JSONL) for the latest usage.
fn read_usage(path: &Path) -> Option<(Option<String>, Option<u64>, u64)> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(512 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = String::new();
    f.read_to_string(&mut buf).ok()?;
    let mut output_total = 0u64;
    let mut found: Option<(Option<String>, Option<u64>)> = None;
    for line in buf.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let msg = &v["message"];
        let usage = &msg["usage"];
        if usage.is_object() {
            output_total += usage["output_tokens"].as_u64().unwrap_or(0);
            if found.is_none() && v["isSidechain"].as_bool() != Some(true) {
                let ctx = usage["input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                    + usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                found = Some((msg["model"].as_str().map(String::from), Some(ctx)));
            }
        }
    }
    found.map(|(m, c)| (m, c, output_total))
}

fn pretty_model(m: &str) -> String {
    // claude-opus-5-5-20261001 → opus 5.5
    let m = m.trim_start_matches("claude-");
    let mut parts = m.split('-').filter(|p| !(p.len() >= 8 && p.chars().all(|c| c.is_ascii_digit())));
    let family = parts.next().unwrap_or(m);
    let ver: Vec<&str> = parts.filter(|p| p.chars().all(|c| c.is_ascii_digit())).collect();
    if ver.is_empty() { family.to_string() } else { format!("{family} {}", ver.join(".")) }
}

fn context_limit(model: Option<&str>, used: u64) -> u64 {
    if model.map(|m| m.contains("[1m]") || m.contains("1m")).unwrap_or(false) || used > 200_000 {
        1_000_000
    } else {
        200_000
    }
}

fn tool_label(payload: &Value) -> String {
    let name = payload["tool_name"].as_str().unwrap_or("tool");
    let input = &payload["tool_input"];
    let arg = input["command"]
        .as_str()
        .or_else(|| input["file_path"].as_str())
        .or_else(|| input["pattern"].as_str())
        .or_else(|| input["url"].as_str())
        .or_else(|| input["description"].as_str())
        .unwrap_or("");
    let arg = arg.lines().next().unwrap_or("");
    let arg = if arg.contains('/') && !arg.contains(' ') {
        Path::new(arg).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| arg.to_string())
    } else {
        arg.to_string()
    };
    if arg.is_empty() { name.to_string() } else { format!("{name}({})", crate::render::text::truncate(&arg, 40)) }
}

impl Extension for Claude {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn reconfigure(&mut self, cfg: &Config) {
        self.enabled = cfg.claude.enabled;
        self.notify_done = cfg.claude.notify_done;
        self.notify_attention = cfg.claude.notify_attention;
        self.command = cfg.claude.command.clone();
        self.args = cfg.claude.args.clone();
        self.status_usage = cfg.claude.status_usage;
        self.installed = util::which(&cfg.claude.command).is_some();
        if !self.enabled {
            self.tabs.clear();
        }
    }

    fn on_event(&mut self, source: &str, tab: Option<u64>, p: &Value, ctx: &mut Ctx) -> bool {
        if source != "claude" || !self.enabled {
            return false;
        }
        let Some(id) = tab else { return true };
        let ev = p["hook_event_name"].as_str().unwrap_or("").to_string();
        let watching = ctx.watching(tab);
        let t = self.tabs.entry(id).or_insert_with(ClaudeTab::new);
        if let Some(sid) = p["session_id"].as_str() {
            t.session_id = Some(sid.to_string());
        }
        if let Some(tp) = p["transcript_path"].as_str() {
            t.transcript = Some(PathBuf::from(tp));
        }
        if let Some(cwd) = p["cwd"].as_str() {
            t.cwd = Some(PathBuf::from(cwd));
        }
        if let Some(m) = p["model"].as_str().or_else(|| p["model"]["id"].as_str()) {
            t.model = Some(m.to_string());
        }
        let mut refresh = false;
        match ev.as_str() {
            "SessionStart" => {
                t.set(State::Idle);
                refresh = true;
            }
            "UserPromptSubmit" => {
                t.set(State::Thinking);
                t.turn_started = Some(Instant::now());
                t.tool = None;
                t.message = None;
            }
            "PreToolUse" => {
                t.tool = Some(tool_label(p));
                t.set(State::Tool);
            }
            "PostToolUse" => {
                t.tool = None;
                t.set(State::Thinking);
                refresh = true;
            }
            "PreCompact" => {
                t.tool = Some("compacting".into());
                t.set(State::Tool);
            }
            "Notification" => {
                let msg = p["message"].as_str().unwrap_or("Claude needs your attention").to_string();
                let needs_permission = msg.to_lowercase().contains("permission");
                t.message = Some(msg.clone());
                if needs_permission {
                    t.set(State::Attention);
                    if self.notify_attention && !watching {
                        let place = t.place();
                        ctx.push(Action::Notify(Notice {
                            title: format!("claude needs you · {place}"),
                            body: msg,
                            tab,
                            level: Level::Warn,
                            actions: vec![
                                ("allow ⏎".into(), Action::SendInput { tab, data: "\r".into() }),
                                ("deny ⎋".into(), Action::SendInput { tab, data: "\x1b".into() }),
                                ("go to tab".into(), Action::FocusTab(id)),
                            ],
                            native: true,
                            toast: true,
                        }));
                    }
                }
            }
            "Stop" => {
                let took = t.turn_started.take().map(|s| s.elapsed());
                t.last_turn = took;
                t.tool = None;
                t.set(if watching { State::Idle } else { State::Done });
                refresh = true;
                if self.notify_done && !watching {
                    let place = t.place();
                    let body = match took {
                        Some(d) => format!("finished in {}", util::short_duration(d)),
                        None => "finished".into(),
                    };
                    ctx.push(Action::Notify(Notice {
                        title: format!("claude · {place}"),
                        body,
                        tab,
                        level: Level::Ok,
                        actions: vec![("go to tab".into(), Action::FocusTab(id))],
                        native: true,
                        toast: true,
                    }));
                }
            }
            "SessionEnd" => {
                self.tabs.remove(&id);
                return true;
            }
            _ => {}
        }
        if refresh {
            self.refresh_usage(id);
            self.poke.store(true, Ordering::Relaxed);
        }
        true
    }

    fn on_message(&mut self, msg: Box<dyn Any + Send>, _ctx: &mut Ctx) {
        let msg = match msg.downcast::<UsageSummary>() {
            Ok(s) => {
                self.usage = Some(*s);
                return;
            }
            Err(m) => m,
        };
        if let Ok(u) = msg.downcast::<Usage>() {
            if let Some(t) = self.tabs.get_mut(&u.tab) {
                if u.model.is_some() {
                    t.model = u.model;
                }
                if u.ctx.is_some() {
                    t.ctx_tokens = u.ctx;
                }
                t.output_tokens = u.output;
            }
        }
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        // Seen it → clear the "done"/"attention" badge.
        if ctx.visible {
            if let Some(id) = ctx.active {
                if let Some(t) = self.tabs.get_mut(&id) {
                    if t.state == State::Done {
                        t.set(State::Idle);
                    }
                }
            }
        }
        // Forget tabs that closed.
        let live: Vec<u64> = ctx.tabs.iter().map(|t| t.id).collect();
        self.tabs.retain(|id, _| live.contains(id));
    }

    fn on_tab_closed(&mut self, id: u64) {
        self.tabs.remove(&id);
    }

    fn save_tab(&self, tab: u64) -> Option<Value> {
        let t = self.tabs.get(&tab)?;
        Some(serde_json::json!({
            "session_id": t.session_id,
            "transcript": t.transcript,
            "cwd": t.cwd,
            "model": t.model,
        }))
    }

    fn restore_tab(&mut self, tab: u64, v: &Value) {
        let mut t = ClaudeTab::new();
        t.session_id = v["session_id"].as_str().map(String::from);
        t.transcript = v["transcript"].as_str().map(PathBuf::from);
        t.cwd = v["cwd"].as_str().map(PathBuf::from);
        t.model = v["model"].as_str().map(String::from);
        self.tabs.insert(tab, t);
        self.refresh_usage(tab);
    }

    fn tab_decor(&self, tab: &TabInfo) -> Option<TabDecor> {
        if !self.enabled {
            return None;
        }
        if let Some(t) = self.tabs.get(&tab.id) {
            let badge = match t.state {
                State::Thinking | State::Tool => Badge::Spinner,
                State::Attention => Badge::Attention,
                State::Done => Badge::Done,
                State::Idle => Badge::None,
            };
            return Some(TabDecor { kind: Some(TabKind::Claude), subtitle: Some(t.place()), badge });
        }
        // No hooks installed (or not yet fired): recognise Claude by process/title.
        let fg = tab.fg.as_deref().unwrap_or("");
        let title_hit = tab.title.contains("Claude") || tab.title.starts_with('✳') || tab.title.starts_with('✻');
        if fg == "claude" || title_hit || tab.kind == TabKind::Claude && fg != "zsh" && fg != "bash" && fg != "fish" {
            let place = tab.cwd.as_ref().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned());
            return Some(TabDecor { kind: Some(TabKind::Claude), subtitle: place, badge: Badge::None });
        }
        None
    }

    fn status_left(&self, tab: &TabInfo, _ctx: &Ctx) -> Vec<Segment> {
        if tab.kind != TabKind::Claude {
            return vec![];
        }
        let Some(t) = self.tabs.get(&tab.id) else {
            return vec![
                Segment::text("claude", Tone::Text).icon(Icon::Claude),
                Segment::text("run `fluteterm install claude` for live status", Tone::Faint),
            ];
        };
        let mut v = vec![];
        let model = t.model.as_deref().map(pretty_model).unwrap_or_else(|| "claude".into());
        v.push(Segment::text(model, Tone::Text).icon(Icon::Claude));
        if let Some(used) = t.ctx_tokens {
            let limit = context_limit(t.model.as_deref(), used);
            let frac = used as f32 / limit as f32;
            let tone = if frac > 0.85 { Tone::Warn } else { Tone::Dim };
            v.push(Segment::text(format!("ctx {}%", (frac * 100.0).round() as u32), tone).meter(frac, 10));
        }
        let (label, tone) = match t.state {
            State::Thinking => ("thinking".to_string(), Tone::Accent),
            State::Tool => (t.tool.clone().unwrap_or_else(|| "working".into()), Tone::Accent),
            State::Attention => ("needs you".to_string(), Tone::Warn),
            State::Done => ("done".to_string(), Tone::Ok),
            State::Idle => ("idle".to_string(), Tone::Faint),
        };
        let mut seg = Segment::text(label, tone);
        if matches!(t.state, State::Thinking | State::Tool) {
            seg = seg.spinning();
        }
        v.push(seg);
        if let Some(start) = t.turn_started {
            v.push(Segment::text(util::short_duration(start.elapsed()), Tone::Dim));
        } else if let Some(d) = t.last_turn {
            v.push(Segment::text(format!("last turn {}", util::short_duration(d)), Tone::Faint));
        }
        if t.output_tokens > 0 {
            v.push(Segment::text(format!("↓ {} tokens", util::short_tokens(t.output_tokens)), Tone::Faint));
        }
        v
    }

    fn status_right(&self, ctx: &Ctx) -> Vec<Segment> {
        if !self.enabled || !self.status_usage {
            return vec![];
        }
        let mut v = vec![];
        // Live Claude sessions in fluteterm tabs.
        let live: Vec<&TabInfo> = ctx.tabs.iter().filter(|t| t.kind == TabKind::Claude).collect();
        if !live.is_empty() {
            let state = |t: &TabInfo| self.tabs.get(&t.id).map(|c| c.state).unwrap_or(State::Idle);
            let attn: Vec<&&TabInfo> = live.iter().filter(|t| state(t) == State::Attention).collect();
            let working = live.iter().filter(|t| matches!(state(t), State::Thinking | State::Tool)).count();
            let mut text = format!("{} live", live.len());
            if working > 0 {
                text.push_str(&format!(" · {working} working"));
            }
            if !attn.is_empty() {
                text.push_str(&format!(" · {} needs you", attn.len()));
            }
            let action = match attn.first() {
                Some(t) => Action::FocusTab(t.id),
                None => Action::OpenPalette(PaletteMode::Agents),
            };
            let mut seg = Segment::text(text, if attn.is_empty() { Tone::Dim } else { Tone::Warn }).icon(Icon::Claude).on_click(action);
            if working > 0 {
                seg = seg.spinning();
            }
            v.push(seg);
        }
        // The 5-hour usage window + today's tokens (all sessions on this Mac).
        if let Some(u) = &self.usage {
            let now = util::now_unix();
            if let Some((start, end)) = u.block {
                let left = end.saturating_sub(now);
                let frac = (now.saturating_sub(start)) as f32 / BLOCK_SECS as f32;
                let tone = if left < 30 * 60 { Tone::Warn } else { Tone::Dim };
                let left_s = if left >= 3600 { format!("{}h {:02}m", left / 3600, (left % 3600) / 60) } else { format!("{}m", left / 60) };
                v.push(
                    Segment::text(format!("{left_s} left in 5h window · {}", util::short_tokens(u.block_tokens)), tone)
                        .meter(frac, 5)
                        .on_click(Action::OpenPalette(PaletteMode::Agents)),
                );
            }
            if u.today_tokens > 0 {
                v.push(Segment::text(format!("today {}", util::short_tokens(u.today_tokens)), Tone::Faint).on_click(Action::OpenPalette(PaletteMode::Agents)));
            }
        }
        v
    }

    fn commands(&self, ctx: &Ctx) -> Vec<Command> {
        if !self.enabled || !self.installed {
            return vec![];
        }
        let cwd = ctx.active_cwd();
        let here = cwd.as_deref().map(util::tildify).unwrap_or_else(|| "~".into());
        let active_claude = ctx.active_tab().filter(|t| t.kind == TabKind::Claude).map(|t| t.id);
        let mut v = vec![
            Command::new(PaletteMode::Agents, Icon::Claude, "claude", format!("new session in {here}"), Action::NewTab(self.spec(cwd.clone(), &[])))
                .keys("⌘⇧C"),
            Command::new(PaletteMode::Agents, Icon::Claude, "claude --continue", "continue the last conversation here", Action::NewTab(self.spec(cwd.clone(), &["--continue"]))),
            Command::new(PaletteMode::Agents, Icon::Claude, "claude --resume", "pick a past conversation", Action::NewTab(self.spec(cwd.clone(), &["--resume"]))),
        ];
        if active_claude.is_some() {
            for (cmd, what) in [("/compact", "compact this conversation"), ("/clear", "clear this conversation"), ("/cost", "show cost & usage"), ("/model", "switch model")] {
                v.push(Command::new(
                    PaletteMode::Agents,
                    Icon::Claude,
                    format!("claude {cmd}"),
                    format!("send to this tab: {what}"),
                    Action::SendInput { tab: active_claude, data: format!("{cmd}\r") },
                ));
            }
        }
        for t in ctx.tabs.iter().filter(|t| t.kind == TabKind::Claude && Some(t.id) != ctx.active) {
            let state = self.tabs.get(&t.id).map(|c| c.state).unwrap_or(State::Idle);
            let (r, tone) = match state {
                State::Attention => ("needs you", Tone::Warn),
                State::Thinking | State::Tool => ("working", Tone::Accent),
                State::Done => ("done", Tone::Ok),
                State::Idle => ("idle", Tone::Faint),
            };
            let place = t.cwd.as_deref().map(util::tildify).unwrap_or_default();
            v.push(Command::new(PaletteMode::Agents, Icon::Claude, format!("go to claude #{}", t.id), place, Action::FocusTab(t.id)).right(r, tone));
        }
        v
    }

    fn tray_lines(&self) -> Vec<String> {
        if !self.enabled {
            return vec![];
        }
        self.tabs
            .values()
            .filter(|t| t.state != State::Idle)
            .map(|t| {
                let s = match t.state {
                    State::Attention => "needs you",
                    State::Done => "done",
                    _ => "working",
                };
                format!("claude · {} — {s}", t.place())
            })
            .collect()
    }

    fn attention(&self) -> bool {
        self.enabled && self.tabs.values().any(|t| t.state == State::Attention)
    }

    fn animating(&self) -> bool {
        self.enabled && self.tabs.values().any(|t| matches!(t.state, State::Thinking | State::Tool))
    }
}
