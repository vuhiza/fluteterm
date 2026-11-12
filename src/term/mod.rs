//! Terminal sessions: an alacritty_terminal `Term` + PTY event loop per tab.

pub mod keys;

use std::borrow::Cow;
use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::tty::{self, Shell};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;
use crate::util;

#[derive(Clone)]
pub struct Listener {
    pub tab: u64,
    pub proxy: EventLoopProxy<UserEvent>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.proxy.send_event(UserEvent::Term(self.tab, event));
    }
}

/// What to run in a new session.
#[derive(Clone, Debug, Default)]
pub struct Spawn {
    /// Program + args to run *inside the user's shell* (so PATH/rc files apply).
    /// None = plain interactive shell.
    pub command: Option<(String, Vec<String>)>,
    pub cwd: Option<PathBuf>,
    /// Keep a shell running after the command exits.
    pub keep_shell: bool,
    /// Run `command` directly instead of through the user's shell (used for tmux).
    pub direct: bool,
    pub env: Vec<(String, String)>,
}

/// The command line a tab runs inside the user's shell (`… ; exec $SHELL -l` keeps a shell after it).
pub fn command_line(cfg: &crate::config::Config, cmd: &str, args: &[String], keep_shell: bool) -> String {
    let shell = cfg.shell.program.clone().unwrap_or_else(util::user_shell);
    let shell_args = if cfg.shell.args.is_empty() { vec!["-l".to_string()] } else { cfg.shell.args.clone() };
    let mut line = std::iter::once(cmd).chain(args.iter().map(String::as_str)).map(util::shell_quote).collect::<Vec<_>>().join(" ");
    if keep_shell {
        line = format!("{line}; exec {} {}", util::shell_quote(&shell), shell_args.join(" "));
    }
    format!("{} -l -i -c {}", util::shell_quote(&shell), util::shell_quote(&line))
}

pub struct Session {
    pub id: u64,
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    pub child_pid: i32,
    master_fd: i32,
    pub cols: usize,
    pub rows: usize,
}

impl Session {
    pub fn spawn(
        id: u64,
        spec: &Spawn,
        cfg: &crate::config::Config,
        cols: usize,
        rows: usize,
        cell: (u16, u16),
        proxy: EventLoopProxy<UserEvent>,
    ) -> anyhow::Result<Self> {
        let shell = cfg.shell.program.clone().unwrap_or_else(util::user_shell);
        let shell_args = if cfg.shell.args.is_empty() { vec!["-l".to_string()] } else { cfg.shell.args.clone() };

        let program = match &spec.command {
            None => Shell::new(shell.clone(), shell_args.clone()),
            Some((cmd, args)) if spec.direct => Shell::new(cmd.clone(), args.clone()),
            Some((cmd, args)) => {
                let mut line = std::iter::once(cmd.as_str())
                    .chain(args.iter().map(String::as_str))
                    .map(util::shell_quote)
                    .collect::<Vec<_>>()
                    .join(" ");
                if spec.keep_shell {
                    line = format!("{line}; exec {} {}", util::shell_quote(&shell), shell_args.join(" "));
                }
                // -i so .zshrc (PATH additions like ~/.local/bin) is loaded.
                Shell::new(shell.clone(), vec!["-l".into(), "-i".into(), "-c".into(), line])
            }
        };

        let mut env: HashMap<String, String> = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "fluteterm".into());
        env.insert("TERM_PROGRAM_VERSION".into(), env!("CARGO_PKG_VERSION").into());
        env.insert("FLUTETERM_TAB_ID".into(), id.to_string());
        env.insert("FLUTETERM_SOCKET".into(), util::socket_path().display().to_string());
        if let Ok(exe) = std::env::current_exe() {
            env.insert("FLUTETERM_BIN".into(), exe.display().to_string());
        }
        if std::env::var("LANG").map(|l| l.is_empty()).unwrap_or(true) {
            env.insert("LANG".into(), "en_US.UTF-8".into());
        }
        env.insert("PATH".into(), util::augmented_path());
        for (k, v) in &spec.env {
            env.insert(k.clone(), v.clone());
        }

        let cwd = spec
            .cwd
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| cfg.shell.cwd.as_deref().map(util::expand_tilde).filter(|p| p.is_dir()))
            .unwrap_or_else(util::home);

        let opts = tty::Options { shell: Some(program), working_directory: Some(cwd), drain_on_exit: true, env };
        let ws = WindowSize { num_lines: rows as u16, num_cols: cols as u16, cell_width: cell.0, cell_height: cell.1 };
        let pty = tty::new(&opts, ws, id)?;
        let child_pid = pty.child().id() as i32;
        let master_fd = unsafe { libc::dup(pty.file().as_raw_fd()) };

        let listener = Listener { tab: id, proxy };
        let tcfg = TermConfig { scrolling_history: cfg.scrollback, ..Default::default() };
        let term = Arc::new(FairMutex::new(Term::new(tcfg, &TermSize::new(cols, rows), listener.clone())));
        let el = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = el.channel();
        el.spawn();
        Ok(Self { id, term, sender, child_pid, master_fd, cols, rows })
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.sender.send(Msg::Input(bytes.into()));
    }

    pub fn write_str(&self, s: &str) {
        self.write(s.as_bytes().to_vec());
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell: (u16, u16)) {
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.lock().resize(TermSize::new(cols, rows));
        let _ = self.sender.send(Msg::Resize(WindowSize {
            num_lines: rows as u16,
            num_cols: cols as u16,
            cell_width: cell.0,
            cell_height: cell.1,
        }));
    }

    pub fn foreground(&self) -> Option<i32> {
        util::foreground_pid(self.master_fd)
    }

    /// Name of the foreground process (e.g. "zsh", "claude", "vim").
    pub fn foreground_name(&self) -> Option<String> {
        self.foreground().and_then(util::process_name)
    }

    /// Working directory of the foreground process, falling back to the shell.
    pub fn cwd(&self) -> Option<PathBuf> {
        self.foreground().and_then(util::process_cwd).or_else(|| util::process_cwd(self.child_pid))
    }

    pub fn shutdown(&self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
        unsafe {
            libc::close(self.master_fd);
        }
    }
}
