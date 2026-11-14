//! Normal and visual mode commands, operators and registers.

use super::buffer::Pos;
use super::keys::{self, Key, KeyCode, Sp, CTRL, SHIFT};
use super::motion::{self, char_at, col_at_vcol, first_nonblank, last_char_start, next_char_end, prev_char_start, vcol_of, Motion};
use super::{Editor, Mode, RegKind, Register, VisualKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingState {
    /// Nothing typed yet (or only a count/register): normal mappings apply.
    Fresh,
    /// Waiting for a motion: operator-pending mappings apply.
    Operator,
    /// Waiting for a literal character argument: no mappings.
    NoMap,
}

pub enum Res {
    Done,
    Incomplete(PendingState),
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Delete,
    Yank,
    Change,
    ShiftLeft,
    ShiftRight,
    Indent,
    Tilde,
    Lower,
    Upper,
    Rot13,
    Format,
    Filter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RangeKind {
    Char,
    Line,
    Block,
}

#[derive(Clone, Copy, Debug)]
pub struct OpRange {
    pub start: Pos,
    /// Exclusive end for `Char`; last line for `Line`; opposite corner for `Block`.
    pub end: Pos,
    pub kind: RangeKind,
    /// Display columns (inclusive) of a block.
    pub vcols: (usize, usize),
    pub to_eol: bool,
}

fn ch(k: &Key) -> Option<char> {
    k.as_char()
}

impl Editor {
    pub fn normal_key(&mut self, key: Key) {
        if key == keys::ESC || key.is_ctrl('c') {
            if !self.pending.is_empty() {
                self.pending.clear();
                self.pending_state = PendingState::Fresh;
            } else if matches!(self.mode, Mode::Visual(_)) {
                self.exit_visual();
            } else if key == keys::ESC {
                self.clear_msg();
            }
            return;
        }
        self.pending.push(key);
        let keys = self.pending.clone();
        let tick = self.buf().changedtick;
        let buf_id = self.cur_buf_id();
        let visual_before = self.visual_range();
        self.dot_skip = false;
        self.buf_mut().undo_cursor = self.cursor();
        match self.exec_normal(&keys) {
            Res::Done => {
                self.pending.clear();
                self.pending_state = PendingState::Fresh;
                // insert-mode CTRL-O: back to insert mode after one command
                if self.restart_insert && self.mode == Mode::Normal && self.cmdline.is_none() {
                    self.restart_insert = false;
                    self.mode = Mode::Insert;
                    let cur = self.cursor();
                    self.ins.start = cur;
                    self.buf_mut().undo_cursor = cur;
                }
                let in_insert = matches!(self.mode, Mode::Insert | Mode::Replace);
                if !in_insert {
                    // every normal mode command is a separate undo step
                    let cur = self.cursor();
                    if let Some(b) = self.buffers.get_mut(&buf_id) {
                        b.close_undo(cur);
                    }
                }
                let changed = self.cur_buf_id() == buf_id && self.buf().changedtick != tick;
                if (changed || in_insert) && !self.dot_skip && !self.replaying_dot {
                    let visual = visual_before.map(|(s, e, k)| {
                        let cols = if s.line == e.line { e.col - s.col } else { e.col };
                        (k, e.line - s.line, cols)
                    });
                    if in_insert {
                        self.dot_recording = Some(keys);
                        self.dot_visual_pending = visual;
                    } else {
                        self.dot.keys = keys;
                        self.dot.visual = visual;
                    }
                }
            }
            Res::Incomplete(st) => self.pending_state = st,
            Res::Invalid => {
                self.pending.clear();
                self.pending_state = PendingState::Fresh;
            }
        }
    }

    pub fn exit_visual(&mut self) {
        if let Mode::Visual(kind) = self.mode {
            self.last_visual = Some((kind, self.visual_start, self.cursor()));
            self.mode = Mode::Normal;
            self.check_cursor();
        }
    }

    fn enter_visual(&mut self, kind: VisualKind) {
        self.visual_start = self.cursor();
        self.visual_block_dollar = false;
        self.mode = Mode::Visual(kind);
    }

    /// Parse and (when complete) execute a normal/visual mode command.
    pub fn exec_normal(&mut self, keys: &[Key]) -> Res {
        let mut i = 0;
        let mut reg: Option<char> = None;
        let mut count: Option<usize> = None;
        loop {
            let Some(k) = keys.get(i) else {
                return Res::Incomplete(PendingState::Fresh);
            };
            match ch(k) {
                Some('"') => {
                    let Some(r) = keys.get(i + 1) else {
                        return Res::Incomplete(PendingState::NoMap);
                    };
                    match ch(r) {
                        Some(c) if c.is_ascii_alphanumeric() || "\"-*+_/:.%#=".contains(c) => reg = Some(c),
                        _ => return Res::Invalid,
                    }
                    i += 2;
                }
                Some(c) if c.is_ascii_digit() && (c != '0' || count.is_some()) => {
                    count = Some(count.unwrap_or(0).saturating_mul(10).saturating_add(c as usize - '0' as usize).min(99_999_999));
                    i += 1;
                }
                _ => break,
            }
        }
        let k = keys[i];
        let rest = &keys[i + 1..];
        let visual = matches!(self.mode, Mode::Visual(_));

        // ---- operators
        if let Some((op, used)) = self.parse_operator(&keys[i..], visual) {
            let Some(used) = used else {
                return Res::Incomplete(PendingState::NoMap);
            };
            if visual {
                let r = self.visual_op_range();
                self.exit_visual();
                self.apply_operator(op, r, reg, count.unwrap_or(1));
                return Res::Done;
            }
            return self.operator_pending(op, &keys[i..i + used], &keys[i + used..], reg, count);
        }

        // ---- motions
        match self.parse_motion(keys, i) {
            Some(Ok((m, _))) => {
                if let Some(r) = self.motion_target(&m, count, false) {
                    if r.jump {
                        self.push_jump();
                    }
                    self.win_mut().cursor = r.pos;
                    if let Some(c) = r.curswant {
                        self.win_mut().curswant = c;
                    }
                    if visual && matches!(m, Motion::LineEnd) {
                        self.visual_block_dollar = true;
                        // in visual mode `$` goes past the last character
                        let l = self.cur_line().len();
                        if self.opt_str("selection") != "old" && self.mode != Mode::Visual(VisualKind::Block) {
                            let _ = l;
                        }
                    } else if visual && r.curswant.is_some() {
                        self.visual_block_dollar = false;
                    }
                } else if matches!(m, Motion::Left | Motion::Right | Motion::Up | Motion::Down) {
                    // silently stay at the edge
                }
                return Res::Done;
            }
            Some(Err(st)) => return Res::Incomplete(st),
            None => {}
        }

        let n = count.unwrap_or(1).max(1);
        let Some(c) = ch(&k) else {
            return self.special_key_command(k, count);
        };

        // ---- visual-only commands
        if visual {
            match c {
                'o' | 'O' => {
                    let cur = self.cursor();
                    if c == 'O' && self.mode == Mode::Visual(VisualKind::Block) {
                        let s = self.visual_start;
                        self.visual_start = Pos::new(s.line, cur.col);
                        self.set_cursor(Pos::new(cur.line, s.col));
                    } else {
                        self.win_mut().cursor = self.visual_start;
                        self.visual_start = cur;
                    }
                    return Res::Done;
                }
                'i' | 'a' => {
                    let Some(t) = rest.first() else {
                        return Res::Incomplete(PendingState::NoMap);
                    };
                    let Some(tc) = ch(t) else {
                        return Res::Invalid;
                    };
                    if let Some((s, e, linewise)) = self.text_object(c == 'a', tc, n) {
                        let e = self.resolve_eol(e);
                        let cur = self.cursor();
                        if cur == self.visual_start || s < self.visual_start.min(cur) || true {
                            if cur == self.visual_start {
                                self.visual_start = s;
                            } else {
                                self.visual_start = self.visual_start.min(s);
                            }
                            self.win_mut().cursor = e.max(cur);
                        }
                        if linewise && self.mode == Mode::Visual(VisualKind::Char) {
                            self.mode = Mode::Visual(VisualKind::Line);
                        }
                    }
                    return Res::Done;
                }
                'I' | 'A' => {
                    let (s, e, kind) = self.visual_range().unwrap();
                    let r = self.visual_op_range();
                    self.exit_visual();
                    match kind {
                        VisualKind::Block => {
                            let ts = self.tabstop_of(self.win());
                            let ambi = self.ambi_double();
                            let vcol = if c == 'I' { r.vcols.0 } else if r.to_eol { usize::MAX } else { r.vcols.1 + 1 };
                            let line = self.line(s.line).to_string();
                            let col = if vcol == usize::MAX { line.len() } else { col_at_vcol(&line, vcol, ts, ambi) };
                            if vcol != usize::MAX && c == 'A' && line_vwidth(&line, ts, ambi) < vcol {
                                // pad a short first line
                                let pad = vcol - line_vwidth(&line, ts, ambi);
                                let l = s.line;
                                let new = format!("{}{}", line, " ".repeat(pad));
                                self.buf_mut().set_line(l, new);
                                let len = self.line(l).len();
                                self.win_mut().cursor = Pos::new(l, len);
                            } else {
                                self.win_mut().cursor = Pos::new(s.line, col);
                            }
                            self.start_insert(if c == 'I' { 'i' } else { 'a' }, 1);
                            self.ins.block = Some((s.line + 1, e.line, vcol, c == 'A'));
                        }
                        _ => {
                            if c == 'I' {
                                let col = if kind == VisualKind::Line { 0 } else { s.col };
                                self.win_mut().cursor = Pos::new(s.line, col);
                                self.start_insert('i', 1);
                            } else {
                                let l = self.line(e.line);
                                let col = if kind == VisualKind::Line { l.len() } else { next_char_end(l, e.col) };
                                self.win_mut().cursor = Pos::new(e.line, col);
                                self.start_insert('a', 1);
                            }
                        }
                    }
                    return Res::Done;
                }
                'x' | 'X' | 'D' | 's' | 'S' | 'C' | 'Y' | 'R' | 'u' | 'U' | 'J' | 'r' | 'p' | 'P' | '~' => {
                    return self.visual_simple(c, rest, reg, n);
                }
                'v' | 'V' => {
                    let kind = if c == 'v' { VisualKind::Char } else { VisualKind::Line };
                    if self.mode == Mode::Visual(kind) {
                        self.exit_visual();
                    } else {
                        self.mode = Mode::Visual(kind);
                    }
                    return Res::Done;
                }
                ':' => {
                    self.exit_visual();
                    self.start_cmdline(':', "'<,'>");
                    return Res::Done;
                }
                _ => {}
            }
        }

        match c {
            // ---- entering insert mode
            'i' | 'a' | 'I' | 'A' | 'o' | 'O' if !visual => {
                self.begin_insert_command(c, n);
                Res::Done
            }
            'R' if !visual => {
                self.start_insert('R', n);
                Res::Done
            }
            // ---- simple aliases
            'x' | 'X' | 'D' | 'C' | 's' | 'S' | 'Y' if !visual => {
                let alias = match c {
                    'x' => "dl",
                    'X' => "dh",
                    'D' => "d$",
                    'C' => "c$",
                    's' => "cl",
                    'S' => "cc",
                    _ => "yy",
                };
                if (c == 'x' || c == 's') && self.cur_line().is_empty() {
                    if c == 's' {
                        self.start_insert('i', 1);
                    }
                    return Res::Done;
                }
                if c == 'X' && self.cursor().col == 0 {
                    return Res::Done;
                }
                let mut seq: Vec<Key> = Vec::new();
                if let Some(r) = reg {
                    seq.push(Key::ch('"'));
                    seq.push(Key::ch(r));
                }
                if let Some(cnt) = count {
                    seq.extend(cnt.to_string().chars().map(Key::ch));
                }
                seq.extend(alias.chars().map(Key::ch));
                self.exec_normal(&seq)
            }
            'r' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                let rc = match t.printable() {
                    Some(c) => c,
                    None if *t == keys::CR => '\r',
                    None => return Res::Invalid,
                };
                self.replace_chars(rc, n);
                Res::Done
            }
            'J' => {
                let l = self.cursor().line;
                self.join_lines(l, l + n.max(2) - 1, true);
                Res::Done
            }
            'u' => {
                self.dot_skip = true;
                for _ in 0..n {
                    self.undo();
                }
                Res::Done
            }
            'U' => {
                self.dot_skip = true;
                self.undo();
                Res::Done
            }
            '.' => {
                self.dot_skip = true;
                self.repeat_dot(count);
                Res::Done
            }
            '~' => {
                let cur = self.cursor();
                let line = self.cur_line().to_string();
                let mut end = cur.col;
                for _ in 0..n {
                    if end >= line.len() {
                        break;
                    }
                    end = next_char_end(&line, end);
                }
                let new: String = line[cur.col..end].chars().map(toggle_case).collect();
                let text = format!("{}{}{}", &line[..cur.col], new, &line[end..]);
                self.buf_mut().set_line(cur.line, text);
                let l = self.cur_line();
                let col = (cur.col + new.len()).min(last_char_start(l));
                self.win_mut().cursor.col = col;
                Res::Done
            }
            'p' | 'P' => {
                self.put(reg, c == 'P', n, false);
                Res::Done
            }
            'v' | 'V' => {
                self.enter_visual(if c == 'v' { VisualKind::Char } else { VisualKind::Line });
                if let Some(cnt) = count {
                    if c == 'V' && cnt > 1 {
                        let l = (self.cursor().line + cnt - 1).min(self.buf().last_line());
                        self.win_mut().cursor.line = l;
                    }
                }
                Res::Done
            }
            ':' => {
                let init = match count {
                    Some(cnt) if cnt > 1 => format!(".,.+{}", cnt - 1),
                    Some(_) => ".".to_string(),
                    None => String::new(),
                };
                self.start_cmdline(':', &init);
                Res::Done
            }
            '/' | '?' => {
                self.start_cmdline(c, "");
                Res::Done
            }
            '&' => {
                self.exec_ex_reporting("s");
                Res::Done
            }
            'm' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                match ch(t) {
                    Some(mc) if mc.is_ascii_alphabetic() || matches!(mc, '\'' | '`' | '[' | ']' | '<' | '>') => {
                        let pos = self.cursor();
                        let mc = if mc == '`' { '\'' } else { mc };
                        self.set_mark(mc, pos);
                        Res::Done
                    }
                    _ => Res::Invalid,
                }
            }
            'q' => {
                if let Some((r, mut rec)) = self.recording.take() {
                    rec.pop(); // the terminating `q`
                    let text = keys::keys_to_string(&rec);
                    self.reg_set(r, text, RegKind::Char);
                    self.msg("");
                    return Res::Done;
                }
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                match ch(t) {
                    Some(r) if r.is_ascii_alphanumeric() || r == '"' => {
                        self.recording = Some((r, Vec::new()));
                        Res::Done
                    }
                    _ => Res::Invalid,
                }
            }
            '@' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                let Some(mut r) = ch(t) else {
                    return Res::Invalid;
                };
                if r == '@' {
                    match self.last_macro {
                        Some(l) => r = l,
                        None => {
                            self.emsg("E748: No previously used register");
                            return Res::Done;
                        }
                    }
                }
                if r == ':' {
                    if let Some(cmd) = self.last_cmdline.clone() {
                        for _ in 0..n {
                            self.exec_ex_reporting(&cmd);
                        }
                    }
                    return Res::Done;
                }
                self.last_macro = Some(r);
                if let Some(regv) = self.reg_get(r) {
                    let mut text = regv.text.clone();
                    if regv.kind == RegKind::Line {
                        text.push('\n');
                    }
                    let ks = keys::parse_keys_raw(&text);
                    // the recorded `@x` itself must not end up in the macro being recorded
                    for _ in 0..n {
                        self.feed_front_keys(&ks, true);
                    }
                }
                Res::Done
            }
            'z' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                self.z_command(*t, count);
                Res::Done
            }
            'Z' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                match ch(t) {
                    Some('Z') => self.exec_ex_reporting("x"),
                    Some('Q') => self.exec_ex_reporting("q!"),
                    _ => return Res::Invalid,
                }
                Res::Done
            }
            'g' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                self.g_command(*t, count, reg)
            }
            '\x17' => {
                // CTRL-W
                let mut j = 0;
                let mut wcount: Option<usize> = count;
                while let Some(d) = rest.get(j).and_then(ch).filter(|c| c.is_ascii_digit()) {
                    wcount = Some(wcount.unwrap_or(0) * 10 + (d as usize - '0' as usize));
                    j += 1;
                }
                let Some(t) = rest.get(j) else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                if ch(t) == Some('g') {
                    let Some(_) = rest.get(j + 1) else {
                        return Res::Incomplete(PendingState::NoMap);
                    };
                    return Res::Done;
                }
                self.window_command(*t, wcount);
                Res::Done
            }
            '\x12' => {
                // CTRL-R
                self.dot_skip = true;
                for _ in 0..n {
                    self.redo();
                }
                Res::Done
            }
            '\x16' => {
                // CTRL-V
                if self.mode == Mode::Visual(VisualKind::Block) {
                    self.exit_visual();
                } else if visual {
                    self.mode = Mode::Visual(VisualKind::Block);
                } else {
                    self.enter_visual(VisualKind::Block);
                }
                Res::Done
            }
            '\x0f' | '\t' => {
                // CTRL-O / CTRL-I
                self.jump_list(c == '\x0f', n);
                Res::Done
            }
            '\x07' => {
                self.file_info();
                Res::Done
            }
            '\x0c' => {
                self.clear_msg();
                Res::Done
            }
            '\x05' | '\x19' => {
                self.scroll_lines(if c == '\x05' { n as isize } else { -(n as isize) });
                Res::Done
            }
            '\x01' | '\x18' => {
                self.increment(if c == '\x01' { n as i64 } else { -(n as i64) });
                Res::Done
            }
            '\x1e' => {
                match count.or(self.win().alt_buf) {
                    Some(b) if self.buffers.contains_key(&b) => match self.can_abandon(false) {
                        Ok(()) => self.set_cur_buf(b),
                        Err(e) => self.emsg(e),
                    },
                    _ => self.emsg("E23: No alternate file"),
                }
                Res::Done
            }
            '\x1d' => {
                self.emsg("E433: No tags file");
                Res::Done
            }
            'K' => {
                if let Some((s, e)) = self.word_under_cursor() {
                    let w = self.cur_line()[s..e].to_string();
                    self.exec_ex_reporting(&format!("help {}", w));
                }
                Res::Done
            }
            'Q' => Res::Done,
            _ => Res::Invalid,
        }
    }

    fn special_key_command(&mut self, k: Key, count: Option<usize>) -> Res {
        let n = count.unwrap_or(1).max(1);
        match k.code {
            KeyCode::Sp(Sp::Insert) if k.mods == 0 => {
                self.begin_insert_command('i', n);
                Res::Done
            }
            KeyCode::Sp(Sp::Delete) if k.mods == 0 => {
                if matches!(self.mode, Mode::Visual(_)) {
                    return self.visual_simple('x', &[], None, 1);
                }
                self.exec_normal(&keys::parse_keys_raw(&format!("{}x", n)))
            }
            KeyCode::Sp(Sp::F(1)) => {
                self.exec_ex_reporting("help");
                Res::Done
            }
            KeyCode::Sp(Sp::ScrollUp) => {
                self.scroll_lines(-3);
                Res::Done
            }
            KeyCode::Sp(Sp::ScrollDown) => {
                self.scroll_lines(3);
                Res::Done
            }
            KeyCode::Sp(Sp::PageDown) if k.mods == CTRL => {
                let t = (self.cur_tab + 1) % self.tabs.len();
                self.goto_tab(t);
                Res::Done
            }
            KeyCode::Sp(Sp::PageUp) if k.mods == CTRL => {
                let t = (self.cur_tab + self.tabs.len() - 1) % self.tabs.len();
                self.goto_tab(t);
                Res::Done
            }
            _ => Res::Invalid,
        }
    }

    /// Returns the operator and how many keys it used (`None` = need more).
    fn parse_operator(&self, keys: &[Key], visual: bool) -> Option<(Op, Option<usize>)> {
        let c = ch(&keys[0])?;
        let op = match c {
            'd' => Op::Delete,
            'y' => Op::Yank,
            'c' => Op::Change,
            '<' => Op::ShiftLeft,
            '>' => Op::ShiftRight,
            '=' => Op::Indent,
            '!' => Op::Filter,
            '~' if self.opt_global("tildeop").as_bool() && !visual => Op::Tilde,
            'g' => {
                let t = keys.get(1).and_then(ch);
                let op = match t {
                    Some('~') => Op::Tilde,
                    Some('u') => Op::Lower,
                    Some('U') => Op::Upper,
                    Some('?') => Op::Rot13,
                    Some('q') | Some('w') => Op::Format,
                    _ => return None,
                };
                return Some((op, Some(2)));
            }
            _ => return None,
        };
        Some((op, Some(1)))
    }

    fn operator_pending(&mut self, op: Op, op_keys: &[Key], rest: &[Key], reg: Option<char>, count1: Option<usize>) -> Res {
        let mut j = 0;
        let mut count2: Option<usize> = None;
        while let Some(d) = rest.get(j).and_then(ch) {
            if d.is_ascii_digit() && (d != '0' || count2.is_some()) {
                count2 = Some(count2.unwrap_or(0) * 10 + (d as usize - '0' as usize));
                j += 1;
            } else {
                break;
            }
        }
        let count = match (count1, count2) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(1) * b.unwrap_or(1)),
        };
        let Some(k) = rest.get(j) else {
            return Res::Incomplete(PendingState::Operator);
        };
        let n = count.unwrap_or(1).max(1);
        let cur = self.cursor();
        let last = self.buf().last_line();
        let kc = ch(k);
        // doubled operator: linewise on N lines
        let last_op_key = *op_keys.last().unwrap();
        let doubled = *k == last_op_key
            || (op_keys.len() == 2 && kc == Some('g') && rest.get(j + 1) == Some(&last_op_key))
            || (op_keys.len() == 2 && kc == Some('g') && rest.get(j + 1).is_none() && false);
        if op_keys.len() == 2 && kc == Some('g') && rest.get(j + 1).is_none() {
            return Res::Incomplete(PendingState::NoMap);
        }
        if doubled {
            if cur.line + n - 1 > last && n > 1 && cur.line == last {
                return Res::Invalid;
            }
            let end = (cur.line + n - 1).min(last);
            let r = OpRange { start: Pos::new(cur.line, 0), end: Pos::new(end, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false };
            self.apply_operator(op, r, reg, 1);
            return Res::Done;
        }
        // text objects
        if matches!(kc, Some('i') | Some('a')) {
            let Some(t) = rest.get(j + 1) else {
                return Res::Incomplete(PendingState::NoMap);
            };
            let Some(tc) = ch(t) else {
                return Res::Invalid;
            };
            let Some((s, e, linewise)) = self.text_object(kc == Some('a'), tc, n) else {
                return Res::Invalid;
            };
            let r = if linewise {
                OpRange { start: Pos::new(s.line, 0), end: Pos::new(e.line, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false }
            } else if e.col == usize::MAX {
                // empty object (e.g. `ci"` on "")
                OpRange { start: s, end: s, kind: RangeKind::Char, vcols: (0, 0), to_eol: false }
            } else {
                let end = Pos::new(e.line, next_char_end(self.line(e.line), e.col));
                OpRange { start: s, end, kind: RangeKind::Char, vcols: (0, 0), to_eol: false }
            };
            self.apply_operator(op, r, reg, 1);
            return Res::Done;
        }
        // search as a motion: finish the operator when the search is entered
        if matches!(kc, Some('/') | Some('?')) {
            let mut saved: Vec<Key> = Vec::new();
            if let Some(r) = reg {
                saved.push(Key::ch('"'));
                saved.push(Key::ch(r));
            }
            saved.extend_from_slice(op_keys);
            self.pending_search_op = Some(saved);
            self.start_cmdline(kc.unwrap(), "");
            return Res::Done;
        }
        // forced motion type (v, V, CTRL-V) is accepted and ignored
        let mut mkeys: Vec<Key> = rest[j..].to_vec();
        let mut force: Option<char> = None;
        if matches!(kc, Some('v') | Some('V')) {
            force = kc;
            mkeys.remove(0);
            if mkeys.is_empty() {
                return Res::Incomplete(PendingState::Operator);
            }
        }
        let (m, _) = match self.parse_motion(&mkeys, 0) {
            Some(Ok(x)) => x,
            Some(Err(_)) => return Res::Incomplete(PendingState::NoMap),
            None => return Res::Invalid,
        };
        // `cw` on a non-blank behaves like `ce` (Vim's nv_wordcmd)
        if op == Op::Change {
            if let Motion::WordFwd(big) = m {
                let on_word = char_at(self.cur_line(), cur.col).map(|c| c != ' ' && c != '\t').unwrap_or(false);
                if on_word {
                    let e = motion::change_word_end(self.buf(), cur, n, big);
                    let line = self.line(e.line);
                    let end = Pos::new(e.line, if e.col < line.len() { next_char_end(line, e.col) } else { line.len() });
                    let r = OpRange { start: cur, end, kind: RangeKind::Char, vcols: (0, 0), to_eol: false };
                    self.apply_operator(op, r, reg, 1);
                    return Res::Done;
                }
            }
        }
        let Some(t) = self.motion_target(&m, count, true) else {
            return Res::Invalid;
        };
        if t.jump {
            self.push_jump();
        }
        let r = self.range_from_motion(cur, t.pos, t.linewise, t.inclusive, force);
        self.apply_operator(op, r, reg, 1);
        Res::Done
    }

    /// Finish an operator whose motion was a `/` or `?` search.
    pub fn finish_search_operator(&mut self, op_keys: Vec<Key>, target: Pos) {
        let mut reg = None;
        let mut i = 0;
        if op_keys.first().and_then(ch) == Some('"') {
            reg = op_keys.get(1).and_then(ch);
            i = 2;
        }
        if let Some((op, _)) = self.parse_operator(&op_keys[i..], false) {
            let cur = self.cursor();
            let r = self.range_from_motion(cur, target, false, false, None);
            self.buf_mut().undo_cursor = cur;
            self.apply_operator(op, r, reg, 1);
        }
    }

    pub fn range_from_motion(&self, a: Pos, b: Pos, linewise: bool, inclusive: bool, force: Option<char>) -> OpRange {
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        let mut linewise = linewise;
        let mut inclusive = inclusive;
        match force {
            Some('V') => linewise = true,
            Some('v') => {
                if linewise {
                    linewise = false;
                    inclusive = false;
                } else {
                    inclusive = !inclusive;
                }
            }
            _ => {}
        }
        if linewise {
            return OpRange { start: Pos::new(s.line, 0), end: Pos::new(e.line, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false };
        }
        let mut end = e;
        if inclusive {
            end.col = next_char_end(self.line(e.line), e.col);
        } else if e.col == 0 && e.line > s.line {
            // exclusive motion ending in column 0: stop at the end of the previous line
            if s.col <= first_nonblank(self.line(s.line)) && !self.line(s.line).is_empty()
                || s.col == 0
            {
                if force.is_none() {
                    return OpRange { start: Pos::new(s.line, 0), end: Pos::new(e.line - 1, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false };
                }
            }
            end = Pos::new(e.line - 1, self.line(e.line - 1).len());
        }
        OpRange { start: s, end, kind: RangeKind::Char, vcols: (0, 0), to_eol: false }
    }

    fn resolve_eol(&self, p: Pos) -> Pos {
        if p.col == usize::MAX {
            Pos::new(p.line, last_char_start(self.line(p.line)))
        } else {
            p
        }
    }

    /// The range covered by the current visual selection.
    pub fn visual_op_range(&self) -> OpRange {
        let (s, e, kind) = self.visual_range().unwrap();
        match kind {
            VisualKind::Line => OpRange { start: Pos::new(s.line, 0), end: Pos::new(e.line, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false },
            VisualKind::Char => {
                let l = self.line(e.line);
                let exclusive = self.opt_str("selection") == "exclusive";
                let end = if exclusive { e } else { Pos::new(e.line, next_char_end(l, e.col)) };
                OpRange { start: s, end, kind: RangeKind::Char, vcols: (0, 0), to_eol: false }
            }
            VisualKind::Block => {
                let ts = self.tabstop_of(self.win());
                let ambi = self.ambi_double();
                let a = self.visual_start;
                let b = self.cursor();
                let va = vcol_of(self.line(a.line), a.col, ts, ambi);
                let vb = vcol_of(self.line(b.line), b.col, ts, ambi);
                let wa = char_at(self.line(a.line), a.col).map(|c| cell_w(c, va, ts, ambi)).unwrap_or(1);
                let wb = char_at(self.line(b.line), b.col).map(|c| cell_w(c, vb, ts, ambi)).unwrap_or(1);
                let left = va.min(vb);
                let right = (va + wa).max(vb + wb) - 1;
                OpRange { start: s, end: e, kind: RangeKind::Block, vcols: (left, right), to_eol: self.visual_block_dollar }
            }
        }
    }

    fn block_cols(&self, line: &str, r: &OpRange) -> (usize, usize) {
        let ts = self.tabstop_of(self.win());
        let ambi = self.ambi_double();
        let s = col_at_vcol(line, r.vcols.0, ts, ambi);
        let e = if r.to_eol { line.len() } else { col_at_vcol(line, r.vcols.1 + 1, ts, ambi) };
        (s.min(line.len()), e.max(s).min(line.len()))
    }

    fn range_text(&self, r: &OpRange) -> (String, RegKind) {
        match r.kind {
            RangeKind::Char => (self.buf().get_text(r.start, r.end), RegKind::Char),
            RangeKind::Line => (self.buf().lines[r.start.line..=r.end.line.min(self.buf().last_line())].join("\n"), RegKind::Line),
            RangeKind::Block => {
                let mut parts = Vec::new();
                for l in r.start.line..=r.end.line {
                    let line = self.line(l);
                    let (s, e) = self.block_cols(line, r);
                    parts.push(line[s..e].to_string());
                }
                (parts.join("\n"), RegKind::Block)
            }
        }
    }

    pub fn apply_operator(&mut self, op: Op, r: OpRange, reg: Option<char>, count: usize) {
        let last = self.buf().last_line();
        let mut r = r;
        r.end.line = r.end.line.min(last);
        match op {
            Op::Yank => {
                let (text, kind) = self.range_text(&r);
                let nlines = r.end.line - r.start.line + 1;
                self.store_register(reg, text, kind, false);
                let cur = self.cursor();
                match r.kind {
                    RangeKind::Line => {
                        if r.start.line < cur.line {
                            self.win_mut().cursor.line = r.start.line;
                        }
                    }
                    RangeKind::Char => self.win_mut().cursor = r.start,
                    RangeKind::Block => {
                        let (s, _) = self.block_cols(self.line(r.start.line), &r);
                        self.win_mut().cursor = Pos::new(r.start.line, s);
                    }
                }
                self.set_mark('[', r.start);
                self.set_mark(']', r.end);
                if nlines > self.opt_global("report").as_num().max(0) as usize {
                    self.msg(format!("{} lines yanked", nlines));
                }
                self.check_cursor();
            }
            Op::Delete | Op::Change => {
                let (text, kind) = self.range_text(&r);
                let nlines = r.end.line - r.start.line + 1;
                self.store_register(reg, text, kind, true);
                match r.kind {
                    RangeKind::Char => {
                        self.buf_mut().delete_range(r.start, r.end);
                        self.win_mut().cursor = r.start;
                        if op == Op::Change {
                            self.start_insert('c', 1);
                        }
                    }
                    RangeKind::Line => {
                        if op == Op::Change {
                            let indent = if self.opt_bool("autoindent") { motion::indent_of(self.line(r.start.line)).to_string() } else { String::new() };
                            let len = indent.len();
                            self.buf_mut().splice(r.start.line, r.end.line + 1, vec![indent]);
                            self.win_mut().cursor = Pos::new(r.start.line, len);
                            self.start_insert('c', 1);
                        } else {
                            self.buf_mut().delete_lines(r.start.line, r.end.line + 1);
                            let l = r.start.line.min(self.buf().last_line());
                            let col = first_nonblank(self.line(l));
                            self.win_mut().cursor = Pos::new(l, col);
                            if nlines > self.opt_global("report").as_num().max(0) as usize {
                                self.msg(format!("{} fewer lines", nlines));
                            }
                        }
                    }
                    RangeKind::Block => {
                        let mut first_col = 0;
                        for l in r.start.line..=r.end.line {
                            let line = self.line(l).to_string();
                            let (s, e) = self.block_cols(&line, &r);
                            if l == r.start.line {
                                first_col = s;
                            }
                            if e > s {
                                let new = format!("{}{}", &line[..s], &line[e..]);
                                self.buf_mut().set_line(l, new);
                            }
                        }
                        self.win_mut().cursor = Pos::new(r.start.line, first_col);
                        if op == Op::Change {
                            self.start_insert('c', 1);
                            self.ins.block = Some((r.start.line + 1, r.end.line, r.vcols.0, false));
                        }
                    }
                }
                let cur = self.cursor();
                self.set_mark('[', cur);
                self.set_mark(']', cur);
                if op == Op::Delete {
                    self.check_cursor();
                }
            }
            Op::ShiftLeft | Op::ShiftRight => {
                let amount = count.max(1) as isize * if op == Op::ShiftLeft { -1 } else { 1 };
                self.shift_lines(r.start.line, r.end.line, amount);
                let l = r.start.line;
                let col = first_nonblank(self.line(l));
                self.win_mut().cursor = Pos::new(l, col);
                let nlines = r.end.line - r.start.line + 1;
                if nlines > self.opt_global("report").as_num().max(0) as usize {
                    self.msg(format!("{} lines {}ed 1 time", nlines, if amount < 0 { "<" } else { ">" }));
                }
            }
            Op::Indent => {
                self.reindent_lines(r.start.line, r.end.line);
                let l = r.start.line;
                let col = first_nonblank(self.line(l));
                self.win_mut().cursor = Pos::new(l, col);
            }
            Op::Tilde | Op::Lower | Op::Upper | Op::Rot13 => {
                let f = |c: char| -> String {
                    match op {
                        Op::Tilde => toggle_case(c).to_string(),
                        Op::Lower => c.to_lowercase().collect(),
                        Op::Upper => c.to_uppercase().collect(),
                        _ => rot13(c).to_string(),
                    }
                };
                for l in r.start.line..=r.end.line {
                    let line = self.line(l).to_string();
                    let (s, e) = match r.kind {
                        RangeKind::Line => (0, line.len()),
                        RangeKind::Char => (if l == r.start.line { r.start.col } else { 0 }, if l == r.end.line { r.end.col.min(line.len()) } else { line.len() }),
                        RangeKind::Block => self.block_cols(&line, &r),
                    };
                    if e <= s {
                        continue;
                    }
                    let mid: String = line[s..e].chars().map(&f).collect();
                    let new = format!("{}{}{}", &line[..s], mid, &line[e..]);
                    self.buf_mut().set_line(l, new);
                }
                let pos = match r.kind {
                    RangeKind::Line => Pos::new(r.start.line, self.cursor().col.min(self.line(r.start.line).len())),
                    RangeKind::Char => r.start,
                    RangeKind::Block => {
                        let (s, _) = self.block_cols(self.line(r.start.line), &r);
                        Pos::new(r.start.line, s)
                    }
                };
                self.win_mut().cursor = pos;
                self.check_cursor();
            }
            Op::Format => {
                self.format_lines(r.start.line, r.end.line);
            }
            Op::Filter => {
                let n = r.end.line - r.start.line;
                let range = if n == 0 { ".".to_string() } else { format!(".,.+{}", n) };
                self.win_mut().cursor.line = r.start.line;
                self.start_cmdline(':', &format!("{}!", range));
            }
        }
    }

    fn visual_simple(&mut self, c: char, rest: &[Key], reg: Option<char>, n: usize) -> Res {
        let mut r = self.visual_op_range();
        let kind = self.visual_range().unwrap().2;
        match c {
            'x' => {
                self.exit_visual();
                self.apply_operator(Op::Delete, r, reg, 1);
            }
            's' => {
                self.exit_visual();
                self.apply_operator(Op::Change, r, reg, 1);
            }
            'X' | 'D' | 'S' | 'C' | 'R' | 'Y' => {
                if kind == VisualKind::Block && matches!(c, 'D' | 'C') {
                    r.to_eol = true;
                } else if kind != VisualKind::Block || c != 'Y' {
                    r = OpRange { start: Pos::new(r.start.line, 0), end: Pos::new(r.end.line, 0), kind: RangeKind::Line, vcols: (0, 0), to_eol: false };
                }
                self.exit_visual();
                let op = match c {
                    'X' | 'D' => Op::Delete,
                    'Y' => Op::Yank,
                    _ => Op::Change,
                };
                self.apply_operator(op, r, reg, 1);
            }
            'u' | 'U' | '~' => {
                self.exit_visual();
                let op = match c {
                    'u' => Op::Lower,
                    'U' => Op::Upper,
                    _ => Op::Tilde,
                };
                self.apply_operator(op, r, reg, 1);
            }
            'J' => {
                self.exit_visual();
                let end = r.end.line.max(r.start.line + 1);
                self.join_lines(r.start.line, end, true);
            }
            'r' => {
                let Some(t) = rest.first() else {
                    return Res::Incomplete(PendingState::NoMap);
                };
                let Some(rc) = t.printable() else {
                    return Res::Invalid;
                };
                self.exit_visual();
                for l in r.start.line..=r.end.line {
                    let line = self.line(l).to_string();
                    let (s, e) = match r.kind {
                        RangeKind::Line => (0, line.len()),
                        RangeKind::Char => (if l == r.start.line { r.start.col } else { 0 }, if l == r.end.line { r.end.col.min(line.len()) } else { line.len() }),
                        RangeKind::Block => self.block_cols(&line, &r),
                    };
                    if e <= s {
                        continue;
                    }
                    let mid: String = line[s..e].chars().map(|_| rc).collect();
                    let new = format!("{}{}{}", &line[..s], mid, &line[e..]);
                    self.buf_mut().set_line(l, new);
                }
                self.win_mut().cursor = r.start;
                self.check_cursor();
            }
            'p' | 'P' => {
                // replace the selection with a register
                let Some(regv) = self.reg_get(reg.unwrap_or('"')) else {
                    self.exit_visual();
                    return Res::Done;
                };
                self.exit_visual();
                let saved_unnamed = self.registers.get(&'"').cloned();
                let saved_clip = self.clip_out.clone();
                self.apply_operator(Op::Delete, r, None, 1);
                let deleted = self.registers.get(&'"').cloned();
                self.registers.insert('\u{1}', regv.clone());
                let at_eol = r.kind == RangeKind::Char && self.cursor().col + 1 >= self.cur_line().len() && r.start.col > self.cursor().col;
                let line_end = r.kind == RangeKind::Char && r.start.col >= self.cur_line().len() && !self.cur_line().is_empty();
                let before = match r.kind {
                    RangeKind::Line => r.start.line <= self.buf().last_line() && !(r.start.line > self.cursor().line),
                    _ => !(at_eol || line_end),
                };
                // deleting the last lines moves the cursor up: paste below then
                let before = if r.kind == RangeKind::Line { self.cursor().line == r.start.line && before } else { before };
                self.put_register(regv, before, n, false);
                self.registers.remove(&'\u{1}');
                if c == 'P' {
                    if let Some(u) = saved_unnamed {
                        self.registers.insert('"', u);
                    }
                    self.clip_out = saved_clip;
                } else if let Some(d) = deleted {
                    self.reg_set('"', d.text, d.kind);
                }
            }
            _ => return Res::Invalid,
        }
        Res::Done
    }

    /// Parse a motion starting at `keys[i]`.
    /// `None`: not a motion.  `Some(Err)`: needs more keys.
    fn parse_motion(&self, keys: &[Key], i: usize) -> Option<Result<(Motion, usize), PendingState>> {
        let k = keys[i];
        let next = keys.get(i + 1);
        if let KeyCode::Sp(sp) = k.code {
            let m = match (sp, k.mods) {
                (Sp::Left, 0) => Motion::Left,
                (Sp::Right, 0) => Motion::Right,
                (Sp::Up, 0) => Motion::Up,
                (Sp::Down, 0) => Motion::Down,
                (Sp::Home, 0) => Motion::LineStart,
                (Sp::End, 0) => Motion::LineEnd,
                (Sp::Home, CTRL) => Motion::FirstLine,
                (Sp::End, CTRL) => Motion::GotoLine,
                (Sp::PageDown, 0) | (Sp::Down, SHIFT) => Motion::PageDown,
                (Sp::PageUp, 0) | (Sp::Up, SHIFT) => Motion::PageUp,
                (Sp::Backspace, 0) => Motion::Backspace,
                (Sp::Left, SHIFT) => Motion::WordBack(false),
                (Sp::Right, SHIFT) => Motion::WordFwd(false),
                (Sp::Left, CTRL) => Motion::WordBack(true),
                (Sp::Right, CTRL) => Motion::WordFwd(true),
                _ => return None,
            };
            return Some(Ok((m, 1)));
        }
        let c = k.as_char()?;
        let one = |m: Motion| Some(Ok((m, 1)));
        match c {
            'h' | '\x08' => one(if c == 'h' { Motion::Left } else { Motion::Backspace }),
            'l' => one(Motion::Right),
            ' ' => one(Motion::Space),
            'j' | '\x0e' | '\n' => one(Motion::Down),
            'k' | '\x10' => one(Motion::Up),
            '0' => one(Motion::LineStart),
            '^' => one(Motion::FirstNonBlank),
            '$' => one(Motion::LineEnd),
            '|' => one(Motion::Column),
            'w' => one(Motion::WordFwd(false)),
            'W' => one(Motion::WordFwd(true)),
            'b' => one(Motion::WordBack(false)),
            'B' => one(Motion::WordBack(true)),
            'e' => one(Motion::WordEnd(false)),
            'E' => one(Motion::WordEnd(true)),
            'f' | 'F' | 't' | 'T' => match next {
                None => Some(Err(PendingState::NoMap)),
                Some(t) => {
                    let tc = t.printable()?;
                    Some(Ok((Motion::Find { ch: tc, forward: c == 'f' || c == 't', till: c == 't' || c == 'T' }, 2)))
                }
            },
            ';' => one(Motion::RepeatFind { reverse: false }),
            ',' => one(Motion::RepeatFind { reverse: true }),
            'G' => one(Motion::GotoLine),
            'H' => one(Motion::ScreenTop),
            'M' => one(Motion::ScreenMiddle),
            'L' => one(Motion::ScreenBottom),
            '{' => one(Motion::ParaBack),
            '}' => one(Motion::ParaFwd),
            '(' => one(Motion::SentBack),
            ')' => one(Motion::SentFwd),
            '%' => one(Motion::MatchPair),
            'n' => one(Motion::SearchNext { reverse: false }),
            'N' => one(Motion::SearchNext { reverse: true }),
            '*' => one(Motion::Star { forward: true, whole: true }),
            '#' => one(Motion::Star { forward: false, whole: true }),
            '\'' | '`' => match next {
                None => Some(Err(PendingState::NoMap)),
                Some(t) => Some(Ok((Motion::Mark { ch: t.as_char()?, linewise: c == '\'' }, 2))),
            },
            '+' | '\r' => one(Motion::LineDown),
            '-' => one(Motion::LineUp),
            '_' => one(Motion::Underscore),
            '\x06' => one(Motion::PageDown),
            '\x02' => one(Motion::PageUp),
            '\x04' => one(Motion::HalfDown),
            '\x15' => one(Motion::HalfUp),
            'g' => match next {
                None => None, // handled by the caller (may be an operator or command)
                Some(t) => {
                    let m = match t.code {
                        KeyCode::Char('g') => Motion::FirstLine,
                        KeyCode::Char('j') | KeyCode::Sp(Sp::Down) => Motion::DispDown,
                        KeyCode::Char('k') | KeyCode::Sp(Sp::Up) => Motion::DispUp,
                        KeyCode::Char('e') => Motion::WordEndBack(false),
                        KeyCode::Char('E') => Motion::WordEndBack(true),
                        KeyCode::Char('_') => Motion::LastNonBlank,
                        KeyCode::Char('0') | KeyCode::Sp(Sp::Home) => Motion::DispLineStart,
                        KeyCode::Char('$') | KeyCode::Sp(Sp::End) => Motion::DispLineEnd,
                        KeyCode::Char('m') => Motion::DispMiddle,
                        KeyCode::Char('*') => Motion::Star { forward: true, whole: false },
                        KeyCode::Char('#') => Motion::Star { forward: false, whole: false },
                        KeyCode::Char('\'') | KeyCode::Char('`') => {
                            return match keys.get(i + 2) {
                                None => Some(Err(PendingState::NoMap)),
                                Some(mk) => Some(Ok((Motion::Mark { ch: mk.as_char()?, linewise: t.is('\'') }, 3))),
                            };
                        }
                        _ => return None,
                    };
                    Some(Ok((m, 2)))
                }
            },
            '[' | ']' => match next {
                None => Some(Err(PendingState::NoMap)),
                Some(t) => {
                    let tc = t.as_char()?;
                    let m = match (c, tc) {
                        ('[', '[') => Motion::SectionBack,
                        (']', ']') => Motion::SectionFwd,
                        ('[', ']') => Motion::SectionBack,
                        (']', '[') => Motion::SectionFwd,
                        ('[', '(') | ('[', '{') => Motion::Unmatched { ch: tc, forward: false },
                        (']', ')') | (']', '}') => Motion::Unmatched { ch: tc, forward: true },
                        _ => return None,
                    };
                    Some(Ok((m, 2)))
                }
            },
            _ => None,
        }
    }

    fn g_command(&mut self, t: Key, count: Option<usize>, reg: Option<char>) -> Res {
        let n = count.unwrap_or(1).max(1);
        let visual = matches!(self.mode, Mode::Visual(_));
        let Some(c) = ch(&t) else {
            return Res::Invalid;
        };
        match c {
            'v' => {
                if let Some((kind, s, e)) = self.last_visual {
                    let cur_sel = self.visual_range().map(|(_, _, k)| (k, self.visual_start, self.cursor()));
                    self.visual_start = self.buf().clamp(s);
                    self.win_mut().cursor = self.buf().clamp(e);
                    self.mode = Mode::Visual(kind);
                    if let Some(sel) = cur_sel {
                        self.last_visual = Some(sel);
                    }
                }
                Res::Done
            }
            'J' => {
                if visual {
                    let r = self.visual_op_range();
                    self.exit_visual();
                    self.join_lines(r.start.line, r.end.line.max(r.start.line + 1), false);
                } else {
                    let l = self.cursor().line;
                    self.join_lines(l, l + n.max(2) - 1, false);
                }
                Res::Done
            }
            'p' | 'P' => {
                self.put(reg, c == 'P', n, true);
                Res::Done
            }
            'i' => {
                if let Some(p) = self.buf().marks.get(&'^').copied() {
                    self.set_cursor(p);
                }
                self.start_insert('i', n);
                Res::Done
            }
            'I' => {
                self.win_mut().cursor.col = 0;
                self.start_insert('i', n);
                Res::Done
            }
            't' => {
                let idx = match count {
                    Some(c) => c.saturating_sub(1),
                    None => (self.cur_tab + 1) % self.tabs.len(),
                };
                self.goto_tab(idx);
                Res::Done
            }
            'T' => {
                let len = self.tabs.len();
                let idx = (self.cur_tab + len - (n % len)) % len;
                self.goto_tab(idx);
                Res::Done
            }
            'a' => {
                match char_at(self.cur_line(), self.cursor().col) {
                    Some(c) => self.msg(format!("<{}> {},  Hex {:04x},  Oct {:o}", c, c as u32, c as u32, c as u32)),
                    None => self.msg("NUL"),
                }
                Res::Done
            }
            '8' => {
                if let Some(c) = char_at(self.cur_line(), self.cursor().col) {
                    let mut b = [0u8; 4];
                    let s: Vec<String> = c.encode_utf8(&mut b).bytes().map(|x| format!("{:02x}", x)).collect();
                    self.msg(s.join(" "));
                }
                Res::Done
            }
            'f' | 'F' => {
                let line = self.cur_line().to_string();
                let col = self.cursor().col.min(line.len());
                let is_f = |c: char| !c.is_whitespace() && !"\"'<>|()[]{}".contains(c);
                let s = line[..col].rfind(|c: char| !is_f(c)).map(|i| i + 1).unwrap_or(0);
                let e = line[col..].find(|c: char| !is_f(c)).map(|i| i + col).unwrap_or(line.len());
                let name = line[s..e].trim_end_matches([',', '.', ':', ';']).to_string();
                if name.is_empty() {
                    self.emsg("E446: No file name under cursor");
                } else {
                    let p = std::path::PathBuf::from(&name);
                    if p.exists() {
                        self.push_jump();
                        if let Err(e) = self.edit_file(&p, false) {
                            self.emsg(e);
                        }
                    } else {
                        self.emsg(format!("E447: Can't find file \"{}\" in path", name));
                    }
                }
                Res::Done
            }
            'd' | 'D' => {
                if let Some((s, e)) = self.word_under_cursor() {
                    let w = self.cur_line()[s..e].to_string();
                    let pat = format!("\\<{}\\>", motion::escape_pattern(&w));
                    self.push_jump();
                    let last = self.buf().last_line();
                    let from = Pos::new(last, self.line(last).len());
                    self.last_search = Some(pat.clone());
                    self.hlsearch_off = false;
                    if let Some(p) = self.search(&pat, true, from, 1, false) {
                        self.set_cursor(p);
                    }
                }
                Res::Done
            }
            '&' => {
                self.exec_ex_reporting("%s//~/&");
                Res::Done
            }
            '\x07' => {
                let words: usize = self.buf().lines.iter().map(|l| l.split_whitespace().count()).sum();
                let bytes: usize = self.buf().lines.iter().map(|l| l.len() + 1).sum();
                let cur = self.cursor();
                self.msg(format!("Col {}; Line {} of {}; {} words; {} bytes", cur.col + 1, cur.line + 1, self.buf().line_count(), words, bytes));
                Res::Done
            }
            'x' | 's' | 'h' | 'H' | 'n' | 'N' | ';' | ',' | '-' | '+' | 'Q' | 'R' | 'r' | '<' | ']' | '@' => Res::Done,
            _ => Res::Invalid,
        }
    }

    fn z_command(&mut self, t: Key, count: Option<usize>) {
        if let Some(c) = count {
            let l = c.saturating_sub(1).min(self.buf().last_line());
            self.win_mut().cursor.line = l;
        }
        let cur = self.cursor();
        let h = self.win().height;
        let tc = ch(&t);
        let first_nb = matches!(tc, Some('\r') | Some('.') | Some('-'));
        match tc {
            Some('t') | Some('\r') => self.win_mut().topline = cur.line,
            Some('z') | Some('.') => {
                let mut top = cur.line;
                let mut rows = self.line_rows(self.win(), cur.line);
                while top > 0 {
                    let r = self.line_rows(self.win(), top - 1);
                    if rows + r * 2 > h {
                        break;
                    }
                    rows += r * 2;
                    top -= 1;
                }
                self.win_mut().topline = top;
            }
            Some('b') | Some('-') => {
                let mut top = cur.line;
                let mut rows = self.line_rows(self.win(), cur.line);
                while top > 0 {
                    let r = self.line_rows(self.win(), top - 1);
                    if rows + r > h {
                        break;
                    }
                    rows += r;
                    top -= 1;
                }
                self.win_mut().topline = top;
            }
            Some('h') | Some('l') | Some('H') | Some('L') => {
                let tw = self.text_width(self.win());
                let amount = match tc {
                    Some('h') | Some('l') => count.unwrap_or(1),
                    _ => tw / 2,
                };
                let w = self.win_mut();
                if matches!(tc, Some('h') | Some('H')) {
                    w.leftcol = w.leftcol.saturating_sub(amount);
                } else {
                    w.leftcol += amount;
                }
            }
            Some('s') => {
                let ts = self.tabstop_of(self.win());
                let v = vcol_of(self.cur_line(), cur.col, ts, self.ambi_double());
                self.win_mut().leftcol = v;
            }
            Some('e') => {
                let ts = self.tabstop_of(self.win());
                let v = vcol_of(self.cur_line(), cur.col, ts, self.ambi_double());
                let tw = self.text_width(self.win());
                self.win_mut().leftcol = (v + 1).saturating_sub(tw);
            }
            _ => {} // folds are not supported: zo, zc, za, zR, zM ... are no-ops
        }
        if first_nb {
            let col = first_nonblank(self.cur_line());
            self.win_mut().cursor.col = col;
        }
    }

    pub fn window_command(&mut self, t: Key, count: Option<usize>) {
        let n = count.unwrap_or(1).max(1);
        let c = match t.code {
            KeyCode::Char(c) if t.mods == 0 => match c {
                // CTRL-W CTRL-X is the same as CTRL-W x
                c if (c as u32) < 0x20 && c != '\r' => ((c as u8) + 0x60) as char,
                c => c,
            },
            KeyCode::Sp(Sp::Left) => 'h',
            KeyCode::Sp(Sp::Right) => 'l',
            KeyCode::Sp(Sp::Up) => 'k',
            KeyCode::Sp(Sp::Down) => 'j',
            _ => return,
        };
        match c {
            'h' | 'j' | 'k' | 'l' => {
                let id = self.window_in_direction(c, n);
                self.goto_window(id);
            }
            'w' | 'W' | 'p' | 't' | 'b' => {
                let wins = self.tab_wins();
                let cur = wins.iter().position(|w| *w == self.cur_win_id()).unwrap_or(0);
                let target = match c {
                    'w' => match count {
                        Some(cnt) => wins.get(cnt - 1).copied().unwrap_or(*wins.last().unwrap()),
                        None => wins[(cur + 1) % wins.len()],
                    },
                    'W' => wins[(cur + wins.len() - 1) % wins.len()],
                    'p' => self.tabs[self.cur_tab].prev,
                    't' => wins[0],
                    _ => *wins.last().unwrap(),
                };
                self.goto_window(target);
            }
            's' | 'S' => self.exec_ex_reporting("split"),
            'v' => self.exec_ex_reporting("vsplit"),
            'n' => self.exec_ex_reporting("new"),
            'c' => self.exec_ex_reporting("close"),
            'q' => self.exec_ex_reporting("quit"),
            'o' => self.exec_ex_reporting("only"),
            '=' => self.equalize_windows(),
            '+' => self.resize_window(false, n as isize, None),
            '-' => self.resize_window(false, -(n as isize), None),
            '>' => self.resize_window(true, n as isize, None),
            '<' => self.resize_window(true, -(n as isize), None),
            '_' => self.resize_window(false, 0, Some(count.unwrap_or(10_000))),
            '|' => self.resize_window(true, 0, Some(count.unwrap_or(10_000))),
            'r' => self.rotate_windows(false),
            'R' => self.rotate_windows(true),
            'x' => self.exchange_window(),
            'H' | 'J' | 'K' | 'L' => self.move_window_far(c),
            'T' => {
                if self.tab_wins().len() > 1 {
                    let buf = self.cur_buf_id();
                    let pos = self.cursor();
                    let id = self.cur_win_id();
                    let _ = self.close_window(id);
                    self.new_tab(Some(buf), None);
                    self.set_cursor(pos);
                }
            }
            '^' => {
                self.exec_ex_reporting("split #");
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------- commands

    fn begin_insert_command(&mut self, c: char, n: usize) {
        let cur = self.cursor();
        match c {
            'i' => {}
            'a' => {
                let line = self.cur_line();
                if !line.is_empty() {
                    let col = next_char_end(line, cur.col);
                    self.win_mut().cursor.col = col;
                }
            }
            'I' => {
                let line = self.cur_line();
                let col = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
                self.win_mut().cursor.col = col;
            }
            'A' => {
                let len = self.cur_line().len();
                self.win_mut().cursor.col = len;
            }
            'o' | 'O' => {
                let at = if c == 'o' { cur.line + 1 } else { cur.line };
                let indent = self.indent_for_open(cur.line, c == 'o');
                let len = indent.len();
                self.buf_mut().insert_lines(at, vec![indent]);
                self.win_mut().cursor = Pos::new(at, len);
            }
            _ => {}
        }
        self.start_insert(c, n);
    }

    fn replace_chars(&mut self, rc: char, n: usize) {
        let cur = self.cursor();
        let line = self.cur_line().to_string();
        let mut end = cur.col;
        let mut count = 0;
        while count < n && end < line.len() {
            end = next_char_end(&line, end);
            count += 1;
        }
        if count < n {
            return; // not enough characters
        }
        if rc == '\r' {
            let indent = if self.opt_bool("autoindent") { motion::indent_of(&line).to_string() } else { String::new() };
            let head = line[..cur.col].trim_end().to_string();
            let tail = format!("{}{}", indent, &line[end..]);
            self.buf_mut().splice(cur.line, cur.line + 1, vec![head, tail]);
            self.win_mut().cursor = Pos::new(cur.line + 1, indent.len());
            return;
        }
        let mid: String = std::iter::repeat_n(rc, n).collect();
        let new = format!("{}{}{}", &line[..cur.col], mid, &line[end..]);
        self.buf_mut().set_line(cur.line, new);
        let col = cur.col + mid.len() - rc.len_utf8();
        self.win_mut().cursor.col = col;
    }

    /// Join lines `first..=last`.
    pub fn join_lines(&mut self, first: usize, last: usize, spaces: bool) {
        let last = last.min(self.buf().last_line());
        if last <= first {
            return;
        }
        let mut out = self.line(first).to_string();
        let mut cursor_col = out.len();
        for l in first + 1..=last {
            let next = self.line(l);
            if spaces {
                let t = next.trim_start_matches([' ', '\t']);
                let trimmed_len = out.trim_end_matches([' ', '\t']).len();
                if !t.is_empty() {
                    out.truncate(trimmed_len);
                    cursor_col = out.len();
                    if !out.is_empty() && !t.starts_with(')') {
                        out.push(' ');
                    }
                    out.push_str(t);
                } else {
                    cursor_col = out.len().saturating_sub(1);
                }
            } else {
                cursor_col = out.len();
                out.push_str(next);
            }
        }
        self.buf_mut().splice(first, last + 1, vec![out]);
        self.win_mut().cursor = Pos::new(first, cursor_col);
        self.check_cursor();
    }

    pub fn undo(&mut self) {
        match self.buf_mut().undo() {
            Some(p) => {
                self.win_mut().cursor = p;
                self.check_cursor();
            }
            None => self.msg("Already at oldest change"),
        }
    }

    pub fn redo(&mut self) {
        match self.buf_mut().redo() {
            Some(p) => {
                self.win_mut().cursor = p;
                self.check_cursor();
            }
            None => self.msg("Already at newest change"),
        }
    }

    fn repeat_dot(&mut self, count: Option<usize>) {
        let d = self.dot.clone();
        if d.keys.is_empty() {
            return;
        }
        let mut keys = d.keys.clone();
        if let Some(c) = count {
            // replace the original count
            let mut i = 0;
            if keys.first().and_then(ch) == Some('"') {
                i = 2;
            }
            let mut j = i;
            while keys.get(j).and_then(ch).map(|c| c.is_ascii_digit() && (c != '0' || j > i)).unwrap_or(false) {
                j += 1;
            }
            keys.splice(i..j, c.to_string().chars().map(Key::ch));
        }
        self.replaying_dot = true;
        if let Some((kind, lines, cols)) = d.visual {
            let cur = self.cursor();
            self.visual_start = cur;
            let last = self.buf().last_line();
            let l = (cur.line + lines).min(last);
            let col = if lines == 0 { cur.col + cols } else { cols };
            let col = col.min(last_char_start(self.line(l)));
            self.win_mut().cursor = Pos::new(l, col);
            self.mode = Mode::Visual(kind);
        }
        self.run_keys(&keys, false);
        self.replaying_dot = false;
        // the nested commands reset this; `.` itself must never become the repeat
        self.dot_skip = true;
    }

    /// `p` / `P`
    pub fn put(&mut self, reg: Option<char>, before: bool, count: usize, cursor_after: bool) {
        let name = reg.unwrap_or('"');
        let Some(r) = self.reg_get(name) else {
            if name != '"' {
                self.emsg(format!("E353: Nothing in register {}", name));
            }
            return;
        };
        self.put_register(r, before, count, cursor_after);
    }

    pub fn put_register(&mut self, r: Register, before: bool, count: usize, cursor_after: bool) {
        let cur = self.cursor();
        let count = count.max(1);
        match r.kind {
            RegKind::Line => {
                let mut lines: Vec<String> = Vec::new();
                for _ in 0..count {
                    lines.extend(r.text.split('\n').map(|s| s.to_string()));
                }
                let n = lines.len();
                let at = if before { cur.line } else { cur.line + 1 };
                // pasting into an empty buffer placeholder keeps the placeholder above
                self.buf_mut().insert_lines(at, lines);
                if cursor_after {
                    let l = (at + n).min(self.buf().last_line());
                    self.win_mut().cursor = Pos::new(l, 0);
                } else {
                    let col = first_nonblank(self.line(at));
                    self.win_mut().cursor = Pos::new(at, col);
                }
                self.set_mark('[', Pos::new(at, 0));
                self.set_mark(']', Pos::new(at + n - 1, 0));
                if n > self.opt_global("report").as_num().max(0) as usize {
                    self.msg(format!("{} more lines", n));
                }
            }
            RegKind::Char => {
                let line = self.cur_line();
                let at = if before || line.is_empty() { cur } else { Pos::new(cur.line, next_char_end(line, cur.col)) };
                let text = r.text.repeat(count);
                let end = self.buf_mut().insert_text(at, &text);
                self.set_mark('[', at);
                self.set_mark(']', end);
                if cursor_after {
                    self.win_mut().cursor = end;
                } else if text.contains('\n') {
                    self.win_mut().cursor = at;
                } else {
                    let l = self.line(end.line);
                    self.win_mut().cursor = Pos::new(end.line, prev_char_start(l, end.col));
                }
                self.check_cursor();
            }
            RegKind::Block => {
                let ts = self.tabstop_of(self.win());
                let ambi = self.ambi_double();
                let line = self.cur_line().to_string();
                let col = if before || line.is_empty() { cur.col } else { next_char_end(&line, cur.col) };
                let vcol = vcol_of(&line, col, ts, ambi);
                let parts: Vec<&str> = r.text.split('\n').collect();
                let width = parts.iter().map(|p| motion::line_width(p, ts, ambi)).max().unwrap_or(0);
                for (i, p) in parts.iter().enumerate() {
                    let l = cur.line + i;
                    if l > self.buf().last_line() {
                        let at = self.buf().line_count();
                        self.buf_mut().insert_lines(at, vec![String::new()]);
                    }
                    let mut text = self.line(l).to_string();
                    let lw = motion::line_width(&text, ts, ambi);
                    if lw < vcol {
                        text.push_str(&" ".repeat(vcol - lw));
                    }
                    let c = col_at_vcol(&text, vcol, ts, ambi).min(text.len());
                    let mut ins = p.repeat(count);
                    if c < text.len() {
                        let pw = motion::line_width(p, ts, ambi);
                        ins.push_str(&" ".repeat((width - pw) * count));
                    }
                    text.insert_str(c, &ins);
                    self.buf_mut().set_line(l, text);
                }
                self.win_mut().cursor = Pos::new(cur.line, col);
                self.check_cursor();
            }
        }
    }

    /// Shift lines by `amount` * 'shiftwidth'.
    pub fn shift_lines(&mut self, first: usize, last: usize, amount: isize) {
        let sw = self.shiftwidth();
        let ts = self.opt_num("tabstop").max(1) as usize;
        let round = self.opt_bool("shiftround");
        let et = self.opt_bool("expandtab");
        for l in first..=last.min(self.buf().last_line()) {
            let line = self.line(l).to_string();
            if line.trim().is_empty() {
                continue;
            }
            let ind = motion::indent_of(&line);
            let cur = motion::line_width(ind, ts, false) as isize;
            let mut new = cur + amount * sw as isize;
            if round {
                new = if amount > 0 { (cur / sw as isize + amount) * sw as isize } else { ((cur + sw as isize - 1) / sw as isize + amount) * sw as isize };
            }
            let new = new.max(0) as usize;
            let text = format!("{}{}", make_indent(new, ts, et), &line[ind.len()..]);
            self.buf_mut().set_line(l, text);
        }
    }

    pub fn shiftwidth(&self) -> usize {
        let sw = self.opt_num("shiftwidth");
        if sw <= 0 { self.opt_num("tabstop").max(1) as usize } else { sw as usize }
    }

    fn format_lines(&mut self, first: usize, last: usize) {
        let tw = match self.opt_num("textwidth") {
            n if n > 0 => n as usize,
            _ => 79,
        };
        let ts = self.opt_num("tabstop").max(1) as usize;
        let ambi = self.ambi_double();
        let last = last.min(self.buf().last_line());
        let mut out: Vec<String> = Vec::new();
        let mut para: Vec<String> = Vec::new();
        let mut indent = String::new();
        let flush = |para: &mut Vec<String>, indent: &str, out: &mut Vec<String>| {
            if para.is_empty() {
                return;
            }
            let mut cur = indent.to_string();
            let mut empty = true;
            for w in para.iter().flat_map(|l| l.split_whitespace()) {
                let wlen = motion::line_width(w, ts, ambi);
                if !empty && motion::line_width(&cur, ts, ambi) + 1 + wlen > tw {
                    out.push(std::mem::replace(&mut cur, indent.to_string()));
                    empty = true;
                }
                if !empty {
                    cur.push(' ');
                }
                cur.push_str(w);
                empty = false;
            }
            if !empty {
                out.push(cur);
            }
            para.clear();
        };
        for l in first..=last {
            let line = self.line(l).to_string();
            if line.trim().is_empty() {
                flush(&mut para, &indent, &mut out);
                out.push(String::new());
            } else {
                if para.is_empty() {
                    indent = motion::indent_of(&line).to_string();
                }
                para.push(line);
            }
        }
        flush(&mut para, &indent, &mut out);
        let n = out.len();
        self.buf_mut().splice(first, last + 1, out);
        let l = (first + n).saturating_sub(1).min(self.buf().last_line());
        let col = first_nonblank(self.line(l));
        self.win_mut().cursor = Pos::new(l, col);
    }

    fn jump_list(&mut self, back: bool, n: usize) {
        if back {
            if self.jumpidx == self.jumplist.len() {
                // remember where we came from so CTRL-I can return
                let entry = (self.cur_buf_id(), self.cursor());
                let idx = self.jumpidx;
                self.jumplist.retain(|e| !(e.0 == entry.0 && e.1.line == entry.1.line));
                self.jumplist.push(entry);
                self.jumpidx = (self.jumplist.len() - 1).min(idx);
                if self.jumplist.len() - 1 < idx {
                    self.jumpidx = self.jumplist.len() - 1;
                }
            }
            if self.jumpidx < n {
                return;
            }
            self.jumpidx -= n;
        } else {
            if self.jumpidx + n >= self.jumplist.len() {
                return;
            }
            self.jumpidx += n;
        }
        let (buf, pos) = self.jumplist[self.jumpidx];
        if buf != self.cur_buf_id() {
            if !self.buffers.contains_key(&buf) || self.can_abandon(false).is_err() {
                return;
            }
            self.set_cur_buf(buf);
        }
        self.set_cursor(pos);
    }

    pub fn file_info(&mut self) {
        let b = self.buf();
        let cur = self.cursor();
        let n = b.line_count();
        let pct = (cur.line + 1) * 100 / n.max(1);
        let msg = format!(
            "\"{}\"{}{} {} line{} --{}%--",
            b.display_name(),
            if b.modified { " [Modified]" } else { "" },
            if self.opt_bool("readonly") { " [readonly]" } else { "" },
            n,
            if n == 1 { "" } else { "s" },
            pct
        );
        self.msg(msg);
    }

    pub fn scroll_lines(&mut self, n: isize) {
        let last = self.buf().last_line();
        let top = self.win().topline as isize + n;
        let top = top.clamp(0, last as isize) as usize;
        self.win_mut().topline = top;
        // keep the cursor inside the window
        let so = self.opt_num("scrolloff").max(0) as usize;
        let h = self.win().height;
        let so = so.min(h.saturating_sub(1) / 2);
        let bot = self.botline(self.win());
        let cur = self.cursor();
        let mut line = cur.line;
        if line < top + so && top > 0 {
            line = (top + so).min(last);
        } else if line < top {
            line = top;
        }
        if line + so > bot && bot < last {
            line = bot.saturating_sub(so).max(top);
        } else if line > bot {
            line = bot;
        }
        if line != cur.line {
            let r = self.motion_vertical_to(line);
            self.win_mut().cursor = r;
        }
        // scroll_to_cursor (in after_input) must not undo the scroll
    }

    fn motion_vertical_to(&self, line: usize) -> Pos {
        let w = self.win();
        let ts = self.tabstop_of(w);
        let text = self.line(line);
        let col = if w.curswant == usize::MAX { last_char_start(text) } else { col_at_vcol(text, w.curswant, ts, self.ambi_double()).min(last_char_start(text)) };
        Pos::new(line, col)
    }

    /// CTRL-A / CTRL-X
    fn increment(&mut self, delta: i64) {
        let cur = self.cursor();
        let line = self.cur_line().to_string();
        let bytes = line.as_bytes();
        // find the number under or after the cursor
        let mut s = cur.col.min(line.len());
        while s > 0 && (bytes[s - 1].is_ascii_hexdigit() || bytes[s - 1] == b'x' || bytes[s - 1] == b'X') {
            s -= 1;
        }
        let mut i = s;
        while i < bytes.len() && !bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i >= bytes.len() {
            return;
        }
        let hex = self.opt_str("nrformats").contains("hex") && bytes[i] == b'0' && i + 2 < bytes.len() + 1 && matches!(bytes.get(i + 1), Some(b'x') | Some(b'X')) && bytes.get(i + 2).map(|b| b.is_ascii_hexdigit()).unwrap_or(false);
        if hex {
            let start = i + 2;
            let mut e = start;
            while e < bytes.len() && bytes[e].is_ascii_hexdigit() {
                e += 1;
            }
            let v = u64::from_str_radix(&line[start..e], 16).unwrap_or(0);
            let nv = (v as i64).wrapping_add(delta) as u64;
            let upper = line[start..e].chars().any(|c| c.is_ascii_uppercase());
            let s = if upper { format!("{:0w$X}", nv, w = e - start) } else { format!("{:0w$x}", nv, w = e - start) };
            let new = format!("{}{}{}", &line[..start], s, &line[e..]);
            self.buf_mut().set_line(cur.line, new);
            self.win_mut().cursor.col = start + s.len() - 1;
            return;
        }
        // decimal: back up to the start of the digit run
        while i > 0 && bytes[i - 1].is_ascii_digit() {
            i -= 1;
        }
        let mut e = i;
        while e < bytes.len() && bytes[e].is_ascii_digit() {
            e += 1;
        }
        let neg = i > 0 && bytes[i - 1] == b'-';
        let start = if neg { i - 1 } else { i };
        let v: i64 = line[start..e].parse().unwrap_or(0);
        let s = v.wrapping_add(delta).to_string();
        let new = format!("{}{}{}", &line[..start], s, &line[e..]);
        self.buf_mut().set_line(cur.line, new);
        self.win_mut().cursor.col = start + s.len() - 1;
    }

    pub fn feed_front_keys(&mut self, keys: &[Key], remap: bool) {
        self.feed_front(keys, remap, false, 0);
    }
}

fn cell_w(c: char, vcol: usize, ts: usize, ambi: bool) -> usize {
    if c == '\t' { ts - vcol % ts } else { motion::char_width(c, ambi).max(1) }
}

fn line_vwidth(line: &str, ts: usize, ambi: bool) -> usize {
    motion::line_width(line, ts, ambi)
}

pub fn make_indent(width: usize, ts: usize, expandtab: bool) -> String {
    if expandtab {
        " ".repeat(width)
    } else {
        format!("{}{}", "\t".repeat(width / ts), " ".repeat(width % ts))
    }
}

fn toggle_case(c: char) -> char {
    if c.is_uppercase() {
        c.to_lowercase().next().unwrap_or(c)
    } else if c.is_lowercase() {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    }
}

fn rot13(c: char) -> char {
    match c {
        'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
        'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
        _ => c,
    }
}
