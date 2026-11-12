//! winit key events → bytes for the PTY (xterm conventions, macOS niceties).

use alacritty_terminal::term::TermMode;
use winit::event::{ElementState, KeyEvent, Modifiers};
use winit::keyboard::{Key, KeyCode, ModifiersKeyState, NamedKey, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;

use crate::config::OptionAsAlt;

fn csi_mod(shift: bool, alt: bool, ctrl: bool) -> u8 {
    1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8
}

/// The US-QWERTY character of the physical key (layout-independent).
pub fn physical_char(ev: &KeyEvent) -> Option<char> {
    let PhysicalKey::Code(code) = ev.physical_key else { return None };
    use KeyCode::*;
    Some(match code {
        KeyA => 'a', KeyB => 'b', KeyC => 'c', KeyD => 'd', KeyE => 'e', KeyF => 'f', KeyG => 'g',
        KeyH => 'h', KeyI => 'i', KeyJ => 'j', KeyK => 'k', KeyL => 'l', KeyM => 'm', KeyN => 'n',
        KeyO => 'o', KeyP => 'p', KeyQ => 'q', KeyR => 'r', KeyS => 's', KeyT => 't', KeyU => 'u',
        KeyV => 'v', KeyW => 'w', KeyX => 'x', KeyY => 'y', KeyZ => 'z',
        Digit0 => '0', Digit1 => '1', Digit2 => '2', Digit3 => '3', Digit4 => '4',
        Digit5 => '5', Digit6 => '6', Digit7 => '7', Digit8 => '8', Digit9 => '9',
        Minus => '-', Equal => '=', BracketLeft => '[', BracketRight => ']', Backslash => '\\',
        Semicolon => ';', Quote => '\'', Comma => ',', Period => '.', Slash => '/', Backquote => '`',
        _ => return None,
    })
}

/// Key for shortcut matching: the layout's character when it is ASCII (so Dvorak etc.
/// behave as typed), otherwise the physical key (so ⌘C / ⌘T / ⌘1 work on Russian,
/// Greek, Hebrew… layouts — like every other Mac app).
pub fn shortcut_key(ev: &KeyEvent) -> String {
    if let Key::Character(s) = ev.key_without_modifiers() {
        let s = s.to_lowercase();
        if !s.is_empty() && s.is_ascii() {
            return s;
        }
    }
    physical_char(ev).map(|c| c.to_string()).unwrap_or_default()
}

pub fn encode(ev: &KeyEvent, mods: &Modifiers, mode: TermMode, opt: OptionAsAlt) -> Option<Vec<u8>> {
    if ev.state != ElementState::Pressed {
        return None;
    }
    let st = mods.state();
    let shift = st.shift_key();
    let ctrl = st.control_key();
    let cmd = st.super_key();
    let alt_l = mods.lalt_state() == ModifiersKeyState::Pressed;
    let alt_r = mods.ralt_state() == ModifiersKeyState::Pressed;
    let any_alt = st.alt_key();
    let alt_meta = match opt {
        OptionAsAlt::Both => any_alt,
        OptionAsAlt::Left => alt_l || (any_alt && !alt_r && !alt_l),
        OptionAsAlt::Right => alt_r,
        OptionAsAlt::None => false,
    };
    let app_cursor = mode.contains(TermMode::APP_CURSOR);

    if let Key::Named(named) = &ev.logical_key {
        let m = csi_mod(shift, any_alt, ctrl);
        let has_mods = m > 1;
        let arrow = |c: char| -> Vec<u8> {
            if has_mods {
                format!("\x1b[1;{m}{c}").into_bytes()
            } else if app_cursor {
                format!("\x1bO{c}").into_bytes()
            } else {
                format!("\x1b[{c}").into_bytes()
            }
        };
        let tilde = |n: u8| -> Vec<u8> {
            if has_mods { format!("\x1b[{n};{m}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
        };
        let ss3 = |c: char| -> Vec<u8> {
            if has_mods { format!("\x1b[1;{m}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
        };
        let out: Option<Vec<u8>> = match named {
            NamedKey::Enter => Some(if shift || any_alt { b"\x1b\r".to_vec() } else { b"\r".to_vec() }),
            NamedKey::Tab => Some(if shift { b"\x1b[Z".to_vec() } else { b"\t".to_vec() }),
            NamedKey::Backspace => Some(if cmd {
                vec![0x15]
            } else if any_alt {
                b"\x1b\x7f".to_vec()
            } else if ctrl {
                vec![0x08]
            } else {
                vec![0x7f]
            }),
            NamedKey::Escape => Some(vec![0x1b]),
            NamedKey::Space => Some(if ctrl {
                vec![0]
            } else if alt_meta {
                b"\x1b ".to_vec()
            } else {
                b" ".to_vec()
            }),
            NamedKey::ArrowUp => Some(arrow('A')),
            NamedKey::ArrowDown => Some(arrow('B')),
            NamedKey::ArrowRight => Some(if cmd {
                vec![0x05]
            } else if any_alt && !shift && !ctrl {
                b"\x1bf".to_vec()
            } else {
                arrow('C')
            }),
            NamedKey::ArrowLeft => Some(if cmd {
                vec![0x01]
            } else if any_alt && !shift && !ctrl {
                b"\x1bb".to_vec()
            } else {
                arrow('D')
            }),
            NamedKey::Home => Some(if has_mods { arrow('H') } else if app_cursor { b"\x1bOH".to_vec() } else { b"\x1b[H".to_vec() }),
            NamedKey::End => Some(if has_mods { arrow('F') } else if app_cursor { b"\x1bOF".to_vec() } else { b"\x1b[F".to_vec() }),
            NamedKey::PageUp => Some(tilde(5)),
            NamedKey::PageDown => Some(tilde(6)),
            NamedKey::Insert => Some(tilde(2)),
            NamedKey::Delete => Some(tilde(3)),
            NamedKey::F1 => Some(ss3('P')),
            NamedKey::F2 => Some(ss3('Q')),
            NamedKey::F3 => Some(ss3('R')),
            NamedKey::F4 => Some(ss3('S')),
            NamedKey::F5 => Some(tilde(15)),
            NamedKey::F6 => Some(tilde(17)),
            NamedKey::F7 => Some(tilde(18)),
            NamedKey::F8 => Some(tilde(19)),
            NamedKey::F9 => Some(tilde(20)),
            NamedKey::F10 => Some(tilde(21)),
            NamedKey::F11 => Some(tilde(23)),
            NamedKey::F12 => Some(tilde(24)),
            _ => None,
        };
        if out.is_some() {
            return out;
        }
    }

    if cmd {
        return None;
    }

    // Character keys. For Ctrl / Meta combos a non-Latin layout falls back to the physical
    // key, so Ctrl+C is ^C and Option+B is ESC b whatever the input language.
    let base = match ev.key_without_modifiers() {
        Key::Character(s) if (ctrl || alt_meta) && !s.is_ascii() => match physical_char(ev) {
            Some(c) => Key::Character(c.to_string().into()),
            None => Key::Character(s),
        },
        other => other,
    };
    if ctrl {
        if let Key::Character(s) = &base {
            if let Some(c) = s.chars().next() {
                let b = match c.to_ascii_lowercase() {
                    c @ 'a'..='z' => Some(c as u8 - b'a' + 1),
                    '@' | '2' | ' ' => Some(0),
                    '[' | '3' => Some(0x1b),
                    '\\' | '4' => Some(0x1c),
                    ']' | '5' => Some(0x1d),
                    '^' | '6' => Some(0x1e),
                    '_' | '-' | '/' | '7' => Some(0x1f),
                    '8' => Some(0x7f),
                    _ => None,
                };
                if let Some(b) = b {
                    return Some(if alt_meta { vec![0x1b, b] } else { vec![b] });
                }
            }
        }
    }
    if alt_meta {
        if let Key::Character(s) = &base {
            let s = if shift { s.to_uppercase() } else { s.to_string() };
            let mut v = vec![0x1b];
            v.extend_from_slice(s.as_bytes());
            return Some(v);
        }
    }
    let text = ev.text.as_ref()?;
    if text.is_empty() || text.chars().all(|c| c.is_control() && c != '\r' && c != '\t' && c != '\x1b') {
        return None;
    }
    Some(text.as_bytes().to_vec())
}
