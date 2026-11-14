//! Text measurement helpers, cursor motions, text objects and searching.

use unicode_width::UnicodeWidthChar;

use super::buffer::{Buffer, Pos};
use super::{Editor, VisualKind};

// ------------------------------------------------------------ text helpers

pub fn char_width(c: char, ambi_double: bool) -> usize {
    if (c as u32) < 0x20 || c == '\x7f' {
        return 2; // shown as ^X
    }
    let w = if ambi_double { c.width_cjk() } else { c.width() };
    w.unwrap_or(1).max(if c.is_control() { 1 } else { 0 }).max(if w == Some(0) { 0 } else { 1 })
}

/// Display width of a whole line.
pub fn line_width(line: &str, ts: usize, ambi: bool) -> usize {
    let mut v = 0;
    for c in line.chars() {
        v += if c == '\t' { ts - v % ts } else { char_width(c, ambi) };
    }
    v
}

/// Display column at which the character at byte `col` starts.
pub fn vcol_of(line: &str, col: usize, ts: usize, ambi: bool) -> usize {
    let mut v = 0;
    for (i, c) in line.char_indices() {
        if i >= col {
            break;
        }
        v += if c == '\t' { ts - v % ts } else { char_width(c, ambi) };
    }
    v
}

/// Byte column of the character covering display column `vcol`
/// (`line.len()` when the line is shorter).
pub fn col_at_vcol(line: &str, vcol: usize, ts: usize, ambi: bool) -> usize {
    let mut v = 0;
    for (i, c) in line.char_indices() {
        let w = if c == '\t' { ts - v % ts } else { char_width(c, ambi) };
        if vcol < v + w {
            return i;
        }
        v += w;
    }
    line.len()
}

pub fn char_at(line: &str, idx: usize) -> Option<char> {
    line.get(idx..).and_then(|s| s.chars().next())
}

pub fn next_char_end(line: &str, idx: usize) -> usize {
    match char_at(line, idx) {
        Some(c) => idx + c.len_utf8(),
        None => line.len(),
    }
}

pub fn prev_char_start(line: &str, idx: usize) -> usize {
    let mut i = idx.min(line.len());
    if i == 0 {
        return 0;
    }
    i -= 1;
    while i > 0 && !line.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub fn last_char_start(line: &str) -> usize {
    prev_char_start(line, line.len())
}

pub fn first_nonblank(line: &str) -> usize {
    line.find(|c: char| c != ' ' && c != '\t').unwrap_or_else(|| last_char_start(line))
}

pub fn indent_of(line: &str) -> &str {
    let end = line.find(|c: char| c != ' ' && c != '\t').unwrap_or(line.len());
    &line[..end]
}

/// Character class used by word motions.
pub fn char_class(c: char, big: bool) -> u8 {
    if c == ' ' || c == '\t' || c == '\0' || c == '\u{3000}' {
        return 0;
    }
    if big {
        return 2;
    }
    let u = c as u32;
    if u < 0x80 {
        return if c.is_ascii_alphanumeric() || c == '_' { 2 } else { 1 };
    }
    match u {
        0x3040..=0x309f => 3,                                   // hiragana
        0x30a0..=0x30ff => 4,                                   // katakana
        0x4e00..=0x9fff | 0x3400..=0x4dbf | 0xf900..=0xfaff => 5, // kanji
        0x3001..=0x303f | 0xff01..=0xff0f | 0xff1a..=0xff20 | 0xff3b..=0xff40 | 0xff5b..=0xff65 => 1,
        0x2000..=0x206f | 0x2190..=0x2bff => 1,
        0xac00..=0xd7af => 6, // hangul
        _ => 2,
    }
}

/// A movable position over a buffer with Vim's inc()/dec() semantics: the
/// position just past the last character of a line (Vim's NUL) is visited.
struct Cur<'a> {
    b: &'a Buffer,
    p: Pos,
}

impl<'a> Cur<'a> {
    fn line(&self) -> &'a str {
        self.b.line(self.p.line)
    }
    /// Character class under the cursor; 0 for blanks and the end of line.
    fn cls(&self, big: bool) -> u8 {
        match char_at(self.line(), self.p.col) {
            Some(c) => char_class(c, big),
            None => 0,
        }
    }
    fn empty_line(&self) -> bool {
        self.line().is_empty()
    }
    /// 0: moved within the line, 2: moved onto the end of line,
    /// 1: moved to the next line, -1: at the end of the buffer.
    fn inc(&mut self) -> i32 {
        let line = self.line();
        if self.p.col < line.len() {
            let n = next_char_end(line, self.p.col);
            self.p.col = n;
            return if n < line.len() { 0 } else { 2 };
        }
        if self.p.line < self.b.last_line() {
            self.p.line += 1;
            self.p.col = 0;
            return 1;
        }
        -1
    }
    /// 0: moved within the line, 1: moved to the end of the previous line,
    /// -1: at the start of the buffer.
    fn dec(&mut self) -> i32 {
        if self.p.col > 0 {
            self.p.col = prev_char_start(self.line(), self.p.col);
            return 0;
        }
        if self.p.line > 0 {
            self.p.line -= 1;
            self.p.col = self.line().len();
            return 1;
        }
        -1
    }
    /// Skip characters of class `class`; true when the buffer boundary was hit.
    fn skip_chars(&mut self, class: u8, forward: bool, big: bool) -> bool {
        while self.cls(big) == class {
            if (if forward { self.inc() } else { self.dec() }) == -1 {
                return true;
            }
        }
        false
    }
}

/// `w` / `W` (Vim's fwd_word).  `eol`: stop at the end of the line (operators).
/// Returns the new position and whether the motion succeeded.
fn fwd_word(b: &Buffer, start: Pos, count: usize, big: bool, eol: bool) -> (Pos, bool) {
    let mut c = Cur { b, p: start };
    let mut count = count as i64;
    while count > 0 {
        count -= 1;
        let sclass = c.cls(big);
        let last_line = c.p.line == b.last_line();
        let i = c.inc();
        if i == -1 || (i >= 1 && last_line) {
            return (c.p, false);
        }
        if i >= 1 && eol && count == 0 {
            return (c.p, true);
        }
        if sclass != 0 {
            while c.cls(big) == sclass {
                let i = c.inc();
                if i == -1 || (i >= 1 && eol && count == 0) {
                    return (c.p, true);
                }
            }
        }
        while c.cls(big) == 0 {
            if c.p.col == 0 && c.empty_line() {
                break;
            }
            let i = c.inc();
            if i == -1 || (i >= 1 && eol && count == 0) {
                return (c.p, true);
            }
        }
    }
    (c.p, true)
}

/// `b` / `B` (Vim's bck_word).
fn bck_word(b: &Buffer, start: Pos, count: usize, big: bool, stop: bool) -> (Pos, bool) {
    let mut c = Cur { b, p: start };
    let mut stop = stop;
    for _ in 0..count {
        let sclass = c.cls(big);
        if c.dec() == -1 {
            return (c.p, false);
        }
        let mut finished = false;
        if !stop || sclass == c.cls(big) || sclass == 0 {
            while c.cls(big) == 0 {
                if c.p.col == 0 && c.empty_line() {
                    finished = true;
                    break;
                }
                if c.dec() == -1 {
                    return (c.p, true);
                }
            }
            if !finished {
                let cls = c.cls(big);
                if c.skip_chars(cls, false, big) {
                    return (c.p, true);
                }
            }
        }
        if !finished {
            c.inc(); // overshot - forward one
        }
        stop = false;
    }
    (c.p, true)
}

/// `e` / `E` (Vim's end_word).  `stop`: when already at the end of a word
/// move one word less (used by `cw`).
fn end_word(b: &Buffer, start: Pos, count: usize, big: bool, stop: bool, empty: bool) -> (Pos, bool) {
    let mut c = Cur { b, p: start };
    let mut stop = stop;
    for _ in 0..count {
        let sclass = c.cls(big);
        if c.inc() == -1 {
            return (c.p, false);
        }
        let mut finished = false;
        if c.cls(big) == sclass && sclass != 0 {
            if c.skip_chars(sclass, true, big) {
                return (c.p, false);
            }
        } else if !stop || sclass == 0 {
            while c.cls(big) == 0 {
                if empty && c.p.col == 0 && c.empty_line() {
                    finished = true;
                    break;
                }
                if c.inc() == -1 {
                    return (c.p, false);
                }
            }
            if !finished {
                let cls = c.cls(big);
                if c.skip_chars(cls, true, big) {
                    return (c.p, false);
                }
            }
        }
        if !finished {
            c.dec(); // overshot - one char backward
        }
        stop = false;
    }
    (c.p, true)
}

/// `ge` / `gE` (Vim's bckend_word).
fn bckend_word(b: &Buffer, start: Pos, count: usize, big: bool, eol: bool) -> (Pos, bool) {
    let mut c = Cur { b, p: start };
    for _ in 0..count {
        let sclass = c.cls(big);
        let i = c.dec();
        if i == -1 {
            return (c.p, false);
        }
        if eol && i == 1 {
            return (c.p, true);
        }
        if sclass != 0 {
            while c.cls(big) == sclass {
                let i = c.dec();
                if i == -1 || (eol && i == 1) {
                    return (c.p, true);
                }
            }
        }
        while c.cls(big) == 0 {
            if c.p.col == 0 && c.empty_line() {
                break;
            }
            let i = c.dec();
            if i == -1 || (eol && i == 1) {
                return (c.p, true);
            }
        }
    }
    (c.p, true)
}

/// `cw` on a non-blank: like `ce`, but on the last character of a word only
/// that character is changed (end_word with stop=TRUE).
pub fn change_word_end(b: &Buffer, start: Pos, count: usize, big: bool) -> Pos {
    end_word(b, start, count, big, true, false).0
}

// ------------------------------------------------------------------ motions

#[derive(Clone, Debug, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    DispUp,
    DispDown,
    LineStart,
    FirstNonBlank,
    LineEnd,
    LastNonBlank,
    Column,
    WordFwd(bool),
    WordBack(bool),
    WordEnd(bool),
    WordEndBack(bool),
    Find { ch: char, forward: bool, till: bool },
    RepeatFind { reverse: bool },
    GotoLine,
    FirstLine,
    ScreenTop,
    ScreenMiddle,
    ScreenBottom,
    ParaFwd,
    ParaBack,
    SentFwd,
    SentBack,
    MatchPair,
    SearchNext { reverse: bool },
    Star { forward: bool, whole: bool },
    Mark { ch: char, linewise: bool },
    LineDown,
    LineUp,
    Underscore,
    Space,
    Backspace,
    Unmatched { ch: char, forward: bool },
    SectionBack,
    SectionFwd,
    DispLineStart,
    DispLineEnd,
    DispMiddle,
    PageDown,
    PageUp,
    HalfDown,
    HalfUp,
    /// Target computed elsewhere (search from the command line).
    To(Pos),
}

#[derive(Clone, Copy, Debug)]
pub struct MotionResult {
    pub pos: Pos,
    pub linewise: bool,
    pub inclusive: bool,
    /// New curswant; `None` keeps the current one (vertical motions).
    pub curswant: Option<usize>,
    pub jump: bool,
}

impl Editor {
    fn mres(&self, pos: Pos, inclusive: bool) -> MotionResult {
        let ts = self.tabstop_of(self.win());
        let v = vcol_of(self.line(pos.line), pos.col, ts, self.ambi_double());
        MotionResult { pos, linewise: false, inclusive, curswant: Some(v), jump: false }
    }

    fn vertical(&self, line: usize) -> MotionResult {
        let w = self.win();
        let ts = self.tabstop_of(w);
        let text = self.line(line);
        let col = if w.curswant == usize::MAX {
            if matches!(self.mode, super::Mode::Visual(_)) { text.len() } else { last_char_start(text) }
        } else {
            let c = col_at_vcol(text, w.curswant, ts, self.ambi_double());
            if c >= text.len() { last_char_start(text) } else { c }
        };
        MotionResult { pos: Pos::new(line, col), linewise: true, inclusive: true, curswant: None, jump: false }
    }

    fn first_nb(&self, line: usize, jump: bool) -> MotionResult {
        let pos = Pos::new(line, first_nonblank(self.line(line)));
        let mut r = self.mres(pos, true);
        r.linewise = true;
        r.jump = jump;
        r
    }

    /// Compute the target of a motion without moving the cursor.
    pub fn motion_target(&mut self, m: &Motion, count: Option<usize>, op_pending: bool) -> Option<MotionResult> {
        let cur = self.cursor();
        let n = count.unwrap_or(1).max(1);
        let last = self.buf().last_line();
        let line = self.cur_line().to_string();
        let ww = self.opt_global("whichwrap").as_string();
        match m {
            Motion::Left | Motion::Backspace => {
                let mut p = cur;
                for _ in 0..n {
                    if p.col > 0 {
                        p.col = prev_char_start(self.line(p.line), p.col);
                    } else if *m == Motion::Backspace && ww.contains('b') && p.line > 0 {
                        p.line -= 1;
                        p.col = self.line(p.line).len();
                    } else {
                        break;
                    }
                }
                if p == cur {
                    return None;
                }
                Some(self.mres(p, false))
            }
            Motion::Right | Motion::Space => {
                let mut p = cur;
                for _ in 0..n {
                    let l = self.line(p.line);
                    let next = next_char_end(l, p.col);
                    let limit = if op_pending || matches!(self.mode, super::Mode::Visual(_)) { l.len() } else { last_char_start(l) };
                    if p.col < limit && next <= l.len() {
                        p.col = next.min(l.len());
                    } else if *m == Motion::Space && ww.contains('s') && p.line < last {
                        p.line += 1;
                        p.col = 0;
                    } else {
                        break;
                    }
                }
                if p == cur {
                    return None;
                }
                Some(self.mres(p, false))
            }
            Motion::Up => {
                if cur.line == 0 {
                    return None;
                }
                Some(self.vertical(cur.line.saturating_sub(n)))
            }
            Motion::Down => {
                if cur.line >= last {
                    return None;
                }
                Some(self.vertical((cur.line + n).min(last)))
            }
            Motion::DispUp | Motion::DispDown => self.display_line_motion(*m == Motion::DispDown, n),
            Motion::LineStart => Some(self.mres(Pos::new(cur.line, 0), false)),
            Motion::FirstNonBlank => Some(self.mres(Pos::new(cur.line, first_nonblank(&line)), false)),
            Motion::LineEnd => {
                let l = (cur.line + n - 1).min(last);
                let text = self.line(l);
                let mut r = self.mres(Pos::new(l, last_char_start(text)), true);
                r.curswant = Some(usize::MAX);
                Some(r)
            }
            Motion::LastNonBlank => {
                let l = (cur.line + n - 1).min(last);
                let text = self.line(l);
                let end = text.trim_end_matches([' ', '\t']).len();
                Some(self.mres(Pos::new(l, prev_char_start(text, end.max(1))), true))
            }
            Motion::Column => {
                let ts = self.tabstop_of(self.win());
                let c = col_at_vcol(&line, n - 1, ts, self.ambi_double());
                Some(self.mres(Pos::new(cur.line, c.min(last_char_start(&line))), false))
            }
            Motion::WordFwd(big) => {
                let (p, ok) = fwd_word(self.buf(), cur, n, *big, op_pending);
                if (!ok && !op_pending) || p == cur {
                    return None;
                }
                Some(self.mres(p, false))
            }
            Motion::WordBack(big) => {
                let (p, ok) = bck_word(self.buf(), cur, n, *big, false);
                if !ok || p == cur {
                    return None;
                }
                Some(self.mres(p, false))
            }
            Motion::WordEnd(big) => {
                let (p, _) = end_word(self.buf(), cur, n, *big, false, false);
                if p == cur {
                    return None;
                }
                // never leave the cursor on the end-of-line position
                let p = if p.col >= self.line(p.line).len() && p.col > 0 { Pos::new(p.line, prev_char_start(self.line(p.line), p.col)) } else { p };
                Some(self.mres(p, true))
            }
            Motion::WordEndBack(big) => {
                let (p, ok) = bckend_word(self.buf(), cur, n, *big, false);
                if !ok || p == cur {
                    return None;
                }
                let p = if p.col >= self.line(p.line).len() && p.col > 0 { Pos::new(p.line, prev_char_start(self.line(p.line), p.col)) } else { p };
                Some(self.mres(p, true))
            }
            Motion::Find { ch, forward, till } => {
                self.last_ft = Some((if *forward { if *till { 't' } else { 'f' } } else if *till { 'T' } else { 'F' }, *ch));
                self.find_char(*ch, *forward, *till, n, false)
            }
            Motion::RepeatFind { reverse } => {
                let (kind, ch) = self.last_ft?;
                let mut forward = matches!(kind, 'f' | 't');
                let till = matches!(kind, 't' | 'T');
                if *reverse {
                    forward = !forward;
                }
                self.find_char(ch, forward, till, n, true)
            }
            Motion::GotoLine => {
                let l = count.map(|c| c.saturating_sub(1)).unwrap_or(last).min(last);
                Some(self.first_nb(l, true))
            }
            Motion::FirstLine => {
                let l = count.map(|c| c.saturating_sub(1)).unwrap_or(0).min(last);
                Some(self.first_nb(l, true))
            }
            Motion::ScreenTop | Motion::ScreenMiddle | Motion::ScreenBottom => {
                let w = self.win();
                let top = w.topline;
                let bot = self.botline(w).min(last);
                let so = self.opt_num("scrolloff").max(0) as usize;
                let l = match m {
                    Motion::ScreenTop => (top + (n - 1).max(if top > 0 { so } else { 0 })).min(bot),
                    Motion::ScreenBottom => bot.saturating_sub((n - 1).max(if bot < last { so } else { 0 })).max(top),
                    _ => (top + bot) / 2,
                };
                Some(self.first_nb(l, true))
            }
            Motion::ParaFwd => {
                let mut l = cur.line;
                for _ in 0..n {
                    while l < last && self.line(l).is_empty() {
                        l += 1;
                    }
                    while l < last && !self.line(l).is_empty() {
                        l += 1;
                    }
                }
                let col = if self.line(l).is_empty() { 0 } else { if op_pending { self.line(l).len() } else { last_char_start(self.line(l)) } };
                let mut r = self.mres(Pos::new(l, col), false);
                r.jump = true;
                Some(r)
            }
            Motion::ParaBack => {
                let mut l = cur.line;
                for _ in 0..n {
                    while l > 0 && self.line(l).is_empty() {
                        l -= 1;
                    }
                    while l > 0 && !self.line(l).is_empty() {
                        l -= 1;
                    }
                }
                let mut r = self.mres(Pos::new(l, 0), false);
                r.jump = true;
                Some(r)
            }
            Motion::SentFwd | Motion::SentBack => self.sentence_motion(*m == Motion::SentFwd, n),
            Motion::MatchPair => {
                if let Some(pct) = count {
                    let l = ((pct.min(100) * (last + 1) + 99) / 100).saturating_sub(1).min(last);
                    return Some(self.first_nb(l, true));
                }
                let p = self.match_pair(cur)?;
                let mut r = self.mres(p, true);
                r.jump = true;
                Some(r)
            }
            Motion::SearchNext { reverse } => {
                let pat = match self.last_search.clone() {
                    Some(p) => p,
                    None => {
                        self.emsg("E35: No previous regular expression");
                        return None;
                    }
                };
                let forward = self.search_forward != *reverse;
                self.hlsearch_off = false;
                let p = self.search(&pat, forward, cur, n, true)?;
                let mut r = self.mres(p, false);
                r.jump = true;
                Some(r)
            }
            Motion::Star { forward, whole } => {
                let (s, e) = self.word_under_cursor()?;
                let word = line[s..e].to_string();
                let esc = escape_pattern(&word);
                let keyword = word.chars().next().map(|c| char_class(c, false) >= 2).unwrap_or(false);
                let pat = if *whole && keyword { format!("\\<{}\\>", esc) } else { esc };
                self.last_search = Some(pat.clone());
                self.search_forward = *forward;
                self.hlsearch_off = false;
                self.search_history.push(pat.clone());
                let from = Pos::new(cur.line, s);
                let p = self.search(&pat, *forward, from, n, true)?;
                let mut r = self.mres(p, false);
                r.jump = true;
                Some(r)
            }
            Motion::Mark { ch, linewise } => {
                let p = self.get_mark(*ch)?;
                let p = self.buf().clamp(p);
                let mut r = if *linewise { self.first_nb(p.line, false) } else { self.mres(p, false) };
                r.jump = true;
                Some(r)
            }
            Motion::LineDown => {
                if cur.line + n > last && cur.line >= last {
                    return None;
                }
                Some(self.first_nb((cur.line + n).min(last), false))
            }
            Motion::LineUp => {
                if cur.line == 0 {
                    return None;
                }
                Some(self.first_nb(cur.line.saturating_sub(n), false))
            }
            Motion::Underscore => Some(self.first_nb((cur.line + n - 1).min(last), false)),
            Motion::Unmatched { ch, forward } => {
                let (open, close) = match ch {
                    '(' | ')' => ('(', ')'),
                    _ => ('{', '}'),
                };
                let mut p = cur;
                for _ in 0..n {
                    p = find_unmatched(self.buf(), p, open, close, *forward)?;
                }
                let mut r = self.mres(p, false);
                r.jump = true;
                Some(r)
            }
            Motion::SectionBack | Motion::SectionFwd => {
                let fwd = *m == Motion::SectionFwd;
                let mut l = cur.line;
                for _ in 0..n {
                    loop {
                        if fwd {
                            if l >= last {
                                break;
                            }
                            l += 1;
                        } else {
                            if l == 0 {
                                break;
                            }
                            l -= 1;
                        }
                        if self.line(l).starts_with('{') {
                            break;
                        }
                    }
                }
                let mut r = self.mres(Pos::new(l, 0), false);
                r.jump = true;
                Some(r)
            }
            Motion::DispLineStart | Motion::DispLineEnd | Motion::DispMiddle => {
                let w = self.win();
                let ts = self.tabstop_of(w);
                let tw = self.text_width(w);
                let ambi = self.ambi_double();
                let v = vcol_of(&line, cur.col, ts, ambi);
                let wrap = self.opt_bool("wrap");
                let base = if wrap { v / tw * tw } else { w.leftcol };
                let target = match m {
                    Motion::DispLineStart => base,
                    Motion::DispLineEnd => base + tw - 1,
                    _ => base + tw / 2,
                };
                let c = col_at_vcol(&line, target, ts, ambi).min(last_char_start(&line));
                Some(self.mres(Pos::new(cur.line, c), *m == Motion::DispLineEnd))
            }
            Motion::PageDown | Motion::PageUp | Motion::HalfDown | Motion::HalfUp => {
                let h = self.win().height.max(1);
                let amount = match m {
                    Motion::PageDown | Motion::PageUp => h.saturating_sub(2).max(1) * n,
                    _ => {
                        let sc = self.opt_num("scroll");
                        if let Some(c) = count {
                            let _ = self.set_opt_by_name("scroll", super::options::OptVal::Num(c as i64), 'l');
                            c
                        } else if sc > 0 {
                            sc as usize
                        } else {
                            (h / 2).max(1)
                        }
                    }
                };
                let down = matches!(m, Motion::PageDown | Motion::HalfDown);
                if (down && cur.line >= last) || (!down && cur.line == 0) {
                    return None;
                }
                let l = if down { (cur.line + amount).min(last) } else { cur.line.saturating_sub(amount) };
                let top = self.win().topline;
                let newtop = if down { (top + amount).min(last) } else { top.saturating_sub(amount) };
                self.win_mut().topline = newtop;
                let mut r = self.first_nb(l, false);
                if matches!(m, Motion::HalfDown | Motion::HalfUp) {
                    let v = self.vertical(l);
                    r.pos = v.pos;
                    r.curswant = None;
                }
                Some(r)
            }
            Motion::To(p) => {
                let mut r = self.mres(*p, false);
                r.jump = true;
                Some(r)
            }
        }
    }

    fn display_line_motion(&mut self, down: bool, n: usize) -> Option<MotionResult> {
        let w = self.win();
        if !self.opt_bool("wrap") {
            let cur = w.cursor;
            let last = self.buf().last_line();
            if (down && cur.line >= last) || (!down && cur.line == 0) {
                return None;
            }
            let mut r = self.vertical(if down { (cur.line + n).min(last) } else { cur.line.saturating_sub(n) });
            r.linewise = false;
            r.inclusive = false;
            return Some(r);
        }
        let ts = self.tabstop_of(w);
        let tw = self.text_width(w);
        let ambi = self.ambi_double();
        let last = self.buf().last_line();
        let mut p = w.cursor;
        let want = w.curswant;
        let cur_v = vcol_of(self.line(p.line), p.col, ts, ambi);
        let want_in_row = if want == usize::MAX { tw - 1 } else { want % tw };
        let mut row = cur_v / tw;
        for _ in 0..n {
            let width = line_width(self.line(p.line), ts, ambi);
            let rows = width.max(1).div_ceil(tw);
            if down {
                if row + 1 < rows {
                    row += 1;
                } else if p.line < last {
                    p.line += 1;
                    row = 0;
                } else {
                    break;
                }
            } else if row > 0 {
                row -= 1;
            } else if p.line > 0 {
                p.line -= 1;
                let width = line_width(self.line(p.line), ts, ambi);
                row = width.max(1).div_ceil(tw) - 1;
            } else {
                break;
            }
        }
        let text = self.line(p.line);
        let target = row * tw + want_in_row;
        let c = col_at_vcol(text, target, ts, ambi);
        p.col = if c >= text.len() { last_char_start(text) } else { c };
        if p == self.cursor() {
            return None;
        }
        Some(MotionResult { pos: p, linewise: false, inclusive: false, curswant: None, jump: false })
    }

    fn find_char(&self, ch: char, forward: bool, till: bool, count: usize, repeat: bool) -> Option<MotionResult> {
        let cur = self.cursor();
        let line = self.cur_line();
        let mut col = cur.col;
        let mut n = count;
        // `;` after `t` should not get stuck on the adjacent character
        let mut skip_adjacent = repeat && till;
        if forward {
            let mut i = next_char_end(line, col);
            while i < line.len() {
                let c = char_at(line, i)?;
                if c == ch {
                    if skip_adjacent && i == next_char_end(line, cur.col) {
                        skip_adjacent = false;
                    } else {
                        n -= 1;
                        if n == 0 {
                            col = if till { prev_char_start(line, i) } else { i };
                            return Some(self.mres(Pos::new(cur.line, col), true));
                        }
                    }
                }
                i += c.len_utf8();
            }
        } else {
            let mut i = col;
            while i > 0 {
                i = prev_char_start(line, i);
                let c = char_at(line, i)?;
                if c == ch {
                    if skip_adjacent && next_char_end(line, i) == cur.col {
                        skip_adjacent = false;
                    } else {
                        n -= 1;
                        if n == 0 {
                            col = if till { next_char_end(line, i) } else { i };
                            return Some(self.mres(Pos::new(cur.line, col), false));
                        }
                    }
                }
            }
        }
        None
    }

    fn sentence_motion(&self, forward: bool, n: usize) -> Option<MotionResult> {
        let b = self.buf();
        let mut c = Cur { b, p: self.cursor() };
        let is_end = |c: &Cur| -> bool {
            // position is the start of a sentence?
            if c.p.col == 0 && c.empty_line() {
                return true;
            }
            let line = c.line();
            if c.cls(true) == 0 {
                return false;
            }
            if c.p.col == 0 {
                return c.p.line == 0 || b.line(c.p.line - 1).is_empty() || b.line(c.p.line - 1).trim_end().ends_with(['.', '!', '?', '。']);
            }
            let before = &line[..c.p.col];
            let trimmed = before.trim_end_matches([' ', '\t']);
            if before.ends_with('。') {
                return true;
            }
            trimmed.len() < before.len() && trimmed.trim_end_matches([')', ']', '"', '\'']).ends_with(['.', '!', '?'])
                || trimmed.is_empty() && (c.p.line == 0 || b.line(c.p.line - 1).is_empty())
        };
        for _ in 0..n {
            loop {
                let r = if forward { c.inc() } else { c.dec() };
                if r == -1 {
                    break;
                }
                if is_end(&c) {
                    break;
                }
            }
        }
        let mut r = self.mres(c.p, false);
        r.jump = true;
        Some(r)
    }

    pub fn get_mark(&mut self, ch: char) -> Option<Pos> {
        match ch {
            '<' | '>' => {
                let (_, s, e) = self.last_visual?;
                let (a, b) = if s <= e { (s, e) } else { (e, s) };
                Some(if ch == '<' { a } else { b })
            }
            '`' | '\'' => self.buf().marks.get(&'\'').copied().or(Some(Pos::default())),
            '.' | '[' | ']' | '^' | '"' => self.buf().marks.get(&ch).copied(),
            c if c.is_ascii_uppercase() => {
                let (buf, pos) = self.global_marks_get(c)?;
                if buf != self.cur_buf_id() {
                    self.set_cur_buf(buf);
                }
                Some(pos)
            }
            c => {
                let r = self.buf().marks.get(&c).copied();
                if r.is_none() {
                    self.emsg("E20: Mark not set");
                }
                r
            }
        }
    }

    fn global_marks_get(&self, c: char) -> Option<(usize, Pos)> {
        for b in self.buffers.values() {
            if let Some(p) = b.marks.get(&c) {
                return Some((b.id, *p));
            }
        }
        None
    }

    pub fn set_mark(&mut self, ch: char, pos: Pos) {
        if ch.is_ascii_uppercase() {
            for b in self.buffers.values_mut() {
                b.marks.remove(&ch);
            }
        }
        self.buf_mut().marks.insert(ch, pos);
    }

    /// Byte range of the keyword (or non-blank run) under/after the cursor.
    pub fn word_under_cursor(&self) -> Option<(usize, usize)> {
        let line = self.cur_line();
        let cur = self.cursor();
        let mut i = cur.col.min(line.len());
        // find a keyword character at or after the cursor
        let mut start = None;
        let mut j = i;
        while j < line.len() {
            let c = char_at(line, j)?;
            if char_class(c, false) >= 2 {
                start = Some(j);
                break;
            }
            j += c.len_utf8();
        }
        let (cls_min, mut s) = match start {
            Some(s) => (2, s),
            None => {
                // fall back to a non-blank run
                while i < line.len() && char_class(char_at(line, i)?, false) == 0 {
                    i = next_char_end(line, i);
                }
                if i >= line.len() {
                    return None;
                }
                (1, i)
            }
        };
        let cls = char_class(char_at(line, s)?, false);
        let _ = cls_min;
        while s > 0 {
            let p = prev_char_start(line, s);
            if char_class(char_at(line, p)?, false) != cls {
                break;
            }
            s = p;
        }
        let mut e = s;
        while e < line.len() && char_class(char_at(line, e)?, false) == cls {
            e = next_char_end(line, e);
        }
        Some((s, e))
    }

    pub fn match_pair(&self, cur: Pos) -> Option<Pos> {
        let line = self.line(cur.line);
        let mps = self.opt_str("matchpairs");
        let pairs: Vec<(char, char)> = mps
            .split(',')
            .filter_map(|p| {
                let mut it = p.split(':');
                Some((it.next()?.chars().next()?, it.next()?.chars().next()?))
            })
            .collect();
        let mut i = cur.col;
        while i < line.len() {
            let c = char_at(line, i)?;
            for (o, cl) in &pairs {
                if c == *o {
                    return find_match(self.buf(), Pos::new(cur.line, i), *o, *cl, true);
                }
                if c == *cl {
                    return find_match(self.buf(), Pos::new(cur.line, i), *o, *cl, false);
                }
            }
            i += c.len_utf8();
        }
        None
    }

    // ---------------------------------------------------------------- search

    /// Search for `pat` starting after (or before) `from`.
    pub fn search(&mut self, pat: &str, forward: bool, from: Pos, count: usize, report: bool) -> Option<Pos> {
        let re = match self.compile_pattern(pat) {
            Ok(r) => r,
            Err(e) => {
                if report {
                    self.emsg(e);
                }
                return None;
            }
        };
        let wrapscan = self.opt_global("wrapscan").as_bool();
        let b = self.buf();
        let nlines = b.line_count();
        let mut pos = from;
        let mut wrapped = false;
        for _ in 0..count.max(1) {
            let mut found = None;
            if forward {
                // rest of the current line
                let line = b.line(pos.line);
                let start = next_char_end(line, pos.col);
                if start <= line.len() && !line.is_empty() {
                    if let Some(m) = re.find_at(line, start) {
                        found = Some(Pos::new(pos.line, m.start));
                    }
                }
                if found.is_none() {
                    for k in 1..=nlines {
                        let l = pos.line + k;
                        let l = if l >= nlines {
                            if !wrapscan {
                                break;
                            }
                            wrapped = true;
                            l - nlines
                        } else {
                            l
                        };
                        let line = b.line(l);
                        if let Some(m) = re.find_at(line, 0) {
                            if l == pos.line && m.start > pos.col {
                                continue;
                            }
                            found = Some(Pos::new(l, m.start));
                            break;
                        }
                    }
                }
            } else {
                let line = b.line(pos.line);
                if let Some(m) = re.find_all(line).into_iter().filter(|m| m.start < pos.col).next_back() {
                    found = Some(Pos::new(pos.line, m.start));
                }
                if found.is_none() {
                    for k in 1..=nlines {
                        let l = if pos.line >= k {
                            pos.line - k
                        } else {
                            if !wrapscan {
                                break;
                            }
                            wrapped = true;
                            pos.line + nlines - k
                        };
                        let line = b.line(l);
                        let all = re.find_all(line);
                        let pick = if l == pos.line { all.into_iter().filter(|m| m.start > pos.col).next_back() } else { all.into_iter().next_back() };
                        if let Some(m) = pick {
                            found = Some(Pos::new(l, m.start));
                            break;
                        }
                    }
                }
            }
            match found {
                Some(p) => pos = p,
                None => {
                    if report {
                        if wrapscan {
                            self.emsg(format!("E486: Pattern not found: {}", pat));
                        } else if forward {
                            self.emsg(format!("E385: Search hit BOTTOM without match for: {}", pat));
                        } else {
                            self.emsg(format!("E384: Search hit TOP without match for: {}", pat));
                        }
                    }
                    return None;
                }
            }
        }
        if report && wrapped {
            self.msg_hl(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" }, "WarningMsg");
        }
        let line = self.line(pos.line);
        if pos.col >= line.len() && !line.is_empty() {
            pos.col = last_char_start(line);
        }
        Some(pos)
    }

    // ---------------------------------------------------------- text objects

    /// Returns (start, inclusive end, linewise).
    pub fn text_object(&mut self, around: bool, ch: char, count: usize) -> Option<(Pos, Pos, bool)> {
        let cur = self.cursor();
        let line = self.cur_line().to_string();
        match ch {
            'w' | 'W' => {
                let big = ch == 'W';
                if line.is_empty() {
                    return Some((cur, cur, false));
                }
                let col = cur.col.min(last_char_start(&line));
                let class_at = |i: usize| char_at(&line, i).map(|c| char_class(c, big));
                let mut s = col;
                let cls = class_at(col)?;
                while s > 0 && class_at(prev_char_start(&line, s)) == Some(cls) {
                    s = prev_char_start(&line, s);
                }
                let mut e = col;
                let run_end = |mut e: usize, cls: u8| {
                    while next_char_end(&line, e) < line.len() && class_at(next_char_end(&line, e)) == Some(cls) {
                        e = next_char_end(&line, e);
                    }
                    e
                };
                e = run_end(e, cls);
                let mut remaining = if around { count * 2 - 1 } else { count - 1 };
                let mut took_trailing = false;
                while remaining > 0 {
                    let n = next_char_end(&line, e);
                    if n >= line.len() {
                        break;
                    }
                    let c2 = class_at(n)?;
                    e = run_end(n, c2);
                    took_trailing = true;
                    remaining -= 1;
                }
                if around && !took_trailing && cls != 0 {
                    // no trailing white space: include leading white space instead
                    while s > 0 && class_at(prev_char_start(&line, s)) == Some(0) {
                        s = prev_char_start(&line, s);
                    }
                }
                Some((Pos::new(cur.line, s), Pos::new(cur.line, e), false))
            }
            'p' => {
                let last = self.buf().last_line();
                let blank = |l: usize| self.line(l).trim().is_empty();
                let kind = blank(cur.line);
                let mut s = cur.line;
                while s > 0 && blank(s - 1) == kind {
                    s -= 1;
                }
                let mut e = cur.line;
                while e < last && blank(e + 1) == kind {
                    e += 1;
                }
                let mut n = if around { count * 2 - 1 } else { count - 1 };
                while n > 0 && e < last {
                    let k = blank(e + 1);
                    e += 1;
                    while e < last && blank(e + 1) == k {
                        e += 1;
                    }
                    n -= 1;
                }
                Some((Pos::new(s, 0), Pos::new(e, 0), true))
            }
            '(' | ')' | 'b' | '{' | '}' | 'B' | '[' | ']' | '<' | '>' => {
                let (open, close) = match ch {
                    '(' | ')' | 'b' => ('(', ')'),
                    '{' | '}' | 'B' => ('{', '}'),
                    '[' | ']' => ('[', ']'),
                    _ => ('<', '>'),
                };
                let b = self.buf();
                let mut start = if char_at(&line, cur.col) == Some(open) {
                    cur
                } else if char_at(&line, cur.col) == Some(close) {
                    find_match(b, cur, open, close, false)?
                } else {
                    find_unmatched(b, cur, open, close, false)?
                };
                for _ in 1..count {
                    start = find_unmatched(b, start, open, close, false)?;
                }
                let end = find_match(b, start, open, close, true)?;
                if around {
                    return Some((start, end, false));
                }
                let mut s = Pos::new(start.line, start.col + open.len_utf8());
                let mut linewise = false;
                if s.col >= b.line(s.line).len() && s.line < end.line {
                    s = Pos::new(s.line + 1, 0);
                    linewise = true;
                }
                let mut e = end;
                if b.line(end.line)[..end.col].trim().is_empty() && end.line > s.line {
                    // closing bracket alone on its line: stop at the end of the previous line
                    e = Pos::new(end.line - 1, b.line(end.line - 1).len());
                    if linewise || s.col == 0 {
                        return Some((Pos::new(s.line, 0), Pos::new(e.line, 0), true));
                    }
                    e.col = last_char_start(b.line(e.line));
                    return Some((s, e, false));
                }
                if s == end {
                    return Some((s, s, false)).filter(|_| false).or(Some((s, Pos::new(s.line, usize::MAX), false)));
                }
                e = if e.col == 0 { Pos::new(e.line - 1, b.line(e.line - 1).len()) } else { Pos::new(e.line, prev_char_start(b.line(e.line), e.col)) };
                Some((s, e, false))
            }
            '"' | '\'' | '`' => {
                let quotes: Vec<usize> = {
                    let mut v = Vec::new();
                    let mut esc = false;
                    for (i, c) in line.char_indices() {
                        if esc {
                            esc = false;
                            continue;
                        }
                        if c == '\\' {
                            esc = true;
                        } else if c == ch {
                            v.push(i);
                        }
                    }
                    v
                };
                let mut pair = None;
                for p in quotes.chunks(2) {
                    if p.len() == 2 && cur.col <= p[1] {
                        pair = Some((p[0], p[1]));
                        break;
                    }
                }
                let (s, e) = pair?;
                if around {
                    let mut e2 = e;
                    let mut s2 = s;
                    let after = &line[e + 1..];
                    let ws = after.len() - after.trim_start_matches([' ', '\t']).len();
                    if ws > 0 {
                        e2 = e + ws;
                    } else {
                        let before = &line[..s];
                        s2 = before.trim_end_matches([' ', '\t']).len();
                    }
                    Some((Pos::new(cur.line, s2), Pos::new(cur.line, e2), false))
                } else if e == s + 1 {
                    Some((Pos::new(cur.line, e), Pos::new(cur.line, usize::MAX), false))
                } else {
                    Some((Pos::new(cur.line, s + 1), Pos::new(cur.line, prev_char_start(&line, e)), false))
                }
            }
            's' => {
                let s = self.sentence_motion(false, 1).map(|r| r.pos).unwrap_or(cur);
                let saved = self.cursor();
                self.win_mut().cursor = s;
                let e = self.sentence_motion(true, count).map(|r| r.pos).unwrap_or(saved);
                self.win_mut().cursor = saved;
                let mut c = Cur { b: self.buf(), p: e };
                c.dec();
                if !around {
                    while c.cls(true) == 0 && c.p > s {
                        if c.dec() == -1 {
                            break;
                        }
                    }
                }
                Some((s, c.p, false))
            }
            't' => self.tag_object(around, count),
            _ => None,
        }
    }

    fn tag_object(&self, around: bool, count: usize) -> Option<(Pos, Pos, bool)> {
        // Work on a flattened copy of the buffer around the cursor.
        let b = self.buf();
        let cur = self.cursor();
        let mut offsets = Vec::with_capacity(b.line_count());
        let mut text = String::new();
        for l in &b.lines {
            offsets.push(text.len());
            text.push_str(l);
            text.push('\n');
        }
        let cur_off = offsets[cur.line] + cur.col;
        let re = regex::Regex::new(r"<(/?)([A-Za-z][\w:.-]*)[^<>]*?(/?)>").ok()?;
        let mut stack: Vec<(String, usize, usize)> = Vec::new();
        let mut found: Vec<(usize, usize, usize, usize)> = Vec::new();
        for m in re.captures_iter(&text) {
            let whole = m.get(0).unwrap();
            if &m[3] == "/" {
                continue;
            }
            let name = m[2].to_string();
            if &m[1] == "/" {
                if let Some(i) = stack.iter().rposition(|(n, _, _)| *n == name) {
                    let (_, os, oe) = stack[i].clone();
                    stack.truncate(i);
                    if os <= cur_off && cur_off < whole.end() {
                        found.push((os, oe, whole.start(), whole.end()));
                    }
                }
            } else {
                stack.push((name, whole.start(), whole.end()));
            }
        }
        found.sort_by_key(|f| f.3 - f.0);
        let (os, oe, cs, ce) = *found.get(count - 1)?;
        let to_pos = |off: usize| -> Pos {
            let line = match offsets.binary_search(&off) {
                Ok(i) => i,
                Err(i) => i - 1,
            };
            Pos::new(line, off - offsets[line])
        };
        if around {
            Some((to_pos(os), to_pos(ce - 1), false))
        } else if cs == oe {
            Some((to_pos(oe), Pos::new(to_pos(oe).line, usize::MAX), false))
        } else {
            Some((to_pos(oe), to_pos(cs - 1), false))
        }
    }

    /// Normalised visual selection: (start, end inclusive, kind).
    pub fn visual_range(&self) -> Option<(Pos, Pos, VisualKind)> {
        let super::Mode::Visual(kind) = self.mode else {
            return None;
        };
        let a = self.visual_start;
        let b = self.cursor();
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        Some((s, e, kind))
    }
}

pub fn escape_pattern(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if matches!(c, '\\' | '/' | '.' | '*' | '$' | '^' | '~' | '[' | ']') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Find the bracket matching the one at `pos`.
pub fn find_match(b: &Buffer, pos: Pos, open: char, close: char, forward: bool) -> Option<Pos> {
    let mut depth = 0i32;
    let mut c = Cur { b, p: pos };
    let mut steps = 0usize;
    loop {
        if let Some(ch) = char_at(c.line(), c.p.col) {
            if ch == open {
                depth += if forward { 1 } else { -1 };
            } else if ch == close {
                depth += if forward { -1 } else { 1 };
            }
            if depth == 0 {
                return Some(c.p);
            }
        }
        let r = if forward { c.inc() } else { c.dec() };
        steps += 1;
        if r == -1 || steps > 5_000_000 {
            return None;
        }
    }
}

/// Find the enclosing unmatched bracket (`[(`, `])` ...).
pub fn find_unmatched(b: &Buffer, pos: Pos, open: char, close: char, forward: bool) -> Option<Pos> {
    let mut depth = 0i32;
    let mut c = Cur { b, p: pos };
    let mut steps = 0usize;
    loop {
        let r = if forward { c.inc() } else { c.dec() };
        steps += 1;
        if r == -1 || steps > 5_000_000 {
            return None;
        }
        if let Some(ch) = char_at(c.line(), c.p.col) {
            if forward {
                if ch == open {
                    depth += 1;
                } else if ch == close {
                    if depth == 0 {
                        return Some(c.p);
                    }
                    depth -= 1;
                }
            } else if ch == close {
                depth += 1;
            } else if ch == open {
                if depth == 0 {
                    return Some(c.p);
                }
                depth -= 1;
            }
        }
    }
}
