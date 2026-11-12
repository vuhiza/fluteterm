//! Menu-bar status item: left click toggles the panel, right click opens a
//! menu with live sessions, agents and actions.

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use winit::event_loop::EventLoopProxy;

use crate::app::UserEvent;

/// The fluteterm mark: a dropping wedge over a split line, in pixels.
const MARK: [&str; 8] = [
    "........",
    "########",
    ".######.",
    "..####..",
    "...##...",
    "........",
    "###.####",
    "........",
];

fn icon(alert: bool) -> Icon {
    let scale = 4usize;
    let size = 8 * scale;
    let mut rgba = vec![0u8; size * size * 4];
    for (y, row) in MARK.iter().enumerate() {
        for (x, ch) in row.bytes().enumerate() {
            if ch != b'#' {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let i = ((y * scale + dy) * size + x * scale + dx) * 4;
                    rgba[i + 3] = 255;
                }
            }
        }
    }
    if alert {
        // A small dot in the top-right corner.
        for y in 0..scale + 2 {
            for x in size - scale - 2..size {
                let i = (y * size + x) * 4;
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size as u32, size as u32).expect("valid icon")
}

pub struct MenuSession {
    pub id: u64,
    pub label: String,
}

pub struct MenuAgent {
    pub label: String,
}

pub struct Tray {
    icon: TrayIcon,
    last_key: String,
    alert: bool,
}

pub fn install_handlers(proxy: EventLoopProxy<UserEvent>) {
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
            let _ = p.send_event(UserEvent::TrayToggle);
        }
    }));
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let _ = proxy.send_event(UserEvent::Menu(e.id.0.clone()));
    }));
}

impl Tray {
    pub fn new() -> Option<Self> {
        let icon = TrayIconBuilder::new()
            .with_icon(icon(false))
            .with_icon_as_template(true)
            .with_menu_on_left_click(false)
            .with_tooltip("fluteterm")
            .build()
            .ok()?;
        Some(Self { icon, last_key: String::new(), alert: false })
    }

    pub fn set_alert(&mut self, alert: bool) {
        if alert != self.alert {
            self.alert = alert;
            let _ = self.icon.set_icon_with_as_template(Some(icon(alert)), true);
        }
    }

    pub fn update_menu(&mut self, hotkey: &str, sessions: &[MenuSession], agents: &[MenuAgent], dnd: bool, claude: bool, orca: bool) {
        let key = format!(
            "{hotkey}|{dnd}|{claude}|{orca}|{}|{}",
            sessions.iter().map(|s| format!("{}:{}", s.id, s.label)).collect::<Vec<_>>().join(","),
            agents.iter().map(|a| a.label.clone()).collect::<Vec<_>>().join(",")
        );
        if key == self.last_key {
            return;
        }
        self.last_key = key;
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id("toggle", format!("Toggle fluteterm    {hotkey}"), true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        if !sessions.is_empty() {
            let _ = menu.append(&MenuItem::with_id("noop", "Sessions", false, None));
            for s in sessions {
                let _ = menu.append(&MenuItem::with_id(format!("tab:{}", s.id), format!("   {}", s.label), true, None));
            }
            let _ = menu.append(&PredefinedMenuItem::separator());
        }
        if !agents.is_empty() {
            let sub = Submenu::new("Orca agents", true);
            for (i, a) in agents.iter().enumerate() {
                let _ = sub.append(&MenuItem::with_id(format!("orca:{i}"), &a.label, true, None));
            }
            let _ = menu.append(&sub);
            let _ = menu.append(&PredefinedMenuItem::separator());
        }
        let _ = menu.append(&MenuItem::with_id("new_shell", "New shell tab", true, None));
        if claude {
            let _ = menu.append(&MenuItem::with_id("new_claude", "New Claude Code session", true, None));
        }
        if orca {
            let _ = menu.append(&MenuItem::with_id("orca_panel", "Orca worktrees…", true, None));
        }
        let _ = menu.append(&MenuItem::with_id("palette", "Command palette…", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&CheckMenuItem::with_id("dnd", "Do not disturb", true, dnd, None));
        let _ = menu.append(&MenuItem::with_id("settings", "Settings…", true, None));
        let _ = menu.append(&MenuItem::with_id("config", "Open config file…", true, None));
        let _ = menu.append(&MenuItem::with_id("reload", "Reload config", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "Quit fluteterm (tabs keep running)", true, None));
        let _ = menu.append(&MenuItem::with_id("quit_all", "Quit and close all tabs", true, None));
        self.icon.set_menu(Some(Box::new(menu)));
    }
}
