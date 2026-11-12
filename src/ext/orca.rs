//! Orca integration: polls `orca worktree ps --json` (+ `terminal list`) in the
//! background and exposes worktrees + agents as palette rows (⌘⇧O), status
//! segments, tab subtitles, tray lines and state-change notifications.
//!
//! Every Orca action runs the CLI in the background and reports back with a
//! toast: what happened, or the CLI's error. Agents that live in an Orca
//! terminal can be *attached*: a fluteterm tab mirrors the Orca terminal live and
//! forwards keystrokes to it (`fluteterm orca-attach <handle>`).

use std::any::Any;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::Value;
use winit::event_loop::EventLoopProxy;

use super::*;
use crate::app::UserEvent;
use crate::util;

#[derive(Clone, Debug)]
struct Agent {
    key: String,
    kind: String,
    state: String,
    tool: Option<String>,
    title: Option<String>,
    /// Orca terminal handle when the agent runs in a terminal (not a chat).
    handle: Option<String>,
}

#[derive(Clone, Debug)]
struct Worktree {
    name: String,
    path: PathBuf,
    branch: String,
    status: String,
    agents: Vec<Agent>,
}

struct Snapshot {
    ok: bool,
    worktrees: Vec<Worktree>,
}

/// Result of a background CLI action.
struct Done {
    what: String,
    worktree: Option<String>,
    ok: bool,
    handle: Option<String>,
    message: String,
}

pub struct Orca {
    proxy: EventLoopProxy<UserEvent>,
    enabled: bool,
    command: Option<String>,
    agent: String,
    notify: bool,
    worktrees: Vec<Worktree>,
    ok: bool,
    seen_once: bool,
    states: HashMap<String, String>,
    interval: Arc<AtomicU64>,
    poke: Arc<AtomicBool>,
    poll_secs: u64,
    idle_secs: u64,
    /// Shared with the poller thread so toggling in settings pauses/resumes it.
    on: Arc<AtomicBool>,
}

fn busy(state: &str) -> bool {
    matches!(state, "working" | "running" | "busy" | "thinking" | "active" | "tool")
}

fn needs_you(state: &str) -> bool {
    let s = state.to_lowercase();
    s.contains("wait") || s.contains("permission") || s.contains("block") || s.contains("input") || s.contains("attention")
}

/// "tabId:leafId" → (handle, title) from `orca terminal list --json`.
fn terminal_map(v: &Value) -> HashMap<String, String> {
    let r = &v["result"];
    let list = r["terminals"].as_array().or_else(|| r.as_array()).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|t| {
            let key = format!("{}:{}", t["tabId"].as_str()?, t["leafId"].as_str()?);
            Some((key, t["handle"].as_str()?.to_string()))
        })
        .collect()
}

fn parse(v: &Value, terms: &HashMap<String, String>) -> Vec<Worktree> {
    let list = v["result"]["worktrees"].as_array().cloned().unwrap_or_default();
    list.iter()
        .filter(|w| w["isArchived"].as_bool() != Some(true))
        .map(|w| Worktree {
            name: w["displayName"].as_str().or_else(|| w["repo"].as_str()).unwrap_or("worktree").to_string(),
            path: PathBuf::from(w["path"].as_str().unwrap_or("")),
            branch: w["branch"].as_str().unwrap_or("").to_string(),
            status: w["workspaceStatus"].as_str().or_else(|| w["status"].as_str()).unwrap_or("").to_string(),
            agents: w["agents"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|a| {
                            let key = a["paneKey"].as_str().unwrap_or("").to_string();
                            Agent {
                                handle: terms.get(&key).cloned(),
                                key,
                                kind: a["agentType"].as_str().unwrap_or("agent").to_string(),
                                state: a["state"].as_str().unwrap_or("").to_string(),
                                tool: a["toolName"].as_str().map(String::from),
                                title: a["conversationName"].as_str().or_else(|| a["taskTitle"].as_str()).map(String::from),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// Run the orca CLI; return (ok, parsed JSON or None, human message).
fn run_orca(cmd: &str, args: &[&str]) -> (bool, Option<Value>, String) {
    let out = std::process::Command::new(cmd)
        .args(args)
        .env("PATH", util::augmented_path())
        .stdin(std::process::Stdio::null())
        .output();
    match out {
        Err(e) => (false, None, format!("could not run orca: {e}")),
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let json = serde_json::from_str::<Value>(&stdout).ok();
            let ok = o.status.success() && json.as_ref().map(|j| j["ok"].as_bool() != Some(false)).unwrap_or(true);
            let msg = json
                .as_ref()
                .and_then(|j| {
                    j["result"]["receipt"]["detail"]
                        .as_str()
                        .or_else(|| j["error"]["message"].as_str())
                        .or_else(|| j["error"].as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| {
                    let s = if stderr.trim().is_empty() { stdout } else { stderr };
                    s.lines().filter(|l| !l.trim().is_empty()).take(2).collect::<Vec<_>>().join(" · ")
                });
            (ok, json, msg)
        }
    }
}

pub fn attach_spec(handle: &str, label: &str, cwd: Option<PathBuf>) -> TabSpec {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "fluteterm".into());
    TabSpec {
        kind: TabKind::Orca,
        title: Some(label.to_string()),
        command: Some((exe, vec!["orca-attach".into(), handle.to_string(), "--title".into(), label.to_string()])),
        cwd,
        keep_shell: false,
    }
}

impl Orca {
    pub fn new(cfg: &Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        let interval = Arc::new(AtomicU64::new(cfg.orca.idle_poll_secs.max(2)));
        let poke = Arc::new(AtomicBool::new(true));
        let command = util::which(&cfg.orca.command).map(|p| p.display().to_string());
        let on = Arc::new(AtomicBool::new(cfg.orca.enabled));
        let s = Self {
            on: on.clone(),
            proxy: proxy.clone(),
            enabled: cfg.orca.enabled,
            command: command.clone(),
            agent: cfg.orca.agent.clone(),
            notify: cfg.orca.notify,
            worktrees: vec![],
            ok: false,
            seen_once: false,
            states: HashMap::new(),
            interval: interval.clone(),
            poke: poke.clone(),
            poll_secs: cfg.orca.poll_secs.max(2),
            idle_secs: cfg.orca.idle_poll_secs.max(2),
        };
        // The poller only exists when the Orca CLI is installed, and idles while disabled.
        if let Some(cmd) = command {
            std::thread::Builder::new()
                .name("fluteterm-orca".into())
                .spawn(move || {
                    loop {
                        if !on.load(Ordering::Relaxed) {
                            std::thread::sleep(Duration::from_millis(500));
                            continue;
                        }
                        let terms = util::run_with_timeout(&cmd, &["terminal", "list", "--json"], Duration::from_secs(10))
                            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                            .map(|v| terminal_map(&v))
                            .unwrap_or_default();
                        let out = util::run_with_timeout(&cmd, &["worktree", "ps", "--json"], Duration::from_secs(10));
                        let snap = match out.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
                            Some(v) if v["ok"].as_bool() == Some(true) => Snapshot { ok: true, worktrees: parse(&v, &terms) },
                            _ => Snapshot { ok: false, worktrees: vec![] },
                        };
                        super::post(&proxy, "orca", snap);
                        let secs = interval.load(Ordering::Relaxed);
                        for _ in 0..secs * 10 {
                            if poke.swap(false, Ordering::Relaxed) {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                })
                .ok();
        }
        s
    }

    /// Enabled in config *and* the Orca CLI is installed.
    fn active(&self) -> bool {
        self.enabled && self.command.is_some()
    }

    fn cmd(&self) -> String {
        self.command.clone().unwrap_or_else(|| "orca".into())
    }

    fn worktree_for(&self, cwd: &std::path::Path) -> Option<&Worktree> {
        self.worktrees
            .iter()
            .filter(|w| !w.path.as_os_str().is_empty() && cwd.starts_with(&w.path))
            .max_by_key(|w| w.path.as_os_str().len())
    }

    /// Run an orca CLI action in the background; the result comes back as `Done`.
    fn spawn(&self, what: &str, worktree: Option<String>, args: Vec<String>) {
        let cmd = self.cmd();
        let proxy = self.proxy.clone();
        let what = what.to_string();
        let poke = self.poke.clone();
        std::thread::spawn(move || {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            let (ok, json, message) = run_orca(&cmd, &argv);
            let handle = json.as_ref().and_then(|j| j["result"]["outcome"]["handle"].as_str().map(String::from));
            poke.store(true, Ordering::Relaxed);
            super::post(&proxy, "orca", Done { what, worktree, ok, handle, message });
        });
    }

    fn dots(w: &Worktree) -> (String, Tone) {
        if w.agents.is_empty() {
            return (format!("{} ○", w.status), Tone::Faint);
        }
        let dots: String = w.agents.iter().map(|a| if busy(&a.state) { '●' } else { '○' }).collect();
        let tone = if w.agents.iter().any(|a| needs_you(&a.state)) {
            Tone::Warn
        } else if w.agents.iter().any(|a| busy(&a.state)) {
            Tone::Accent
        } else {
            Tone::Dim
        };
        (format!("{} {dots}", w.status), tone)
    }

    fn handle_snapshot(&mut self, snap: Snapshot, ctx: &mut Ctx) {
        if !self.active() {
            return;
        }
        self.ok = snap.ok;
        if !snap.ok {
            return;
        }
        // State-change notifications (skip the very first snapshot).
        let mut next = HashMap::new();
        for w in &snap.worktrees {
            for a in &w.agents {
                let key = format!("{}|{}", w.name, a.key);
                if self.seen_once && self.notify {
                    let prev = self.states.get(&key).cloned().unwrap_or_default();
                    if prev != a.state {
                        let title = format!("orca · {}", w.name);
                        let what = a.title.clone().unwrap_or_else(|| a.kind.clone());
                        let mut actions = vec![];
                        if let Some(h) = &a.handle {
                            actions.push(("attach here".to_string(), Action::NewTab(attach_spec(h, &what, Some(w.path.clone())))));
                            actions.push(("show in Orca".to_string(), Action::ExtCall { ext: "orca", call: format!("show:{h}") }));
                        } else {
                            actions.push(("show in Orca".to_string(), Action::ExtCall { ext: "orca", call: "open".into() }));
                        }
                        if needs_you(&a.state) && !needs_you(&prev) {
                            ctx.push(Action::Notify(Notice {
                                title,
                                body: format!("{} needs you — {what}", a.kind),
                                tab: None,
                                level: Level::Warn,
                                actions,
                                native: true,
                                toast: true,
                            }));
                        } else if busy(&prev) && !busy(&a.state) {
                            ctx.push(Action::Notify(Notice {
                                title,
                                body: format!("{} {} — {what}", a.kind, if a.state.is_empty() { "finished" } else { &a.state }),
                                tab: None,
                                level: Level::Ok,
                                actions,
                                native: true,
                                toast: true,
                            }));
                        }
                    }
                }
                next.insert(key, a.state.clone());
            }
        }
        self.states = next;
        self.seen_once = true;
        self.worktrees = snap.worktrees;
    }
}

impl Extension for Orca {
    fn id(&self) -> &'static str {
        "orca"
    }

    fn reconfigure(&mut self, cfg: &Config) {
        if self.enabled != cfg.orca.enabled {
            self.enabled = cfg.orca.enabled;
            self.on.store(self.enabled, Ordering::Relaxed);
            if self.enabled {
                self.poke.store(true, Ordering::Relaxed);
            } else {
                // Forget everything so nothing Orca-related is shown.
                self.worktrees.clear();
                self.states.clear();
                self.ok = false;
                self.seen_once = false;
            }
        }
        self.agent = cfg.orca.agent.clone();
        self.notify = cfg.orca.notify;
        self.poll_secs = cfg.orca.poll_secs.max(2);
        self.idle_secs = cfg.orca.idle_poll_secs.max(2);
    }

    fn on_call(&mut self, call: &str, ctx: &mut Ctx) {
        let cmd = self.cmd();
        if let Some(path) = call.strip_prefix("launch:") {
            let name = self.worktrees.iter().find(|w| w.path.display().to_string() == path).map(|w| w.name.clone());
            ctx.push(Action::Notify(Notice {
                title: format!("orca · starting {}", self.agent),
                body: format!("in {}…", name.clone().unwrap_or_else(|| path.to_string())),
                tab: None,
                level: Level::Info,
                actions: vec![],
                native: false,
                toast: true,
            }));
            self.spawn(
                "launch",
                name,
                vec![
                    "agent".into(),
                    "launch".into(),
                    "--agent".into(),
                    self.agent.clone(),
                    "--worktree".into(),
                    format!("path:{path}"),
                    "--surface".into(),
                    "terminal".into(),
                    "--json".into(),
                ],
            );
        } else if let Some(h) = call.strip_prefix("show:") {
            // Bring Orca forward on that terminal and get out of the way.
            let h = h.to_string();
            std::thread::spawn(move || {
                let _ = run_orca(&cmd, &["terminal", "switch", "--terminal", &h, "--json"]);
                let _ = std::process::Command::new("open").args(["-a", "Orca"]).status();
            });
            ctx.push(Action::Hide);
        } else if call == "open" {
            std::thread::spawn(move || {
                let _ = run_orca(&cmd, &["open", "--json"]);
                let _ = std::process::Command::new("open").args(["-a", "Orca"]).status();
            });
            ctx.push(Action::Hide);
        } else if call == "refresh" {
            self.poke.store(true, Ordering::Relaxed);
        }
    }

    fn on_message(&mut self, msg: Box<dyn Any + Send>, ctx: &mut Ctx) {
        let msg = match msg.downcast::<Snapshot>() {
            Ok(snap) => return self.handle_snapshot(*snap, ctx),
            Err(m) => m,
        };
        if let Ok(d) = msg.downcast::<Done>() {
            let place = d.worktree.clone().unwrap_or_else(|| "worktree".into());
            if !d.ok {
                ctx.push(Action::Notify(Notice {
                    title: format!("orca {} failed", d.what),
                    body: if d.message.is_empty() { "orca returned an error".into() } else { d.message },
                    tab: None,
                    level: Level::Warn,
                    actions: vec![],
                    native: true,
                    toast: true,
                }));
                return;
            }
            let mut actions = vec![];
            if let Some(h) = &d.handle {
                let label = format!("{} · {place}", self.agent);
                let path = self.worktrees.iter().find(|w| w.name == place).map(|w| w.path.clone());
                actions.push(("attach here".into(), Action::NewTab(attach_spec(h, &label, path))));
                actions.push(("show in Orca".into(), Action::ExtCall { ext: "orca", call: format!("show:{h}") }));
            }
            ctx.push(Action::Notify(Notice {
                title: format!("orca · {} started in {place}", self.agent),
                body: if d.message.is_empty() { "running in an Orca terminal".into() } else { d.message },
                tab: None,
                level: Level::Ok,
                actions,
                native: true,
                toast: true,
            }));
        }
    }

    fn tick(&mut self, ctx: &mut Ctx) {
        let want = if ctx.visible { self.poll_secs } else { self.idle_secs };
        if self.interval.swap(want, Ordering::Relaxed) > want && ctx.visible {
            self.poke.store(true, Ordering::Relaxed);
        }
    }

    fn tab_decor(&self, tab: &TabInfo) -> Option<TabDecor> {
        if !self.active() {
            return None;
        }
        let w = self.worktree_for(tab.cwd.as_deref()?)?;
        let busy_n = w.agents.iter().filter(|a| busy(&a.state)).count().min(3) as u8;
        Some(TabDecor {
            kind: (tab.kind == TabKind::Orca).then_some(TabKind::Orca),
            subtitle: Some(w.name.clone()),
            badge: if busy_n > 0 && tab.kind != TabKind::Claude { Badge::Dots(busy_n) } else { Badge::None },
        })
    }

    fn status_right(&self, _ctx: &Ctx) -> Vec<Segment> {
        if !self.active() {
            return vec![];
        }
        if !self.ok {
            return vec![Segment::text("orca offline", Tone::Faint).icon(Icon::Orca)];
        }
        let agents: Vec<&Agent> = self.worktrees.iter().flat_map(|w| &w.agents).collect();
        let busy_n = agents.iter().filter(|a| busy(&a.state)).count();
        let attn = agents.iter().any(|a| needs_you(&a.state));
        let text = format!("{}/{} agents · {} wt", busy_n, agents.len(), self.worktrees.len());
        let mut s = Segment::text(text, if attn { Tone::Warn } else { Tone::Dim }).icon(Icon::Orca).on_click(Action::OpenPalette(PaletteMode::Orca));
        if busy_n > 0 {
            s = s.spinning();
        }
        vec![s]
    }

    fn commands(&self, ctx: &Ctx) -> Vec<Command> {
        if !self.active() {
            return vec![];
        }
        let orca = self.cmd();
        let mut v = vec![];
        for w in &self.worktrees {
            let (right, tone) = Self::dots(w);
            let detail = if w.branch.is_empty() { util::tildify(&w.path) } else { format!("{} · {}", w.branch, util::tildify(&w.path)) };
            v.push(
                Command::new(PaletteMode::Orca, Icon::Orca, w.name.clone(), format!("shell here · {detail}"), Action::NewTab(TabSpec::shell(Some(w.path.clone()))))
                    .right(right, tone)
                    .alt(Action::NewTab(TabSpec {
                        kind: TabKind::Claude,
                        title: Some("claude".into()),
                        command: Some((ctx.cfg.claude.command.clone(), ctx.cfg.claude.args.clone())),
                        cwd: Some(w.path.clone()),
                        keep_shell: true,
                    })),
            );
            for a in &w.agents {
                let label = a.title.clone().unwrap_or_else(|| a.kind.clone());
                let st = match &a.tool {
                    Some(t) if busy(&a.state) => format!("{} · {t}", a.state),
                    _ => a.state.clone(),
                };
                let tone = if needs_you(&a.state) { Tone::Warn } else if busy(&a.state) { Tone::Accent } else { Tone::Faint };
                let (detail, action, alt) = match &a.handle {
                    Some(h) => (
                        format!("{} in {} · ⏎ attach here · ⌘⏎ show in Orca", a.kind, w.name),
                        Action::NewTab(attach_spec(h, &label, Some(w.path.clone()))),
                        Action::ExtCall { ext: "orca", call: format!("show:{h}") },
                    ),
                    None => (
                        format!("{} chat in {} · ⏎ show in Orca", a.kind, w.name),
                        Action::ExtCall { ext: "orca", call: "open".into() },
                        Action::ExtCall { ext: "orca", call: "open".into() },
                    ),
                };
                v.push(Command::new(PaletteMode::Orca, Icon::Claude, format!("   {label}"), detail, action).right(st, tone).alt(alt));
            }
            v.push(Command::new(
                PaletteMode::Orca,
                Icon::Bolt,
                format!("launch {} in {}", self.agent, w.name),
                "new Orca agent in this worktree, then attach or show it",
                Action::ExtCall { ext: "orca", call: format!("launch:{}", w.path.display()) },
            ));
        }
        v.push(Command::new(
            PaletteMode::Orca,
            Icon::Orca,
            "orca worktree ps",
            "summary across worktrees in a new tab",
            Action::NewTab(TabSpec {
                kind: TabKind::Orca,
                title: Some("orca".into()),
                command: Some((orca, vec!["worktree".into(), "ps".into()])),
                cwd: ctx.active_cwd(),
                keep_shell: true,
            }),
        ));
        v.push(Command::new(PaletteMode::Orca, Icon::Orca, "orca open", "bring the Orca app to the front", Action::ExtCall { ext: "orca", call: "open".into() }));
        v.push(Command::new(PaletteMode::Orca, Icon::Orca, "orca refresh", "re-read worktrees & agents now", Action::ExtCall { ext: "orca", call: "refresh".into() }));
        v
    }

    fn tray_lines(&self) -> Vec<String> {
        if !self.active() {
            return vec![];
        }
        self.worktrees
            .iter()
            .flat_map(|w| w.agents.iter().map(move |a| format!("{} · {} — {}", w.name, a.kind, if a.state.is_empty() { "idle" } else { &a.state })))
            .collect()
    }

    fn attention(&self) -> bool {
        self.active() && self.worktrees.iter().flat_map(|w| &w.agents).any(|a| needs_you(&a.state))
    }

    fn animating(&self) -> bool {
        self.active() && self.worktrees.iter().flat_map(|w| &w.agents).any(|a| busy(&a.state))
    }
}
