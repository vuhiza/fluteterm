# fluteterm

A drop-down terminal for macOS, written in Rust. It slides down from under the menu bar when
you press **F12**, like Yakuake/Guake on Linux. It looks like pixel art but isn't: text is
smooth, and the pixel feel comes from crisp 1px details, dithered textures and stepped motion.
Claude Code and Orca are built in but optional. Their tabs, status segments, palette entries and
menu items only appear when the tool is installed and switched on in settings, so on a Mac without
them it's just a clean drop-down terminal.


## Install

```sh
./scripts/install.sh            # builds, installs ~/Applications/fluteterm.app + ~/.local/bin/fluteterm, launches it
./scripts/install.sh --claude   # …and wires up Claude Code hooks
./scripts/install.sh --login    # …and starts fluteterm at login
```

To build a drag-to-Applications disk image instead:

```sh
./scripts/dmg.sh                # → dist/fluteterm-<version>-<arch>.dmg (ad-hoc signed)
```

Press **F12** to toggle. On Mac laptop keyboards F12 is a media key by default, so either press
**fn+F12** or set another key in the config (`hotkey = "Alt+Space"`), or enable *System Settings →
Keyboard → Use F1, F2, etc. keys as standard function keys*. Left-click the ▼ menu-bar icon to
toggle; right-click it for the menu.

Requirements: macOS 12+, Rust 1.85+ (only for building).

## Keys

| key | action |
|---|---|
| F12 (global) | drop down / pull up |
| ⌘T / ⌘W | new tab (same directory) / close tab |
| ⌘1…⌘9, ⌘⇧[ ⌘⇧], ⌃Tab | switch tabs |
| ⌘K (or ⌘⇧P) | command palette |
| ⌘⇧C | new Claude Code session in the current directory |
| ⌘⇧O | Orca worktrees and agents (⏎ shell there, ⌘⏎ Claude there) |
| ⌘⇧A | agent commands (/compact, /clear, resume, go to agent…) |
| ⌘C / ⌘V, right-click | copy selection / paste (bracketed) |
| ⌘+ ⌘- ⌘0 | text size |
| ⌘, | settings window (⌘K → "edit config file" for the raw TOML) |
| ⌘Q / ⌘H | hide (quit from the menu-bar menu or palette) |
| ⇧PgUp/PgDn/Home/End, wheel | scrollback |
| ⌘-click | open URL / path |
| drag the bottom grip | resize the panel (saved) |
| Option | Meta/Alt (left Option by default, `option_as_alt`) |
| ⇧⏎ | newline in Claude Code (sends ESC CR) |

## Persistent sessions

Tabs survive quitting fluteterm, restarts and (partly) reboots:

- **tmux backend** (default when `tmux` is installed): every tab is a session on a private tmux server
  (`tmux -L fluteterm`) with tmux's UI fully hidden: no status bar, no prefix key. **Quit fluteterm** and
  shells, Claude sessions, builds and scrollback keep running. The next start re-attaches to all of
  them, with the same tabs, order, active tab and directories. Inside these tabs the mouse wheel
  scrolls tmux history, dragging copies to the clipboard, and ⇧-drag uses fluteterm's own selection.
- **After a reboot** (processes gone): tabs come back in their directories with their titles, and
  Claude tabs resume their conversation with `claude --resume <session id>`.
- **plain backend** (`[session] backend = "plain"`, or no tmux): the same layout restore, plus the
  last 2000 lines of each tab replayed dimmed above a `── restored ──` marker.

⌘W ends a tab for good. The menu bar has **Quit fluteterm (tabs keep running)** and **Quit and close
all tabs**. State lives in `~/.config/fluteterm/session.json`; add your own tmux settings in
`~/.config/fluteterm/tmux.local.conf`.

## Claude Code

`fluteterm install claude` adds hooks to `~/.claude/settings.json`; a backup is made first, and
`fluteterm uninstall claude` reverts it. Inside fluteterm tabs, Claude Code then reports what it's doing:

- **Tab badge**: `✻ claude <project>` shows a spinner while it works, `⚠` when it needs
  permission or input, and ✓ when a turn finished while you were in another tab.
- **Status bar**: model, a context-window meter (read from the transcript), the current
  tool (`Bash(cargo check)`), turn time and output tokens.
- **Notifications**:
  - When the panel is hidden, you get a Notification Center banner; clicking it drops the panel into that tab.
  - When the panel is open, you get a pixel pop-up with **allow ⏎ / deny ⎋ / go to tab**. Answering
    from there sends the key to that Claude session, so you don't have to switch tabs.
- **Status bar, on every tab** (right side, all clickable):
  - `✻ 2 live · 1 working · 1 needs you`: clicking jumps to the session that needs you, or opens the agents list.
  - `2h 58m left in 5h window · 44.7M`: the current 5-hour usage window that Claude subscription limits
    are counted in, with a meter for elapsed time and the tokens used in it.
  - `today 76.1M`: today's tokens.

  The window and today's total cover **every** Claude Code session on this Mac (fluteterm, other terminals,
  Orca agents). They come from `~/.claude/projects/**/*.jsonl`, read incrementally and de-duplicated.
  Turn them off with `[claude] status_usage = false` or in settings → claude.
- **Palette**: new session / `--continue` / `--resume`, send `/compact`, `/clear`, `/cost`, `/model`,
  jump to any Claude tab.

Hooks are no-ops outside fluteterm, because they need `$FLUTETERM_TAB_ID`. Without hooks, Claude tabs are
still recognised by their process and window title.

## Orca

The `orca` extension polls `orca worktree ps --json`: every 5 s while the panel is visible, every 20 s
while hidden.

- **Status bar**: `◆ busy/total agents · N wt`.
- **Tabs**: a shell inside an Orca worktree is labelled with the worktree name and shows dots for its busy agents.
- **⌘⇧O**: worktrees with status and agents:
  - **worktree row**: ⏎ opens a shell there, ⌘⏎ runs Claude there.
  - **agent row**: ⏎ **attaches** the agent's Orca terminal in a fluteterm tab, ⌘⏎ shows it in Orca.
    Attach is a live mirror: `orca terminal read --screen` about 4×/s, with your keystrokes forwarded
    by `orca terminal send`. `ctrl-]` detaches and the agent keeps running.
  - **launch**: starts an Orca agent in the background, then a pop-up reports the result (or Orca's error)
    with **attach here / show in Orca** buttons.
  - "orca open" and "show in Orca" pull the panel up so the Orca app is actually visible.
- **Notifications** when an Orca agent finishes or starts waiting for you.
- **Menu-bar menu**: a live list of Orca agents.

## Customising

Everything lives in `~/.config/fluteterm/config.toml`, which is created with comments on first run and
reloaded live. Highlights:

```toml
hotkey = "F12"
theme = "graphite"          # graphite clay moss glacier orchid blood paper, or your own
accent = "#e0a030"          # optional: override the theme's accent (omit = theme default)
[font]   family = "JetBrains Mono"  size = 14  line_height = 1.2
[window] height = 0.5  shape = "strip"  opacity = 0.86  blur = 14  crt = false
[motion] pattern = "bayer"  cell = 2  open_ms = 220  close_ms = 160
```

**Themes** are one accent colour plus optional overrides (`~/.config/fluteterm/themes/<name>.toml`;
see `example.toml`). Every tone is derived from the accent, the same way as in the design board.

**Code-free extensions** in config.toml:

```toml
[[profile]]          # new tab type, in the palette, optional shortcut
name = "htop"
command = "htop"
key = "cmd+shift+h"

[[module]]           # status-bar segment from any command
name = "battery"
command = "pmset -g batt | grep -Eo '[0-9]+%'"
interval = 60

[[command]]          # palette entry
name = "deploy staging"
run = "make deploy-staging"
tab = true
```

## Scripting / IPC

The binary is also its own remote control. Commands go over a unix socket at
`~/.config/fluteterm/fluteterm.sock` as newline-delimited JSON.

```sh
fluteterm toggle | show | hide
fluteterm open claude --cwd ~/Projects/x        # or: fluteterm open -- htop
fluteterm send --tab 3 'cargo test\n'
fluteterm notify "build" "green" --level ok
fluteterm status ci "✓ green"                    # custom status segment (empty text clears)
fluteterm list                                   # tabs as JSON
fluteterm palette orca
fluteterm snapshot /tmp/frame.png                # PNG of the current frame
```

Every tab exports `FLUTETERM_TAB_ID`, `FLUTETERM_SOCKET`, `FLUTETERM_BIN` and `TERM_PROGRAM=fluteterm`, so
scripts running inside a tab can address it (e.g. `fluteterm notify done --tab $FLUTETERM_TAB_ID`).
Any tool can send `{"cmd":"event","source":"<name>","tab":N,"payload":{…}}`. Built-in extensions
pick up the sources they own, and new Rust extensions can claim new ones.

## Architecture

```
src/
  main.rs          entry
  cli.rs           CLI, hook entry point, installers
  app/mod.rs       winit app: window, show/hide animation, tabs, input, actions, IPC dispatch
  app/draw.rs      frame composition: panel, tab bar, terminal grid, status bar, toasts, palette, dissolve
  app/palette.rs   command palette state + fuzzy matching
  term/            alacritty_terminal sessions (PTY), key encoding
  render/          canvas (premultiplied ARGB), text (fontdue + lazy system fallbacks),
                   procedural glyphs (box drawing, blocks, shades-as-dither, braille, Claude symbols),
                   pixel icons
  ext/             extension trait + built-ins: claude, orca, user (config-defined), core
  platform/        macOS: window level/Spaces/blur/activation, screens, tray, notifications
  ipc.rs           socket server/client
  config.rs, theme.rs
vendor/softbuffer  softbuffer 0.4.8 with three patches: alpha channel (transparency),
                   screen colour space (no per-frame colour conversion), buffer recycling
```

**Writing an extension** means implementing `ext::Extension`. It observes IPC events, its own
background messages and a periodic `tick`. It contributes palette commands, tab decorations,
status segments and tray lines, and it acts by pushing `Action`s. Register it in `App::new`.
`ext/orca.rs` is a complete example with a background poller.

Rendering is CPU-only into a shared buffer: about 6–10% CPU on an M-series Mac while something animates,
0% while idle or hidden, and a ~70 MB footprint.
