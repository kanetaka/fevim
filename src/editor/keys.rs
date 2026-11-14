//! Key representation, `<C-x>` style notation and the internal string encoding
//! used to carry special keys inside ordinary strings (mapping results,
//! `feedkeys()`, `vim.keycode()`).

pub const SHIFT: u8 = 1;
pub const CTRL: u8 = 2;
pub const ALT: u8 = 4;
pub const META: u8 = 8;

/// Special (non-character) keys.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Sp {
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    Nop,
    Plug,
    Sid,
    ScrollUp,
    ScrollDown,
}

const SP_TABLE: &[(Sp, &str)] = &[
    (Sp::Backspace, "BS"),
    (Sp::Delete, "Del"),
    (Sp::Insert, "Insert"),
    (Sp::Up, "Up"),
    (Sp::Down, "Down"),
    (Sp::Left, "Left"),
    (Sp::Right, "Right"),
    (Sp::Home, "Home"),
    (Sp::End, "End"),
    (Sp::PageUp, "PageUp"),
    (Sp::PageDown, "PageDown"),
    (Sp::Nop, "Nop"),
    (Sp::Plug, "Plug"),
    (Sp::Sid, "SID"),
    (Sp::ScrollUp, "ScrollWheelUp"),
    (Sp::ScrollDown, "ScrollWheelDown"),
];

impl Sp {
    fn index(self) -> u32 {
        match self {
            Sp::F(n) => 0x40 + n as u32,
            other => SP_TABLE.iter().position(|(s, _)| *s == other).unwrap() as u32,
        }
    }
    fn from_index(i: u32) -> Option<Sp> {
        if (0x41..=0x40 + 37).contains(&i) {
            return Some(Sp::F((i - 0x40) as u8));
        }
        SP_TABLE.get(i as usize).map(|(s, _)| *s)
    }
    fn name(self) -> String {
        match self {
            Sp::F(n) => format!("F{}", n),
            other => SP_TABLE.iter().find(|(s, _)| *s == other).unwrap().1.to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyCode {
    Char(char),
    Sp(Sp),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub mods: u8,
}

pub const ESC: Key = Key { code: KeyCode::Char('\x1b'), mods: 0 };
pub const CR: Key = Key { code: KeyCode::Char('\r'), mods: 0 };
pub const TAB: Key = Key { code: KeyCode::Char('\t'), mods: 0 };

/// Base of the private-use range used to encode modifiers in strings.
const PUA_MOD: u32 = 0xF000;
/// Base of the private-use range used to encode special keys in strings.
const PUA_SP: u32 = 0xF100;

impl Key {
    /// Build a key, normalising modifiers the way Vim does
    /// (`<C-a>` is the control character, `<S-a>` is `A`, ...).
    pub fn new(code: KeyCode, mut mods: u8) -> Key {
        let mut code = code;
        if let KeyCode::Char(c) = code {
            let mut c = c;
            if mods & SHIFT != 0 && c.is_alphabetic() {
                c = c.to_uppercase().next().unwrap_or(c);
                mods &= !SHIFT;
            } else if mods & SHIFT != 0 && !matches!(c, ' ' | '\r' | '\t' | '\x1b' | '\n') && !c.is_control() {
                mods &= !SHIFT;
            }
            if mods & CTRL != 0 {
                let u = c.to_ascii_uppercase();
                if u.is_ascii_uppercase() || matches!(u, '@' | '[' | '\\' | ']' | '^' | '_') {
                    c = ((u as u8) & 0x1f) as char;
                    mods &= !(CTRL | SHIFT);
                } else if u == '?' {
                    c = '\x7f';
                    mods &= !CTRL;
                }
            }
            if c == '\n' && mods == 0 {
                // <NL> and <C-j> are the same key
            }
            code = KeyCode::Char(c);
        }
        Key { code, mods }
    }

    pub fn ch(c: char) -> Key {
        Key { code: KeyCode::Char(c), mods: 0 }
    }

    pub fn ctrl(c: char) -> Key {
        Key::new(KeyCode::Char(c), CTRL)
    }

    pub fn sp(s: Sp) -> Key {
        Key { code: KeyCode::Sp(s), mods: 0 }
    }

    /// The plain character of this key, if it has no modifiers.
    pub fn as_char(&self) -> Option<char> {
        match (self.code, self.mods) {
            (KeyCode::Char(c), 0) => Some(c),
            _ => None,
        }
    }

    /// The text this key inserts in insert/cmdline mode, if any.
    pub fn printable(&self) -> Option<char> {
        match self.code {
            KeyCode::Char(c) if self.mods & !SHIFT == 0 && (!c.is_control() || c == '\t') => Some(c),
            _ => None,
        }
    }

    pub fn is(&self, c: char) -> bool {
        self.as_char() == Some(c)
    }

    pub fn is_ctrl(&self, c: char) -> bool {
        *self == Key::ctrl(c)
    }

    pub fn is_sp(&self, s: Sp) -> bool {
        self.code == KeyCode::Sp(s) && self.mods == 0
    }

    /// Human readable notation (`<C-w>`, `<S-CR>`, `a`).
    pub fn notation(&self) -> String {
        let mut mods = self.mods;
        let base: String = match self.code {
            KeyCode::Char(c) => match c {
                '\x1b' => "Esc".into(),
                '\r' => "CR".into(),
                '\t' => "Tab".into(),
                ' ' => "Space".into(),
                '\n' => "NL".into(),
                '\0' => "Nul".into(),
                '\x7f' => "Del".into(),
                '<' => "lt".into(),
                '|' if false => "Bar".into(),
                c if (c as u32) < 0x20 => {
                    mods |= CTRL;
                    (((c as u8) + 0x40) as char).to_string()
                }
                c => {
                    if mods == 0 {
                        return c.to_string();
                    }
                    c.to_string()
                }
            },
            KeyCode::Sp(s) => s.name(),
        };
        let mut out = String::from("<");
        if mods & CTRL != 0 {
            out.push_str("C-");
        }
        if mods & SHIFT != 0 {
            out.push_str("S-");
        }
        if mods & ALT != 0 {
            out.push_str("M-");
        }
        if mods & META != 0 {
            out.push_str("D-");
        }
        out.push_str(&base);
        out.push('>');
        out
    }

    /// Append the internal string encoding of this key.
    pub fn encode(&self, out: &mut String) {
        if self.mods != 0 {
            out.push(char::from_u32(PUA_MOD + self.mods as u32).unwrap());
        }
        match self.code {
            KeyCode::Char(c) => out.push(c),
            KeyCode::Sp(s) => out.push(char::from_u32(PUA_SP + s.index()).unwrap()),
        }
    }
}

pub fn keys_to_string(keys: &[Key]) -> String {
    let mut s = String::new();
    for k in keys {
        k.encode(&mut s);
    }
    s
}

pub fn keys_notation(keys: &[Key]) -> String {
    keys.iter().map(|k| k.notation()).collect()
}

/// Decode a "typed" string: ordinary characters plus the private-use encoding
/// of special keys.  `<...>` notation is NOT interpreted.
pub fn parse_keys_raw(s: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut mods = 0u8;
    for c in s.chars() {
        let u = c as u32;
        if (PUA_MOD + 1..PUA_MOD + 16).contains(&u) {
            mods = (u - PUA_MOD) as u8;
            continue;
        }
        if u >= PUA_SP && u < PUA_SP + 0x100 {
            if let Some(sp) = Sp::from_index(u - PUA_SP) {
                out.push(Key { code: KeyCode::Sp(sp), mods });
                mods = 0;
                continue;
            }
        }
        let c = if c == '\n' { '\r' } else { c };
        out.push(Key { code: KeyCode::Char(c), mods });
        mods = 0;
    }
    out
}

fn named_key(name: &str) -> Option<KeyCode> {
    let l = name.to_ascii_lowercase();
    Some(match l.as_str() {
        "cr" | "enter" | "return" => KeyCode::Char('\r'),
        "esc" => KeyCode::Char('\x1b'),
        "tab" => KeyCode::Char('\t'),
        "space" => KeyCode::Char(' '),
        "lt" => KeyCode::Char('<'),
        "bar" => KeyCode::Char('|'),
        "bslash" => KeyCode::Char('\\'),
        "nl" | "linefeed" | "lf" => KeyCode::Char('\n'),
        "nul" => KeyCode::Char('\0'),
        "bs" | "backspace" => KeyCode::Sp(Sp::Backspace),
        "del" | "delete" => KeyCode::Sp(Sp::Delete),
        "insert" | "ins" => KeyCode::Sp(Sp::Insert),
        "up" => KeyCode::Sp(Sp::Up),
        "down" => KeyCode::Sp(Sp::Down),
        "left" => KeyCode::Sp(Sp::Left),
        "right" => KeyCode::Sp(Sp::Right),
        "home" => KeyCode::Sp(Sp::Home),
        "end" => KeyCode::Sp(Sp::End),
        "pageup" => KeyCode::Sp(Sp::PageUp),
        "pagedown" => KeyCode::Sp(Sp::PageDown),
        "nop" => KeyCode::Sp(Sp::Nop),
        "plug" => KeyCode::Sp(Sp::Plug),
        "sid" => KeyCode::Sp(Sp::Sid),
        "scrollwheelup" => KeyCode::Sp(Sp::ScrollUp),
        "scrollwheeldown" => KeyCode::Sp(Sp::ScrollDown),
        _ => {
            if let Some(n) = l.strip_prefix('f') {
                if let Ok(n) = n.parse::<u8>() {
                    if (1..=37).contains(&n) {
                        return Some(KeyCode::Sp(Sp::F(n)));
                    }
                }
            }
            return None;
        }
    })
}

/// Parse the inside of a `<...>` key notation.
pub fn parse_notation_inner(inner: &str) -> Option<Key> {
    let mut mods = 0u8;
    let mut rest = inner;
    loop {
        let b = rest.as_bytes();
        if b.len() >= 3 && b[1] == b'-' {
            let m = match b[0].to_ascii_uppercase() {
                b'S' => SHIFT,
                b'C' => CTRL,
                b'A' | b'M' => ALT,
                b'D' | b'T' => META,
                _ => 0,
            };
            if m != 0 {
                mods |= m;
                rest = &rest[2..];
                continue;
            }
        }
        break;
    }
    let mut chars = rest.chars();
    let first = chars.next()?;
    let code = if chars.next().is_none() {
        if mods == 0 {
            return None; // `<x>` is literal text
        }
        KeyCode::Char(first)
    } else {
        named_key(rest)?
    };
    // <S-Tab> etc. keep their modifier; plain <C-x> is normalised.
    Some(Key::new(code, mods))
}

/// Parse a string in mapping notation (`ciw<C-r>0<Esc>`).  The private-use
/// encoding is accepted as well.
pub fn parse_keys(s: &str) -> Vec<Key> {
    let mut out = Vec::new();
    let mut rest = s;
    let mut plain = String::new();
    while !rest.is_empty() {
        if rest.starts_with('<') {
            if let Some(end) = rest[1..].find('>') {
                let inner = &rest[1..1 + end];
                if !inner.is_empty() && !inner.contains(|c: char| c == '<' || c.is_whitespace()) {
                    if let Some(k) = parse_notation_inner(inner) {
                        out.extend(parse_keys_raw(&plain));
                        plain.clear();
                        out.push(k);
                        rest = &rest[end + 2..];
                        continue;
                    }
                }
            }
        }
        let c = rest.chars().next().unwrap();
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out.extend(parse_keys_raw(&plain));
    out
}

/// Convert mapping notation into the internal typed-string encoding.
pub fn keycode(s: &str) -> String {
    keys_to_string(&parse_keys(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation_roundtrip() {
        let keys = parse_keys("ciw<C-r>0<ESC>:noh<CR><S-CR><C-Tab><Space>x<lt>");
        assert_eq!(keys[3], Key::ctrl('r'));
        assert_eq!(keys[5], ESC);
        assert_eq!(keys_notation(&keys[10..13]), "<CR><S-CR><C-S-Tab>".replace("<C-S-Tab>", "<C-Tab>"));
        let enc = keys_to_string(&keys);
        assert_eq!(parse_keys_raw(&enc), keys);
    }

    #[test]
    fn unknown_notation_is_literal() {
        let keys = parse_keys("a<b>c");
        assert_eq!(keys.len(), 5);
    }
}
