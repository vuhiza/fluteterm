//! Code-free extensions from config.toml: `[[profile]]` tab types,
//! `[[command]]` palette entries and `[[module]]` status-bar segments.

use std::any::Any;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use super::*;
use crate::app::UserEvent;
use crate::util;

struct ModuleOut {
    generation: u64,
    index: usize,
    text: String,
}

pub struct User {
    proxy: EventLoopProxy<UserEvent>,
    profiles: Vec<crate::config::Profile>,
    commands: Vec<crate::config::UserCommand>,
    modules: Vec<(String, String)>,
    generation: Arc<AtomicU64>,
}

impl User {
    pub fn new(cfg: &Config, proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut s = Self { proxy, profiles: vec![], commands: vec![], modules: vec![], generation: Arc::new(AtomicU64::new(0)) };
        s.reconfigure(cfg);
        s
    }

    pub fn profile_spec(p: &crate::config::Profile, cwd: Option<std::path::PathBuf>) -> TabSpec {
        TabSpec {
            kind: TabKind::Custom(p.icon.as_deref().and_then(|s| s.chars().next()).unwrap_or('▸')),
            title: Some(p.name.clone()),
            command: Some((p.command.clone(), p.args.clone())),
            cwd: p.cwd.as_deref().map(util::expand_tilde).or(cwd),
            keep_shell: false,
        }
    }
}

impl Extension for User {
    fn id(&self) -> &'static str {
        "user"
    }

    fn reconfigure(&mut self, cfg: &Config) {
        self.profiles = cfg.profiles.clone();
        self.commands = cfg.commands.clone();
        // Restart module pollers with a new generation; old threads exit.
        let my_gen = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.modules = cfg.modules.iter().map(|m| (m.name.clone(), String::new())).collect();
        for (i, m) in cfg.modules.iter().enumerate() {
            let cmd = m.command.clone();
            let every = m.interval.max(1);
            let generation = self.generation.clone();
            let proxy = self.proxy.clone();
            std::thread::spawn(move || {
                while generation.load(Ordering::SeqCst) == my_gen {
                    let out = util::run_shell_with_timeout(&cmd, Duration::from_secs(10)).unwrap_or_default();
                    let text = out.lines().next().unwrap_or("").trim().to_string();
                    super::post(&proxy, "user", ModuleOut { generation: my_gen, index: i, text });
                    for _ in 0..every * 10 {
                        if generation.load(Ordering::SeqCst) != my_gen {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            });
        }
    }

    fn on_message(&mut self, msg: Box<dyn Any + Send>, _ctx: &mut Ctx) {
        if let Ok(m) = msg.downcast::<ModuleOut>() {
            if m.generation == self.generation.load(Ordering::SeqCst) {
                if let Some(slot) = self.modules.get_mut(m.index) {
                    slot.1 = m.text;
                }
            }
        }
    }

    fn status_right(&self, _ctx: &Ctx) -> Vec<Segment> {
        self.modules
            .iter()
            .filter(|(_, t)| !t.is_empty())
            .map(|(_, t)| Segment::text(t.clone(), Tone::Dim))
            .collect()
    }

    fn commands(&self, ctx: &Ctx) -> Vec<Command> {
        let mut v = vec![];
        for p in &self.profiles {
            let mut c = Command::new(
                PaletteMode::All,
                Icon::Arrow,
                p.name.clone(),
                format!("{} {}", p.command, p.args.join(" ")),
                Action::NewTab(Self::profile_spec(p, ctx.active_cwd())),
            );
            if let Some(k) = &p.key {
                c = c.keys(k);
            }
            v.push(c);
        }
        for c in &self.commands {
            let action = if c.tab {
                Action::NewTab(TabSpec {
                    kind: TabKind::Shell,
                    title: Some(c.name.clone()),
                    command: Some((util::user_shell(), vec!["-c".into(), c.run.clone()])),
                    cwd: ctx.active_cwd(),
                    keep_shell: true,
                })
            } else {
                Action::RunDetached { program: util::user_shell(), args: vec!["-l".into(), "-c".into(), c.run.clone()] }
            };
            v.push(Command::new(PaletteMode::All, Icon::Bolt, c.name.clone(), c.detail.clone().unwrap_or_else(|| c.run.clone()), action));
        }
        v
    }
}
