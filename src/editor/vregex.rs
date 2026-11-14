//! Translation of Vim regular expressions into `regex` crate syntax.

use regex::Regex;

/// A compiled Vim pattern.
#[derive(Clone)]
pub struct VimRegex {
    pub re: Regex,
    has_zs: bool,
    has_ze: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub start: usize,
    pub end: usize,
}

impl VimRegex {
    /// Compile `pat`.  `ignorecase`/`smartcase` follow the options; `\c` and
    /// `\C` in the pattern override them.
    pub fn new(pat: &str, ignorecase: bool, smartcase: bool) -> Result<VimRegex, String> {
        let t = translate(pat)?;
        let mut ic = ignorecase;
        if ic && smartcase && t.has_upper {
            ic = false;
        }
        if let Some(force) = t.force_case {
            ic = force;
        }
        let src = if ic { format!("(?i){}", t.out) } else { t.out.clone() };
        let re = Regex::new(&src).map_err(|e| {
            let msg = e.to_string();
            format!("E486: Invalid pattern: {} ({})", pat, msg.lines().last().unwrap_or(""))
        })?;
        Ok(VimRegex { re, has_zs: t.has_zs, has_ze: t.has_ze })
    }

    fn adjust(&self, caps: &regex::Captures) -> Match {
        let m = caps.get(0).unwrap();
        let mut start = m.start();
        let mut end = m.end();
        if self.has_zs {
            if let Some(z) = caps.name("zs") {
                start = z.start();
            }
        }
        if self.has_ze {
            if let Some(z) = caps.name("ze") {
                end = z.start();
            }
        }
        Match { start, end: end.max(start) }
    }

    /// First match starting at or after byte `from`.
    pub fn find_at(&self, text: &str, from: usize) -> Option<Match> {
        if from > text.len() {
            return None;
        }
        if !self.has_zs && !self.has_ze {
            return self.re.find_at(text, from).map(|m| Match { start: m.start(), end: m.end() });
        }
        let mut at = from;
        loop {
            let caps = self.re.captures_at(text, at)?;
            let m = self.adjust(&caps);
            if m.start >= from {
                return Some(m);
            }
            at = next_boundary(text, caps.get(0).unwrap().start())?;
        }
    }

    pub fn captures_at<'t>(&self, text: &'t str, from: usize) -> Option<(Match, Vec<Option<&'t str>>)> {
        if from > text.len() {
            return None;
        }
        let caps = self.re.captures_at(text, from)?;
        let m = self.adjust(&caps);
        let mut groups = Vec::new();
        groups.push(Some(&text[m.start..m.end]));
        let mut n = 0;
        for (i, name) in self.re.capture_names().enumerate() {
            if i == 0 || name.is_some() {
                continue;
            }
            n += 1;
            if n > 9 {
                break;
            }
            groups.push(caps.get(i).map(|g| g.as_str()));
        }
        Some((m, groups))
    }

    /// All non-overlapping matches in a line.
    pub fn find_all(&self, text: &str) -> Vec<Match> {
        let mut out = Vec::new();
        let mut at = 0;
        while let Some(m) = self.find_at(text, at) {
            out.push(m);
            at = if m.end > m.start {
                m.end
            } else {
                match next_boundary(text, m.end) {
                    Some(n) => n,
                    None => break,
                }
            };
            if out.len() > 10_000 {
                break;
            }
        }
        out
    }

    pub fn is_match(&self, text: &str) -> bool {
        self.re.is_match(text)
    }
}

fn next_boundary(text: &str, i: usize) -> Option<usize> {
    if i >= text.len() {
        return None;
    }
    let mut n = i + 1;
    while n < text.len() && !text.is_char_boundary(n) {
        n += 1;
    }
    Some(n)
}

struct Translated {
    out: String,
    has_upper: bool,
    force_case: Option<bool>,
    has_zs: bool,
    has_ze: bool,
}

fn is_regex_meta(c: char) -> bool {
    matches!(c, '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$' | '\\' | '#' | '&' | '-' | '~')
}

fn push_lit(out: &mut String, c: char) {
    if is_regex_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

/// Translate with the default ("magic") rules, honouring `\v`, `\m`, `\M`, `\V`.
fn translate(pat: &str) -> Result<Translated, String> {
    #[derive(PartialEq, Clone, Copy)]
    enum Magic {
        Very,
        Magic,
        NoMagic,
        VeryNo,
    }
    let mut magic = Magic::Magic;
    let chars: Vec<char> = pat.chars().collect();
    let mut out = String::new();
    let mut t = Translated { out: String::new(), has_upper: false, force_case: None, has_zs: false, has_ze: false };
    let mut i = 0;
    // Whether the previous emitted thing is an "atom start" position (for ^ / *).
    let mut at_start = true;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let (c, escaped) = if c == '\\' {
            if i >= chars.len() {
                out.push_str("\\\\");
                break;
            }
            let n = chars[i];
            i += 1;
            (n, true)
        } else {
            (c, false)
        };
        // Is `c` special ("magic") in the current mode?
        let special = |ch: char| -> bool {
            match magic {
                Magic::Very => !escaped && !(ch.is_alphanumeric() || ch == '_'),
                Magic::Magic => {
                    if escaped {
                        !matches!(ch, '.' | '*' | '[' | '~' | '^' | '$')
                    } else {
                        matches!(ch, '.' | '*' | '[' | '~' | '^' | '$')
                    }
                }
                Magic::NoMagic => {
                    if escaped {
                        !matches!(ch, '^' | '$')
                    } else {
                        matches!(ch, '^' | '$')
                    }
                }
                Magic::VeryNo => {
                    if escaped {
                        true
                    } else {
                        matches!(ch, '^' | '$') && false
                    }
                }
            }
        };
        if escaped {
            match c {
                'v' => {
                    magic = Magic::Very;
                    continue;
                }
                'm' => {
                    magic = Magic::Magic;
                    continue;
                }
                'M' => {
                    magic = Magic::NoMagic;
                    continue;
                }
                'V' => {
                    magic = Magic::VeryNo;
                    continue;
                }
                'c' => {
                    t.force_case = Some(true);
                    continue;
                }
                'C' => {
                    t.force_case = Some(false);
                    continue;
                }
                _ => {}
            }
        }
        if !special(c) {
            // Literal character (possibly an escaped one such as `\/`).
            if escaped && magic != Magic::Very {
                match c {
                    't' | 'n' | 'r' | 'e' => {} // handled below as specials
                    _ => {}
                }
            }
            if c.is_uppercase() {
                t.has_upper = true;
            }
            push_lit(&mut out, c);
            at_start = false;
            continue;
        }
        let was_start = at_start;
        at_start = false;
        match c {
            '.' => out.push('.'),
            '*' => {
                if was_start {
                    out.push_str("\\*");
                } else {
                    out.push('*');
                }
            }
            '^' => {
                if was_start {
                    out.push('^');
                    at_start = true;
                } else {
                    out.push_str("\\^");
                }
            }
            '$' => {
                // `$` is only an anchor at the end of the pattern or before \| / \)
                let rest: String = chars[i..].iter().collect();
                let end = rest.is_empty()
                    || rest.starts_with("\\|")
                    || rest.starts_with("\\)")
                    || rest.starts_with("\\n")
                    || (magic == Magic::Very && (rest.starts_with('|') || rest.starts_with(')')));
                if end {
                    out.push('$');
                } else {
                    out.push_str("\\$");
                }
            }
            '~' => out.push_str("\\~"),
            '[' => {
                // Collection: copy to the closing bracket.
                let mut j = i;
                let mut body = String::new();
                if j < chars.len() && chars[j] == '^' {
                    body.push('^');
                    j += 1;
                }
                if j < chars.len() && chars[j] == ']' {
                    body.push_str("\\]");
                    j += 1;
                }
                let mut closed = false;
                while j < chars.len() {
                    let ch = chars[j];
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    if ch == '[' && j + 1 < chars.len() && chars[j + 1] == ':' {
                        // [:alpha:] class
                        if let Some(end) = chars[j..].iter().collect::<String>().find(":]") {
                            let cls: String = chars[j..j + end + 2].iter().collect();
                            body.push_str(&cls);
                            j += end + 2;
                            continue;
                        }
                    }
                    if ch == '\\' && j + 1 < chars.len() {
                        let n = chars[j + 1];
                        match n {
                            'e' => body.push_str("\\x1b"),
                            't' => body.push_str("\\t"),
                            'r' => body.push_str("\\r"),
                            'n' => body.push_str("\\n"),
                            'b' => body.push_str("\\x08"),
                            ']' | '^' | '-' | '\\' => {
                                body.push('\\');
                                body.push(n);
                            }
                            _ => {
                                body.push_str("\\\\");
                                j += 1;
                                continue;
                            }
                        }
                        j += 2;
                        continue;
                    }
                    if ch == '[' || ch == '&' || ch == '~' || ch == '|' {
                        body.push('\\');
                    }
                    if ch.is_uppercase() {
                        t.has_upper = true;
                    }
                    body.push(ch);
                    j += 1;
                }
                if closed {
                    out.push('[');
                    out.push_str(&body);
                    out.push(']');
                    i = j + 1;
                } else {
                    out.push_str("\\[");
                }
            }
            '<' | '>' => out.push_str("\\b"),
            '(' => {
                out.push('(');
                at_start = true;
            }
            ')' => out.push(')'),
            '|' => {
                out.push('|');
                at_start = true;
            }
            '&' => {
                // \& (concat/and) is not supported: treat as alternation-free concat
                out.push_str("");
            }
            '+' => out.push('+'),
            '=' | '?' => out.push('?'),
            '{' => {
                // \{n,m}  \{-n,m} (non-greedy)  \{}  \{-}
                let rest: String = chars[i..].iter().collect();
                let close = rest.find('}').ok_or_else(|| "E554: Syntax error in \\{...}".to_string())?;
                let mut inner = rest[..close].trim_end_matches('\\').to_string();
                i += close + 1;
                let lazy = inner.starts_with('-');
                if lazy {
                    inner.remove(0);
                }
                let q = if inner.is_empty() {
                    "*".to_string()
                } else if let Some((a, b)) = inner.split_once(',') {
                    format!("{{{},{}}}", if a.is_empty() { "0" } else { a }, b)
                } else {
                    format!("{{{}}}", inner)
                };
                out.push_str(&q);
                if lazy {
                    out.push('?');
                }
            }
            '@' => {
                // lookaround is not supported by the regex crate
                return Err("E486: lookaround (\\@=, \\@!, \\@<=) is not supported".to_string());
            }
            '%' => {
                // \%( \)  non-capturing group; \%^ \%$ file anchors; \%V etc. unsupported
                if i < chars.len() {
                    let n = chars[i];
                    i += 1;
                    match n {
                        '(' => {
                            out.push_str("(?:");
                            at_start = true;
                        }
                        '^' => out.push_str("^"),
                        '$' => out.push_str("$"),
                        'd' | 'x' | 'u' | 'U' | 'o' => {
                            let radix = match n {
                                'd' => 10,
                                'o' => 8,
                                _ => 16,
                            };
                            let mut digits = String::new();
                            while i < chars.len() && chars[i].is_digit(radix) {
                                digits.push(chars[i]);
                                i += 1;
                            }
                            if let Some(ch) = u32::from_str_radix(&digits, radix).ok().and_then(char::from_u32) {
                                out.push_str(&format!("\\x{{{:x}}}", ch as u32));
                            }
                        }
                        '[' => out.push_str("\\["),
                        _ => {
                            // \%23l, \%V, \%# ... : ignore the item
                            while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '<' | '>')) {
                                i += 1;
                            }
                            if i < chars.len() && matches!(chars[i], 'l' | 'c' | 'v') {
                                i += 1;
                            }
                        }
                    }
                }
            }
            'z' => {
                if i < chars.len() {
                    let n = chars[i];
                    i += 1;
                    match n {
                        's' => {
                            out.push_str("(?P<zs>)");
                            t.has_zs = true;
                        }
                        'e' => {
                            out.push_str("(?P<ze>)");
                            t.has_ze = true;
                        }
                        _ => {}
                    }
                }
            }
            's' => out.push_str("[ \\t]"),
            'S' => out.push_str("[^ \\t]"),
            'd' => out.push_str("[0-9]"),
            'D' => out.push_str("[^0-9]"),
            'w' => out.push_str("[0-9A-Za-z_]"),
            'W' => out.push_str("[^0-9A-Za-z_]"),
            'a' => out.push_str("[A-Za-z]"),
            'A' => out.push_str("[^A-Za-z]"),
            'l' => out.push_str("[a-z]"),
            'L' => out.push_str("[^a-z]"),
            'u' => out.push_str("[A-Z]"),
            'U' => out.push_str("[^A-Z]"),
            'x' => out.push_str("[0-9A-Fa-f]"),
            'X' => out.push_str("[^0-9A-Fa-f]"),
            'o' => out.push_str("[0-7]"),
            'O' => out.push_str("[^0-7]"),
            'h' => out.push_str("[A-Za-z_]"),
            'H' => out.push_str("[^A-Za-z_]"),
            'i' | 'k' => out.push_str("[\\w]"),
            'I' | 'K' => out.push_str("[\\w&&[^0-9]]"),
            'f' => out.push_str("[\\w/.\\-+,#$%~=]"),
            'p' => out.push_str("[^\\x00-\\x1f]"),
            'e' => out.push_str("\\x1b"),
            't' => out.push_str("\\t"),
            'r' => out.push_str("\\r"),
            'n' => out.push_str("\\n"),
            'b' => out.push_str("\\x08"),
            '1'..='9' => {
                return Err("E486: back references are not supported".to_string());
            }
            '_' => {
                // \_s, \_., \_[ : multi-line variants; treat like the single-line form
                if i < chars.len() {
                    let n = chars[i];
                    i += 1;
                    match n {
                        '.' => out.push_str("(?s:.)"),
                        's' => out.push_str("\\s"),
                        '^' => out.push('^'),
                        '$' => out.push('$'),
                        '[' => {
                            i -= 1;
                            // re-process the collection as a normal one
                            let rest: String = std::iter::once('[').chain(chars[i + 1..].iter().copied()).collect();
                            let sub = translate(&rest)?;
                            out.push_str(&sub.out);
                            i = chars.len();
                        }
                        other => {
                            let sub = translate(&format!("\\{}", other))?;
                            out.push_str(&sub.out);
                        }
                    }
                }
            }
            other => {
                if other.is_uppercase() {
                    t.has_upper = true;
                }
                push_lit(&mut out, other);
            }
        }
    }
    t.out = out;
    Ok(t)
}

/// Translate a file glob (autocommand pattern) into an anchored regex.
pub fn glob_to_regex(glob: &str) -> String {
    let mut out = String::from("^");
    let mut depth = 0;
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => out.push_str(".*"),
            '?' => out.push('.'),
            '{' => {
                depth += 1;
                out.push_str("(?:");
            }
            '}' if depth > 0 => {
                depth -= 1;
                out.push(')');
            }
            ',' if depth > 0 => out.push('|'),
            '\\' => {
                if i < chars.len() {
                    let n = chars[i];
                    i += 1;
                    if n == '\\' || n == '/' {
                        out.push_str("[/\\\\]");
                    } else {
                        push_lit(&mut out, n);
                    }
                }
            }
            '/' => out.push_str("[/\\\\]"),
            '[' => {
                if let Some(end) = chars[i..].iter().position(|c| *c == ']') {
                    out.push('[');
                    for ch in &chars[i..i + end] {
                        if *ch == '\\' {
                            out.push_str("\\\\");
                        } else {
                            out.push(*ch);
                        }
                    }
                    out.push(']');
                    i += end + 1;
                } else {
                    out.push_str("\\[");
                }
            }
            c => push_lit(&mut out, c),
        }
    }
    out.push('$');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, text: &str) -> Option<(usize, usize)> {
        VimRegex::new(pat, false, false).unwrap().find_at(text, 0).map(|m| (m.start, m.end))
    }

    #[test]
    fn magic_patterns() {
        assert_eq!(m("foo", "a foo b"), Some((2, 5)));
        assert_eq!(m("\\<foo\\>", "foobar foo"), Some((7, 10)));
        assert_eq!(m("a\\+b", "caaab"), Some((1, 5)));
        assert_eq!(m("a+b", "aab a+b"), Some((4, 7)));
        assert_eq!(m("\\(ab\\|cd\\)x", "cdx"), Some((0, 3)));
        assert_eq!(m("(ab)", "ab (ab)"), Some((3, 7)));
        assert_eq!(m("\\vfoo|bar", "xbar"), Some((1, 4)));
        assert_eq!(m("a.c", "abc"), Some((0, 3)));
        assert_eq!(m("\\s\\+$", "ab   "), Some((2, 5)));
        assert_eq!(m("$", "ab"), Some((2, 2)));
        assert_eq!(m("　", "a　b"), Some((1, 4)));
        assert_eq!(m("foo\\zsbar", "foobar"), Some((3, 6)));
        assert_eq!(m("x\\{2,3}", "axxxx"), Some((1, 4)));
        assert_eq!(m("\\k", "!a"), Some((1, 2)));
    }

    #[test]
    fn case_rules() {
        assert!(VimRegex::new("foo", true, true).unwrap().is_match("FOO"));
        assert!(!VimRegex::new("Foo", true, true).unwrap().is_match("FOO"));
        assert!(VimRegex::new("\\cFoo", false, false).unwrap().is_match("FOO"));
    }

    #[test]
    fn globs() {
        let r = Regex::new(&glob_to_regex("*.{c,h}")).unwrap();
        assert!(r.is_match("foo.c") && r.is_match("a/b.h") && !r.is_match("foo.cpp"));
    }
}
