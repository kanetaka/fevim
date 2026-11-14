//! Insert and replace mode, auto-indent and keyword completion.

use super::buffer::Pos;
use super::cmdline::CmdLine;
use super::keys::{self, Key, KeyCode, Sp, CTRL};
use super::motion::{self, char_at, col_at_vcol, first_nonblank, indent_of, last_char_start, line_width, next_char_end, prev_char_start, vcol_of, Motion};
use super::normal::make_indent;
use super::{Completion, Editor, Mode, RegKind};

#[derive(Default, Clone, Debug)]
pub struct InsertState {
    pub count: usize,
    pub cmd: char,
    /// Keys typed during this insert (for count repeats).
    pub typed: Vec<Key>,
    pub replaying: bool,
    pub literal_next: bool,
    pub pending_register: bool,
    pub ctrl_x: bool,
    /// Visual-block insert: (first line, last line, display column, append).
    pub block: Option<(usize, usize, usize, bool)>,
    pub start: Pos,
    /// Line whose auto-indent should be removed if nothing is typed on it.
    pub auto_indent_line: Option<usize>,
    pub inserted: String,
}

impl Editor {
    pub fn start_insert(&mut self, cmd: char, count: usize) {
        self.mode = if cmd == 'R' { Mode::Replace } else { Mode::Insert };
        let cur = self.cursor();
        let auto = if matches!(cmd, 'o' | 'O') && !self.cur_line().is_empty() { Some(cur.line) } else { None };
        self.ins = InsertState { count, cmd, start: cur, auto_indent_line: auto, ..Default::default() };
        if !self.buf().has_open_undo() {
            self.buf_mut().undo_cursor = cur;
        }
        self.completion = None;
        self.restart_insert = false;
        if !self.replaying_dot {
            self.do_autocmd("InsertEnter", "");
        }
    }

    pub fn leave_insert(&mut self) {
        self.completion = None;
        self.strip_auto_indent();
        // count: repeat the typed text
        if self.ins.count > 1 && !self.ins.replaying {
            let mut typed = self.ins.typed.clone();
            typed.pop(); // the <Esc>
            let count = self.ins.count;
            let cmd = self.ins.cmd;
            self.ins.replaying = true;
            for _ in 1..count {
                if matches!(cmd, 'o' | 'O') {
                    let cur = self.cursor();
                    let at = if cmd == 'o' { cur.line + 1 } else { cur.line };
                    let indent = self.indent_for_open(cur.line, cmd == 'o');
                    let len = indent.len();
                    self.buf_mut().insert_lines(at, vec![indent]);
                    self.win_mut().cursor = Pos::new(at, len);
                }
                for k in &typed {
                    self.insert_key_inner(*k);
                }
            }
            self.ins.replaying = false;
        }
        // visual block insert: replicate on the other lines
        if let Some((first, last, vcol, append)) = self.ins.block.take() {
            let cur = self.cursor();
            let start = self.ins.start;
            if cur.line == start.line && cur.col > start.col {
                let text = self.cur_line()[start.col..cur.col].to_string();
                let ts = self.tabstop_of(self.win());
                let ambi = self.ambi_double();
                for l in first..=last.min(self.buf().last_line()) {
                    let mut line = self.line(l).to_string();
                    let w = line_width(&line, ts, ambi);
                    let col = if vcol == usize::MAX {
                        line.len()
                    } else if w < vcol {
                        if !append {
                            continue; // short lines are skipped for `I`
                        }
                        line.push_str(&" ".repeat(vcol - w));
                        line.len()
                    } else {
                        col_at_vcol(&line, vcol, ts, ambi).min(line.len())
                    };
                    line.insert_str(col, &text);
                    self.buf_mut().set_line(l, line);
                }
                self.win_mut().cursor = start;
            }
        }
        let cur = self.cursor();
        self.set_mark('^', cur);
        self.set_mark('.', cur);
        self.last_insert = std::mem::take(&mut self.ins.inserted);
        let was_restart = self.restart_insert;
        self.mode = Mode::Normal;
        self.restart_insert = false;
        if cur.col > 0 && !was_restart {
            let c = prev_char_start(self.cur_line(), cur.col);
            self.win_mut().cursor.col = c;
        }
        let ts = self.tabstop_of(self.win());
        let v = vcol_of(self.cur_line(), self.cursor().col, ts, self.ambi_double());
        self.win_mut().curswant = v;
        if let Some(rec) = self.dot_recording.take() {
            self.dot.keys = rec;
            self.dot.visual = self.dot_visual_pending.take();
        }
        let c = self.cursor();
        self.buf_mut().close_undo(c);
        if !self.replaying_dot {
            self.do_autocmd("InsertLeave", "");
        }
    }

    fn strip_auto_indent(&mut self) {
        if let Some(l) = self.ins.auto_indent_line.take() {
            if l <= self.buf().last_line() && !self.line(l).is_empty() && self.line(l).trim().is_empty() {
                self.buf_mut().set_line(l, String::new());
                if self.cursor().line == l {
                    self.win_mut().cursor.col = 0;
                }
            }
        }
    }

    pub fn insert_key(&mut self, key: Key) {
        if let Some(rec) = &mut self.dot_recording {
            rec.push(key);
        }
        if !self.ins.replaying {
            self.ins.typed.push(key);
        }
        self.insert_key_inner(key);
    }

    fn insert_key_inner(&mut self, key: Key) {
        if self.ins.literal_next {
            self.ins.literal_next = false;
            match key.code {
                KeyCode::Char(c) => self.insert_text_at_cursor(&c.to_string()),
                _ => {}
            }
            return;
        }
        if self.ins.pending_register {
            self.ins.pending_register = false;
            if key.is('=') {
                self.expr_prompt = Some(CmdLine::new('=', ""));
                return;
            }
            if key.is_ctrl('r') || key.is_ctrl('o') || key.is_ctrl('p') {
                self.ins.pending_register = true;
                return;
            }
            if let Some(c) = key.as_char() {
                if let Some(r) = self.reg_get(c) {
                    let mut text = r.text;
                    if r.kind == RegKind::Line {
                        text.push('\n');
                    }
                    self.insert_text_at_cursor(&text);
                }
            }
            return;
        }
        if self.ins.ctrl_x {
            self.ins.ctrl_x = false;
            if key.is_ctrl('n') || key.is_ctrl('p') {
                self.complete(key.is_ctrl('n'));
                return;
            }
            if key.is_ctrl('l') {
                self.complete_line();
                return;
            }
            if key.is_ctrl('e') || key.is_ctrl('y') {
                self.scroll_lines(if key.is_ctrl('e') { 1 } else { -1 });
                return;
            }
            return;
        }
        let pum = self.completion.is_some();
        if key == keys::ESC || key.is_ctrl('c') {
            self.leave_insert();
            return;
        }
        if key.is_ctrl('n') || key.is_ctrl('p') {
            self.complete(key.is_ctrl('n'));
            return;
        }
        if pum {
            if key.is_ctrl('y') {
                self.completion = None;
                return;
            }
            if key.is_ctrl('e') {
                self.apply_completion(None);
                self.completion = None;
                return;
            }
            if key.is_sp(Sp::Down) || key.is_sp(Sp::Up) {
                self.complete(key.is_sp(Sp::Down));
                return;
            }
            self.completion = None;
        }
        match key.code {
            KeyCode::Char('\r') | KeyCode::Char('\n') if key.mods == 0 => self.insert_newline(),
            KeyCode::Char('\t') if key.mods == 0 => self.insert_tab(),
            KeyCode::Sp(Sp::Backspace) | KeyCode::Char('\x08') if key.mods & !super::keys::SHIFT == 0 => self.backspace(),
            KeyCode::Sp(Sp::Delete) | KeyCode::Char('\x7f') if key.mods == 0 => {
                let cur = self.cursor();
                let line = self.cur_line();
                if cur.col < line.len() {
                    let e = next_char_end(line, cur.col);
                    self.buf_mut().delete_range(cur, Pos::new(cur.line, e));
                } else if cur.line < self.buf().last_line() {
                    self.buf_mut().delete_range(cur, Pos::new(cur.line + 1, 0));
                }
            }
            KeyCode::Char('\x17') if key.mods == 0 => {
                // CTRL-W: delete the word before the cursor
                let cur = self.cursor();
                if cur.col == 0 {
                    self.backspace();
                } else {
                    let line = self.cur_line().to_string();
                    let mut s = cur.col;
                    while s > 0 && matches!(char_at(&line, prev_char_start(&line, s)), Some(' ') | Some('\t')) {
                        s = prev_char_start(&line, s);
                    }
                    if s > 0 {
                        let cls = motion::char_class(char_at(&line, prev_char_start(&line, s)).unwrap(), false);
                        while s > 0 && char_at(&line, prev_char_start(&line, s)).map(|c| motion::char_class(c, false)) == Some(cls) {
                            s = prev_char_start(&line, s);
                        }
                    }
                    if cur.line == self.ins.start.line && self.ins.start.col < cur.col && s < self.ins.start.col && !self.opt_str("backspace").contains("start") {
                        s = self.ins.start.col;
                    }
                    self.buf_mut().delete_range(Pos::new(cur.line, s), cur);
                    self.win_mut().cursor.col = s;
                }
            }
            KeyCode::Char('\x15') if key.mods == 0 => {
                // CTRL-U: delete all entered text before the cursor in the line
                let cur = self.cursor();
                let line = self.cur_line().to_string();
                let ind = indent_of(&line).len();
                let s = if cur.col > ind { ind } else { 0 };
                self.buf_mut().delete_range(Pos::new(cur.line, s), cur);
                self.win_mut().cursor.col = s;
            }
            KeyCode::Char('\x14') | KeyCode::Char('\x04') if key.mods == 0 => {
                let cur = self.cursor();
                let before = self.cur_line().len();
                self.shift_lines(cur.line, cur.line, if key.is_ctrl('t') { 1 } else { -1 });
                let after = self.cur_line().len();
                let col = (cur.col as isize + after as isize - before as isize).max(0) as usize;
                self.win_mut().cursor.col = col.min(after);
            }
            KeyCode::Char('\x12') if key.mods == 0 => self.ins.pending_register = true,
            KeyCode::Char('\x0f') if key.mods == 0 => {
                // CTRL-O: execute one normal mode command
                self.mode = Mode::Normal;
                self.restart_insert = true;
                let cur = self.cursor();
                self.buf_mut().close_undo(cur);
            }
            KeyCode::Char('\x16') | KeyCode::Char('\x11') if key.mods == 0 => self.ins.literal_next = true,
            KeyCode::Char('\x18') if key.mods == 0 => self.ins.ctrl_x = true,
            KeyCode::Char('\x01') if key.mods == 0 => {
                let t = self.last_insert.clone();
                self.insert_text_at_cursor(&t);
            }
            KeyCode::Char('\x05') | KeyCode::Char('\x19') if key.mods == 0 => {
                // copy the character below / above the cursor
                let cur = self.cursor();
                let other = if key.is_ctrl('e') { cur.line + 1 } else { cur.line.wrapping_sub(1) };
                if other <= self.buf().last_line() {
                    let ts = self.tabstop_of(self.win());
                    let ambi = self.ambi_double();
                    let v = vcol_of(self.cur_line(), cur.col, ts, ambi);
                    let ol = self.line(other);
                    let c = col_at_vcol(ol, v, ts, ambi);
                    if let Some(chr) = char_at(ol, c) {
                        self.insert_char(chr);
                    }
                }
            }
            KeyCode::Sp(Sp::Insert) if key.mods == 0 => {
                self.mode = if self.mode == Mode::Insert { Mode::Replace } else { Mode::Insert };
            }
            KeyCode::Sp(sp) => {
                let m = match (sp, key.mods) {
                    (Sp::Left, 0) => Some(Motion::Left),
                    (Sp::Right, 0) => Some(Motion::Right),
                    (Sp::Up, 0) => Some(Motion::Up),
                    (Sp::Down, 0) => Some(Motion::Down),
                    (Sp::Home, 0) => Some(Motion::LineStart),
                    (Sp::End, 0) => Some(Motion::LineEnd),
                    (Sp::Home, CTRL) => Some(Motion::FirstLine),
                    (Sp::End, CTRL) => Some(Motion::GotoLine),
                    (Sp::PageUp, 0) => Some(Motion::PageUp),
                    (Sp::PageDown, 0) => Some(Motion::PageDown),
                    (Sp::Left, _) => Some(Motion::WordBack(false)),
                    (Sp::Right, _) => Some(Motion::WordFwd(false)),
                    (Sp::ScrollUp, _) => {
                        self.scroll_lines(-3);
                        None
                    }
                    (Sp::ScrollDown, _) => {
                        self.scroll_lines(3);
                        None
                    }
                    _ => None,
                };
                if let Some(m) = m {
                    self.strip_auto_indent();
                    let is_end = m == Motion::LineEnd;
                    let is_right = m == Motion::Right;
                    if let Some(r) = self.motion_target(&m, None, true) {
                        let mut p = r.pos;
                        if is_end {
                            p.col = self.line(p.line).len();
                        }
                        self.win_mut().cursor = p;
                        if let Some(c) = r.curswant {
                            self.win_mut().curswant = c;
                        }
                    } else if is_right {
                        let l = self.cur_line().len();
                        self.win_mut().cursor.col = l;
                    }
                    // cursor movement starts a new undoable insert
                    let cur = self.cursor();
                    self.buf_mut().close_undo(cur);
                    self.buf_mut().undo_cursor = cur;
                    self.ins.start = cur;
                }
            }
            KeyCode::Char(_) => {
                if let Some(c) = key.printable() {
                    self.insert_char(c);
                }
            }
        }
    }

    /// Insert literal text at the cursor (no indent processing).
    pub fn insert_text_at_cursor(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.ins.auto_indent_line = None;
        let cur = self.cursor();
        let end = self.buf_mut().insert_text(cur, text);
        self.win_mut().cursor = end;
        self.ins.inserted.push_str(text);
    }

    pub fn insert_char(&mut self, c: char) {
        let cur = self.cursor();
        self.ins.auto_indent_line = None;
        if self.mode == Mode::Replace {
            let line = self.cur_line();
            if cur.col < line.len() {
                let e = next_char_end(line, cur.col);
                self.buf_mut().delete_range(cur, Pos::new(cur.line, e));
            }
        }
        // closing brace: line up with the opening one
        let smart = self.opt_bool("cindent") || self.opt_bool("smartindent");
        if c == '}' && smart && !self.cur_line().is_empty() && self.cur_line()[..cur.col].trim().is_empty() {
            if let Some(open) = motion::find_unmatched(self.buf(), cur, '{', '}', false) {
                let ind = indent_of(self.line(open.line)).to_string();
                let rest = self.cur_line()[cur.col..].to_string();
                let new = format!("{}{}", ind, rest);
                self.buf_mut().set_line(cur.line, new);
                self.win_mut().cursor.col = ind.len();
            }
        }
        let cur = self.cursor();
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        let end = self.buf_mut().insert_text(cur, s);
        self.win_mut().cursor = end;
        self.ins.inserted.push(c);
        // automatic wrapping at 'textwidth'
        let tw = self.opt_num("textwidth");
        if tw > 0 && c != ' ' && c != '\t' && self.opt_str("formatoptions").contains('t') {
            let ts = self.tabstop_of(self.win());
            let line = self.cur_line().to_string();
            if line_width(&line, ts, self.ambi_double()) > tw as usize {
                let cur = self.cursor();
                if let Some(sp) = line[..cur.col].rfind(' ') {
                    if !line[..sp].trim().is_empty() {
                        let indent = if self.opt_bool("autoindent") { indent_of(&line).to_string() } else { String::new() };
                        let head = line[..sp].trim_end().to_string();
                        let tail = format!("{}{}", indent, &line[sp + 1..]);
                        let newcol = indent.len() + (cur.col - sp - 1);
                        self.buf_mut().splice(cur.line, cur.line + 1, vec![head, tail]);
                        self.win_mut().cursor = Pos::new(cur.line + 1, newcol);
                    }
                }
            }
        }
    }

    /// Indent for a line opened below (`o`) or above (`O`) line `lnum`.
    pub fn indent_for_open(&self, lnum: usize, below: bool) -> String {
        let line = self.line(lnum);
        if below {
            self.indent_after(line, "")
        } else if self.opt_bool("autoindent") || self.opt_bool("cindent") || self.opt_bool("smartindent") {
            indent_of(line).to_string()
        } else {
            String::new()
        }
    }

    /// Indent of a new line following `before` (the text left of the cursor),
    /// with `after` being the text that moves to the new line.
    fn indent_after(&self, before: &str, after: &str) -> String {
        let ai = self.opt_bool("autoindent");
        let si = self.opt_bool("smartindent");
        let ci = self.opt_bool("cindent");
        if !(ai || si || ci) {
            return String::new();
        }
        let ts = self.opt_num("tabstop").max(1) as usize;
        let et = self.opt_bool("expandtab");
        let sw = self.shiftwidth();
        let base = line_width(indent_of(before), ts, false);
        let mut width = base;
        let t = before.trim();
        if (si || ci) && !after.trim_start().starts_with('}') {
            let opens = t.ends_with('{') || (ci && (t.ends_with('(') || t.ends_with('[')));
            let first_word = t.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or("");
            let cinword = si && !ci && !first_word.is_empty() && self.opt_str("cinwords").split(',').any(|w| w == first_word);
            let python_colon = self.opt_str("filetype") == "python" && t.ends_with(':');
            if opens || cinword || python_colon {
                width += sw;
            }
        }
        if t.is_empty() && before.is_empty() {
            return String::new();
        }
        make_indent(width, ts, et)
    }

    fn insert_newline(&mut self) {
        let cur = self.cursor();
        let line = self.cur_line().to_string();
        let (before, after) = line.split_at(cur.col.min(line.len()));
        let indent = self.indent_after(before, after);
        // an auto-indented line left empty loses its indent
        let head = if self.ins.auto_indent_line == Some(cur.line) && before.trim().is_empty() { String::new() } else { before.to_string() };
        let head = if (self.opt_bool("autoindent") || self.opt_bool("cindent")) && !head.trim().is_empty() { head.trim_end_matches([' ', '\t']).to_string() } else { head };
        let after_t = if indent.is_empty() { after.to_string() } else { after.trim_start_matches([' ', '\t']).to_string() };
        let tail = format!("{}{}", indent, after_t);
        self.buf_mut().splice(cur.line, cur.line + 1, vec![head, tail]);
        self.win_mut().cursor = Pos::new(cur.line + 1, indent.len());
        self.ins.auto_indent_line = if indent.is_empty() || !after_t.is_empty() { None } else { Some(cur.line + 1) };
        self.ins.inserted.push('\n');
    }

    fn insert_tab(&mut self) {
        let cur = self.cursor();
        let ts = self.opt_num("tabstop").max(1) as usize;
        let sts = match self.opt_num("softtabstop") {
            n if n < 0 => self.shiftwidth(),
            n => n as usize,
        };
        let in_indent = self.cur_line()[..cur.col].trim().is_empty();
        let step = if self.opt_bool("smarttab") && in_indent { self.shiftwidth() } else if sts > 0 { sts } else { ts };
        if self.opt_bool("expandtab") || (sts > 0 && sts != ts) || (self.opt_bool("smarttab") && in_indent && step != ts) {
            let v = vcol_of(self.cur_line(), cur.col, ts, self.ambi_double());
            let n = step - v % step;
            if self.opt_bool("expandtab") {
                self.insert_text_at_cursor(&" ".repeat(n));
            } else {
                // fill with tabs where possible
                let target = v + n;
                let line = self.cur_line().to_string();
                let ws_start = line[..cur.col].trim_end_matches([' ', '\t']).len();
                let v0 = vcol_of(&line, ws_start, ts, self.ambi_double());
                let mut ws = String::new();
                let mut v1 = v0;
                while (v1 / ts + 1) * ts <= target {
                    ws.push('\t');
                    v1 = (v1 / ts + 1) * ts;
                }
                ws.push_str(&" ".repeat(target - v1));
                let new = format!("{}{}{}", &line[..ws_start], ws, &line[cur.col..]);
                self.buf_mut().set_line(cur.line, new);
                self.win_mut().cursor.col = ws_start + ws.len();
                self.ins.auto_indent_line = None;
            }
        } else {
            self.insert_text_at_cursor("\t");
        }
    }

    fn backspace(&mut self) {
        let cur = self.cursor();
        let bs = self.opt_str("backspace");
        let all = bs == "2" || bs == "3";
        let allow = |what: &str| all || bs.split(',').any(|x| x == what) || (bs == "1" && what != "start");
        if cur.col == 0 {
            if cur.line > 0 && allow("eol") {
                let prev_len = self.line(cur.line - 1).len();
                self.buf_mut().delete_range(Pos::new(cur.line - 1, prev_len), cur);
                self.win_mut().cursor = Pos::new(cur.line - 1, prev_len);
            }
            return;
        }
        let line = self.cur_line().to_string();
        if !allow("start") && cur.line == self.ins.start.line && cur.col <= self.ins.start.col {
            return;
        }
        let ts = self.opt_num("tabstop").max(1) as usize;
        let sts = match self.opt_num("softtabstop") {
            n if n < 0 => self.shiftwidth(),
            n => n as usize,
        };
        let in_indent = line[..cur.col].trim().is_empty();
        let step = if self.opt_bool("smarttab") && in_indent { self.shiftwidth() } else { sts };
        let mut s = prev_char_start(&line, cur.col);
        if step > 0 && char_at(&line, s) == Some(' ') {
            let v = vcol_of(&line, cur.col, ts, self.ambi_double());
            let target = (v - 1) / step * step;
            while s > 0 && char_at(&line, prev_char_start(&line, s)) == Some(' ') && vcol_of(&line, s, ts, self.ambi_double()) > target {
                s = prev_char_start(&line, s);
            }
        }
        if self.mode == Mode::Replace {
            self.win_mut().cursor.col = s;
            return;
        }
        self.buf_mut().delete_range(Pos::new(cur.line, s), cur);
        self.win_mut().cursor.col = s;
    }

    // ----------------------------------------------------------- completion

    fn complete(&mut self, forward: bool) {
        if self.completion.is_none() {
            let cur = self.cursor();
            let line = self.cur_line().to_string();
            let mut s = cur.col;
            while s > 0 {
                let p = prev_char_start(&line, s);
                if char_at(&line, p).map(|c| motion::char_class(c, false) >= 2).unwrap_or(false) {
                    s = p;
                } else {
                    break;
                }
            }
            let base = line[s..cur.col].to_string();
            let items = self.collect_words(&base, cur.line, forward);
            if items.is_empty() {
                self.msg_hl("-- Keyword completion (^N^P) Pattern not found", "ErrorMsg");
                return;
            }
            self.completion = Some(Completion { items, selected: None, start: Pos::new(cur.line, s), base });
        }
        let c = self.completion.as_ref().unwrap();
        let n = c.items.len();
        let next = match (c.selected, forward) {
            (None, true) => Some(0),
            (None, false) => Some(n - 1),
            (Some(i), true) => {
                if i + 1 < n { Some(i + 1) } else { None }
            }
            (Some(i), false) => {
                if i > 0 { Some(i - 1) } else { None }
            }
        };
        self.apply_completion(next);
    }

    fn apply_completion(&mut self, sel: Option<usize>) {
        let Some(c) = self.completion.clone() else {
            return;
        };
        let text = match sel {
            Some(i) => c.items[i].clone(),
            None => c.base.clone(),
        };
        let cur = self.cursor();
        self.buf_mut().delete_range(c.start, cur);
        let end = self.buf_mut().insert_text(c.start, &text);
        self.win_mut().cursor = end;
        if let Some(cm) = &mut self.completion {
            cm.selected = sel;
        }
    }

    fn collect_words(&self, base: &str, cur_line: usize, forward: bool) -> Vec<String> {
        let ic = self.opt_bool("ignorecase");
        let matches = |w: &str| -> bool {
            if w.len() <= base.len() && w != base {
                return false;
            }
            if w == base {
                return false;
            }
            if ic { w.to_lowercase().starts_with(&base.to_lowercase()) } else { w.starts_with(base) }
        };
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let mut scan = |line: &str, out: &mut Vec<String>| {
            let mut start: Option<usize> = None;
            for (i, c) in line.char_indices().chain(std::iter::once((line.len(), ' '))) {
                let kw = motion::char_class(c, false) >= 2;
                match (start, kw) {
                    (None, true) => start = Some(i),
                    (Some(s), false) => {
                        let w = &line[s..i];
                        if matches(w) && seen.insert(w.to_string()) {
                            out.push(w.to_string());
                        }
                        start = None;
                    }
                    _ => {}
                }
            }
        };
        let b = self.buf();
        let n = b.line_count();
        let order: Vec<usize> = if forward {
            (cur_line..n).chain(0..cur_line).collect()
        } else {
            (0..=cur_line).rev().chain((cur_line + 1..n).rev()).collect()
        };
        for l in order {
            scan(b.line(l), &mut out);
            if out.len() > 500 {
                break;
            }
        }
        for other in self.buffers.values() {
            if other.id == b.id || out.len() > 500 {
                continue;
            }
            for l in other.lines.iter().take(20_000) {
                scan(l, &mut out);
            }
        }
        if !forward {
            out.reverse();
        }
        out
    }

    fn complete_line(&mut self) {
        let cur = self.cursor();
        let prefix = self.cur_line()[..cur.col].trim_start().to_string();
        if prefix.is_empty() {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let items: Vec<String> = self
            .buf()
            .lines
            .iter()
            .enumerate()
            .filter(|(i, l)| *i != cur.line && l.trim_start().starts_with(&prefix))
            .map(|(_, l)| l.trim_start().to_string())
            .filter(|l| seen.insert(l.clone()))
            .collect();
        if items.is_empty() {
            return;
        }
        let start = Pos::new(cur.line, first_nonblank(self.cur_line()).min(cur.col));
        self.completion = Some(Completion { items, selected: None, start, base: prefix });
        self.apply_completion(Some(0));
    }

    // --------------------------------------------------------------- indent

    /// `=` operator: re-indent lines with a simple brace based rule.
    pub fn reindent_lines(&mut self, first: usize, last: usize) {
        let ts = self.opt_num("tabstop").max(1) as usize;
        let et = self.opt_bool("expandtab");
        let sw = self.shiftwidth();
        let python = self.opt_str("filetype") == "python";
        for l in first..=last.min(self.buf().last_line()) {
            let line = self.line(l).to_string();
            let t = line.trim();
            if t.is_empty() {
                if !line.is_empty() {
                    self.buf_mut().set_line(l, String::new());
                }
                continue;
            }
            if python {
                continue; // indentation is significant
            }
            let mut p = l;
            let mut width = 0usize;
            while p > 0 {
                p -= 1;
                let pl = self.line(p);
                let pt = pl.trim();
                if pt.is_empty() {
                    continue;
                }
                width = line_width(indent_of(pl), ts, false);
                let code = pt.split("//").next().unwrap_or(pt).trim_end();
                if code.ends_with('{') || code.ends_with('(') || code.ends_with('[') {
                    width += sw;
                } else if (code.starts_with("if ") || code.starts_with("if(") || code.starts_with("for ") || code.starts_with("for(") || code.starts_with("while ") || code.starts_with("while(") || code == "else")
                    && !code.ends_with(';')
                    && !code.ends_with('}')
                {
                    width += sw;
                } else if p > 0 {
                    // undo the extra indent of a brace-less single statement body
                    let mut q = p;
                    while q > 0 {
                        q -= 1;
                        let ql = self.line(q).trim();
                        if ql.is_empty() {
                            continue;
                        }
                        let qc = ql.split("//").next().unwrap_or(ql).trim_end();
                        if (qc.starts_with("if ") || qc.starts_with("if(") || qc.starts_with("for ") || qc.starts_with("for(") || qc.starts_with("while ") || qc.starts_with("while(") || qc == "else")
                            && !qc.ends_with(';')
                            && !qc.ends_with('{')
                            && !qc.ends_with('}')
                        {
                            width = line_width(indent_of(self.line(q)), ts, false);
                        }
                        break;
                    }
                }
                break;
            }
            if t.starts_with('}') || t.starts_with(')') || t.starts_with(']') {
                width = width.saturating_sub(sw);
            }
            if t.starts_with('#') && matches!(self.opt_str("filetype").as_str(), "c" | "cpp") {
                width = 0;
            }
            let new = format!("{}{}", make_indent(width, ts, et), line.trim_start());
            self.buf_mut().set_line(l, new);
        }
    }

    /// Text typed through the IME or pasted by the front end.
    pub fn input_text(&mut self, text: &str) {
        let keys: Vec<Key> = text.chars().map(|c| if c == '\n' { keys::CR } else { Key::ch(c) }).collect();
        self.input(&keys);
    }

    pub fn eol_col(&self, line: usize) -> usize {
        last_char_start(self.line(line))
    }
}
