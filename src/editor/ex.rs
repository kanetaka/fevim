//! Ex command line parsing: ranges, command names, `|` separation.

use super::buffer::Pos;
use super::Editor;

/// Command flag: the argument extends to the end of the line (`|` is not a separator).
pub const NOBAR: u8 = 1;
/// Accepted and ignored (features fevim does not have).
pub const IGNORE: u8 = 2;
/// Command modifier: the rest of the line is another command.
pub const MODIFIER: u8 = 4;

/// (name, minimum abbreviation length, flags)
pub static COMMANDS: &[(&str, usize, u8)] = &[
    ("substitute", 1, NOBAR),
    ("set", 2, 0),
    ("setlocal", 4, 0),
    ("setglobal", 4, 0),
    ("setfiletype", 4, 0),
    ("edit", 1, 0),
    ("enew", 3, 0),
    ("ex", 2, 0),
    ("write", 1, 0),
    ("wq", 2, 0),
    ("wall", 2, 0),
    ("wqall", 3, 0),
    ("wnext", 2, IGNORE),
    ("winpos", 4, 0),
    ("winsize", 4, 0),
    ("wincmd", 4, 0),
    ("windo", 5, NOBAR),
    ("quit", 1, 0),
    ("quitall", 5, 0),
    ("qall", 2, 0),
    ("xit", 1, 0),
    ("xall", 2, 0),
    ("exit", 3, 0),
    ("update", 2, 0),
    ("undo", 1, 0),
    ("unmap", 3, NOBAR),
    ("read", 1, NOBAR),
    ("redo", 3, 0),
    ("redraw", 4, 0),
    ("redrawstatus", 7, 0),
    ("registers", 3, 0),
    ("resize", 3, 0),
    ("retab", 3, 0),
    ("runtime", 2, 0),
    ("delete", 1, 0),
    ("delcommand", 4, 0),
    ("delmarks", 4, 0),
    ("display", 2, 0),
    ("doautocmd", 2, 0),
    ("doautoall", 7, 0),
    ("digraphs", 3, IGNORE),
    ("yank", 1, 0),
    ("put", 2, 0),
    ("print", 1, 0),
    ("previous", 4, IGNORE),
    ("pwd", 2, 0),
    ("move", 1, 0),
    ("mark", 2, 0),
    ("marks", 5, 0),
    ("map", 3, NOBAR),
    ("mapclear", 4, 0),
    ("match", 3, NOBAR),
    ("messages", 3, 0),
    ("make", 3, NOBAR),
    ("copy", 2, 0),
    ("t", 1, 0),
    ("k", 1, 0),
    ("join", 1, 0),
    ("jumps", 2, 0),
    ("global", 1, NOBAR),
    ("vglobal", 1, NOBAR),
    ("grep", 2, NOBAR),
    ("grepadd", 5, NOBAR),
    ("vimgrep", 3, NOBAR),
    ("normal", 4, NOBAR),
    ("nohlsearch", 3, 0),
    ("new", 3, 0),
    ("next", 1, IGNORE),
    ("number", 2, 0),
    ("only", 2, 0),
    ("split", 2, 0),
    ("sview", 2, 0),
    ("sbuffer", 2, 0),
    ("saveas", 3, 0),
    ("source", 2, 0),
    ("sort", 3, 0),
    ("sleep", 2, 0),
    ("shell", 2, IGNORE),
    ("startinsert", 4, 0),
    ("stopinsert", 5, 0),
    ("syntax", 2, NOBAR),
    ("silent", 3, MODIFIER),
    ("unsilent", 3, MODIFIER),
    ("vsplit", 2, 0),
    ("vnew", 3, 0),
    ("view", 3, 0),
    ("visual", 2, 0),
    ("version", 2, 0),
    ("vertical", 4, MODIFIER),
    ("verbose", 4, MODIFIER),
    ("tab", 3, MODIFIER),
    ("tabnew", 6, 0),
    ("tabedit", 4, 0),
    ("tabnext", 4, 0),
    ("tabNext", 4, 0),
    ("tabprevious", 4, 0),
    ("tabfirst", 6, 0),
    ("tabrewind", 4, 0),
    ("tablast", 4, 0),
    ("tabclose", 4, 0),
    ("tabonly", 4, 0),
    ("tabmove", 4, 0),
    ("tabs", 4, 0),
    ("tabdo", 5, NOBAR),
    ("buffer", 1, 0),
    ("bnext", 2, 0),
    ("bNext", 2, 0),
    ("bprevious", 2, 0),
    ("bfirst", 2, 0),
    ("brewind", 2, 0),
    ("blast", 2, 0),
    ("bdelete", 2, 0),
    ("bwipeout", 2, 0),
    ("buffers", 7, 0),
    ("bufdo", 5, NOBAR),
    ("ls", 2, 0),
    ("files", 5, 0),
    ("file", 1, 0),
    ("filetype", 5, 0),
    ("find", 3, 0),
    ("first", 3, IGNORE),
    ("close", 3, 0),
    ("cd", 2, 0),
    ("chdir", 3, 0),
    ("lcd", 2, 0),
    ("lchdir", 3, 0),
    ("colorscheme", 4, 0),
    ("command", 3, NOBAR),
    ("copen", 4, 0),
    ("cclose", 3, 0),
    ("cwindow", 2, 0),
    ("cnext", 2, 0),
    ("cprevious", 2, 0),
    ("cquit", 2, 0),
    ("checktime", 6, IGNORE),
    ("compiler", 4, IGNORE),
    ("autocmd", 2, NOBAR),
    ("augroup", 3, 0),
    ("ascii", 2, 0),
    ("highlight", 2, 0),
    ("help", 1, NOBAR),
    ("helptags", 5, IGNORE),
    ("history", 3, 0),
    ("hide", 3, MODIFIER),
    ("lua", 3, NOBAR),
    ("luafile", 4, 0),
    ("language", 3, IGNORE),
    ("last", 2, IGNORE),
    ("echohl", 5, 0),
    ("noautocmd", 3, MODIFIER),
    ("keepjumps", 5, MODIFIER),
    ("keepalt", 5, MODIFIER),
    ("keepmarks", 3, MODIFIER),
    ("keeppatterns", 5, MODIFIER),
    ("lockmarks", 3, MODIFIER),
    ("topleft", 2, MODIFIER),
    ("botright", 2, MODIFIER),
    ("aboveleft", 3, MODIFIER),
    ("belowright", 3, MODIFIER),
    ("leftabove", 5, MODIFIER),
    ("rightbelow", 6, MODIFIER),
    ("confirm", 4, MODIFIER),
    ("browse", 3, MODIFIER),
    ("sandbox", 3, MODIFIER),
    ("legacy", 3, MODIFIER),
    ("scriptencoding", 7, IGNORE),
    ("scriptversion", 7, IGNORE),
    ("packadd", 4, IGNORE),
    ("packloadall", 5, IGNORE),
    ("behave", 2, IGNORE),
    ("sign", 3, IGNORE | NOBAR),
    ("mkview", 5, IGNORE),
    ("loadview", 5, IGNORE),
    ("mksession", 3, IGNORE),
    ("fold", 2, IGNORE),
    ("foldopen", 5, IGNORE),
    ("foldclose", 5, IGNORE),
    ("spellgood", 3, IGNORE),
    ("diffthis", 5, IGNORE),
    ("diffoff", 5, IGNORE),
    ("diffupdate", 5, IGNORE),
    ("terminal", 3, IGNORE | NOBAR),
    ("abbreviate", 2, IGNORE | NOBAR),
    ("iabbrev", 2, IGNORE | NOBAR),
    ("cabbrev", 2, IGNORE | NOBAR),
    ("noreabbrev", 5, IGNORE | NOBAR),
    ("inoreabbrev", 5, IGNORE | NOBAR),
    ("cnoreabbrev", 5, IGNORE | NOBAR),
    ("unabbreviate", 3, IGNORE | NOBAR),
    ("abclear", 3, IGNORE),
    ("menu", 2, IGNORE | NOBAR),
    ("noremenu", 7, IGNORE | NOBAR),
    ("unmenu", 4, IGNORE | NOBAR),
    ("amenu", 2, IGNORE | NOBAR),
    ("anoremenu", 3, IGNORE | NOBAR),
    ("aunmenu", 4, IGNORE | NOBAR),
    ("nmenu", 3, IGNORE | NOBAR),
    ("vmenu", 3, IGNORE | NOBAR),
    ("imenu", 3, IGNORE | NOBAR),
    ("cmenu", 3, IGNORE | NOBAR),
    ("tmenu", 2, IGNORE | NOBAR),
    ("tlmenu", 3, IGNORE | NOBAR),
    ("popup", 4, IGNORE),
    ("simalt", 3, IGNORE),
    ("delfunction", 4, 0),
    ("ImportVimrc", 11, 0),
];

/// Mapping command families: (suffix-less name, min length).
const MAP_CMDS: &[(&str, usize)] = &[
    ("map", 3),
    ("nmap", 2),
    ("vmap", 2),
    ("xmap", 2),
    ("smap", 4),
    ("omap", 2),
    ("imap", 2),
    ("lmap", 2),
    ("cmap", 2),
    ("tmap", 3),
    ("noremap", 2),
    ("nnoremap", 2),
    ("vnoremap", 2),
    ("xnoremap", 2),
    ("snoremap", 4),
    ("onoremap", 3),
    ("inoremap", 3),
    ("lnoremap", 2),
    ("cnoremap", 3),
    ("tnoremap", 3),
    ("unmap", 3),
    ("nunmap", 3),
    ("vunmap", 2),
    ("xunmap", 2),
    ("sunmap", 5),
    ("ounmap", 2),
    ("iunmap", 2),
    ("lunmap", 2),
    ("cunmap", 2),
    ("tunmap", 5),
    ("mapclear", 4),
    ("nmapclear", 5),
    ("vmapclear", 5),
    ("xmapclear", 5),
    ("smapclear", 5),
    ("omapclear", 5),
    ("imapclear", 5),
    ("lmapclear", 5),
    ("cmapclear", 5),
    ("tmapclear", 5),
];

const SCRIPT_CMDS: &[(&str, usize)] = &[
    ("let", 3),
    ("unlet", 3),
    ("if", 2),
    ("elseif", 5),
    ("else", 2),
    ("endif", 2),
    ("while", 2),
    ("endwhile", 4),
    ("for", 3),
    ("endfor", 5),
    ("function", 2),
    ("endfunction", 4),
    ("return", 4),
    ("call", 3),
    ("execute", 3),
    ("echo", 2),
    ("echon", 5),
    ("echomsg", 5),
    ("echoerr", 5),
    ("echoconsole", 5),
    ("try", 3),
    ("catch", 3),
    ("finally", 4),
    ("endtry", 4),
    ("throw", 2),
    ("break", 4),
    ("continue", 3),
    ("finish", 4),
    ("const", 4),
    ("eval", 2),
    ("lockvar", 5),
    ("unlockvar", 6),
];

fn lookup(table: &[(&'static str, usize)], word: &str) -> Option<&'static str> {
    table.iter().find(|(name, min)| word.len() >= *min && name.starts_with(word)).map(|(n, _)| *n)
}

/// Resolve a (possibly abbreviated) script keyword.
pub fn script_keyword(word: &str) -> Option<&'static str> {
    lookup(SCRIPT_CMDS, word)
}

/// Resolve a (possibly abbreviated) mapping command name.
pub fn map_command(word: &str) -> Option<&'static str> {
    lookup(MAP_CMDS, word)
}

/// Resolve a (possibly abbreviated) built-in command name.
pub fn full_command_name(word: &str) -> Option<&'static str> {
    if let Some(m) = map_command(word) {
        return Some(m);
    }
    COMMANDS.iter().find(|(name, min, _)| word.len() >= *min && name.starts_with(word)).map(|(n, _, _)| *n)
}

fn command_flags(full: &str) -> u8 {
    if map_command(full).is_some() {
        return NOBAR;
    }
    COMMANDS.iter().find(|(n, _, _)| *n == full).map(|(_, _, f)| *f).unwrap_or(0)
}

fn alpha_len(s: &str) -> usize {
    s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len())
}

/// Does this line start with a script construct (`let`, `if`, `call`, ...)?
pub fn is_script_line(line: &str) -> bool {
    let mut s = line.trim_start_matches([' ', '\t', ':']);
    loop {
        let n = alpha_len(s);
        if n == 0 {
            return false;
        }
        let word = &s[..n];
        if script_keyword(word).is_some() {
            // `:e`, `:el`... are never script keywords because of minimum lengths
            return true;
        }
        match full_command_name(word) {
            Some(full) if command_flags(full) & MODIFIER != 0 => {
                s = s[n..].trim_start_matches('!').trim_start_matches([' ', '\t', ':']);
            }
            _ => return false,
        }
    }
}

/// Length of the range prefix of a command line.
pub fn scan_range(s: &str) -> usize {
    let b = s.as_bytes();
    let mut i = 0;
    loop {
        while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
            i += 1;
        }
        let start = i;
        if i < b.len() {
            match b[i] {
                b'%' | b'.' | b'$' => i += 1,
                b'0'..=b'9' => {
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                b'\'' => {
                    if i + 1 < b.len() {
                        i += 1;
                        i += s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(0);
                    }
                }
                b'/' | b'?' => {
                    let d = b[i];
                    i += 1;
                    while i < b.len() && b[i] != d {
                        if b[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    i = (i + 1).min(b.len());
                }
                b'\\' if i + 1 < b.len() && matches!(b[i + 1], b'/' | b'?' | b'&') => i += 2,
                _ => {}
            }
        }
        loop {
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                break;
            }
        }
        if i < b.len() && (b[i] == b',' || b[i] == b';') {
            i += 1;
            continue;
        }
        if i == start {
            // nothing consumed: back up over skipped blanks
            return start;
        }
        return i;
    }
}

#[derive(Debug, Clone)]
pub struct ParsedCmd {
    pub range: String,
    /// Name as typed (may be abbreviated).
    pub name: String,
    /// Resolved built-in name, if any.
    pub full: Option<&'static str>,
    pub bang: bool,
    pub arg: String,
    /// Text after the `|` separator.
    pub rest: Option<String>,
}

/// Split a `|`-separated argument (honouring `\|` and CTRL-V escapes).
fn split_at_bar(s: &str) -> (String, Option<String>) {
    let mut out = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c == '\\' {
            if let Some((_, n)) = chars.peek() {
                if *n == '|' {
                    out.push('|');
                    chars.next();
                    continue;
                }
            }
        }
        if c == '\x16' {
            if let Some((_, n)) = chars.next() {
                out.push(n);
            }
            continue;
        }
        if c == '|' {
            return (out, Some(s[i + 1..].to_string()));
        }
        out.push(c);
    }
    (out, None)
}

/// Like `split_at_bar` for mapping commands: `\|` and `<Bar>` stay in the
/// argument, a bare `|` ends the command.
fn split_map_bar(s: &str) -> (String, Option<String>) {
    let mut out = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if (c == '\\' || c == '\x16') && chars.peek().map(|(_, n)| *n == '|').unwrap_or(false) {
            out.push('|');
            chars.next();
            continue;
        }
        if c == '|' {
            return (out, Some(s[i + 1..].to_string()));
        }
        out.push(c);
    }
    (out, None)
}

/// Split one command off the front of a command line.
pub fn parse_command(line: &str) -> Option<ParsedCmd> {
    let s = line.trim_start_matches([' ', '\t', ':']);
    if s.is_empty() || s.starts_with('"') {
        return None;
    }
    let rlen = scan_range(s);
    let range = s[..rlen].trim().to_string();
    let after = s[rlen..].trim_start_matches([' ', '\t', ':']);
    if after.is_empty() {
        return Some(ParsedCmd { range, name: String::new(), full: None, bang: false, arg: String::new(), rest: None });
    }
    if after.starts_with('|') {
        return Some(ParsedCmd { range, name: String::new(), full: None, bang: false, arg: String::new(), rest: Some(after[1..].to_string()) });
    }
    let first = after.chars().next().unwrap();
    let (name, mut tail): (String, &str) = if first.is_ascii_alphabetic() {
        let mut n = alpha_len(after);
        if first.is_ascii_uppercase() {
            // user commands may contain digits
            n = after.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(after.len());
        } else {
            // `:s` followed by flags or a delimiter, `:dl`, `:k` + mark ...
            let word = &after[..n];
            if full_command_name(word).is_none() && script_keyword(word).is_none() {
                if word.starts_with('s') && n > 1 && word[1..].chars().all(|c| "cegiInp#lr&".contains(c)) {
                    n = 1;
                } else if word.starts_with('k') && n == 2 {
                    n = 1;
                } else if (word.starts_with('d') || word.starts_with('y')) && n == 2 {
                    n = 1;
                }
            }
            // py3 / 2match style digits
            if matches!(&after[..n], "py" | "python" | "pyx") {
                n += after[n..].chars().take_while(|c| c.is_ascii_digit()).count();
            }
        }
        (after[..n].to_string(), &after[n..])
    } else {
        let n = if after.starts_with("&&") { 2 } else { first.len_utf8() };
        (after[..n].to_string(), &after[n..])
    };
    let mut bang = false;
    if tail.starts_with('!') && name != "!" {
        bang = true;
        tail = &tail[1..];
    }
    let mut full = if first.is_ascii_lowercase() { full_command_name(&name) } else { None };
    if name == "ImportVimrc" {
        full = Some("ImportVimrc");
    }
    // `2match` / `3match` are parsed as range + "match"
    let flags = full.map(command_flags).unwrap_or(0);
    let is_map = full.map(|f| map_command(f).is_some()).unwrap_or(false);
    let nobar = flags & NOBAR != 0
        || flags & MODIFIER != 0
        || matches!(name.as_str(), "!" | "&" | "&&" | "~" | "@" | "*")
        || (first.is_ascii_uppercase() && name != "ImportVimrc")
        || full.is_none() && script_keyword(&name).is_some();
    let tail_trim = tail.strip_prefix([' ', '\t']).unwrap_or(tail);
    let tail_use = if is_map || matches!(full, Some("normal")) { tail_trim } else { tail.trim_start() };
    let (arg, rest) = if is_map {
        split_map_bar(tail_use)
    } else if matches!(full, Some("read") | Some("write")) && tail_use.trim_start().starts_with('!') {
        (tail_use.to_string(), None)
    } else if nobar && !matches!(full, Some("read")) {
        (tail_use.to_string(), None)
    } else {
        split_at_bar(tail_use)
    };
    Some(ParsedCmd { range, name, full, bang, arg, rest })
}

/// A command ready for execution.
pub struct ExCmd {
    /// 0-based inclusive line range, if one was given.
    pub range: Option<(usize, usize)>,
    pub addr_count: usize,
    /// The first number of the range as typed (for count-like uses).
    pub count: Option<usize>,
    pub bang: bool,
    pub arg: String,
    pub name: String,
}

impl ExCmd {
    pub fn lines(&self, cur: usize) -> (usize, usize) {
        self.range.unwrap_or((cur, cur))
    }
}

impl Editor {
    /// Evaluate a range string. Returns (line1, line2) 1-based plus the number
    /// of addresses.
    pub fn eval_range(&mut self, s: &str) -> Result<(usize, usize, usize), String> {
        let mut addrs: Vec<usize> = Vec::new();
        let mut i = 0;
        let b = s.as_bytes();
        let last = self.buf().line_count();
        let mut cur = self.cursor().line + 1;
        loop {
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            let mut line: Option<isize> = None;
            if i < b.len() {
                match b[i] {
                    b'%' => {
                        i += 1;
                        addrs.push(1);
                        line = Some(last as isize);
                    }
                    b'.' => {
                        i += 1;
                        line = Some(cur as isize);
                    }
                    b'$' => {
                        i += 1;
                        line = Some(last as isize);
                    }
                    b'0'..=b'9' => {
                        let st = i;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                        line = Some(s[st..i].parse::<isize>().unwrap_or(0));
                    }
                    b'\'' => {
                        i += 1;
                        let c = s[i..].chars().next().ok_or("E20: Mark not set")?;
                        i += c.len_utf8();
                        let p = self.get_mark(c).ok_or("E20: Mark not set")?;
                        line = Some(p.line as isize + 1);
                    }
                    b'/' | b'?' => {
                        let d = b[i];
                        i += 1;
                        let st = i;
                        while i < b.len() && b[i] != d {
                            if b[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                        let pat = s[st..i.min(s.len())].to_string();
                        i = (i + 1).min(b.len());
                        let pat = if pat.is_empty() { self.last_search.clone().ok_or("E35: No previous regular expression")? } else { pat };
                        let from = if d == b'/' { Pos::new(cur - 1, self.line(cur - 1).len()) } else { Pos::new(cur - 1, 0) };
                        let p = self.search(&pat, d == b'/', from, 1, false).ok_or_else(|| format!("E486: Pattern not found: {}", pat))?;
                        self.last_search = Some(pat);
                        line = Some(p.line as isize + 1);
                    }
                    b'\\' if i + 1 < b.len() => {
                        let d = b[i + 1];
                        i += 2;
                        let pat = self.last_search.clone().ok_or("E35: No previous regular expression")?;
                        let from = Pos::new(cur - 1, if d == b'?' { 0 } else { self.line(cur - 1).len() });
                        let p = self.search(&pat, d != b'?', from, 1, false).ok_or_else(|| format!("E486: Pattern not found: {}", pat))?;
                        line = Some(p.line as isize + 1);
                    }
                    _ => {}
                }
            }
            // offsets
            loop {
                while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                    let neg = b[i] == b'-';
                    i += 1;
                    let st = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    let n: isize = if st == i { 1 } else { s[st..i].parse().unwrap_or(1) };
                    let base = line.unwrap_or(cur as isize);
                    line = Some(if neg { base - n } else { base + n });
                } else {
                    break;
                }
            }
            let had = line.is_some();
            if let Some(l) = line {
                if l < 0 || l as usize > last {
                    return Err("E16: Invalid range".into());
                }
                addrs.push(l as usize);
            }
            if i < b.len() && (b[i] == b',' || b[i] == b';') {
                if !had {
                    addrs.push(cur);
                }
                if b[i] == b';' {
                    if let Some(l) = addrs.last() {
                        cur = (*l).max(1);
                        let p = Pos::new(cur - 1, 0);
                        self.set_cursor(p);
                    }
                }
                i += 1;
                // `1,` means `1,.`
                if s[i..].trim().is_empty() {
                    addrs.push(cur);
                    break;
                }
                continue;
            }
            break;
        }
        let n = addrs.len();
        match n {
            0 => Ok((cur, cur, 0)),
            1 => Ok((addrs[0], addrs[0], 1)),
            _ => {
                let (a, b2) = (addrs[n - 2], addrs[n - 1]);
                Ok((a.min(b2), a.max(b2), 2))
            }
        }
    }

    /// Execute one Ex command line (may contain several `|`-separated commands).
    pub fn exec_ex(&mut self, line: &str) -> Result<(), String> {
        let mut rest: String = line.to_string();
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 1000 {
                return Err("E169: Command too recursive".into());
            }
            if is_script_line(&rest) {
                return match self.script.clone() {
                    Some(h) => h.exec_vim(self, &rest, "cmdline"),
                    None => Err("E319: scripting is not available".into()),
                };
            }
            let Some(p) = parse_command(&rest) else {
                return Ok(());
            };
            let next = p.rest.clone();
            self.exec_parsed(p)?;
            match next {
                Some(n) => rest = n,
                None => return Ok(()),
            }
        }
    }

    fn exec_parsed(&mut self, p: ParsedCmd) -> Result<(), String> {
        // `:2match` / `:3match`: the number is an id, not a line
        if matches!(p.full, Some("match")) && !p.range.is_empty() && p.range.chars().all(|c| c.is_ascii_digit()) {
            let id: usize = p.range.parse().unwrap_or(1);
            let cmd = ExCmd { range: None, addr_count: 1, count: Some(id), bang: p.bang, arg: p.arg, name: p.name };
            return self.run_ex("match", &cmd);
        }
        let (l1, l2, n) = self.eval_range(&p.range)?;
        let count = if n > 0 { Some(l2) } else { None };
        let range = if n > 0 { Some((l1.max(1) - 1, l2.max(1) - 1)) } else { None };
        if p.name.is_empty() {
            // a bare range: go to that line
            if n > 0 {
                self.push_jump();
                let l = l2.max(1) - 1;
                let col = super::motion::first_nonblank(self.line(l));
                self.set_cursor(Pos::new(l, col));
            }
            return Ok(());
        }
        let cmd = ExCmd { range, addr_count: n, count, bang: p.bang, arg: p.arg, name: p.name.clone() };
        if let Some(full) = p.full {
            let flags = command_flags(full);
            if flags & IGNORE != 0 {
                return Ok(());
            }
            return self.run_ex(full, &cmd);
        }
        // symbols
        match p.name.as_str() {
            "!" | "&" | "&&" | "<" | ">" | "=" | "@" | "~" | "#" | "*" => return self.run_ex(&p.name, &cmd),
            _ => {}
        }
        if p.name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            return self.run_user_command(&cmd);
        }
        Err(format!("E492: Not an editor command: {}", p.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(full_command_name("w"), Some("write"));
        assert_eq!(full_command_name("s"), Some("substitute"));
        assert_eq!(full_command_name("se"), Some("set"));
        assert_eq!(full_command_name("sp"), Some("split"));
        assert_eq!(full_command_name("vs"), Some("vsplit"));
        assert_eq!(full_command_name("nn"), Some("nnoremap"));
        assert_eq!(full_command_name("au"), Some("autocmd"));
        assert_eq!(full_command_name("noh"), Some("nohlsearch"));
        assert_eq!(full_command_name("tabnew"), Some("tabnew"));
        assert_eq!(full_command_name("tabn"), Some("tabnext"));
        assert_eq!(full_command_name("cw"), Some("cwindow"));
        assert_eq!(full_command_name("setl"), Some("setlocal"));
        assert_eq!(full_command_name("hi"), Some("highlight"));
        assert_eq!(full_command_name("e"), Some("edit"));
        assert_eq!(full_command_name("q"), Some("quit"));
        assert_eq!(full_command_name("x"), Some("xit"));
        assert_eq!(full_command_name("d"), Some("delete"));
        assert_eq!(full_command_name("m"), Some("move"));
        assert_eq!(full_command_name("g"), Some("global"));
        assert_eq!(full_command_name("b"), Some("buffer"));
        assert_eq!(full_command_name("r"), Some("read"));
        assert_eq!(full_command_name("f"), Some("file"));
        assert_eq!(full_command_name("u"), Some("undo"));
        assert_eq!(full_command_name("colo"), Some("colorscheme"));
        assert_eq!(full_command_name("so"), Some("source"));
        assert_eq!(full_command_name("winp"), Some("winpos"));
        assert_eq!(script_keyword("end"), Some("endif"));
        assert_eq!(script_keyword("exe"), Some("execute"));
        assert!(is_script_line("  let@/=@1"));
        assert!(is_script_line("silent exec 'x'"));
        assert!(!is_script_line("set nu"));
        assert!(!is_script_line("e foo"));
    }

    #[test]
    fn parse() {
        let p = parse_command("set grepprg=grep\\ -nH | cw").unwrap();
        assert_eq!(p.full, Some("set"));
        assert_eq!(p.arg.trim(), "grepprg=grep\\ -nH");
        assert_eq!(p.rest.as_deref(), Some(" cw"));
        let p = parse_command("'<,'>s/a|b/c/g").unwrap();
        assert_eq!(p.range, "'<,'>");
        assert_eq!(p.full, Some("substitute"));
        assert_eq!(p.arg, "/a|b/c/g");
        let p = parse_command("%!xxd -g 1").unwrap();
        assert_eq!((p.range.as_str(), p.name.as_str(), p.arg.as_str()), ("%", "!", "xxd -g 1"));
        let p = parse_command("nnoremap sv :<C-u>vs<CR>").unwrap();
        assert_eq!(p.full, Some("nnoremap"));
        assert_eq!(p.arg, "sv :<C-u>vs<CR>");
        let p = parse_command("w! foo.txt").unwrap();
        assert!(p.bang);
        assert_eq!(p.arg, "foo.txt");
        let p = parse_command("Fontzoom +1").unwrap();
        assert_eq!(p.name, "Fontzoom");
        let p = parse_command("2match EOL /$/").unwrap();
        assert_eq!((p.range.as_str(), p.full), ("2", Some("match")));
    }
}
