//! Text buffers: line storage, undo history and file I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::options::LocalOpts;
use super::syntax::LineState;

/// A position in a buffer: 0-based line and byte column.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, PartialOrd, Ord, Hash)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Pos {
        Pos { line, col }
    }
}

#[derive(Clone, Debug)]
struct Change {
    line: usize,
    old: Vec<String>,
    new: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct UndoEntry {
    changes: Vec<Change>,
    cursor_before: Pos,
    cursor_after: Pos,
    modified_before: bool,
}

pub struct Buffer {
    pub id: usize,
    pub lines: Vec<String>,
    pub path: Option<PathBuf>,
    pub modified: bool,
    pub changedtick: u64,
    pub opts: LocalOpts,
    pub marks: HashMap<char, Pos>,
    pub listed: bool,
    pub loaded: bool,
    /// Cursor position when the buffer was last shown in a window.
    pub last_cursor: Pos,
    /// Syntax state at the start of each line (lazily extended).
    pub syn_states: Vec<LineState>,
    pub mtime: Option<std::time::SystemTime>,
    /// Cursor to record as the "before" position of the next undo entry.
    pub undo_cursor: Pos,
    undo: Vec<UndoEntry>,
    redo: Vec<UndoEntry>,
    group: Option<UndoEntry>,
    /// Lowest line changed since the last redraw bookkeeping (for caches).
    pub scratch: bool,
    pub bomb: bool,
}

pub struct ReadResult {
    pub lines: Vec<String>,
    pub fileformat: &'static str,
    pub encoding: String,
    pub bomb: bool,
    pub eol: bool,
}

impl Buffer {
    pub fn new(id: usize) -> Buffer {
        Buffer {
            id,
            lines: vec![String::new()],
            path: None,
            modified: false,
            changedtick: 0,
            opts: LocalOpts::new(),
            marks: HashMap::new(),
            listed: true,
            loaded: true,
            last_cursor: Pos::default(),
            syn_states: Vec::new(),
            mtime: None,
            undo_cursor: Pos::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            scratch: false,
            bomb: false,
        }
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, l: usize) -> &str {
        self.lines.get(l).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn last_line(&self) -> usize {
        self.lines.len() - 1
    }

    pub fn display_name(&self) -> String {
        match &self.path {
            Some(p) => p.to_string_lossy().replace('\\', "/"),
            None => "[No Name]".to_string(),
        }
    }

    /// Clamp a position to valid text (column may equal the line length).
    pub fn clamp(&self, p: Pos) -> Pos {
        let line = p.line.min(self.last_line());
        let s = self.line(line);
        let mut col = p.col.min(s.len());
        while col > 0 && !s.is_char_boundary(col) {
            col -= 1;
        }
        Pos { line, col }
    }

    // ---------------------------------------------------------------- edits

    /// Replace lines `start..end` with `new`.  This is the only primitive that
    /// modifies text; everything else is built on it so undo stays simple.
    pub fn splice(&mut self, start: usize, end: usize, new: Vec<String>) {
        let start = start.min(self.lines.len());
        let end = end.clamp(start, self.lines.len());
        let old: Vec<String> = self.lines.splice(start..end, new.iter().cloned()).collect();
        let mut emptied = false;
        if self.lines.is_empty() {
            self.lines.push(String::new());
            emptied = true;
        }
        let delta = new.len() as isize - old.len() as isize;
        if delta != 0 {
            for p in self.marks.values_mut() {
                if p.line >= end {
                    p.line = (p.line as isize + delta).max(0) as usize;
                } else if p.line >= start + new.len() {
                    p.line = (start + new.len()).saturating_sub(1);
                }
            }
        }
        let group = self.group.get_or_insert_with(|| UndoEntry {
            changes: Vec::new(),
            cursor_before: self.undo_cursor,
            cursor_after: self.undo_cursor,
            modified_before: self.modified,
        });
        let mut new_rec = new;
        if emptied {
            new_rec.push(String::new());
        }
        group.changes.push(Change { line: start, old, new: new_rec });
        self.redo.clear();
        self.modified = true;
        self.changedtick += 1;
        self.syn_states.truncate(start);
    }

    /// Close the current undo group (called at the end of every command).
    pub fn close_undo(&mut self, cursor: Pos) {
        if let Some(mut g) = self.group.take() {
            g.cursor_after = cursor;
            if !g.changes.is_empty() {
                self.undo.push(g);
                let max = 5000;
                if self.undo.len() > max {
                    let n = self.undo.len() - max;
                    self.undo.drain(0..n);
                }
            }
        }
    }

    pub fn has_open_undo(&self) -> bool {
        self.group.is_some()
    }

    pub fn undo(&mut self) -> Option<Pos> {
        self.close_undo(self.undo_cursor);
        let e = self.undo.pop()?;
        for c in e.changes.iter().rev() {
            let end = c.line + c.new.len();
            let end = end.min(self.lines.len());
            self.lines.splice(c.line..end, c.old.iter().cloned());
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
            self.syn_states.truncate(c.line);
        }
        let pos = e.cursor_before;
        self.modified = true;
        if !e.modified_before && self.undo.is_empty() {
            self.modified = false;
        }
        self.changedtick += 1;
        self.redo.push(e);
        Some(self.clamp(pos))
    }

    pub fn redo(&mut self) -> Option<Pos> {
        self.close_undo(self.undo_cursor);
        let e = self.redo.pop()?;
        let mut first = usize::MAX;
        for c in e.changes.iter() {
            let end = (c.line + c.old.len()).min(self.lines.len());
            // The recorded "new" may include the placeholder empty line.
            self.lines.splice(c.line..end, c.new.iter().cloned());
            if self.lines.is_empty() {
                self.lines.push(String::new());
            }
            first = first.min(c.line);
            self.syn_states.truncate(c.line);
        }
        self.modified = true;
        self.changedtick += 1;
        let pos = Pos::new(first.min(self.last_line()), e.cursor_before.col);
        self.undo.push(e);
        Some(self.clamp(pos))
    }

    pub fn clear_undo(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.group = None;
    }

    pub fn set_line(&mut self, l: usize, text: String) {
        if l < self.lines.len() {
            if self.lines[l] != text {
                self.splice(l, l + 1, vec![text]);
            }
        }
    }

    /// Insert text (may contain newlines) at `pos`; returns the end position.
    pub fn insert_text(&mut self, pos: Pos, text: &str) -> Pos {
        let pos = self.clamp(pos);
        if text.is_empty() {
            return pos;
        }
        let cur = self.line(pos.line).to_string();
        let (head, tail) = cur.split_at(pos.col);
        let mut parts: Vec<&str> = text.split('\n').collect();
        let last = parts.pop().unwrap();
        if parts.is_empty() {
            let new = format!("{}{}{}", head, last, tail);
            self.splice(pos.line, pos.line + 1, vec![new]);
            return Pos::new(pos.line, pos.col + last.len());
        }
        let mut new = Vec::with_capacity(parts.len() + 1);
        new.push(format!("{}{}", head, parts[0]));
        for p in &parts[1..] {
            new.push(p.to_string());
        }
        new.push(format!("{}{}", last, tail));
        let n = new.len();
        self.splice(pos.line, pos.line + 1, new);
        Pos::new(pos.line + n - 1, last.len())
    }

    /// Text between two positions (end exclusive).
    pub fn get_text(&self, start: Pos, end: Pos) -> String {
        let start = self.clamp(start);
        let end = self.clamp(end);
        if end <= start {
            return String::new();
        }
        if start.line == end.line {
            return self.line(start.line)[start.col..end.col].to_string();
        }
        let mut out = String::new();
        out.push_str(&self.line(start.line)[start.col..]);
        for l in start.line + 1..end.line {
            out.push('\n');
            out.push_str(self.line(l));
        }
        out.push('\n');
        out.push_str(&self.line(end.line)[..end.col]);
        out
    }

    /// Delete the text between two positions (end exclusive); returns it.
    pub fn delete_range(&mut self, start: Pos, end: Pos) -> String {
        let start = self.clamp(start);
        let end = self.clamp(end);
        if end <= start {
            return String::new();
        }
        let text = self.get_text(start, end);
        let new = format!("{}{}", &self.line(start.line)[..start.col], &self.line(end.line)[end.col..]);
        self.splice(start.line, end.line + 1, vec![new]);
        text
    }

    pub fn delete_lines(&mut self, start: usize, end: usize) -> Vec<String> {
        let end = end.min(self.lines.len());
        let old = self.lines[start..end].to_vec();
        self.splice(start, end, Vec::new());
        old
    }

    pub fn insert_lines(&mut self, at: usize, lines: Vec<String>) {
        // Inserting into the placeholder of an empty unnamed buffer keeps it.
        self.splice(at, at, lines);
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    // ------------------------------------------------------------- file I/O

    pub fn read_file(path: &Path, fencs: &str) -> std::io::Result<ReadResult> {
        let bytes = std::fs::read(path)?;
        Ok(decode_bytes(&bytes, fencs))
    }

    pub fn encode(lines: &[String], ff: &str, fenc: &str, bomb: bool, eol: bool) -> Vec<u8> {
        let nl = match ff {
            "dos" => "\r\n",
            "mac" => "\r",
            _ => "\n",
        };
        let mut text = lines.join(nl);
        if eol {
            text.push_str(nl);
        }
        let label = match fenc.to_ascii_lowercase().as_str() {
            "" | "utf-8" | "utf8" => "utf-8".to_string(),
            "cp932" | "sjis" => "shift_jis".to_string(),
            "latin1" => "windows-1252".to_string(),
            "ucs-2le" | "utf-16" | "ucs-2" => "utf-16le".to_string(),
            other => other.to_string(),
        };
        let mut out = Vec::new();
        if label == "utf-16le" || label == "utf-16be" {
            let be = label == "utf-16be";
            if bomb {
                out.extend_from_slice(if be { &[0xFE, 0xFF] } else { &[0xFF, 0xFE] });
            }
            for u in text.encode_utf16() {
                out.extend_from_slice(&if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            return out;
        }
        if label == "utf-8" {
            if bomb {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(text.as_bytes());
            return out;
        }
        match encoding_rs::Encoding::for_label(label.as_bytes()) {
            Some(enc) => enc.encode(&text).0.into_owned(),
            None => text.into_bytes(),
        }
    }
}

pub fn decode_bytes(bytes: &[u8], fencs: &str) -> ReadResult {
    let (text, encoding, bomb) = decode_text(bytes, fencs);
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    let cr = text.matches('\r').count();
    let fileformat = if lf > 0 && crlf == lf {
        "dos"
    } else if lf == 0 && cr > 0 {
        "mac"
    } else {
        "unix"
    };
    let mut lines: Vec<String> = match fileformat {
        "dos" => text.split("\r\n").map(|s| s.to_string()).collect(),
        "mac" => text.split('\r').map(|s| s.to_string()).collect(),
        _ => text.split('\n').map(|s| s.to_string()).collect(),
    };
    let mut eol = false;
    if lines.len() > 1 && lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
        eol = true;
    }
    if text.is_empty() {
        eol = true;
    }
    ReadResult { lines, fileformat, encoding, bomb, eol }
}

fn decode_text(bytes: &[u8], fencs: &str) -> (String, String, bool) {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&bytes[3..]).into_owned(), "utf-8".into(), true);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        let be = bytes[0] == 0xFE;
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| if be { u16::from_be_bytes([c[0], c[1]]) } else { u16::from_le_bytes([c[0], c[1]]) })
            .collect();
        return (String::from_utf16_lossy(&units), if be { "utf-16".into() } else { "utf-16le".into() }, true);
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return (s.to_string(), "utf-8".into(), false);
    }
    for name in fencs.split(',') {
        let label = match name.trim().to_ascii_lowercase().as_str() {
            "" | "ucs-bom" | "utf-8" | "utf8" | "default" => continue,
            "cp932" | "sjis" => "shift_jis",
            "euc-jp" | "eucjp" => "euc-jp",
            "iso-2022-jp" => "iso-2022-jp",
            "latin1" => {
                return (bytes.iter().map(|&b| b as char).collect(), "latin1".into(), false);
            }
            _ => name.trim(),
        }
        .to_string();
        if let Some(enc) = encoding_rs::Encoding::for_label(label.as_bytes()) {
            let (s, _, had_errors) = enc.decode(bytes);
            if !had_errors {
                return (s.into_owned(), name.trim().to_string(), false);
            }
        }
    }
    (String::from_utf8_lossy(bytes).into_owned(), "utf-8".into(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_delete_undo() {
        let mut b = Buffer::new(1);
        let end = b.insert_text(Pos::new(0, 0), "hello\nworld");
        assert_eq!(end, Pos::new(1, 5));
        b.close_undo(end);
        assert_eq!(b.lines, vec!["hello", "world"]);
        let t = b.delete_range(Pos::new(0, 3), Pos::new(1, 2));
        assert_eq!(t, "lo\nwo");
        b.close_undo(Pos::new(0, 3));
        assert_eq!(b.lines, vec!["helrld"]);
        b.undo();
        assert_eq!(b.lines, vec!["hello", "world"]);
        b.undo();
        assert_eq!(b.lines, vec![""]);
        b.redo();
        b.redo();
        assert_eq!(b.lines, vec!["helrld"]);
    }

    #[test]
    fn decode_formats() {
        let r = decode_bytes(b"a\r\nb\r\n", "");
        assert_eq!(r.fileformat, "dos");
        assert_eq!(r.lines, vec!["a", "b"]);
        let sjis = [0x82u8, 0xA0, b'\n'];
        let r = decode_bytes(&sjis, "ucs-bom,utf-8,cp932");
        assert_eq!(r.lines, vec!["あ"]);
        assert_eq!(r.encoding, "cp932");
    }
}
