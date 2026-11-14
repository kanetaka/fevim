//! Command-line mode: `:` commands, `/` `?` searches and the `=` expression prompt.

use super::buffer::Pos;
use super::keys::{self, Key, KeyCode, Sp, SHIFT};
use super::motion::{next_char_end, prev_char_start};
use super::{ex, options, Editor, Mode, RegKind};

#[derive(Clone, Debug, Default)]
pub struct Wild {
    pub items: Vec<String>,
    pub idx: Option<usize>,
    /// Byte offset in the command line where the completed word starts.
    pub start: usize,
    pub orig: String,
}

#[derive(Clone, Debug)]
pub struct CmdLine {
    pub kind: char,
    pub text: String,
    pub pos: usize,
    hist_idx: Option<usize>,
    hist_saved: String,
    pub wild: Option<Wild>,
    pending_register: bool,
    literal: bool,
    pub silent: bool,
    origin: Option<(Pos, usize)>,
}

impl CmdLine {
    pub fn new(kind: char, init: &str) -> CmdLine {
        CmdLine {
            kind,
            text: init.to_string(),
            pos: init.len(),
            hist_idx: None,
            hist_saved: String::new(),
            wild: None,
            pending_register: false,
            literal: false,
            silent: false,
            origin: None,
        }
    }

    fn insert(&mut self, s: &str) {
        self.text.insert_str(self.pos, s);
        self.pos += s.len();
    }
}

impl Editor {
    pub fn start_cmdline(&mut self, kind: char, init: &str) {
        let mut c = CmdLine::new(kind, init);
        c.silent = self.cur_key_silent;
        c.origin = Some((self.cursor(), self.win().topline));
        self.cmdline = Some(c);
        self.clear_msg();
    }

    fn active_cmdline(&mut self) -> &mut CmdLine {
        if self.expr_prompt.is_some() {
            self.expr_prompt.as_mut().unwrap()
        } else {
            self.cmdline.as_mut().unwrap()
        }
    }

    pub fn cmdline_key(&mut self, key: Key) {
        let is_expr = self.expr_prompt.is_some();
        // ---- pending sub-states
        {
            let c = self.active_cmdline();
            if c.literal {
                c.literal = false;
                if let KeyCode::Char(ch) = key.code {
                    c.insert(&ch.to_string());
                }
                return;
            }
            if c.pending_register {
                c.pending_register = false;
                if key.is('=') && !is_expr {
                    self.expr_prompt = Some(CmdLine::new('=', ""));
                    return;
                }
                let text = if key.is_ctrl('w') {
                    self.word_under_cursor().map(|(s, e)| self.cur_line()[s..e].to_string())
                } else if key.is_ctrl('a') {
                    let cur = self.cursor();
                    let line = self.cur_line();
                    let s = line[..cur.col.min(line.len())].rfind(char::is_whitespace).map(|i| i + 1).unwrap_or(0);
                    let e = line[cur.col.min(line.len())..].find(char::is_whitespace).map(|i| i + cur.col).unwrap_or(line.len());
                    Some(line[s..e].to_string())
                } else if key.is_ctrl('l') {
                    Some(self.cur_line().to_string())
                } else {
                    key.as_char().and_then(|ch| self.reg_get(ch)).map(|r| if r.kind == RegKind::Line { format!("{}\r", r.text) } else { r.text })
                };
                if let Some(t) = text {
                    let t = t.replace('\n', "\r");
                    self.active_cmdline().insert(&t);
                    self.cmdline_changed();
                }
                return;
            }
        }
        // ---- completion keys
        if key == keys::TAB || (key.code == KeyCode::Char('\t') && key.mods == SHIFT) || ((key.is_ctrl('n') || key.is_ctrl('p')) && self.active_cmdline().wild.is_some()) {
            let forward = key == keys::TAB || key.is_ctrl('n');
            self.cmdline_complete(forward);
            return;
        }
        self.active_cmdline().wild = None;

        if key == keys::ESC || key.is_ctrl('c') {
            self.cancel_cmdline();
            return;
        }
        if key == keys::CR || key.is(' ') && false || key.code == KeyCode::Char('\n') && key.mods == 0 {
            self.accept_cmdline();
            return;
        }
        let c = self.active_cmdline();
        match key.code {
            KeyCode::Sp(Sp::Backspace) | KeyCode::Char('\x08') if key.mods & !SHIFT == 0 => {
                if c.text.is_empty() {
                    self.cancel_cmdline();
                    return;
                }
                if c.pos > 0 {
                    let s = prev_char_start(&c.text, c.pos);
                    c.text.replace_range(s..c.pos, "");
                    c.pos = s;
                }
            }
            KeyCode::Sp(Sp::Delete) if key.mods == 0 => {
                if c.pos < c.text.len() {
                    let e = next_char_end(&c.text, c.pos);
                    c.text.replace_range(c.pos..e, "");
                } else if c.pos > 0 {
                    let s = prev_char_start(&c.text, c.pos);
                    c.text.replace_range(s..c.pos, "");
                    c.pos = s;
                }
            }
            KeyCode::Sp(Sp::Left) if key.mods == 0 => c.pos = prev_char_start(&c.text, c.pos),
            KeyCode::Sp(Sp::Right) if key.mods == 0 => c.pos = next_char_end(&c.text, c.pos),
            KeyCode::Sp(Sp::Left) => {
                let t = &c.text[..c.pos];
                let t2 = t.trim_end();
                c.pos = t2.rfind(' ').map(|i| i + 1).unwrap_or(0);
            }
            KeyCode::Sp(Sp::Right) => {
                let rest = &c.text[c.pos..];
                let skip = rest.len() - rest.trim_start().len();
                c.pos = rest[skip..].find(' ').map(|i| c.pos + skip + i).unwrap_or(c.text.len());
            }
            KeyCode::Sp(Sp::Home) | KeyCode::Char('\x02') if key.mods == 0 => c.pos = 0,
            KeyCode::Sp(Sp::End) | KeyCode::Char('\x05') if key.mods == 0 => c.pos = c.text.len(),
            KeyCode::Char('\x17') if key.mods == 0 => {
                let t = &c.text[..c.pos];
                let t2 = t.trim_end();
                let is_kw = |ch: char| ch.is_alphanumeric() || ch == '_';
                let last_kw = t2.chars().next_back().map(is_kw).unwrap_or(false);
                let s = t2
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| is_kw(*ch) != last_kw || ch.is_whitespace())
                    .map(|(i, ch)| i + ch.len_utf8())
                    .unwrap_or(0);
                c.text.replace_range(s..c.pos, "");
                c.pos = s;
            }
            KeyCode::Char('\x15') if key.mods == 0 => {
                c.text.replace_range(..c.pos, "");
                c.pos = 0;
            }
            KeyCode::Char('\x12') if key.mods == 0 => {
                c.pending_register = true;
                return;
            }
            KeyCode::Char('\x16') | KeyCode::Char('\x11') if key.mods == 0 => {
                c.literal = true;
                return;
            }
            KeyCode::Sp(Sp::Up) | KeyCode::Sp(Sp::Down) | KeyCode::Char('\x10') | KeyCode::Char('\x0e') if key.mods == 0 => {
                let up = matches!(key.code, KeyCode::Sp(Sp::Up) | KeyCode::Char('\x10'));
                let filter = matches!(key.code, KeyCode::Sp(_));
                self.cmdline_history(up, filter);
            }
            KeyCode::Sp(Sp::Insert) => {}
            KeyCode::Char(_) => {
                if let Some(ch) = key.printable() {
                    c.insert(&ch.to_string());
                }
            }
            _ => {}
        }
        self.cmdline_changed();
    }

    fn cmdline_history(&mut self, up: bool, filter: bool) {
        let is_expr = self.expr_prompt.is_some();
        let kind = self.active_cmdline().kind;
        let hist: Vec<String> = if kind == ':' { self.cmd_history.clone() } else if is_expr { Vec::new() } else { self.search_history.clone() };
        let c = self.active_cmdline();
        if c.hist_idx.is_none() {
            c.hist_saved = c.text.clone();
        }
        let prefix = if filter { c.hist_saved.clone() } else { String::new() };
        let mut idx = c.hist_idx.unwrap_or(hist.len());
        loop {
            if up {
                if idx == 0 {
                    return;
                }
                idx -= 1;
            } else {
                if idx >= hist.len() {
                    return;
                }
                idx += 1;
                if idx == hist.len() {
                    c.text = c.hist_saved.clone();
                    c.pos = c.text.len();
                    c.hist_idx = None;
                    return;
                }
            }
            if hist[idx].starts_with(&prefix) {
                c.text = hist[idx].clone();
                c.pos = c.text.len();
                c.hist_idx = Some(idx);
                return;
            }
        }
    }

    /// Incremental search preview.
    fn cmdline_changed(&mut self) {
        if self.expr_prompt.is_some() {
            return;
        }
        let Some(c) = &self.cmdline else {
            return;
        };
        if !matches!(c.kind, '/' | '?') || !self.opt_global("incsearch").as_bool() {
            return;
        }
        let (pat, forward, origin) = (c.text.clone(), c.kind == '/', c.origin);
        let Some((opos, otop)) = origin else {
            return;
        };
        self.win_mut().cursor = opos;
        self.win_mut().topline = otop;
        self.incsearch_match = None;
        if pat.is_empty() {
            return;
        }
        if let Some(p) = self.search(&pat, forward, opos, 1, false) {
            let len = self
                .compile_pattern(&pat)
                .ok()
                .and_then(|re| re.find_at(self.line(p.line), p.col))
                .map(|m| m.end - m.start)
                .unwrap_or(0);
            self.incsearch_match = Some((p, len));
            self.win_mut().cursor = p;
        }
    }

    fn cancel_cmdline(&mut self) {
        if self.expr_prompt.take().is_some() {
            return;
        }
        if let Some(c) = self.cmdline.take() {
            if let Some((p, top)) = c.origin {
                if matches!(c.kind, '/' | '?') {
                    self.win_mut().cursor = p;
                    self.win_mut().topline = top;
                }
            }
        }
        self.incsearch_match = None;
        self.pending_search_op = None;
        if self.mode == Mode::Cmdline {
            self.mode = Mode::Normal;
        }
        self.resume_insert();
    }

    fn resume_insert(&mut self) {
        if self.restart_insert && self.mode == Mode::Normal && self.cmdline.is_none() {
            self.restart_insert = false;
            self.mode = Mode::Insert;
        }
    }

    fn accept_cmdline(&mut self) {
        // ---- expression prompt
        if let Some(e) = self.expr_prompt.take() {
            if e.text.trim().is_empty() {
                return;
            }
            match self.eval_expr(&e.text) {
                Ok(v) => {
                    let s = v.to_display();
                    if let Some(c) = &mut self.cmdline {
                        let s = s.replace('\n', "\r");
                        c.insert(&s);
                    } else {
                        // insert mode: the result is inserted as if typed
                        let ks = keys::parse_keys_raw(&s);
                        self.feed_front_keys(&ks, false);
                    }
                }
                Err(err) => self.emsg(err),
            }
            return;
        }
        let Some(c) = self.cmdline.take() else {
            return;
        };
        if self.mode == Mode::Cmdline {
            self.mode = Mode::Normal;
        }
        self.incsearch_match = None;
        match c.kind {
            ':' => {
                let text = c.text.clone();
                if !text.trim().is_empty() {
                    if !c.silent {
                        self.cmd_history.retain(|h| *h != text);
                        self.cmd_history.push(text.clone());
                    }
                    self.last_cmdline = Some(text.clone());
                    self.exec_ex_reporting(&text);
                }
            }
            '/' | '?' => {
                let forward = c.kind == '/';
                if let Some((p, top)) = c.origin {
                    self.win_mut().cursor = p;
                    self.win_mut().topline = top;
                }
                let (pat, offset) = split_search_offset(&c.text, c.kind);
                let pat = if pat.is_empty() {
                    match self.last_search.clone() {
                        Some(p) => p,
                        None => {
                            self.emsg("E35: No previous regular expression");
                            self.pending_search_op = None;
                            return;
                        }
                    }
                } else {
                    pat
                };
                if !c.silent {
                    self.search_history.retain(|h| *h != pat);
                    self.search_history.push(pat.clone());
                }
                self.last_search = Some(pat.clone());
                self.search_forward = forward;
                self.hlsearch_off = false;
                let cur = self.cursor();
                let op = self.pending_search_op.take();
                if let Some(mut p) = self.search(&pat, forward, cur, 1, true) {
                    let mut linewise_off = false;
                    if let Some(off) = offset {
                        (p, linewise_off) = self.apply_search_offset(p, &pat, &off);
                    }
                    let _ = linewise_off;
                    match op {
                        Some(keys) => self.finish_search_operator(keys, p),
                        None => {
                            self.push_jump();
                            self.set_cursor(p);
                            let ts = self.tabstop_of(self.win());
                            let v = super::motion::vcol_of(self.cur_line(), p.col, ts, self.ambi_double());
                            self.win_mut().curswant = v;
                        }
                    }
                }
            }
            _ => {}
        }
        self.resume_insert();
    }

    fn apply_search_offset(&mut self, p: Pos, pat: &str, off: &str) -> (Pos, bool) {
        let mut chars = off.chars();
        let kind = chars.clone().next().filter(|c| matches!(c, 'e' | 's' | 'b'));
        if kind.is_some() {
            chars.next();
        }
        let num: String = chars.collect();
        let n: isize = match num.as_str() {
            "" | "+" => {
                if num == "+" { 1 } else { 0 }
            }
            "-" => -1,
            s => s.parse().unwrap_or(0),
        };
        match kind {
            Some('e') => {
                let len = self.compile_pattern(pat).ok().and_then(|re| re.find_at(self.line(p.line), p.col)).map(|m| m.end - m.start).unwrap_or(1);
                let line = self.line(p.line);
                let mut col = prev_char_start(line, p.col + len.max(1));
                for _ in 0..n.unsigned_abs() {
                    col = if n > 0 { next_char_end(line, col) } else { prev_char_start(line, col) };
                }
                (Pos::new(p.line, col.min(line.len())), false)
            }
            Some(_) => {
                let line = self.line(p.line);
                let mut col = p.col;
                for _ in 0..n.unsigned_abs() {
                    col = if n > 0 { next_char_end(line, col) } else { prev_char_start(line, col) };
                }
                (Pos::new(p.line, col), false)
            }
            None => {
                let last = self.buf().last_line() as isize;
                let l = (p.line as isize + n).clamp(0, last) as usize;
                (Pos::new(l, super::motion::first_nonblank(self.line(l))), true)
            }
        }
    }

    /// Run an Ex command line typed by the user or coming from a mapping.
    pub fn exec_ex_reporting(&mut self, cmd: &str) {
        let top = self.begin_collect();
        if let Err(e) = self.exec_lines(cmd, "cmdline") {
            self.emsg(e);
        }
        if top {
            self.end_collect();
        }
    }

    // ----------------------------------------------------------- completion

    fn cmdline_complete(&mut self, forward: bool) {
        let is_expr = self.expr_prompt.is_some();
        let (text, pos, kind, has_wild) = {
            let c = self.active_cmdline();
            (c.text.clone(), c.pos, c.kind, c.wild.is_some())
        };
        if kind != ':' || is_expr {
            return;
        }
        if !has_wild {
            let (start, items) = self.completion_candidates(&text[..pos]);
            if items.is_empty() {
                return;
            }
            let orig = text[start..pos].to_string();
            self.active_cmdline().wild = Some(Wild { items, idx: None, start, orig });
        }
        let c = self.active_cmdline();
        let w = c.wild.as_mut().unwrap();
        let n = w.items.len();
        let next = match (w.idx, forward) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) => {
                if i + 1 < n { Some(i + 1) } else { None }
            }
            (Some(i), false) => {
                if i > 0 { Some(i - 1) } else { None }
            }
        };
        w.idx = next;
        let rep = match next {
            Some(i) => w.items[i].clone(),
            None => w.orig.clone(),
        };
        let start = w.start;
        c.text.replace_range(start..c.pos, &rep);
        c.pos = start + rep.len();
        if n == 1 {
            c.wild = None;
        }
    }

    fn completion_candidates(&mut self, text: &str) -> (usize, Vec<String>) {
        // skip the range
        let body_start = text.find(|c: char| c.is_alphabetic()).unwrap_or(text.len());
        let body = &text[body_start..];
        let name_end = body.find(|c: char| !c.is_alphanumeric()).unwrap_or(body.len());
        if name_end == body.len() {
            // completing the command name
            let prefix = body;
            let mut items: Vec<String> = ex::COMMANDS.iter().map(|c| c.0.to_string()).filter(|n| n.starts_with(prefix)).collect();
            items.extend(self.user_cmds.keys().filter(|n| n.starts_with(prefix)).cloned());
            items.sort();
            items.dedup();
            return (body_start, items);
        }
        let cmd = ex::full_command_name(&body[..name_end]).unwrap_or("");
        let arg_start = text.rfind(|c: char| c == ' ' || c == '\t').map(|i| i + 1).unwrap_or(text.len());
        let arg = &text[arg_start..];
        let items: Vec<String> = match cmd {
            "set" | "setlocal" | "setglobal" => {
                let (neg, prefix) = match arg.strip_prefix("no") {
                    Some(p) if !p.is_empty() => ("no", p),
                    _ => ("", arg),
                };
                let mut v: Vec<String> = options::OPTION_DEFS.iter().filter(|d| d.name.starts_with(prefix)).map(|d| format!("{}{}", neg, d.name)).collect();
                v.sort();
                v
            }
            "buffer" | "sbuffer" | "bdelete" | "bwipeout" => self.buffers.values().map(|b| b.display_name()).filter(|n| n.contains(arg)).collect(),
            "colorscheme" => {
                let mut v = vec!["default".to_string(), "hybrid".to_string()];
                for dir in self.runtime_dirs.clone() {
                    if let Ok(rd) = std::fs::read_dir(dir.join("colors")) {
                        for e in rd.flatten() {
                            let n = e.file_name().to_string_lossy().to_string();
                            if let Some(stem) = n.strip_suffix(".vim").or_else(|| n.strip_suffix(".lua")) {
                                v.push(stem.to_string());
                            }
                        }
                    }
                }
                v.sort();
                v.dedup();
                v.into_iter().filter(|n| n.starts_with(arg)).collect()
            }
            "highlight" => self.hl.names().into_iter().filter(|n| n.to_lowercase().starts_with(&arg.to_lowercase())).collect(),
            "help" | "normal" | "let" | "call" | "echo" | "lua" | "execute" => Vec::new(),
            _ => file_candidates(arg),
        };
        (arg_start, items)
    }
}

fn file_candidates(arg: &str) -> Vec<String> {
    let expanded = if let Some(rest) = arg.strip_prefix("~/") {
        match home_dir() {
            Some(h) => format!("{}/{}", h.replace('\\', "/"), rest),
            None => arg.to_string(),
        }
    } else {
        arg.to_string()
    };
    let norm = expanded.replace('\\', "/");
    let (dir, prefix) = match norm.rfind('/') {
        Some(i) => (&norm[..=i], &norm[i + 1..]),
        None => ("", &norm[..]),
    };
    let read = if dir.is_empty() { ".".to_string() } else { dir.to_string() };
    let Ok(rd) = std::fs::read_dir(&read) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let matches = if cfg!(windows) { name.to_lowercase().starts_with(&prefix.to_lowercase()) } else { name.starts_with(prefix) };
        if !matches || (name.starts_with('.') && !prefix.starts_with('.')) {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let mut s = format!("{}{}", dir, name).replace(' ', "\\ ");
        if is_dir {
            s.push('/');
        }
        out.push(s);
    }
    out.sort();
    out.truncate(300);
    out
}

pub fn home_dir() -> Option<String> {
    std::env::var("HOME").ok().filter(|s| !s.is_empty()).or_else(|| std::env::var("USERPROFILE").ok())
}

/// Split `pattern/offset` (or `pattern?offset`).
fn split_search_offset(text: &str, sep: char) -> (String, Option<String>) {
    let mut esc = false;
    for (i, c) in text.char_indices() {
        if esc {
            esc = false;
            continue;
        }
        if c == '\\' {
            esc = true;
        } else if c == sep {
            return (text[..i].to_string(), Some(text[i + 1..].to_string()));
        }
    }
    (text.to_string(), None)
}
