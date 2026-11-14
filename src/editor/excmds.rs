//! Implementations of the Ex commands.

use std::path::{Path, PathBuf};

use super::buffer::Pos;
use super::cmdline::home_dir;
use super::ex::{self, ExCmd};
use super::keys::{self, Key};
use super::motion::first_nonblank;
use super::options::{self, OptKind, OptVal};
use super::window::WinMatch;
use super::{
    AutoAction, Editor, GuiRequest, MapRhs, Mapping, Mode, RegKind, UserCmd, Value, MODE_C, MODE_I, MODE_N, MODE_O, MODE_S, MODE_X,
};

/// Apply `:p:h:t:r:e` style modifiers to a file name.
pub fn fnamemodify(name: &str, mods: &str) -> String {
    let mut s = name.replace('\\', "/");
    let mut rest = mods;
    while let Some(r) = rest.strip_prefix(':') {
        let m = r.chars().next().unwrap_or(' ');
        rest = &r[m.len_utf8().min(r.len())..];
        match m {
            'p' => {
                let p = Path::new(&s);
                if !p.is_absolute() {
                    if let Ok(cwd) = std::env::current_dir() {
                        s = cwd.join(p).to_string_lossy().replace('\\', "/");
                    }
                }
                s = lexical_normalize(&s);
            }
            'h' => {
                let t = s.trim_end_matches('/');
                s = match t.rfind('/') {
                    Some(0) => "/".to_string(),
                    Some(i) => {
                        // keep "C:/" for a drive root
                        if i == 2 && t.as_bytes().get(1) == Some(&b':') { t[..3].to_string() } else { t[..i].to_string() }
                    }
                    None => ".".to_string(),
                };
            }
            't' => {
                s = s.rsplit('/').next().unwrap_or("").to_string();
            }
            'r' => {
                let tail_start = s.rfind('/').map(|i| i + 1).unwrap_or(0);
                if let Some(dot) = s[tail_start..].rfind('.') {
                    if dot > 0 {
                        s.truncate(tail_start + dot);
                    }
                }
            }
            'e' => {
                let tail_start = s.rfind('/').map(|i| i + 1).unwrap_or(0);
                s = match s[tail_start..].rfind('.') {
                    Some(dot) if dot > 0 => s[tail_start + dot + 1..].to_string(),
                    _ => String::new(),
                };
            }
            '~' => {
                if let Some(h) = home_dir() {
                    let h = h.replace('\\', "/");
                    if let Some(t) = s.strip_prefix(&h) {
                        s = format!("~{}", t);
                    }
                }
            }
            '.' => {
                if let Ok(cwd) = std::env::current_dir() {
                    let c = format!("{}/", cwd.to_string_lossy().replace('\\', "/").trim_end_matches('/'));
                    if let Some(t) = s.strip_prefix(&c) {
                        s = t.to_string();
                    }
                }
            }
            _ => {}
        }
    }
    s
}

fn lexical_normalize(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for part in p.split('/') {
        match part {
            "." => {}
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
            }
            "" if !out.is_empty() => {}
            x => out.push(x),
        }
    }
    let s = out.join("/");
    if s.is_empty() { "/".into() } else { s }
}

/// Expand `$VAR` and a leading `~` in a string.
pub fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    if rest == "~" || rest.starts_with("~/") || rest.starts_with("~\\") {
        if let Some(h) = home_dir() {
            out.push_str(&h.replace('\\', "/"));
            rest = &rest[1..];
        }
    }
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let n = after.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(after.len());
        let name = &after[..n];
        match std::env::var(name) {
            Ok(v) if n > 0 => out.push_str(&v.replace('\\', "/")),
            _ => {
                out.push('$');
                out.push_str(name);
            }
        }
        rest = &after[n..];
    }
    out.push_str(rest);
    out
}

/// Split command arguments on unescaped white space.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.peek() {
                if *n == ' ' || *n == '\t' || *n == '\\' || *n == '|' || *n == '"' {
                    cur.push(*n);
                    chars.next();
                    continue;
                }
            }
            cur.push(c);
        } else if c == ' ' || c == '\t' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn map_modes(cmd: &str) -> (u8, bool, &'static str) {
    // returns (modes, noremap, kind) with kind in "map" | "unmap" | "clear"
    let (base, kind): (&str, &'static str) = if let Some(b) = cmd.strip_suffix("mapclear") {
        (b, "clear")
    } else if let Some(b) = cmd.strip_suffix("unmap") {
        (b, "unmap")
    } else if let Some(b) = cmd.strip_suffix("noremap") {
        (b, "noremap")
    } else {
        (cmd.strip_suffix("map").unwrap_or(cmd), "map")
    };
    let modes = match base {
        "" => MODE_N | MODE_X | MODE_S | MODE_O,
        "n" => MODE_N,
        "v" => MODE_X | MODE_S,
        "x" => MODE_X,
        "s" => MODE_S,
        "o" => MODE_O,
        "i" => MODE_I,
        "c" => MODE_C,
        "l" => MODE_I | MODE_C,
        _ => 0,
    };
    (modes, kind == "noremap", if kind == "noremap" { "map" } else { kind })
}

pub fn modes_from_str(s: &str) -> u8 {
    let mut m = 0;
    if s.is_empty() {
        return MODE_N | MODE_X | MODE_S | MODE_O;
    }
    for c in s.chars() {
        m |= match c {
            'n' => MODE_N,
            'v' => MODE_X | MODE_S,
            'x' => MODE_X,
            's' => MODE_S,
            'o' => MODE_O,
            'i' => MODE_I,
            'c' => MODE_C,
            '!' => MODE_I | MODE_C,
            'l' => MODE_I | MODE_C,
            _ => 0,
        };
    }
    m
}

fn modes_label(m: u8) -> String {
    if m & (MODE_N | MODE_X | MODE_S | MODE_O) == MODE_N | MODE_X | MODE_S | MODE_O {
        return " ".into();
    }
    let mut s = String::new();
    if m & MODE_N != 0 {
        s.push('n');
    }
    if m & MODE_X != 0 && m & MODE_S != 0 {
        s.push('v');
    } else if m & MODE_X != 0 {
        s.push('x');
    } else if m & MODE_S != 0 {
        s.push('s');
    }
    if m & MODE_O != 0 {
        s.push('o');
    }
    if m & MODE_I != 0 && m & MODE_C != 0 {
        s.push('!');
    } else if m & MODE_I != 0 {
        s.push('i');
    } else if m & MODE_C != 0 {
        s.push('c');
    }
    s
}

impl Editor {
    /// Expand `%`, `#`, `<cword>` etc. and environment variables in a file argument.
    pub fn expand_filename(&mut self, arg: &str) -> String {
        let cur = self.buf().path.as_ref().map(|_| self.buf().display_name());
        let alt = self.win().alt_buf.and_then(|a| self.buffers.get(&a)).and_then(|b| b.path.as_ref().map(|_| b.display_name()));
        let mut out = String::new();
        let mut chars = arg.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '\\' {
                if let Some((_, n)) = chars.peek() {
                    if matches!(n, '%' | '#' | ' ') {
                        out.push(*n);
                        chars.next();
                        continue;
                    }
                }
                out.push(c);
                continue;
            }
            let special: Option<(String, usize)> = if c == '%' {
                Some((cur.clone().unwrap_or_default(), 1))
            } else if c == '#' && !arg[i + 1..].starts_with(|ch: char| ch.is_ascii_digit()) {
                Some((alt.clone().unwrap_or_default(), 1))
            } else if arg[i..].starts_with("<cword>") {
                Some((self.word_under_cursor().map(|(s, e)| self.cur_line()[s..e].to_string()).unwrap_or_default(), 7))
            } else if arg[i..].starts_with("<cfile>") {
                Some((self.word_under_cursor().map(|(s, e)| self.cur_line()[s..e].to_string()).unwrap_or_default(), 7))
            } else if arg[i..].starts_with("<afile>") || arg[i..].starts_with("<amatch>") {
                Some((cur.clone().unwrap_or_default(), if arg[i..].starts_with("<afile>") { 7 } else { 8 }))
            } else if arg[i..].starts_with("<sfile>") {
                Some((String::new(), 7))
            } else {
                None
            };
            match special {
                Some((val, len)) => {
                    for _ in 1..len {
                        chars.next();
                    }
                    // modifiers
                    let rest = &arg[i + len..];
                    let mut mlen = 0;
                    let rb = rest.as_bytes();
                    while mlen + 1 < rb.len() + 0 && rb[mlen] == b':' && mlen + 1 < rb.len() && b"phtre~.".contains(&rb[mlen + 1]) {
                        mlen += 2;
                    }
                    let mods = &rest[..mlen];
                    for _ in 0..mlen {
                        chars.next();
                    }
                    out.push_str(&fnamemodify(&val, mods));
                }
                None => out.push(c),
            }
        }
        expand_env(&out)
    }

    fn file_arg(&mut self, arg: &str) -> Option<PathBuf> {
        let a = arg.trim();
        if a.is_empty() {
            return None;
        }
        let e = self.expand_filename(a);
        let e = e.replace("\\ ", " ");
        Some(PathBuf::from(e))
    }

    /// `:quit` semantics for the current window.
    fn quit_window(&mut self, force: bool) -> Result<(), String> {
        let nwins: usize = self.tabs.iter().map(|t| t.layout.leaves().len()).sum();
        if nwins > 1 {
            self.can_abandon(force)?;
            let id = self.cur_win_id();
            return self.close_window(id);
        }
        if !force {
            if let Some(b) = self.buffers.values().find(|b| b.modified && !b.scratch) {
                let cur = b.id == self.cur_buf_id();
                return Err(if cur {
                    "E37: No write since last change (add ! to override)".to_string()
                } else {
                    format!("E162: No write since last change for buffer \"{}\"", b.display_name())
                });
            }
        }
        self.do_autocmd("VimLeavePre", "");
        self.do_autocmd("VimLeave", "");
        self.quit = true;
        Ok(())
    }

    fn quit_all(&mut self, force: bool) -> Result<(), String> {
        if !force {
            if let Some(b) = self.buffers.values().find(|b| b.modified && !b.scratch) {
                return Err(format!("E162: No write since last change for buffer \"{}\"", b.display_name()));
            }
        }
        self.do_autocmd("VimLeavePre", "");
        self.do_autocmd("VimLeave", "");
        self.quit = true;
        Ok(())
    }

    fn write_all(&mut self) -> Result<(), String> {
        let ids: Vec<usize> = self.buffers.values().filter(|b| b.modified && !b.scratch).map(|b| b.id).collect();
        for id in ids {
            if self.buffers[&id].path.is_none() {
                return Err(format!("E141: No file name for buffer {}", id));
            }
            self.write_buffer(id, None, None, false, false)?;
        }
        Ok(())
    }

    fn split_cmd(&mut self, vertical: bool, new: bool, arg: &str, c: &ExCmd) -> Result<(), String> {
        let vertical = vertical || self.cmdmod_vertical;
        let path = self.file_arg(arg);
        if self.cmdmod_tab {
            self.cmdmod_tab = false;
            self.new_tab(if new || path.is_some() { None } else { Some(self.cur_buf_id()) }, None);
        } else {
            let buf = if new && path.is_none() { Some(self.new_buffer()) } else { None };
            self.split_window(vertical, buf);
            if let Some(n) = c.count {
                if c.addr_count > 0 {
                    self.resize_window(vertical, 0, Some(n));
                }
            }
        }
        if let Some(p) = path {
            if arg.trim() == "#" {
                if let Some(a) = self.win().alt_buf {
                    self.set_cur_buf(a);
                }
            } else {
                self.edit_file(&p, true)?;
            }
        }
        Ok(())
    }

    fn goto_buffer_cmd(&mut self, target: Option<usize>, force: bool) -> Result<(), String> {
        let Some(id) = target else {
            return Err("E86: Buffer does not exist".into());
        };
        if !self.buffers.contains_key(&id) {
            return Err(format!("E86: Buffer {} does not exist", id));
        }
        if id != self.cur_buf_id() {
            self.can_abandon(force)?;
            self.set_cur_buf(id);
        }
        Ok(())
    }

    fn buffer_by_arg(&self, arg: &str) -> Option<usize> {
        let a = arg.trim();
        if a.is_empty() {
            return Some(self.cur_buf_id());
        }
        if a == "#" {
            return self.win().alt_buf;
        }
        if let Ok(n) = a.parse::<usize>() {
            return self.buffers.contains_key(&n).then_some(n);
        }
        let a = a.replace('\\', "/");
        let matches: Vec<usize> = self.buffers.values().filter(|b| b.display_name().contains(&a)).map(|b| b.id).collect();
        self.buffers.values().find(|b| b.display_name() == a).map(|b| b.id).or(if matches.len() == 1 { Some(matches[0]) } else { None })
    }

    fn cycle_buffer(&mut self, delta: isize, force: bool) -> Result<(), String> {
        let ids: Vec<usize> = self.buffers.values().filter(|b| b.listed).map(|b| b.id).collect();
        if ids.is_empty() {
            return Ok(());
        }
        let cur = ids.iter().position(|i| *i == self.cur_buf_id()).unwrap_or(0) as isize;
        let n = ids.len() as isize;
        let idx = ((cur + delta) % n + n) % n;
        self.goto_buffer_cmd(Some(ids[idx as usize]), force)
    }

    pub fn run_ex(&mut self, name: &str, c: &ExCmd) -> Result<(), String> {
        let cur = self.cursor();
        let arg = c.arg.trim().to_string();
        match name {
            // ------------------------------------------------ modifiers
            "silent" | "unsilent" | "noautocmd" | "keepjumps" | "keepalt" | "keepmarks" | "keeppatterns" | "lockmarks" | "verbose"
            | "topleft" | "botright" | "aboveleft" | "belowright" | "leftabove" | "rightbelow" | "confirm" | "browse" | "sandbox"
            | "legacy" | "hide" | "vertical" | "tab" => {
                if arg.is_empty() {
                    return Ok(());
                }
                match name {
                    "silent" => {
                        self.silent_depth += if c.bang { 2 } else { 1 };
                        let r = self.exec_ex(&c.arg);
                        self.silent_depth -= if c.bang { 2 } else { 1 };
                        if c.bang { Ok(()) } else { r }
                    }
                    "noautocmd" => {
                        let saved = self.opt_global("eventignore");
                        self.opts.global.insert("eventignore", OptVal::Str("all".into()));
                        let r = self.exec_ex(&c.arg);
                        self.opts.global.insert("eventignore", saved);
                        r
                    }
                    "vertical" => {
                        self.cmdmod_vertical = true;
                        let r = self.exec_ex(&c.arg);
                        self.cmdmod_vertical = false;
                        r
                    }
                    "tab" => {
                        self.cmdmod_tab = true;
                        let r = self.exec_ex(&c.arg);
                        self.cmdmod_tab = false;
                        r
                    }
                    "aboveleft" | "leftabove" | "topleft" | "belowright" | "rightbelow" | "botright" => {
                        let before = matches!(name, "aboveleft" | "leftabove" | "topleft");
                        let (sb, sr) = (self.opt_global("splitbelow"), self.opt_global("splitright"));
                        self.opts.global.insert("splitbelow", OptVal::Bool(!before));
                        self.opts.global.insert("splitright", OptVal::Bool(!before));
                        let r = self.exec_ex(&c.arg);
                        self.opts.global.insert("splitbelow", sb);
                        self.opts.global.insert("splitright", sr);
                        r
                    }
                    _ => self.exec_ex(&c.arg),
                }
            }
            // ---------------------------------------------------- files
            "write" | "update" => {
                if name == "update" && !self.buf().modified {
                    return Ok(());
                }
                if let Some(cmd) = arg.strip_prefix('!') {
                    let (s, e) = c.range.unwrap_or((0, self.buf().last_line()));
                    let input = self.buf().lines[s..=e].join("\n") + "\n";
                    let out = self.run_shell(cmd, Some(&input))?;
                    self.msg(out.trim_end().to_string());
                    return Ok(());
                }
                let (append, a) = match arg.strip_prefix(">>") {
                    Some(r) => (true, r.trim().to_string()),
                    None => (false, arg.clone()),
                };
                let path = self.file_arg(&a);
                let id = self.cur_buf_id();
                self.write_buffer(id, path, c.range, c.bang, append)
            }
            "wq" | "xit" | "exit" => {
                if name == "wq" || self.buf().modified {
                    let path = self.file_arg(&arg);
                    let id = self.cur_buf_id();
                    self.write_buffer(id, path, c.range, c.bang, false)?;
                }
                self.quit_window(c.bang)
            }
            "wall" => self.write_all(),
            "wqall" | "xall" => {
                self.write_all()?;
                self.quit_all(c.bang)
            }
            "quit" | "close" => {
                if name == "close" {
                    self.can_abandon(c.bang)?;
                    let id = self.cur_win_id();
                    self.close_window(id)
                } else {
                    self.quit_window(c.bang)
                }
            }
            "quitall" | "qall" => self.quit_all(c.bang),
            "cquit" => {
                self.exit_code = c.count.unwrap_or(1) as i32;
                self.quit = true;
                Ok(())
            }
            "edit" | "ex" | "visual" | "view" | "find" => match self.file_arg(&arg) {
                Some(p) => {
                    if arg == "#" {
                        let alt = self.win().alt_buf;
                        return self.goto_buffer_cmd(alt, c.bang);
                    }
                    self.push_jump();
                    self.edit_file(&p, c.bang)
                }
                None => {
                    if c.bang {
                        self.reload_buffer()
                    } else if self.buf().modified {
                        Err("E37: No write since last change (add ! to override)".into())
                    } else if self.buf().path.is_some() {
                        self.reload_buffer()
                    } else {
                        Ok(())
                    }
                }
            },
            "enew" => {
                self.can_abandon(c.bang)?;
                let b = self.new_buffer();
                self.set_cur_buf(b);
                Ok(())
            }
            "saveas" => {
                let Some(p) = self.file_arg(&arg) else {
                    return Err("E471: Argument required".into());
                };
                let id = self.cur_buf_id();
                self.write_buffer(id, Some(p.clone()), None, c.bang, false)?;
                let b = self.buf_mut();
                b.path = Some(p);
                b.modified = false;
                Ok(())
            }
            "file" => {
                if let Some(p) = self.file_arg(&arg) {
                    self.buf_mut().path = Some(p);
                }
                self.file_info();
                Ok(())
            }
            "read" => {
                let at = match c.range {
                    Some((_, e)) => e + 1,
                    None => cur.line + 1,
                };
                let at = if c.count == Some(0) { 0 } else { at };
                let lines: Vec<String> = if let Some(cmd) = arg.strip_prefix('!') {
                    let out = self.run_shell(cmd, None)?;
                    out.replace("\r\n", "\n").trim_end_matches('\n').split('\n').map(|s| s.to_string()).collect()
                } else {
                    let Some(p) = self.file_arg(&arg).or_else(|| self.buf().path.clone()) else {
                        return Err("E32: No file name".into());
                    };
                    let fencs = self.opt_global("fileencodings").as_string();
                    super::buffer::Buffer::read_file(&p, &fencs).map_err(|e| format!("E484: Can't open file {}: {}", p.display(), e))?.lines
                };
                let n = lines.len();
                self.buf_mut().insert_lines(at, lines);
                self.set_cursor(Pos::new(at, 0));
                if n > 2 {
                    self.msg(format!("{} more lines", n));
                }
                Ok(())
            }
            "source" | "luafile" => {
                let Some(p) = self.file_arg(&arg) else {
                    return Err("E471: Argument required".into());
                };
                self.source_file(&p)
            }
            "runtime" => {
                for pat in split_args(&arg) {
                    for dir in self.runtime_dirs.clone() {
                        let p = dir.join(&pat);
                        if p.is_file() {
                            self.source_file(&p)?;
                            if !c.bang {
                                break;
                            }
                        }
                    }
                }
                Ok(())
            }
            "ImportVimrc" => self.import_vimrc_cmd(&arg, c.bang),
            "cd" | "chdir" | "lcd" | "lchdir" => {
                let target = if arg.is_empty() {
                    match home_dir() {
                        Some(h) => h,
                        None => return Ok(()),
                    }
                } else if arg == "-" {
                    match self.prev_cwd.clone() {
                        Some(p) => p,
                        None => return Err("E186: No previous directory".into()),
                    }
                } else {
                    self.expand_filename(&arg)
                };
                // keep relative buffer names valid
                for b in self.buffers.values_mut() {
                    if let Some(p) = &b.path {
                        if !p.is_absolute() {
                            if let Ok(cwd) = std::env::current_dir() {
                                b.path = Some(cwd.join(p));
                            }
                        }
                    }
                }
                let old = std::env::current_dir().ok().map(|p| p.to_string_lossy().to_string());
                std::env::set_current_dir(&target).map_err(|e| format!("E344: Can't find directory \"{}\" in cdpath ({})", target, e))?;
                self.prev_cwd = old;
                let cwd = std::env::current_dir().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                self.msg(cwd);
                self.do_autocmd("DirChanged", "global");
                Ok(())
            }
            "pwd" => {
                let cwd = std::env::current_dir().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                self.msg(cwd);
                Ok(())
            }
            // -------------------------------------------------- windows
            "split" | "new" | "vsplit" | "vnew" | "sview" => self.split_cmd(matches!(name, "vsplit" | "vnew"), matches!(name, "new" | "vnew"), &arg, c),
            "only" => {
                self.only_window();
                Ok(())
            }
            "resize" => {
                let vertical = self.cmdmod_vertical;
                if arg.is_empty() {
                    self.resize_window(vertical, 0, Some(c.count.unwrap_or(10_000)));
                } else if let Some(n) = arg.strip_prefix('+') {
                    self.resize_window(vertical, n.trim().parse().unwrap_or(1), None);
                } else if let Some(n) = arg.strip_prefix('-') {
                    self.resize_window(vertical, -n.trim().parse::<isize>().unwrap_or(1), None);
                } else {
                    self.resize_window(vertical, 0, Some(arg.parse().map_err(|_| "E474: Invalid argument")?));
                }
                Ok(())
            }
            "wincmd" => {
                let ks = keys::parse_keys_raw(&arg);
                if let Some(k) = ks.first() {
                    self.window_command(*k, c.count.filter(|_| c.addr_count > 0));
                }
                Ok(())
            }
            "windo" | "bufdo" | "tabdo" => {
                match name {
                    "windo" => {
                        for id in self.tab_wins() {
                            self.goto_window(id);
                            self.exec_ex(&c.arg)?;
                        }
                    }
                    "bufdo" => {
                        let ids: Vec<usize> = self.buffers.values().filter(|b| b.listed).map(|b| b.id).collect();
                        for id in ids {
                            self.goto_buffer_cmd(Some(id), c.bang)?;
                            self.exec_ex(&c.arg)?;
                        }
                    }
                    _ => {
                        for i in 0..self.tabs.len() {
                            self.goto_tab(i);
                            self.exec_ex(&c.arg)?;
                        }
                    }
                }
                Ok(())
            }
            // ----------------------------------------------------- tabs
            "tabnew" | "tabedit" => {
                let path = self.file_arg(&arg);
                self.new_tab(None, None);
                if let Some(p) = path {
                    self.edit_file(&p, true)?;
                }
                Ok(())
            }
            "tabnext" => {
                let n = self.tabs.len();
                let a = if arg.is_empty() { c.count.filter(|_| c.addr_count > 0).map(|x| x.to_string()).unwrap_or_default() } else { arg.clone() };
                let idx = if a.is_empty() {
                    (self.cur_tab + 1) % n
                } else if a == "$" {
                    n - 1
                } else if let Some(d) = a.strip_prefix('+') {
                    (self.cur_tab + d.parse::<usize>().unwrap_or(1)) % n
                } else if let Some(d) = a.strip_prefix('-') {
                    (self.cur_tab + n - d.parse::<usize>().unwrap_or(1) % n) % n
                } else {
                    a.parse::<usize>().map_err(|_| "E475: Invalid argument")?.saturating_sub(1)
                };
                if idx >= n {
                    return Err("E475: Invalid argument".into());
                }
                self.goto_tab(idx);
                Ok(())
            }
            "tabprevious" | "tabNext" => {
                let n = self.tabs.len();
                let d = arg.parse::<usize>().unwrap_or(1) % n;
                self.goto_tab((self.cur_tab + n - d) % n);
                Ok(())
            }
            "tabfirst" | "tabrewind" => {
                self.goto_tab(0);
                Ok(())
            }
            "tablast" => {
                self.goto_tab(self.tabs.len() - 1);
                Ok(())
            }
            "tabclose" => {
                if !c.bang {
                    for id in self.tab_wins() {
                        let b = &self.buffers[&self.windows[&id].buf];
                        if b.modified && !self.opt_global("hidden").as_bool() && self.buf_window_count(b.id) <= 1 {
                            return Err("E37: No write since last change (add ! to override)".into());
                        }
                    }
                }
                let idx = if arg.is_empty() { self.cur_tab } else { arg.parse::<usize>().map_err(|_| "E475: Invalid argument")?.saturating_sub(1) };
                self.close_tab(idx)
            }
            "tabonly" => {
                let cur_tab = self.cur_tab;
                let mut i = self.tabs.len();
                while i > 0 {
                    i -= 1;
                    if i != cur_tab && self.tabs.len() > 1 {
                        let cur_now = self.cur_tab;
                        let t = self.tabs.remove(i);
                        for id in t.layout.leaves() {
                            self.windows.remove(&id);
                        }
                        if i < cur_now {
                            self.cur_tab -= 1;
                        }
                    }
                }
                self.prev_tab = self.cur_tab;
                self.layout();
                Ok(())
            }
            "tabmove" => {
                let n = self.tabs.len();
                let cur_tab = self.cur_tab;
                let target = if arg.is_empty() || arg == "$" {
                    n - 1
                } else if let Some(d) = arg.strip_prefix('+') {
                    (cur_tab + d.parse::<usize>().unwrap_or(1)).min(n - 1)
                } else if let Some(d) = arg.strip_prefix('-') {
                    cur_tab.saturating_sub(d.parse::<usize>().unwrap_or(1))
                } else {
                    let t = arg.parse::<usize>().map_err(|_| "E475: Invalid argument")?;
                    if t > cur_tab { t.saturating_sub(1).min(n - 1) } else { t.min(n - 1) }
                };
                let t = self.tabs.remove(cur_tab);
                self.tabs.insert(target, t);
                self.cur_tab = target;
                Ok(())
            }
            "tabs" => {
                let mut all = Vec::new();
                for (i, t) in self.tabs.iter().enumerate() {
                    let mut lines = vec![format!("Tab page {}", i + 1)];
                    for id in t.layout.leaves() {
                        let b = &self.buffers[&self.windows[&id].buf];
                        lines.push(format!("{}   {}", if id == t.cur && i == self.cur_tab { ">" } else { " " }, b.display_name()));
                    }
                    all.extend(lines);
                }
                self.msg(all.join("\n"));
                Ok(())
            }
            // -------------------------------------------------- buffers
            "buffer" | "sbuffer" => {
                let target = if arg.is_empty() && c.addr_count > 0 { c.count } else { self.buffer_by_arg(&arg) };
                if name == "sbuffer" {
                    let t = target.ok_or("E86: Buffer does not exist")?;
                    self.split_window(false, Some(t));
                    return Ok(());
                }
                self.goto_buffer_cmd(target, c.bang)
            }
            "bnext" => self.cycle_buffer(arg.parse().unwrap_or(1), c.bang),
            "bprevious" | "bNext" => self.cycle_buffer(-arg.parse::<isize>().unwrap_or(1), c.bang),
            "bfirst" | "brewind" => {
                let id = self.buffers.values().find(|b| b.listed).map(|b| b.id);
                self.goto_buffer_cmd(id, c.bang)
            }
            "blast" => {
                let id = self.buffers.values().filter(|b| b.listed).next_back().map(|b| b.id);
                self.goto_buffer_cmd(id, c.bang)
            }
            "bdelete" | "bwipeout" => {
                let targets: Vec<usize> = if arg.is_empty() {
                    match c.range {
                        Some((s, e)) if c.addr_count > 0 => (s + 1..=e + 1).filter(|i| self.buffers.contains_key(i)).collect(),
                        _ => vec![self.cur_buf_id()],
                    }
                } else {
                    split_args(&arg).iter().filter_map(|a| self.buffer_by_arg(a)).collect()
                };
                if targets.is_empty() {
                    return Err("E516: No buffers were deleted".into());
                }
                for t in targets {
                    self.delete_buffer(t, c.bang)?;
                }
                Ok(())
            }
            "ls" | "buffers" | "files" => {
                let cur_id = self.cur_buf_id();
                let alt = self.win().alt_buf;
                let mut lines = Vec::new();
                for b in self.buffers.values() {
                    if !b.listed && !c.bang {
                        continue;
                    }
                    let shown = self.windows.values().any(|w| w.buf == b.id);
                    let line = self.windows.values().find(|w| w.buf == b.id).map(|w| w.cursor.line).unwrap_or(b.last_cursor.line) + 1;
                    lines.push(format!(
                        "{:>3} {}{} {} {:<30} line {}",
                        b.id,
                        if b.id == cur_id { '%' } else if Some(b.id) == alt { '#' } else { ' ' },
                        if shown { 'a' } else { 'h' },
                        if b.modified { '+' } else { ' ' },
                        format!("\"{}\"", b.display_name()),
                        line
                    ));
                }
                self.msg(lines.join("\n"));
                Ok(())
            }
            // -------------------------------------------------- options
            "set" | "setlocal" | "setglobal" => {
                let scope = match name {
                    "set" => 'b',
                    "setlocal" => 'l',
                    _ => 'g',
                };
                self.do_set(&c.arg, scope)
            }
            "setfiletype" => {
                let ft = arg.strip_prefix("FALLBACK ").unwrap_or(&arg).trim().to_string();
                if self.opt_str("filetype").is_empty() {
                    self.set_opt_by_name("filetype", OptVal::Str(ft), 'l')?;
                }
                Ok(())
            }
            "filetype" => {
                if arg.split_whitespace().any(|a| a == "off") && !arg.contains("plugin") && !arg.contains("indent") {
                    self.filetype_on = false;
                } else if arg.contains("on") || arg.contains("detect") || arg.is_empty() {
                    if !arg.is_empty() {
                        self.filetype_on = true;
                    }
                    if arg.contains("detect") {
                        self.detect_filetype();
                    }
                    if arg.is_empty() {
                        self.msg(format!("filetype detection:{}  plugin:OFF  indent:OFF", if self.filetype_on { "ON" } else { "OFF" }));
                    }
                }
                Ok(())
            }
            "syntax" => {
                let sub = arg.split_whitespace().next().unwrap_or("");
                match sub {
                    "on" | "enable" => {
                        self.syntax_on = true;
                        self.do_autocmd("Syntax", "");
                    }
                    "off" | "clear" if sub == "off" => self.syntax_on = false,
                    "" => self.msg(format!("syntax highlighting is {}", if self.syntax_on { "on" } else { "off" })),
                    _ => {} // syntax definitions (keyword/match/region) are not supported
                }
                Ok(())
            }
            "colorscheme" => {
                if arg.is_empty() {
                    let n = self.colors_name.clone().unwrap_or_else(|| "default".into());
                    self.msg(n);
                    return Ok(());
                }
                self.load_colorscheme(&arg)
            }
            "highlight" => self.do_highlight(&c.arg, c.bang),
            "match" => {
                let id = match c.count {
                    Some(n) if c.addr_count > 0 && (1..=3).contains(&n) => n,
                    _ => 1,
                };
                let a = c.arg.trim();
                self.win_mut().matches.retain(|m| m.id != id);
                if a.is_empty() || a.eq_ignore_ascii_case("none") {
                    return Ok(());
                }
                let (group, rest) = a.split_once(char::is_whitespace).ok_or("E475: Invalid argument")?;
                let rest = rest.trim();
                let delim = rest.chars().next().ok_or("E475: Invalid argument")?;
                let body = &rest[delim.len_utf8()..];
                let end = body.rfind(delim).unwrap_or(body.len());
                let pattern = body[..end].to_string();
                self.win_mut().matches.push(WinMatch { id, group: group.to_string(), pattern });
                Ok(())
            }
            "nohlsearch" => {
                self.hlsearch_off = true;
                Ok(())
            }
            "echohl" => {
                self.echohl = if arg.is_empty() || arg.eq_ignore_ascii_case("none") { "" } else { leak_group(&arg) };
                Ok(())
            }
            // ------------------------------------------------- mappings
            _ if ex::map_command(name) == Some(name_static(name)) => self.do_map(name, c),
            "autocmd" => self.do_autocmd_cmd(&c.arg, c.bang),
            "augroup" => {
                if arg.eq_ignore_ascii_case("end") {
                    self.cur_augroup = None;
                } else if c.bang {
                    self.clear_autocmds(None, None, Some(&arg));
                    self.augroups.retain(|g| *g != arg);
                } else if !arg.is_empty() {
                    if !self.augroups.contains(&arg) {
                        self.augroups.push(arg.clone());
                    }
                    self.cur_augroup = Some(arg);
                }
                Ok(())
            }
            "doautocmd" | "doautoall" => {
                let mut parts: Vec<&str> = arg.split_whitespace().collect();
                parts.retain(|p| !p.starts_with("<nomodeline>"));
                if parts.first().map(|g| self.augroups.iter().any(|x| x == g)).unwrap_or(false) {
                    parts.remove(0);
                }
                let Some(ev) = parts.first() else {
                    return Ok(());
                };
                let subject = parts.get(1).map(|s| s.to_string()).unwrap_or_default();
                self.do_autocmd(ev, &subject);
                Ok(())
            }
            "command" => self.do_command_cmd(&c.arg, c.bang),
            "delcommand" => {
                self.user_cmds.remove(&arg).map(|_| ()).ok_or_else(|| format!("E184: No such user-defined command: {}", arg))
            }
            "delfunction" => Ok(()),
            // -------------------------------------------------- editing
            "substitute" | "&" | "&&" | "~" => self.do_substitute(name, c),
            "global" | "vglobal" => self.do_global(c, name == "vglobal" || c.bang),
            "delete" | "yank" => {
                let (mut s, mut e) = c.lines(cur.line);
                let (reg, cnt) = parse_reg_count(&arg);
                if let Some(n) = cnt {
                    s = e;
                    e = (s + n.max(1) - 1).min(self.buf().last_line());
                }
                let text = self.buf().lines[s..=e].join("\n");
                self.store_register(reg, text, RegKind::Line, name == "delete");
                if name == "delete" {
                    self.buf_mut().delete_lines(s, e + 1);
                    let l = s.min(self.buf().last_line());
                    let col = first_nonblank(self.line(l));
                    self.set_cursor(Pos::new(l, col));
                    if e - s + 1 > 2 {
                        self.msg(format!("{} fewer lines", e - s + 1));
                    }
                } else if e - s + 1 > 2 {
                    self.msg(format!("{} lines yanked", e - s + 1));
                }
                Ok(())
            }
            "put" => {
                let reg = arg.chars().next();
                let line = c.range.map(|r| r.1).unwrap_or(cur.line);
                let Some(mut r) = self.reg_get(reg.unwrap_or('"')) else {
                    return Err("E353: Nothing in register".into());
                };
                r.kind = RegKind::Line;
                if c.count == Some(0) && c.addr_count > 0 {
                    self.set_cursor(Pos::new(0, 0));
                    self.put_register(r, true, 1, false);
                } else {
                    self.set_cursor(Pos::new(line, 0));
                    self.put_register(r, c.bang, 1, false);
                }
                Ok(())
            }
            "move" | "copy" | "t" => {
                let (s, e) = c.lines(cur.line);
                let (d1, _, n) = self.eval_range(&arg)?;
                if n == 0 && arg.is_empty() {
                    return Err("E14: Invalid address".into());
                }
                let dest = if arg.trim() == "0" { 0 } else { d1 }; // insert after line `dest` (1-based) => index dest
                let lines: Vec<String> = self.buf().lines[s..=e].to_vec();
                let count = lines.len();
                if name == "move" {
                    if dest > s && dest <= e + 1 {
                        if dest == e + 1 || dest == s {
                            return Ok(());
                        }
                        return Err("E134: Cannot move a range of lines into itself".into());
                    }
                    if dest > e {
                        self.buf_mut().insert_lines(dest, lines);
                        self.buf_mut().delete_lines(s, e + 1);
                        self.set_cursor(Pos::new(dest - 1, 0));
                    } else {
                        self.buf_mut().delete_lines(s, e + 1);
                        self.buf_mut().insert_lines(dest, lines);
                        self.set_cursor(Pos::new(dest + count - 1, 0));
                    }
                } else {
                    self.buf_mut().insert_lines(dest, lines);
                    self.set_cursor(Pos::new(dest + count - 1, 0));
                }
                let l = self.cursor().line;
                let col = first_nonblank(self.line(l));
                self.set_cursor(Pos::new(l, col));
                Ok(())
            }
            "join" => {
                let (s, mut e) = c.lines(cur.line);
                if let Ok(n) = arg.parse::<usize>() {
                    e = s + n.max(1) - 1;
                }
                if e == s {
                    e = s + 1;
                }
                self.join_lines(s, e, !c.bang);
                Ok(())
            }
            ">" | "<" => {
                let (s, e) = c.lines(cur.line);
                let extra = arg.chars().take_while(|ch| *ch == name.chars().next().unwrap()).count() as isize;
                self.shift_lines(s, e, (1 + extra) * if name == "<" { -1 } else { 1 });
                let col = first_nonblank(self.line(e));
                self.set_cursor(Pos::new(e, col));
                Ok(())
            }
            "normal" => {
                let ks = keys::parse_keys_raw(&c.arg);
                match c.range {
                    Some((s, e)) if c.addr_count > 0 => {
                        for l in s..=e {
                            if l > self.buf().last_line() {
                                break;
                            }
                            self.set_cursor(Pos::new(l, 0));
                            self.run_normal(&ks, !c.bang);
                        }
                    }
                    _ => self.run_normal(&ks, !c.bang),
                }
                Ok(())
            }
            "undo" => {
                self.undo();
                Ok(())
            }
            "redo" => {
                self.redo();
                Ok(())
            }
            "retab" => {
                let (s, e) = c.range.unwrap_or((0, self.buf().last_line()));
                let old_ts = self.opt_num("tabstop").max(1) as usize;
                if let Ok(n) = arg.parse::<i64>() {
                    self.set_opt_by_name("tabstop", OptVal::Num(n), 'b')?;
                }
                let ts = self.opt_num("tabstop").max(1) as usize;
                let et = self.opt_bool("expandtab");
                for l in s..=e {
                    let line = self.line(l).to_string();
                    if !line.contains('\t') && (et || !c.bang) {
                        continue;
                    }
                    // expand to columns with the old tabstop, then rebuild white space runs
                    let mut out = String::new();
                    let mut col = 0usize;
                    let mut ws_start: Option<(usize, usize)> = None; // (byte index in out, column)
                    for chr in line.chars() {
                        if chr == ' ' || chr == '\t' {
                            if ws_start.is_none() {
                                ws_start = Some((out.len(), col));
                            }
                            col += if chr == '\t' { old_ts - col % old_ts } else { 1 };
                            out.push(chr);
                        } else {
                            if let Some((bi, c0)) = ws_start.take() {
                                let had_tab = out[bi..].contains('\t');
                                if had_tab || c.bang {
                                    out.truncate(bi);
                                    out.push_str(&retab_ws(c0, col, ts, et));
                                }
                            }
                            out.push(chr);
                            col += 1;
                        }
                    }
                    if out != line {
                        self.buf_mut().set_line(l, out);
                    }
                }
                Ok(())
            }
            "sort" => {
                let (s, e) = c.range.unwrap_or((0, self.buf().last_line()));
                let mut lines: Vec<String> = self.buf().lines[s..=e].to_vec();
                let numeric = arg.contains('n');
                let icase = arg.contains('i');
                let unique = arg.contains('u');
                if numeric {
                    let num = |l: &str| -> i64 {
                        let t: String = l.chars().skip_while(|c| !c.is_ascii_digit() && *c != '-').take_while(|c| c.is_ascii_digit() || *c == '-').collect();
                        t.parse().unwrap_or(0)
                    };
                    lines.sort_by_key(|l| num(l));
                } else if icase {
                    lines.sort_by_key(|l| l.to_lowercase());
                } else {
                    lines.sort();
                }
                if c.bang {
                    lines.reverse();
                }
                if unique {
                    lines.dedup_by(|a, b| if icase { a.eq_ignore_ascii_case(b) } else { a == b });
                }
                self.buf_mut().splice(s, e + 1, lines);
                self.set_cursor(Pos::new(s, 0));
                Ok(())
            }
            "!" => {
                if arg.is_empty() {
                    return Err("E471: Argument required".into());
                }
                let cmd = self.expand_bang_cmd(&c.arg);
                match c.range {
                    Some((s, e)) if c.addr_count > 0 => {
                        let input = self.buf().lines[s..=e].join("\n") + "\n";
                        let out = self.run_shell(&cmd, Some(&input))?;
                        let lines: Vec<String> = out.replace("\r\n", "\n").trim_end_matches('\n').split('\n').map(|x| x.to_string()).collect();
                        let n = lines.len();
                        self.buf_mut().splice(s, e + 1, lines);
                        self.set_cursor(Pos::new(s, 0));
                        if n > 2 {
                            self.msg(format!("{} lines filtered", e - s + 1));
                        }
                    }
                    _ => {
                        let out = self.run_shell(&cmd, None)?;
                        self.msg(out.replace("\r\n", "\n").trim_end().to_string());
                    }
                }
                self.last_bang = Some(cmd);
                Ok(())
            }
            "startinsert" => {
                if c.bang {
                    let l = self.cur_line().len();
                    self.win_mut().cursor.col = l;
                }
                self.start_insert('i', 1);
                Ok(())
            }
            "stopinsert" => {
                if matches!(self.mode, Mode::Insert | Mode::Replace) {
                    self.leave_insert();
                }
                Ok(())
            }
            "mark" | "k" => {
                let m = arg.chars().next().ok_or("E471: Argument required")?;
                let l = c.range.map(|r| r.1).unwrap_or(cur.line);
                self.set_mark(m, Pos::new(l, 0));
                Ok(())
            }
            "delmarks" => {
                if c.bang {
                    self.buf_mut().marks.retain(|k, _| !k.is_ascii_lowercase());
                } else {
                    let chars: Vec<char> = arg.chars().filter(|c| !c.is_whitespace()).collect();
                    self.buf_mut().marks.retain(|k, _| !chars.contains(k));
                }
                Ok(())
            }
            // ----------------------------------------------- information
            "registers" | "display" => {
                let mut lines = vec!["Type Name Content".to_string()];
                let mut names: Vec<char> = "\"0123456789-abcdefghijklmnopqrstuvwxyz.:%#/*+".chars().collect();
                if !arg.is_empty() {
                    names.retain(|n| arg.contains(*n));
                }
                for n in names {
                    if let Some(r) = self.reg_get(n) {
                        if r.text.is_empty() {
                            continue;
                        }
                        let kind = match r.kind {
                            RegKind::Char => 'c',
                            RegKind::Line => 'l',
                            RegKind::Block => 'b',
                        };
                        let mut t: String = r.text.replace('\n', "^J").chars().take(self.cols.saturating_sub(12)).collect();
                        if r.kind == RegKind::Line {
                            t.push_str("^J");
                        }
                        lines.push(format!("  {}  \"{}   {}", kind, n, keys::keys_notation(&keys::parse_keys_raw(&t)).replace("<Space>", " ").replace("<lt>", "<")));
                    }
                }
                self.msg(lines.join("\n"));
                Ok(())
            }
            "marks" => {
                let mut lines = vec!["mark line  col file/text".to_string()];
                let mut ms: Vec<(char, Pos)> = self.buf().marks.iter().map(|(k, v)| (*k, *v)).collect();
                ms.sort();
                for (k, p) in ms {
                    lines.push(format!(" {}   {:>4} {:>4} {}", k, p.line + 1, p.col, self.line(p.line.min(self.buf().last_line())).trim()));
                }
                self.msg(lines.join("\n"));
                Ok(())
            }
            "jumps" => {
                let mut lines = vec![" jump line  col file/text".to_string()];
                for (i, (b, p)) in self.jumplist.iter().enumerate() {
                    let name = self.buffers.get(b).map(|x| x.display_name()).unwrap_or_default();
                    lines.push(format!("{}{:>3} {:>5} {:>4} {}", if i == self.jumpidx { ">" } else { " " }, i, p.line + 1, p.col, name));
                }
                self.msg(lines.join("\n"));
                Ok(())
            }
            "messages" => {
                if arg == "clear" {
                    self.messages.clear();
                    return Ok(());
                }
                let msgs = self.messages.clone();
                for m in msgs {
                    self.msg_hl(m.text, m.group);
                }
                Ok(())
            }
            "history" => {
                let lines: Vec<String> = self.cmd_history.iter().enumerate().map(|(i, h)| format!("{:>6}  {}", i + 1, h)).collect();
                self.msg(format!("      #  cmd history\n{}", lines.join("\n")));
                Ok(())
            }
            "print" | "number" | "#" => {
                let (s, e) = c.lines(cur.line);
                let nu = name != "print" || self.opt_bool("number");
                let lines: Vec<String> = (s..=e).map(|l| if nu { format!("{:>3} {}", l + 1, self.line(l)) } else { self.line(l).to_string() }).collect();
                self.msg(lines.join("\n"));
                self.set_cursor(Pos::new(e, 0));
                Ok(())
            }
            "=" => {
                let l = c.range.map(|r| r.1 + 1).unwrap_or(self.buf().line_count());
                self.msg(l.to_string());
                Ok(())
            }
            "ascii" => {
                self.run_normal(&keys::parse_keys_raw("ga"), false);
                Ok(())
            }
            "version" => {
                self.msg(format!(
                    "FeVim {} - Vim reimplemented in Rust\nScripting: Lua 5.4 (Vim script is translated to Lua)\nGUI: GPUI\nCompatible with Vim 9.1 option and command names",
                    env!("CARGO_PKG_VERSION")
                ));
                Ok(())
            }
            "help" => {
                self.msg(
                    "FeVim help: fevim follows Vim's commands.\n  :e file  :w  :q  :sp  :vs  :tabnew  :set option  :map  :autocmd  :highlight\n  :lua {code}            run Lua\n  :source file.{lua,vim} run a script (Vim script is converted to Lua)\n  :ImportVimrc [file]    convert a vimrc/gvimrc into fevim's init.lua / ginit.lua",
                );
                Ok(())
            }
            "redraw" | "redrawstatus" => Ok(()),
            "sleep" => {
                let ms: u64 = if let Some(m) = arg.strip_suffix('m') { m.trim().parse().unwrap_or(0) } else { arg.parse::<u64>().unwrap_or(c.count.unwrap_or(1) as u64) * 1000 };
                std::thread::sleep(std::time::Duration::from_millis(ms.min(10_000)));
                Ok(())
            }
            "@" | "*" => {
                let r = arg.chars().next().unwrap_or('@');
                let reg = if r == '@' { self.last_macro.unwrap_or(':') } else { r };
                if reg == ':' {
                    if let Some(cmd) = self.last_cmdline.clone() {
                        return self.exec_ex(&cmd);
                    }
                    return Ok(());
                }
                match self.reg_get(reg) {
                    Some(rv) => {
                        for l in rv.text.clone().split('\n') {
                            self.exec_ex(l)?;
                        }
                        Ok(())
                    }
                    None => Err(format!("E353: Nothing in register {}", reg)),
                }
            }
            "lua" => {
                let code = c.arg.trim();
                let host = self.script.clone().ok_or("E319: Lua is not available")?;
                if let Some(expr) = code.strip_prefix('=') {
                    host.exec_lua(self, &format!("vim.print({})", expr), ":lua")
                } else {
                    host.exec_lua(self, code, ":lua")
                }
            }
            // ------------------------------------------------------ GUI
            "winpos" => {
                let nums: Vec<i32> = arg.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if nums.len() == 2 {
                    self.win_pos = (nums[0], nums[1]);
                    self.gui_requests.push(GuiRequest::Move(nums[0], nums[1]));
                } else {
                    self.msg(format!("Window position: X {}, Y {}", self.win_pos.0, self.win_pos.1));
                }
                Ok(())
            }
            "winsize" => {
                let nums: Vec<usize> = arg.split_whitespace().filter_map(|x| x.parse().ok()).collect();
                if nums.len() == 2 {
                    self.set_opt_by_name("columns", OptVal::Num(nums[0] as i64), 'g')?;
                    self.set_opt_by_name("lines", OptVal::Num(nums[1] as i64), 'g')?;
                    Ok(())
                } else {
                    Err("E465: :winsize requires two number arguments".into())
                }
            }
            // ------------------------------------------------- quickfix
            "copen" | "cclose" | "cwindow" | "cnext" | "cprevious" => Ok(()),
            "make" | "grep" | "grepadd" | "vimgrep" => Err(format!("E319: :{} is not available in fevim yet", name)),
            _ => Err(format!("E492: Not an editor command: {}", c.name)),
        }
    }

    /// Run normal-mode keys from an Ex command.
    pub fn run_normal(&mut self, ks: &[Key], remap: bool) {
        let saved_pending = std::mem::take(&mut self.pending);
        let saved_state = self.pending_state;
        self.pending_state = super::normal::PendingState::Fresh;
        self.run_keys(ks, remap);
        self.pending = saved_pending;
        self.pending_state = saved_state;
        let cur = self.cursor();
        self.buf_mut().close_undo(cur);
        self.check_cursor();
    }

    fn expand_bang_cmd(&mut self, cmd: &str) -> String {
        // `!` repeats the previous command, `%` is the current file name
        let mut out = String::new();
        let mut chars = cmd.chars().peekable();
        let name = self.buf().path.as_ref().map(|_| self.buf().display_name()).unwrap_or_default();
        while let Some(c) = chars.next() {
            match c {
                '\\' if matches!(chars.peek(), Some('!') | Some('%') | Some('#')) => out.push(chars.next().unwrap()),
                '!' => out.push_str(self.last_bang.as_deref().unwrap_or("")),
                '%' => out.push_str(&name),
                c => out.push(c),
            }
        }
        out
    }

    pub fn run_shell(&mut self, cmd: &str, input: Option<&str>) -> Result<String, String> {
        let (text, code, err) = self.run_shell_inner(cmd, input)?;
        if code != 0 && text.trim().is_empty() {
            return Err(format!("shell returned {}: {}", code, err.trim()));
        }
        Ok(if code != 0 { format!("{}{}", text, err) } else { text })
    }

    /// Run a shell command; returns (stdout, exit code).
    pub fn run_shell_status(&mut self, cmd: &str, input: Option<&str>) -> Result<(String, i64), String> {
        let (text, code, err) = self.run_shell_inner(cmd, input)?;
        Ok((if code != 0 { format!("{}{}", text, err) } else { text }, code))
    }

    fn run_shell_inner(&mut self, cmd: &str, input: Option<&str>) -> Result<(String, i64, String), String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let shell = self.opt_global("shell").as_string();
        let flag = self.opt_global("shellcmdflag").as_string();
        let mut command = Command::new(if shell.is_empty() { if cfg!(windows) { "cmd.exe" } else { "sh" } } else { &shell });
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            if shell.to_lowercase().contains("cmd") || shell.is_empty() {
                command.raw_arg(format!("{} {}", if flag.is_empty() { "/c" } else { &flag }, cmd));
            } else {
                command.arg(if flag.is_empty() { "-c" } else { &flag }).arg(cmd);
            }
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        #[cfg(not(windows))]
        {
            command.arg(if flag.is_empty() { "-c" } else { &flag }).arg(cmd);
        }
        command.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|e| format!("E282: Cannot execute shell {}: {}", shell, e))?;
        if let Some(inp) = input {
            if let Some(mut stdin) = child.stdin.take() {
                let data = inp.as_bytes().to_vec();
                std::thread::spawn(move || {
                    let _ = stdin.write_all(&data);
                });
            }
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        let text = decode_console(&out.stdout);
        let err = decode_console(&out.stderr);
        Ok((text, out.status.code().unwrap_or(-1) as i64, err))
    }

    // ------------------------------------------------------------------ :set

    pub fn do_set(&mut self, args: &str, scope: char) -> Result<(), String> {
        let args = split_args(args);
        if args.is_empty() || args[0] == "all" {
            let all = !args.is_empty();
            let mut lines = vec![if all { "--- Options ---".to_string() } else { "--- Options (changed) ---".to_string() }];
            let mut defs: Vec<&options::OptDef> = options::OPTION_DEFS.iter().collect();
            defs.sort_by_key(|d| d.name);
            for d in defs {
                let v = self.opt(d.name);
                if all || v != options::Options::default_value(d) {
                    lines.push(format_opt(d, &v));
                }
            }
            self.msg(lines.join("\n"));
            return Ok(());
        }
        for a in &args {
            if a.starts_with('"') {
                break; // trailing comment
            }
            self.set_one(a, scope)?;
        }
        Ok(())
    }

    fn set_one(&mut self, a: &str, scope: char) -> Result<(), String> {
        let name_end = a.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(a.len());
        let mut name = &a[..name_end];
        let tail = &a[name_end..];
        let mut prefix = "";
        if options::find_def(name).is_none() {
            if let Some(n) = name.strip_prefix("no") {
                if options::find_def(n).is_some() {
                    prefix = "no";
                    name = n;
                }
            } else if let Some(n) = name.strip_prefix("inv") {
                if options::find_def(n).is_some() {
                    prefix = "inv";
                    name = n;
                }
            }
        }
        // terminal options (t_xx) are accepted and ignored
        if a.starts_with("t_") || a.starts_with("<t_") {
            return Ok(());
        }
        let Some(def) = options::find_def(name) else {
            return Err(format!("E518: Unknown option: {}", a));
        };
        let cur = match scope {
            'g' => self.opt_global(def.name),
            _ => self.opt(def.name),
        };
        if tail == "?" || (tail.is_empty() && prefix.is_empty() && def.kind != OptKind::Bool) {
            self.msg(format_opt(def, &cur));
            return Ok(());
        }
        if tail == "&" || tail == "&vim" || tail == "&vi" {
            return self.set_opt(def, options::Options::default_value(def), scope);
        }
        if tail == "<" {
            match def.scope {
                options::OptScope::Buf => {
                    self.buf_mut().opts.remove(def.name);
                }
                options::OptScope::Win => {
                    self.win_mut().opts.remove(def.name);
                }
                _ => {}
            }
            return Ok(());
        }
        if def.kind == OptKind::Bool {
            let v = match (prefix, tail) {
                ("", "") => true,
                ("no", "") => false,
                ("inv", "") | ("", "!") => !cur.as_bool(),
                _ => return Err(format!("E474: Invalid argument: {}", a)),
            };
            return self.set_opt(def, OptVal::Bool(v), scope);
        }
        if !prefix.is_empty() {
            return Err(format!("E474: Invalid argument: {}", a));
        }
        let (op, val) = if let Some(v) = tail.strip_prefix("+=") {
            ('+', v)
        } else if let Some(v) = tail.strip_prefix("-=") {
            ('-', v)
        } else if let Some(v) = tail.strip_prefix("^=") {
            ('^', v)
        } else if let Some(v) = tail.strip_prefix('=').or_else(|| tail.strip_prefix(':')) {
            ('=', v)
        } else {
            return Err(format!("E474: Invalid argument: {}", a));
        };
        let text = if def.expand { expand_env(val) } else { val.to_string() };
        let newv = options::parse_value(def, &text)?;
        let result = match op {
            '+' => options::append_value(def, &cur, &newv),
            '-' => options::remove_value(def, &cur, &newv),
            '^' => options::prepend_value(def, &cur, &newv),
            _ => newv,
        };
        self.set_opt(def, result, scope)
    }

    // ------------------------------------------------------------------ :map

    fn do_map(&mut self, name: &str, c: &ExCmd) -> Result<(), String> {
        let (mut modes, noremap, kind) = map_modes(name);
        if c.bang {
            modes = MODE_I | MODE_C;
        }
        if kind == "clear" {
            let buffer = c.arg.contains("<buffer>").then(|| self.cur_buf_id());
            for m in self.maps.iter_mut() {
                if m.buffer == buffer {
                    m.modes &= !modes;
                }
            }
            self.maps.retain(|m| m.modes != 0);
            return Ok(());
        }
        let mut rest = c.arg.trim_start();
        let mut silent = false;
        let mut expr = false;
        let mut buffer = None;
        let mut nowait = false;
        loop {
            let lower = rest.to_ascii_lowercase();
            let mut matched = false;
            for (tag, n) in [("<silent>", 8), ("<expr>", 6), ("<buffer>", 8), ("<nowait>", 8), ("<unique>", 8), ("<script>", 8), ("<special>", 9)] {
                if lower.starts_with(tag) {
                    match tag {
                        "<silent>" => silent = true,
                        "<expr>" => expr = true,
                        "<buffer>" => buffer = Some(self.cur_buf_id()),
                        "<nowait>" => nowait = true,
                        _ => {}
                    }
                    rest = rest[n..].trim_start();
                    matched = true;
                    break;
                }
            }
            if !matched {
                break;
            }
        }
        if rest.is_empty() {
            return self.list_maps(modes, "");
        }
        let (lhs_text, rhs_text) = match rest.find([' ', '\t']) {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None => (rest, ""),
        };
        let leader = self.leader();
        let lhs = keys::parse_keys(&replace_leader(lhs_text, &leader));
        if kind == "unmap" {
            if !self.del_mapping(modes, &lhs, buffer) {
                return Err("E31: No such mapping".into());
            }
            return Ok(());
        }
        if rhs_text.is_empty() {
            return self.list_maps(modes, lhs_text);
        }
        let rhs_text = replace_leader(rhs_text, &leader);
        let rhs = if expr { Vec::new() } else { keys::parse_keys(&rhs_text) };
        self.add_mapping(Mapping { modes, lhs, rhs: MapRhs::Keys(rhs), rhs_text, noremap, silent, expr, nowait, buffer });
        Ok(())
    }

    pub fn leader(&mut self) -> String {
        match self.get_var("mapleader") {
            Some(Value::Str(s)) if !s.is_empty() => s,
            _ => "\\".to_string(),
        }
    }

    fn list_maps(&mut self, modes: u8, prefix: &str) -> Result<(), String> {
        let pk = keys::parse_keys(prefix);
        let mut lines = Vec::new();
        for m in &self.maps {
            if m.modes & modes == 0 || !m.lhs.starts_with(&pk) {
                continue;
            }
            lines.push(format!(
                "{:<3}{:<14} {}{}{}",
                modes_label(m.modes),
                keys::keys_notation(&m.lhs),
                if m.noremap { "* " } else { "  " },
                if m.buffer.is_some() { "@" } else { "" },
                match &m.rhs {
                    MapRhs::Lua(_) => "<Lua function>".to_string(),
                    MapRhs::Keys(_) => m.rhs_text.clone(),
                }
            ));
        }
        if lines.is_empty() {
            self.msg("No mapping found");
        } else {
            lines.sort();
            self.msg(lines.join("\n"));
        }
        Ok(())
    }

    // -------------------------------------------------------------- :autocmd

    fn do_autocmd_cmd(&mut self, arg: &str, bang: bool) -> Result<(), String> {
        let mut rest = arg.trim_start();
        let mut group = self.cur_augroup.clone();
        // optional group name
        if let Some(first) = rest.split_whitespace().next() {
            if self.augroups.iter().any(|g| g == first) && !is_event_list(first) {
                group = Some(first.to_string());
                rest = rest[first.len()..].trim_start();
            }
        }
        if rest.is_empty() {
            if bang {
                self.clear_autocmds(None, None, group.as_deref().or(Some("")).filter(|g| !g.is_empty()));
                if group.is_none() {
                    // `:autocmd!` outside a group removes ungrouped autocommands
                    self.autocmds.retain(|a| a.group.is_some());
                }
            } else {
                self.list_autocmds(None, group.as_deref());
            }
            return Ok(());
        }
        let (events, r) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None => (rest, ""),
        };
        if !is_event_list(events) {
            return Err(format!("E216: No such event: {}", events));
        }
        let (pattern, r) = match r.find(char::is_whitespace) {
            Some(i) => (&r[..i], r[i..].trim_start()),
            None => (r, ""),
        };
        let event_list: Vec<String> = if events == "*" { Vec::new() } else { events.split(',').map(|s| s.to_string()).collect() };
        if bang {
            if event_list.is_empty() {
                self.clear_matching(None, if pattern.is_empty() { None } else { Some(pattern) }, group.as_deref());
            }
            for ev in &event_list {
                self.clear_matching(Some(ev), if pattern.is_empty() { None } else { Some(pattern) }, group.as_deref());
            }
        }
        if pattern.is_empty() || r.is_empty() {
            if !bang {
                for ev in &event_list {
                    self.list_autocmds(Some(ev), group.as_deref());
                }
            }
            return Ok(());
        }
        // ++once / ++nested / nested
        let mut cmd = r;
        let mut once = false;
        loop {
            if let Some(x) = cmd.strip_prefix("++once") {
                once = true;
                cmd = x.trim_start();
            } else if let Some(x) = cmd.strip_prefix("++nested") {
                cmd = x.trim_start();
            } else if let Some(x) = cmd.strip_prefix("nested ") {
                cmd = x.trim_start();
            } else {
                break;
            }
        }
        for ev in &event_list {
            self.add_autocmd(ev, pattern, group.clone(), AutoAction::Ex(cmd.to_string()), once);
        }
        Ok(())
    }

    fn clear_matching(&mut self, event: Option<&str>, pattern: Option<&str>, group: Option<&str>) {
        let ev = event.map(super::canon_event);
        self.autocmds.retain(|a| {
            let m = ev.as_ref().map(|e| *e == a.event).unwrap_or(true) && pattern.map(|p| p == a.pattern).unwrap_or(true) && a.group.as_deref() == group;
            !m
        });
    }

    fn list_autocmds(&mut self, event: Option<&str>, group: Option<&str>) {
        let ev = event.map(super::canon_event);
        let mut lines = vec!["--- Autocommands ---".to_string()];
        for a in &self.autocmds {
            if ev.as_ref().map(|e| *e != a.event).unwrap_or(false) {
                continue;
            }
            if group.is_some() && a.group.as_deref() != group {
                continue;
            }
            let cmd = match &a.action {
                AutoAction::Ex(c) => c.clone(),
                AutoAction::Lua(_) => "<Lua function>".to_string(),
            };
            lines.push(format!("{}  {}\n    {:<10} {}", a.group.clone().unwrap_or_default(), a.event, a.pattern, cmd));
        }
        self.msg(lines.join("\n"));
    }

    // ------------------------------------------------------------ :highlight

    pub fn do_highlight(&mut self, arg: &str, bang: bool) -> Result<(), String> {
        let mut words: Vec<&str> = arg.split_whitespace().collect();
        if words.is_empty() {
            let lines: Vec<String> = self.hl.names().iter().filter_map(|n| self.hl.describe(n)).collect();
            self.msg(lines.join("\n"));
            return Ok(());
        }
        let mut default_only = false;
        if words[0] == "default" || words[0] == "def" {
            default_only = true;
            words.remove(0);
        }
        if words.is_empty() {
            return Ok(());
        }
        if words[0] == "clear" {
            match words.get(1) {
                Some(name) => self.hl.clear_group(name),
                None => {
                    let dark = self.opt_str("background") != "light";
                    self.hl.load_builtin("default", dark);
                }
            }
            return Ok(());
        }
        if words[0] == "link" {
            if words.len() < 3 {
                return Err("E412: Not enough arguments: \":highlight link\"".into());
            }
            if default_only && self.hl.exists(words[1]) {
                return Ok(());
            }
            self.hl.link(words[1], words[2], bang || default_only);
            return Ok(());
        }
        let name = words[0];
        if words.len() == 1 {
            match self.hl.describe(name) {
                Some(d) => self.msg(d),
                None => return Err(format!("E411: Highlight group not found: {}", name)),
            }
            return Ok(());
        }
        if words[1].eq_ignore_ascii_case("NONE") && words.len() == 2 {
            self.hl.clear_group(name);
            return Ok(());
        }
        let mut args = Vec::new();
        // values may be quoted: guifg='light blue'
        let rest = arg.trim_start();
        let rest = rest[rest.find(name).unwrap_or(0) + name.len()..].trim_start();
        let mut i = 0;
        let b = rest.as_bytes();
        while i < b.len() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            let ks = i;
            while i < b.len() && b[i] != b'=' && !b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= b.len() || b[i] != b'=' {
                if ks < i {
                    return Err(format!("E416: Missing equal sign: {}", &rest[ks..i]));
                }
                break;
            }
            let key = rest[ks..i].to_string();
            i += 1;
            let val = if i < b.len() && b[i] == b'\'' {
                let vs = i + 1;
                let end = rest[vs..].find('\'').map(|x| x + vs).unwrap_or(rest.len());
                i = (end + 1).min(b.len());
                rest[vs..end].to_string()
            } else {
                let vs = i;
                while i < b.len() && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                rest[vs..i].to_string()
            };
            args.push((key, val));
        }
        self.hl.set(name, &args, default_only);
        Ok(())
    }

    pub fn load_colorscheme(&mut self, name: &str) -> Result<(), String> {
        self.do_autocmd("ColorSchemePre", name);
        let mut loaded = false;
        for dir in self.runtime_dirs.clone() {
            for ext in ["lua", "vim"] {
                let p = dir.join("colors").join(format!("{}.{}", name, ext));
                if p.is_file() {
                    // a broken scheme file falls back to a built-in of the same name
                    match self.source_file(&p) {
                        Ok(()) => loaded = true,
                        Err(e) => {
                            if !super::highlight::Highlights::has_builtin(name) {
                                return Err(e);
                            }
                        }
                    }
                    break;
                }
            }
            if loaded {
                break;
            }
        }
        if !loaded {
            let dark = self.opt_str("background") != "light";
            if !self.hl.load_builtin(name, dark) {
                return Err(format!("E185: Cannot find color scheme '{}'", name));
            }
            if name == "hybrid" {
                self.opts.global.insert("background", OptVal::Str("dark".into()));
            }
        }
        self.colors_name = Some(name.to_string());
        if let Some(h) = self.script.clone() {
            h.set_global(self, "colors_name", Value::Str(name.to_string()));
        }
        self.do_autocmd("ColorScheme", name);
        Ok(())
    }

    // -------------------------------------------------------------- :command

    fn do_command_cmd(&mut self, arg: &str, bang: bool) -> Result<(), String> {
        let mut rest = arg.trim_start();
        if rest.is_empty() {
            let mut lines = vec!["    Name              Args Definition".to_string()];
            for u in self.user_cmds.values() {
                let def = match &u.repl {
                    AutoAction::Ex(s) => s.clone(),
                    AutoAction::Lua(_) => "<Lua function>".into(),
                };
                lines.push(format!("{}   {:<17} {:<4} {}", if u.bang { "!" } else { " " }, u.name, u.nargs, def));
            }
            self.msg(lines.join("\n"));
            return Ok(());
        }
        let mut cmd = UserCmd { name: String::new(), repl: AutoAction::Ex(String::new()), nargs: '0', bang: false, range: false, bar: false };
        while let Some(attr) = rest.strip_prefix('-') {
            let end = attr.find(char::is_whitespace).unwrap_or(attr.len());
            let a = &attr[..end];
            if let Some(n) = a.strip_prefix("nargs=") {
                cmd.nargs = n.chars().next().unwrap_or('0');
            } else if a == "bang" {
                cmd.bang = true;
            } else if a.starts_with("range") || a.starts_with("count") {
                cmd.range = true;
            } else if a == "bar" {
                cmd.bar = true;
            }
            rest = attr[end..].trim_start();
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let name = &rest[..end];
        if !name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            return Err("E183: User defined commands must start with an uppercase letter".into());
        }
        if self.user_cmds.contains_key(name) && !bang {
            return Err(format!("E174: Command already exists: add ! to replace it: {}", name));
        }
        cmd.name = name.to_string();
        cmd.repl = AutoAction::Ex(rest[end..].trim_start().to_string());
        self.user_cmds.insert(cmd.name.clone(), cmd);
        Ok(())
    }

    pub fn run_user_command(&mut self, c: &ExCmd) -> Result<(), String> {
        // allow unambiguous abbreviations
        let cmd = match self.user_cmds.get(&c.name) {
            Some(u) => u.clone(),
            None => {
                let cands: Vec<&UserCmd> = self.user_cmds.values().filter(|u| u.name.starts_with(&c.name)).collect();
                match cands.len() {
                    1 => cands[0].clone(),
                    0 => return Err(format!("E492: Not an editor command: {}", c.name)),
                    _ => return Err("E464: Ambiguous use of user-defined command".into()),
                }
            }
        };
        let args = c.arg.trim().to_string();
        match cmd.nargs {
            '0' if !args.is_empty() => return Err("E488: Trailing characters".into()),
            '1' | '+' if args.is_empty() => return Err("E471: Argument required".into()),
            _ => {}
        }
        let cur = self.cursor().line;
        let (l1, l2) = c.range.unwrap_or((cur, cur));
        match &cmd.repl {
            AutoAction::Ex(repl) => {
                let fargs: Vec<String> = if cmd.nargs == '1' { vec![args.clone()] } else { split_args(&args) };
                let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
                let out = repl
                    .replace("<q-args>", &quote(&args))
                    .replace("<f-args>", &fargs.iter().map(|a| quote(a)).collect::<Vec<_>>().join(", "))
                    .replace("<args>", &args)
                    .replace("<bang>", if c.bang { "!" } else { "" })
                    .replace("<q-bang>", if c.bang { "'!'" } else { "''" })
                    .replace("<line1>", &(l1 + 1).to_string())
                    .replace("<line2>", &(l2 + 1).to_string())
                    .replace("<count>", &c.count.map(|x| x.to_string()).unwrap_or_else(|| "0".into()))
                    .replace("<lt>", "<");
                self.exec_lines(&out, "command")
            }
            AutoAction::Lua(id) => {
                let host = self.script.clone().ok_or("E319: Lua is not available")?;
                let fargs: Vec<Value> = split_args(&args).into_iter().map(Value::Str).collect();
                let arg = Value::Dict(vec![
                    ("name".into(), Value::Str(cmd.name.clone())),
                    ("args".into(), Value::Str(args)),
                    ("fargs".into(), Value::List(fargs)),
                    ("bang".into(), Value::Bool(c.bang)),
                    ("line1".into(), Value::Int(l1 as i64 + 1)),
                    ("line2".into(), Value::Int(l2 as i64 + 1)),
                    ("range".into(), Value::Int(c.addr_count as i64)),
                    ("count".into(), Value::Int(c.count.map(|x| x as i64).unwrap_or(-1))),
                ]);
                host.call_ref(self, *id, vec![arg]).map(|_| ())
            }
        }
    }

    // ----------------------------------------------------------- substitute

    fn do_substitute(&mut self, name: &str, c: &ExCmd) -> Result<(), String> {
        let (s, e) = c.lines(self.cursor().line);
        let arg = c.arg.as_str();
        let (pat, rep, flags_s): (String, String, String);
        let first = arg.chars().next();
        if name == "substitute" && first.map(|d| !d.is_alphanumeric() && d != ' ' && d != '"' && d != '|' && d != '&').unwrap_or(false) {
            let delim = first.unwrap();
            let parts = split_delim(&arg[delim.len_utf8()..], delim);
            let p = parts.first().cloned().unwrap_or_default();
            pat = if p.is_empty() { self.last_search.clone().ok_or("E35: No previous regular expression")? } else { p };
            rep = match parts.get(1) {
                Some(r) => r.clone(),
                None => String::new(),
            };
            flags_s = parts.get(2).cloned().unwrap_or_default();
        } else {
            // repeat: `:s`, `:&`, `:&&`, `:~`
            let (lp, lr, lf) = self.last_sub.clone().ok_or("E35: No previous regular expression")?;
            pat = if name == "~" { self.last_search.clone().unwrap_or(lp) } else { lp };
            rep = lr;
            let mut f = arg.trim().to_string();
            if name == "&&" || f.starts_with('&') {
                f = format!("{}{}", lf, f.trim_start_matches('&'));
            }
            flags_s = f;
        }
        let (flags, count) = {
            let f = flags_s.trim();
            let fend = f.find(|ch: char| ch.is_ascii_digit() || ch.is_whitespace()).unwrap_or(f.len());
            (f[..fend].to_string(), f[fend..].trim().parse::<usize>().ok())
        };
        let (s, e) = match count {
            Some(n) => (e, (e + n.max(1) - 1).min(self.buf().last_line())),
            None => (s, e),
        };
        let rep = if rep == "~" { self.last_sub.as_ref().map(|x| x.1.clone()).unwrap_or_default() } else { rep };
        self.last_sub = Some((pat.clone(), rep.clone(), flags.replace('&', "")));
        self.last_search = Some(pat.clone());
        self.hlsearch_off = false;
        let mut global = self.opt_global("gdefault").as_bool();
        let mut count_only = false;
        let mut quiet = false;
        let mut pat_use = pat.clone();
        for f in flags.chars() {
            match f {
                'g' => global = !global,
                'n' => count_only = true,
                'e' => quiet = true,
                'i' => pat_use = format!("\\c{}", pat),
                'I' => pat_use = format!("\\C{}", pat),
                _ => {}
            }
        }
        let re = self.compile_pattern(&pat_use)?;
        let expr = rep.strip_prefix("\\=").map(|x| x.to_string());
        let mut nsubs = 0usize;
        let mut nlines = 0usize;
        let mut last_line = None;
        let mut l = s;
        let mut end = e.min(self.buf().last_line());
        while l <= end {
            let line = self.line(l).to_string();
            let mut out = String::new();
            let mut at = 0usize;
            let mut changed = false;
            let mut search_from = 0usize;
            loop {
                let Some((m, groups)) = re.captures_at(&line, search_from) else {
                    break;
                };
                if m.start < at {
                    break;
                }
                nsubs += 1;
                changed = true;
                out.push_str(&line[at..m.start]);
                let replacement = match &expr {
                    Some(ex) => {
                        // submatch() is provided through a global table
                        if let Some(h) = self.script.clone() {
                            let list = Value::List(groups.iter().map(|g| Value::Str(g.unwrap_or("").to_string())).collect());
                            h.set_global(self, "__submatch", list);
                        }
                        self.eval_expr(ex)?.to_display()
                    }
                    None => expand_replacement(&rep, &groups),
                };
                out.push_str(&replacement);
                at = m.end;
                if !global {
                    break;
                }
                if m.end == m.start {
                    // empty match: copy one character and move on
                    match line[m.end..].chars().next() {
                        Some(chr) => {
                            out.push(chr);
                            at = m.end + chr.len_utf8();
                            search_from = at;
                        }
                        None => break,
                    }
                } else {
                    search_from = m.end;
                }
                if search_from > line.len() {
                    break;
                }
            }
            if changed {
                nlines += 1;
                if !count_only {
                    out.push_str(&line[at..]);
                    let new_lines: Vec<String> = out.split('\r').map(|x| x.to_string()).collect();
                    let added = new_lines.len() - 1;
                    self.buf_mut().splice(l, l + 1, new_lines);
                    l += added;
                    end += added;
                    last_line = Some(l);
                }
            }
            l += 1;
        }
        if nsubs == 0 {
            if quiet {
                return Ok(());
            }
            return Err(format!("E486: Pattern not found: {}", pat));
        }
        if count_only {
            self.msg(format!("{} match{} on {} line{}", nsubs, if nsubs == 1 { "" } else { "es" }, nlines, if nlines == 1 { "" } else { "s" }));
            return Ok(());
        }
        if let Some(ll) = last_line {
            let col = first_nonblank(self.line(ll));
            self.set_cursor(Pos::new(ll, col));
        }
        if nsubs > self.opt_global("report").as_num().max(0) as usize {
            self.msg(format!("{} substitution{} on {} line{}", nsubs, if nsubs == 1 { "" } else { "s" }, nlines, if nlines == 1 { "" } else { "s" }));
        }
        Ok(())
    }

    fn do_global(&mut self, c: &ExCmd, invert: bool) -> Result<(), String> {
        let arg = c.arg.as_str();
        let delim = arg.chars().next().ok_or("E35: No previous regular expression")?;
        let parts = split_delim(&arg[delim.len_utf8()..], delim);
        let pat = parts.first().cloned().unwrap_or_default();
        let pat = if pat.is_empty() { self.last_search.clone().ok_or("E35: No previous regular expression")? } else { pat };
        // the command is everything after the second delimiter (it may contain the delimiter)
        let cmd = {
            let body = &arg[delim.len_utf8()..];
            let mut esc = false;
            let mut idx = None;
            for (i, ch) in body.char_indices() {
                if esc {
                    esc = false;
                } else if ch == '\\' {
                    esc = true;
                } else if ch == delim {
                    idx = Some(i + ch.len_utf8());
                    break;
                }
            }
            idx.map(|i| body[i..].to_string()).unwrap_or_default()
        };
        let cmd = if cmd.trim().is_empty() { "p".to_string() } else { cmd };
        let re = self.compile_pattern(&pat)?;
        self.last_search = Some(pat.clone());
        let (s, e) = c.range.unwrap_or((0, self.buf().last_line()));
        // mark matching lines, then run the command on each (tracking shifts with a mark)
        let marked: Vec<usize> = (s..=e.min(self.buf().last_line())).filter(|l| re.is_match(self.line(*l)) != invert).collect();
        if marked.is_empty() {
            return Err(format!("E486: Pattern not found: {}", pat));
        }
        // Use line identity tracking: remember content pointers via an offset that follows edits.
        let mut delta: isize = 0;
        let mut prev_count = self.buf().line_count() as isize;
        let top = self.begin_collect();
        for l in marked {
            let target = l as isize + delta;
            if target < 0 || target as usize > self.buf().last_line() {
                continue;
            }
            self.set_cursor(Pos::new(target as usize, 0));
            let r = self.exec_lines(&cmd, "global");
            if let Err(err) = r {
                if top {
                    self.end_collect();
                }
                return Err(err);
            }
            let now = self.buf().line_count() as isize;
            delta += now - prev_count;
            prev_count = now;
        }
        if top {
            self.end_collect();
        }
        Ok(())
    }

    // ----------------------------------------------------------------- misc

    /// Source a `.lua` or Vim script file.
    pub fn source_file(&mut self, path: &Path) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("E484: Can't open file {}: {}", path.display(), e))?;
        let fencs = self.opt_global("fileencodings").as_string();
        let text = super::buffer::decode_bytes(&bytes, &fencs).lines.join("\n");
        let host = self.script.clone().ok_or("E319: scripting is not available")?;
        let name = path.to_string_lossy().replace('\\', "/");
        self.do_autocmd("SourcePre", &name);
        let is_lua = path.extension().map(|e| e.eq_ignore_ascii_case("lua")).unwrap_or(false);
        let r = if is_lua { host.exec_lua(self, &text, &name) } else { host.exec_vim(self, &text, &name) };
        self.do_autocmd("SourcePost", &name);
        r
    }

    fn import_vimrc_cmd(&mut self, arg: &str, bang: bool) -> Result<(), String> {
        let parts = split_args(arg);
        let Some(src) = parts.first() else {
            return Err("E471: Argument required: :ImportVimrc {vimrc} [{output.lua}]".into());
        };
        let src = PathBuf::from(self.expand_filename(src));
        let bytes = std::fs::read(&src).map_err(|e| format!("E484: Can't open file {}: {}", src.display(), e))?;
        let fencs = self.opt_global("fileencodings").as_string();
        let text = super::buffer::decode_bytes(&bytes, &fencs).lines.join("\n");
        let host = self.script.clone().ok_or("E319: scripting is not available")?;
        let lua = host.transpile(&text, &src.to_string_lossy())?;
        let out = match parts.get(1) {
            Some(o) => PathBuf::from(self.expand_filename(o)),
            None => {
                let is_g = src.file_name().map(|n| n.to_string_lossy().to_lowercase().contains("gvimrc")).unwrap_or(false);
                crate::config_dir().join(if is_g { "ginit.lua" } else { "init.lua" })
            }
        };
        if out.exists() && !bang {
            return Err(format!("E13: File exists (add ! to override): {}", out.display()));
        }
        if let Some(dir) = out.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&out, lua).map_err(|e| format!("E212: Can't open file for writing: {} ({})", out.display(), e))?;
        self.msg(format!("Imported {} -> {}", src.display(), out.display()));
        Ok(())
    }
}

/// Decode the output of a shell command: UTF-8, or the system ANSI code page
/// on Windows (e.g. CP932 for Japanese `cmd.exe`).
pub fn decode_console(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    #[cfg(windows)]
    {
        unsafe extern "system" {
            fn GetACP() -> u32;
        }
        let label = match unsafe { GetACP() } {
            932 => "shift_jis",
            936 => "gbk",
            949 => "euc-kr",
            950 => "big5",
            1250 => "windows-1250",
            1251 => "windows-1251",
            _ => "windows-1252",
        };
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            return enc.decode(bytes).0.into_owned();
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

fn retab_ws(from_col: usize, to_col: usize, ts: usize, expandtab: bool) -> String {
    if expandtab {
        return " ".repeat(to_col - from_col);
    }
    let mut out = String::new();
    let mut col = from_col;
    while (col / ts + 1) * ts <= to_col {
        out.push('\t');
        col = (col / ts + 1) * ts;
    }
    out.push_str(&" ".repeat(to_col - col));
    out
}

fn format_opt(d: &options::OptDef, v: &OptVal) -> String {
    match v {
        OptVal::Bool(true) => format!("  {}", d.name),
        OptVal::Bool(false) => format!("no{}", d.name),
        OptVal::Num(n) => format!("  {}={}", d.name, n),
        OptVal::Str(s) => format!("  {}={}", d.name, s),
    }
}

fn is_event_list(s: &str) -> bool {
    s == "*" || s.split(',').all(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphabetic()) && is_known_event(e))
}

fn is_known_event(e: &str) -> bool {
    const EVENTS: &[&str] = &[
        "bufadd", "bufcreate", "bufdelete", "bufenter", "buffilepost", "buffilepre", "bufhidden", "bufleave", "bufnew", "bufnewfile",
        "bufread", "bufreadcmd", "bufreadpost", "bufreadpre", "bufunload", "bufwinenter", "bufwinleave", "bufwipeout", "bufwrite",
        "bufwritecmd", "bufwritepost", "bufwritepre", "cmdlinechanged", "cmdlineenter", "cmdlineleave", "cmdundefined", "cmdwinenter",
        "cmdwinleave", "colorscheme", "colorschemepre", "completechanged", "completedone", "completedonepre", "cursorhold",
        "cursorholdi", "cursormoved", "cursormovedi", "diffupdated", "dirchanged", "dirchangedpre", "encodingchanged", "exitpre",
        "fileappendcmd", "fileappendpost", "fileappendpre", "filechangedro", "filechangedshell", "filechangedshellpost",
        "fileencoding", "filereadcmd", "filereadpost", "filereadpre", "filetype", "filewritecmd", "filewritepost", "filewritepre",
        "filterreadpost", "filterreadpre", "filterwritepost", "filterwritepre", "focusgained", "focuslost", "funcundefined",
        "guienter", "guifailed", "insertchange", "insertcharpre", "insertenter", "insertleave", "insertleavepre", "menupopup",
        "modechanged", "optionset", "quickfixcmdpost", "quickfixcmdpre", "quitpre", "remotereply", "safestate", "safestateagain",
        "sessionloadpost", "shellcmdpost", "shellfilterpost", "sigusr1", "sourcecmd", "sourcepost", "sourcepre", "spellfilemissing",
        "stdinreadpost", "stdinreadpre", "swapexists", "syntax", "tabclosed", "tabenter", "tableave", "tabnew", "termchanged",
        "terminalopen", "terminalwinopen", "termresponse", "textchanged", "textchangedi", "textchangedp", "textchangedt",
        "textyankpost", "user", "vimenter", "vimleave", "vimleavepre", "vimresized", "vimresume", "vimsuspend", "winclosed",
        "winenter", "winleave", "winnew", "winresized", "winscrolled",
    ];
    EVENTS.contains(&e.to_ascii_lowercase().as_str())
}

fn parse_reg_count(arg: &str) -> (Option<char>, Option<usize>) {
    let a = arg.trim();
    let mut reg = None;
    let mut rest = a;
    if let Some(c) = a.chars().next() {
        if !c.is_ascii_digit() {
            reg = Some(c);
            rest = a[c.len_utf8()..].trim();
        }
    }
    (reg, rest.parse().ok())
}

/// Split `pat/rep/flags` on an unescaped delimiter (the escape before the
/// delimiter is dropped).
fn split_delim(s: &str, delim: char) -> Vec<String> {
    let mut parts = vec![String::new()];
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(n) if *n == delim => {
                    parts.last_mut().unwrap().push(delim);
                    chars.next();
                }
                Some(n) => {
                    let n = *n;
                    parts.last_mut().unwrap().push('\\');
                    parts.last_mut().unwrap().push(n);
                    chars.next();
                }
                None => parts.last_mut().unwrap().push('\\'),
            }
        } else if c == delim && parts.len() < 3 {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    parts
}

/// Expand `&`, `\0`-`\9`, `\r`, `\n`, `\t` in a substitute replacement.
/// A line break is returned as `\r`.
pub fn expand_replacement(rep: &str, groups: &[Option<&str>]) -> String {
    let mut out = String::new();
    let mut chars = rep.chars();
    let mut case: Option<char> = None; // \u \l (one char) \U \L (until \E)
    let push = |out: &mut String, s: &str, case: &mut Option<char>| {
        for ch in s.chars() {
            match *case {
                Some('u') => {
                    out.extend(ch.to_uppercase());
                    *case = None;
                }
                Some('l') => {
                    out.extend(ch.to_lowercase());
                    *case = None;
                }
                Some('U') => out.extend(ch.to_uppercase()),
                Some('L') => out.extend(ch.to_lowercase()),
                _ => out.push(ch),
            }
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '&' => push(&mut out, groups.first().copied().flatten().unwrap_or(""), &mut case),
            '\r' => out.push('\r'),
            '\\' => match chars.next() {
                Some(d) if d.is_ascii_digit() => {
                    let i = d as usize - '0' as usize;
                    push(&mut out, groups.get(i).copied().flatten().unwrap_or(""), &mut case)
                }
                Some('r') => out.push('\r'),
                Some('n') => out.push('\0'),
                Some('t') => out.push('\t'),
                Some('u') => case = Some('u'),
                Some('l') => case = Some('l'),
                Some('U') => case = Some('U'),
                Some('L') => case = Some('L'),
                Some('e') | Some('E') => case = None,
                Some(o) => push(&mut out, &o.to_string(), &mut case),
                None => out.push('\\'),
            },
            c => push(&mut out, &c.to_string(), &mut case),
        }
    }
    out
}

pub fn replace_leader(s: &str, leader: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let lower = tail.to_ascii_lowercase();
        if lower.starts_with("<leader>") {
            out.push_str(&leader.replace('<', "<lt>"));
            rest = &tail[8..];
        } else if lower.starts_with("<localleader>") {
            out.push_str(&leader.replace('<', "<lt>"));
            rest = &tail[13..];
        } else {
            out.push('<');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

fn name_static(name: &str) -> &'static str {
    ex::map_command(name).unwrap_or("")
}

/// Highlight group names used with `:echohl` must be `'static`; they come from
/// a small set, so leaking them is fine.
fn leak_group(name: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut m = NAMES.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
    if let Some(s) = m.get(name) {
        return s;
    }
    let s: &'static str = Box::leak(name.to_string().into_boxed_str());
    m.insert(name.to_string(), s);
    s
}

pub fn intern_group(name: &str) -> &'static str {
    leak_group(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers() {
        assert_eq!(fnamemodify("C:/a/b/c.tar.gz", ":t"), "c.tar.gz");
        assert_eq!(fnamemodify("C:/a/b/c.tar.gz", ":h"), "C:/a/b");
        assert_eq!(fnamemodify("C:/a/b/c.tar.gz", ":t:r"), "c.tar");
        assert_eq!(fnamemodify("C:/a/b/c.tar.gz", ":e"), "gz");
        assert_eq!(fnamemodify("c.txt", ":h"), ".");
    }

    #[test]
    fn replacement() {
        assert_eq!(expand_replacement("[&]\\1\\r", &[Some("ab"), Some("a")]), "[ab]a\r");
        assert_eq!(expand_replacement("\\u&", &[Some("ab")]), "Ab");
        assert_eq!(split_delim("a\\/b/c/g", '/'), vec!["a/b", "c", "g"]);
    }
}
