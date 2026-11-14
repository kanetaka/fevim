//! GPUI keystrokes -> fevim keys.

use gpui::Keystroke;

use crate::editor::keys::{ALT, CTRL, Key, KeyCode, META, SHIFT, Sp};

/// Keys that produce text are left to the platform input handler so that
/// input methods (IME) work.  Everything else is handled on key down.
pub fn is_text_input(ks: &Keystroke) -> bool {
    let m = &ks.modifiers;
    if m.platform || m.function {
        return false;
    }
    if m.control && !m.alt {
        return false;
    }
    // Windows AltGr arrives as ctrl+alt with a character
    if m.alt && !(cfg!(windows) && m.control) && !cfg!(target_os = "macos") {
        return false;
    }
    if matches!(ks.key.as_str(), "space" | "enter" | "tab" | "escape" | "backspace" | "delete") {
        return false;
    }
    match &ks.key_char {
        Some(c) => !c.is_empty() && !c.chars().any(|ch| ch.is_control()),
        None => false,
    }
}

/// US-layout shifted characters, used when ctrl hides the typed character.
fn shifted(c: char) -> char {
    match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        c => c.to_ascii_uppercase(),
    }
}

pub fn to_key(ks: &Keystroke) -> Option<Key> {
    let m = &ks.modifiers;
    let mut mods = 0u8;
    if m.shift {
        mods |= SHIFT;
    }
    if m.control {
        mods |= CTRL;
    }
    if m.alt {
        mods |= ALT;
    }
    if m.platform {
        mods |= META;
    }
    let code = match ks.key.as_str() {
        "enter" => KeyCode::Char('\r'),
        "escape" => KeyCode::Char('\x1b'),
        "tab" => KeyCode::Char('\t'),
        "space" => KeyCode::Char(' '),
        "backspace" => KeyCode::Sp(Sp::Backspace),
        "delete" => KeyCode::Sp(Sp::Delete),
        "insert" => KeyCode::Sp(Sp::Insert),
        "up" => KeyCode::Sp(Sp::Up),
        "down" => KeyCode::Sp(Sp::Down),
        "left" => KeyCode::Sp(Sp::Left),
        "right" => KeyCode::Sp(Sp::Right),
        "home" => KeyCode::Sp(Sp::Home),
        "end" => KeyCode::Sp(Sp::End),
        "pageup" => KeyCode::Sp(Sp::PageUp),
        "pagedown" => KeyCode::Sp(Sp::PageDown),
        "shift" | "control" | "alt" | "platform" | "function" | "capslock" | "menu" | "back" | "forward" => return None,
        k if k.len() > 1 && k.starts_with('f') && k[1..].chars().all(|c| c.is_ascii_digit()) => KeyCode::Sp(Sp::F(k[1..].parse().ok()?)),
        k => {
            let mut chars = k.chars();
            let first = chars.next()?;
            if chars.next().is_some() {
                // a named key we do not know; use the typed character if any
                let c = ks.key_char.as_ref()?.chars().next()?;
                KeyCode::Char(c)
            } else if m.control || m.alt || m.platform {
                // the typed character may be hidden by the modifier
                let c = match (&ks.key_char, m.control) {
                    (Some(kc), false) if !kc.is_empty() && !kc.chars().next().unwrap().is_control() => kc.chars().next().unwrap(),
                    _ => {
                        if m.shift {
                            shifted(first)
                        } else {
                            first
                        }
                    }
                };
                if m.shift && !c.is_ascii_alphabetic() {
                    mods &= !SHIFT;
                }
                KeyCode::Char(c)
            } else {
                let c = ks.key_char.as_ref().and_then(|s| s.chars().next()).unwrap_or(if m.shift { shifted(first) } else { first });
                mods &= !SHIFT;
                KeyCode::Char(c)
            }
        }
    };
    // Shift only matters for special keys and Space/Tab/CR (<S-Tab>, <S-CR> ...)
    if let KeyCode::Char(c) = code {
        if !matches!(c, ' ' | '\t' | '\r' | '\x1b') && !c.is_ascii_alphabetic() {
            mods &= !SHIFT;
        }
    }
    Some(Key::new(code, mods))
}

/// Text from the input handler as keys (newlines become <CR>).
pub fn text_to_keys(text: &str) -> Vec<Key> {
    text.chars()
        .filter(|c| *c != '\n' || true)
        .map(|c| match c {
            '\n' | '\r' => Key::ch('\r'),
            c => Key::ch(c),
        })
        .collect()
}
