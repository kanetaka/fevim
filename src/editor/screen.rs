//! Builds the cell grid that the front end paints.

use super::highlight::{Attr, Color};
use super::motion::{char_width, vcol_of};
use super::syntax::{self, LineState};
use super::{Editor, Mode, VisualKind, WinId};

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Cell {
    pub ch: char,
    pub attr: Attr,
    /// 1 or 2 for a character, 0 for the right half of a wide character.
    pub width: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Block,
    Vertical(u8),
    Horizontal(u8),
}

pub struct Grid {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<Cell>,
    pub cursor: (usize, usize),
    pub cursor_shape: CursorShape,
    pub cursor_attr: Attr,
    pub default_fg: Color,
    pub default_bg: Color,
    pub title: String,
}

impl Grid {
    fn new(rows: usize, cols: usize, attr: Attr) -> Grid {
        Grid {
            rows,
            cols,
            cells: vec![Cell { ch: ' ', attr, width: 1 }; rows * cols],
            cursor: (0, 0),
            cursor_shape: CursorShape::Block,
            cursor_attr: attr,
            default_fg: attr.fg.unwrap_or(Color(0xffffff)),
            default_bg: attr.bg.unwrap_or(Color(0)),
            title: String::new(),
        }
    }

    pub fn cell(&self, r: usize, c: usize) -> &Cell {
        &self.cells[r * self.cols + c]
    }

    fn set(&mut self, r: usize, c: usize, cell: Cell) {
        if r < self.rows && c < self.cols {
            // overwriting half of a wide character blanks the other half
            let idx = r * self.cols + c;
            if self.cells[idx].width == 0 && c > 0 {
                self.cells[idx - 1].ch = ' ';
                self.cells[idx - 1].width = 1;
            }
            if self.cells[idx].width == 2 && c + 1 < self.cols {
                self.cells[idx + 1] = Cell { ch: ' ', attr: self.cells[idx].attr, width: 1 };
            }
            self.cells[idx] = cell;
        }
    }

    /// Put a string; returns the column after it.
    fn put(&mut self, r: usize, mut c: usize, s: &str, attr: Attr, limit: usize) -> usize {
        for ch in s.chars() {
            let w = char_width(ch, false).max(1);
            if c + w > limit {
                break;
            }
            let ch = if (ch as u32) < 0x20 { '?' } else { ch };
            self.set(r, c, Cell { ch, attr, width: w as u8 });
            if w == 2 {
                self.set(r, c + 1, Cell { ch: ' ', attr, width: 0 });
            }
            c += w;
        }
        c
    }

    fn fill(&mut self, r: usize, from: usize, to: usize, attr: Attr) {
        for c in from..to.min(self.cols) {
            self.set(r, c, Cell { ch: ' ', attr, width: 1 });
        }
    }

    /// Text of one row (for tests and debugging).
    pub fn row_text(&self, r: usize) -> String {
        (0..self.cols).map(|c| self.cell(r, c)).filter(|c| c.width != 0).map(|c| c.ch).collect()
    }
}

struct ListChars {
    eol: Option<char>,
    tab: Option<(char, char, Option<char>)>,
    trail: Option<char>,
    space: Option<char>,
    nbsp: Option<char>,
    extends: Option<char>,
    precedes: Option<char>,
}

fn parse_listchars(s: &str) -> ListChars {
    let mut l = ListChars { eol: None, tab: None, trail: None, space: None, nbsp: None, extends: None, precedes: None };
    for item in s.split(',') {
        let Some((k, v)) = item.split_once(':') else {
            continue;
        };
        let mut cs = v.chars();
        match k {
            "eol" => l.eol = cs.next(),
            "tab" => {
                let a = cs.next();
                let b = cs.next();
                if let (Some(a), Some(b)) = (a, b) {
                    l.tab = Some((a, b, cs.next()));
                }
            }
            "trail" => l.trail = cs.next(),
            "space" => l.space = cs.next(),
            "nbsp" => l.nbsp = cs.next(),
            "extends" => l.extends = cs.next(),
            "precedes" => l.precedes = cs.next(),
            _ => {}
        }
    }
    l
}

impl Editor {
    fn resolve(&self, a: Attr) -> Attr {
        let n = self.hl.attr("Normal");
        let mut r = Attr { fg: a.fg.or(n.fg), bg: a.bg.or(n.bg), ..a };
        if r.fg.is_none() {
            r.fg = Some(Color(0xd0d0d0));
        }
        if r.bg.is_none() {
            r.bg = Some(Color(0));
        }
        if r.reverse {
            std::mem::swap(&mut r.fg, &mut r.bg);
            r.reverse = false;
        }
        r
    }

    fn group(&self, name: &str) -> Attr {
        self.resolve(self.hl.attr(name))
    }

    /// Syntax state at the start of `lnum`, computing and caching as needed.
    fn syn_state(&mut self, buf: usize, lang: &'static syntax::Lang, ft: &str, lnum: usize) -> LineState {
        let b = self.buffers.get_mut(&buf).unwrap();
        if b.syn_states.is_empty() {
            b.syn_states.push(LineState::Normal);
        }
        // limit how far back we scan on huge files
        if lnum >= b.syn_states.len() + 20_000 {
            return LineState::Normal;
        }
        while b.syn_states.len() <= lnum {
            let l = b.syn_states.len() - 1;
            let st = b.syn_states[l];
            let (_, next) = syntax::highlight_line(lang, ft, &b.lines[l.min(b.lines.len() - 1)], st);
            b.syn_states.push(next);
        }
        b.syn_states[lnum]
    }

    /// Produce the full screen.
    pub fn render(&mut self) -> Grid {
        let normal = self.group("Normal");
        let mut g = Grid::new(self.rows, self.cols, normal);
        g.default_fg = normal.fg.unwrap();
        g.default_bg = normal.bg.unwrap();
        let seps = self.layout();
        // tab line
        if self.tabline_height() > 0 {
            self.draw_tabline(&mut g);
        }
        let wins = self.tab_wins();
        let cur = self.cur_win_id();
        for id in &wins {
            self.draw_window(&mut g, *id, *id == cur);
        }
        let vsep = self.group("VertSplit");
        let fillc = self.opt_global("fillchars").as_string();
        let vch = fillc.split(',').find_map(|x| x.strip_prefix("vert:")).and_then(|v| v.chars().next()).unwrap_or('|');
        for (r, c, h) in seps {
            for row in r..r + h {
                g.set(row, c, Cell { ch: vch, attr: vsep, width: 1 });
            }
        }
        self.draw_cmdline_area(&mut g);
        self.draw_popup(&mut g);
        g.title = self.window_title();
        g
    }

    pub fn window_title(&mut self) -> String {
        let ts = self.opt_global("titlestring").as_string();
        if !ts.is_empty() {
            let cur = self.cur_win_id();
            return self.eval_statusline(&ts, cur, 200).text.trim_end().to_string();
        }
        let b = self.buf();
        let name = match &b.path {
            Some(p) => {
                let full = self.full_path(p);
                let file = full.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                let dir = full.parent().map(|d| d.to_string_lossy().replace('\\', "/")).unwrap_or_default();
                format!("{}{} ({})", file, if b.modified { " +" } else { "" }, dir)
            }
            None => format!("[No Name]{}", if b.modified { " +" } else { "" }),
        };
        format!("{} - FEVIM", name)
    }

    fn draw_tabline(&mut self, g: &mut Grid) {
        let sel = self.group("TabLineSel");
        let tl = self.group("TabLine");
        let fill = self.group("TabLineFill");
        g.fill(0, 0, self.cols, fill);
        let mut c = 0;
        for (i, t) in self.tabs.iter().enumerate() {
            let w = &self.windows[&t.cur];
            let b = &self.buffers[&w.buf];
            let name = b.path.as_ref().map(|p| p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()).unwrap_or_else(|| "[No Name]".into());
            let nwin = t.layout.leaves().len();
            let modified = t.layout.leaves().iter().any(|id| self.buffers[&self.windows[id].buf].modified);
            let mut label = String::from(" ");
            if nwin > 1 {
                label.push_str(&nwin.to_string());
            }
            if modified {
                label.push('+');
            }
            if nwin > 1 || modified {
                label.push(' ');
            }
            label.push_str(&name);
            label.push(' ');
            let attr = if i == self.cur_tab { sel } else { tl };
            c = g.put(0, c, &label, attr, self.cols);
            if c >= self.cols {
                break;
            }
        }
    }

    fn draw_window(&mut self, g: &mut Grid, id: WinId, is_cur: bool) {
        let (bid, row0, col0, width, height, topline, leftcol, cursor, has_status) = {
            let w = &self.windows[&id];
            (w.buf, w.row, w.col, w.width, w.height, w.topline, w.leftcol, w.cursor, w.has_status)
        };
        let w = &self.windows[&id];
        let b = &self.buffers[&bid];
        let opt = |name: &str| self.opt_for(Some(w), Some(b), name);
        let wrap = opt("wrap").as_bool();
        let list = opt("list").as_bool();
        let nu = opt("number").as_bool();
        let rnu = opt("relativenumber").as_bool();
        let cul = opt("cursorline").as_bool();
        let cuc = opt("cursorcolumn").as_bool();
        let ts = opt("tabstop").as_num().max(1) as usize;
        let ft = opt("filetype").as_string();
        let syn_opt = opt("syntax").as_string();
        let cc: Vec<usize> = opt("colorcolumn").as_string().split(',').filter_map(|x| x.trim().parse::<usize>().ok()).collect();
        let lcs = parse_listchars(&self.opt_global("listchars").as_string());
        let ambi = self.ambi_double();
        let numw = self.number_width(w);
        let textw = width.saturating_sub(numw).max(1);
        let nlines = b.line_count();
        let showbreak = self.opt_global("showbreak").as_string();
        let fillchars = self.opt_global("fillchars").as_string();
        let eob = fillchars.split(',').find_map(|x| x.strip_prefix("eob:")).and_then(|v| v.chars().next()).unwrap_or('~');

        let normal = self.group("Normal");
        let a_lnr = self.group("LineNr");
        let a_culnr = self.group("CursorLineNr");
        let a_nontext = self.hl.attr("NonText");
        let a_special = self.hl.attr("SpecialKey");
        let a_cul = self.hl.attr("CursorLine");
        let a_cuc = self.hl.attr("CursorColumn");
        let a_cc = self.hl.attr("ColorColumn");
        let a_visual = self.hl.attr("Visual");
        let a_search = self.hl.attr("Search");
        let a_incsearch = self.hl.attr("IncSearch");
        let a_eob = self.group("EndOfBuffer");

        // search pattern for 'hlsearch'
        let search_re = if self.opt_global("hlsearch").as_bool() && !self.hlsearch_off {
            self.last_search.clone().and_then(|p| self.compile_pattern(&p).ok())
        } else {
            None
        };
        let matches: Vec<(Attr, super::vregex::VimRegex, usize)> = {
            let ms = self.windows[&id].matches.clone();
            let mut v = Vec::new();
            for m in ms {
                if let Ok(re) = self.compile_pattern(&m.pattern) {
                    v.push((self.hl.attr(&m.group), re, m.id));
                }
            }
            v.sort_by_key(|x| std::cmp::Reverse(x.2));
            v
        };
        let lang = if self.syntax_on {
            let key = if syn_opt.is_empty() { ft.as_str() } else { syn_opt.as_str() };
            syntax::lang_for(key)
        } else {
            None
        };
        let visual = if is_cur { self.visual_range() } else { None };
        let visual_block = visual.filter(|v| v.2 == VisualKind::Block).map(|_| {
            let r = self.visual_op_range();
            (r.start.line, r.end.line, r.vcols.0, if r.to_eol { usize::MAX } else { r.vcols.1 })
        });
        let incs = if is_cur { self.incsearch_match } else { None };
        let cursor_vcol = vcol_of(self.buffers[&bid].line(cursor.line), cursor.col, ts, ambi);

        let mut row = 0;
        let mut lnum = topline;
        let mut cursor_screen: Option<(usize, usize)> = None;
        while row < height {
            if lnum >= nlines {
                let r = row0 + row;
                g.fill(r, col0, col0 + width, normal);
                g.put(r, col0, &eob.to_string(), a_eob, col0 + width);
                row += 1;
                continue;
            }
            let line = self.buffers[&bid].line(lnum).to_string();
            // per-character attributes
            let mut attrs: Vec<Attr> = vec![Attr::default(); line.len() + 1];
            if let Some(lang) = lang {
                let st = self.syn_state(bid, lang, if syn_opt.is_empty() { &ft } else { &syn_opt }, lnum);
                let (spans, _) = syntax::highlight_line(lang, &ft, &line, st);
                for sp in spans {
                    let a = self.hl.attr(sp.group);
                    for x in attrs.iter_mut().take(sp.end.min(line.len())).skip(sp.start) {
                        *x = x.combine(&a);
                    }
                }
            }
            for (a, re, _) in &matches {
                for m in re.find_all(&line) {
                    let end = if m.end == m.start { m.end + 1 } else { m.end };
                    for x in attrs.iter_mut().take(end.min(line.len() + 1)).skip(m.start) {
                        *x = x.combine(a);
                    }
                }
            }
            if let Some(re) = &search_re {
                for m in re.find_all(&line) {
                    if m.end > m.start {
                        for x in attrs.iter_mut().take(m.end).skip(m.start) {
                            *x = x.combine(&a_search);
                        }
                    }
                }
            }
            if let Some((p, len)) = incs {
                if p.line == lnum {
                    for x in attrs.iter_mut().take((p.col + len.max(1)).min(line.len() + 1)).skip(p.col) {
                        *x = x.combine(&a_incsearch);
                    }
                }
            }
            if let Some((s, e, kind)) = visual {
                if lnum >= s.line && lnum <= e.line && kind != VisualKind::Block {
                    let (from, to) = match kind {
                        VisualKind::Line => (0, line.len() + 1),
                        _ => {
                            let from = if lnum == s.line { s.col } else { 0 };
                            let to = if lnum == e.line { super::motion::next_char_end(&line, e.col).max(e.col + 1).min(line.len() + 1) } else { line.len() + 1 };
                            (from, to)
                        }
                    };
                    for x in attrs.iter_mut().take(to).skip(from) {
                        *x = x.combine(&a_visual);
                    }
                }
            }
            let trail_start = line.trim_end_matches([' ', '\t']).len();
            let is_cul = cul && lnum == cursor.line && is_cur;

            // lay out the line in display cells
            struct C {
                ch: char,
                attr: Attr,
                w: usize,
                byte: usize,
                nontext: bool,
            }
            let mut cells: Vec<C> = Vec::new();
            let mut vcol = 0usize;
            for (bi, ch) in line.char_indices() {
                let a = attrs[bi];
                if ch == '\t' {
                    let n = ts - vcol % ts;
                    if list {
                        if let Some((a1, a2, a3)) = lcs.tab {
                            for k in 0..n {
                                let c = if k == 0 { a1 } else if k == n - 1 && a3.is_some() { a3.unwrap() } else { a2 };
                                cells.push(C { ch: c, attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                            }
                        } else {
                            cells.push(C { ch: '^', attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                            cells.push(C { ch: 'I', attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                        }
                    } else {
                        for _ in 0..n {
                            cells.push(C { ch: ' ', attr: a, w: 1, byte: bi, nontext: false });
                        }
                    }
                    vcol += if list && lcs.tab.is_none() { 2 } else { n };
                    continue;
                }
                if (ch as u32) < 0x20 || ch == '\x7f' {
                    let c2 = if ch == '\x7f' { '?' } else { ((ch as u8) + 0x40) as char };
                    cells.push(C { ch: '^', attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                    cells.push(C { ch: c2, attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                    vcol += 2;
                    continue;
                }
                if list && ch == ' ' && bi >= trail_start && lcs.trail.is_some() {
                    cells.push(C { ch: lcs.trail.unwrap(), attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                    vcol += 1;
                    continue;
                }
                if list && ch == ' ' && lcs.space.is_some() {
                    cells.push(C { ch: lcs.space.unwrap(), attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                    vcol += 1;
                    continue;
                }
                if list && ch == '\u{a0}' && lcs.nbsp.is_some() {
                    cells.push(C { ch: lcs.nbsp.unwrap(), attr: a.combine(&a_special), w: 1, byte: bi, nontext: true });
                    vcol += 1;
                    continue;
                }
                let w = char_width(ch, ambi);
                if w == 0 {
                    continue; // combining characters are dropped
                }
                cells.push(C { ch, attr: a, w, byte: bi, nontext: false });
                vcol += w;
            }
            if list {
                if let Some(e) = lcs.eol {
                    cells.push(C { ch: e, attr: attrs[line.len()].combine(&a_nontext), w: 1, byte: line.len(), nontext: true });
                }
            }
            // the visual selection / incsearch of the newline position
            let eol_attr = attrs[line.len()];

            // block visual: recolour by display column
            if let Some((bs, be, vl, vr)) = visual_block {
                if lnum >= bs && lnum <= be {
                    let mut v = 0;
                    for c in cells.iter_mut() {
                        if v >= vl && (vr == usize::MAX || v <= vr) {
                            c.attr = c.attr.combine(&a_visual);
                        }
                        v += c.w;
                    }
                }
            }

            // draw rows of this line
            let mut ci = 0usize;
            let mut v = 0usize;
            // skip leftcol when not wrapping
            if !wrap {
                while ci < cells.len() && v + cells[ci].w <= leftcol {
                    v += cells[ci].w;
                    ci += 1;
                }
            }
            let mut first_row = true;
            loop {
                if row >= height {
                    break;
                }
                let r = row0 + row;
                // number column
                let base_attr = if is_cul { normal.combine(&self.resolve_partial(a_cul)) } else { normal };
                g.fill(r, col0, col0 + width, base_attr);
                if numw > 0 {
                    let text = if first_row {
                        let n = if rnu {
                            let d = lnum.abs_diff(cursor.line);
                            if d == 0 && nu { lnum + 1 } else { d }
                        } else {
                            lnum + 1
                        };
                        if rnu && lnum == cursor.line && !nu {
                            format!("{:<w$} ", 0, w = numw - 1)
                        } else if rnu && lnum == cursor.line {
                            format!("{:<w$} ", n, w = numw - 1)
                        } else {
                            format!("{:>w$} ", n, w = numw - 1)
                        }
                    } else {
                        " ".repeat(numw)
                    };
                    let na = if lnum == cursor.line && (cul || rnu) && is_cur { a_culnr } else { a_lnr };
                    g.put(r, col0, &text, na, col0 + numw);
                }
                let mut c = 0usize; // column within text area
                if !first_row && !showbreak.is_empty() {
                    c = g.put(r, col0 + numw, &showbreak, self.resolve(a_nontext), col0 + width) - col0 - numw;
                }
                let start_v = v;
                let _ = start_v;
                while ci < cells.len() {
                    let cell = &cells[ci];
                    if c + cell.w > textw {
                        if !wrap {
                            break;
                        }
                        if cell.w == 2 && c < textw {
                            // wide char does not fit: pad with '>'
                            g.put(r, col0 + numw + c, ">", self.resolve(a_nontext), col0 + width);
                        }
                        break;
                    }
                    let mut a = cell.attr;
                    if cuc && is_cur && v == cursor_vcol && lnum != cursor.line {
                        a = a_cuc.combine(&a);
                    }
                    if cc.contains(&(v + 1)) {
                        a = a_cc.combine(&a);
                    }
                    let a = if is_cul { a_cul.combine(&a) } else { a };
                    let a = self.resolve(a);
                    let sc = col0 + numw + c;
                    g.set(r, sc, Cell { ch: cell.ch, attr: a, width: cell.w as u8 });
                    if cell.w == 2 {
                        g.set(r, sc + 1, Cell { ch: ' ', attr: a, width: 0 });
                    }
                    if is_cur && lnum == cursor.line && cell.byte == cursor.col && !cell.nontext && cursor_screen.is_none() {
                        cursor_screen = Some((r, sc));
                    }
                    if is_cur && lnum == cursor.line && cell.byte == cursor.col && cursor_screen.is_none() && cell.ch != ' ' {
                        cursor_screen = Some((r, sc));
                    }
                    if is_cur && lnum == cursor.line && cell.byte == cursor.col && cursor_screen.is_none() {
                        cursor_screen = Some((r, sc));
                    }
                    c += cell.w;
                    v += cell.w;
                    ci += 1;
                }
                // after the text: visual on the line end, cursor column etc.
                if ci >= cells.len() {
                    let sc = col0 + numw + c;
                    if sc < col0 + width {
                        let ea = if eol_attr != Attr::default() { Some(eol_attr) } else { None };
                        if let Some(ea) = ea {
                            if ea.bg.is_some() {
                                let a = self.resolve(if is_cul { a_cul.combine(&ea) } else { ea });
                                g.set(r, sc, Cell { ch: ' ', attr: a, width: 1 });
                            }
                        }
                        if cuc && is_cur && lnum != cursor.line {
                            let off = cursor_vcol.checked_sub(if wrap { v - c } else { leftcol });
                            if let Some(off) = off {
                                if off >= c && off < textw {
                                    let a = self.resolve(normal_partial(a_cuc));
                                    g.set(r, col0 + numw + off, Cell { ch: ' ', attr: a, width: 1 });
                                }
                            }
                        }
                        for col in &cc {
                            let off = (col - 1).checked_sub(if wrap { v - c } else { leftcol });
                            if let Some(off) = off {
                                if off >= c && off < textw {
                                    let a = self.resolve(normal_partial(a_cc));
                                    g.set(r, col0 + numw + off, Cell { ch: ' ', attr: a, width: 1 });
                                }
                            }
                        }
                    }
                    if is_cur && lnum == cursor.line && cursor_screen.is_none() {
                        // cursor after the end of the line (insert mode / empty line)
                        let off = if list && lcs.eol.is_some() && cursor.col >= line.len() { c.saturating_sub(1) } else { c };
                        cursor_screen = Some((r, (col0 + numw + off).min(col0 + width - 1)));
                    }
                    row += 1;
                    break;
                }
                row += 1;
                first_row = false;
                if !wrap {
                    break;
                }
            }
            if !wrap && lcs.extends.is_some() && list {
                // indicate text beyond the right edge
                let total: usize = cells.iter().map(|c| c.w).sum();
                if total > leftcol + textw {
                    let r = row0 + row - 1;
                    g.put(r, col0 + width - 1, &lcs.extends.unwrap().to_string(), self.resolve(a_nontext), col0 + width);
                }
            }
            lnum += 1;
            // a line that did not fit at the bottom is shown as '@' rows
            if row >= height && lnum <= cursor.line && is_cur && wrap {
                break;
            }
        }
        // status line
        if has_status {
            let r = row0 + height;
            let group = if is_cur { "StatusLine" } else { "StatusLineNC" };
            let a = self.group(group);
            g.fill(r, col0, col0 + width, a);
            let stl = self.opt_for(Some(&self.windows[&id]), Some(&self.buffers[&bid]), "statusline").as_string();
            if stl.is_empty() {
                let b = &self.buffers[&bid];
                let name = b.path.as_ref().map(|_| b.display_name()).unwrap_or_else(|| "[No Name]".into());
                let left = format!("{}{}", name, if b.modified { " [+]" } else { "" });
                g.put(r, col0, &left, a, col0 + width);
                if self.opt_global("ruler").as_bool() {
                    let ruler = self.ruler_text(id);
                    let x = (col0 + width).saturating_sub(18).max(col0 + left.chars().count() + 1);
                    g.put(r, x, &ruler, a, col0 + width);
                }
            } else {
                let out = self.eval_statusline(&stl, id, width);
                // highlight switches
                let mut cur_attr = a;
                let mut gi = 0;
                let mut c = col0;
                for (i, ch) in out.text.chars().enumerate() {
                    while gi < out.groups.len() && out.groups[gi].0 <= i {
                        let name = &out.groups[gi].1;
                        cur_attr = if name.is_empty() || name == "*" { a } else { self.resolve(a.combine(&self.hl.attr(name))) };
                        gi += 1;
                    }
                    let w = char_width(ch, false).max(1);
                    if c + w > col0 + width {
                        break;
                    }
                    g.set(r, c, Cell { ch, attr: cur_attr, width: w as u8 });
                    if w == 2 {
                        g.set(r, c + 1, Cell { ch: ' ', attr: cur_attr, width: 0 });
                    }
                    c += w;
                }
            }
        }
        if is_cur {
            if let Some(p) = cursor_screen {
                g.cursor = p;
            } else {
                g.cursor = (row0, col0 + numw);
            }
        }
    }

    fn resolve_partial(&self, a: Attr) -> Attr {
        // only the background of CursorLine is applied to empty cells
        Attr { fg: None, ..a }
    }

    pub fn ruler_text(&self, id: WinId) -> String {
        let w = &self.windows[&id];
        let b = &self.buffers[&w.buf];
        let line = b.line(w.cursor.line);
        let ts = self.tabstop_of(w);
        let v = vcol_of(line, w.cursor.col, ts, self.ambi_double()) + 1;
        let c = if line.is_empty() { 0 } else { w.cursor.col + 1 };
        let col = if line.is_empty() { "0-1".to_string() } else if v != c { format!("{}-{}", c, v) } else { c.to_string() };
        let nlines = b.line_count();
        let bot = self.botline(w);
        let pct = if w.topline == 0 && bot + 1 >= nlines {
            "All".to_string()
        } else if w.topline == 0 {
            "Top".to_string()
        } else if bot + 1 >= nlines {
            "Bot".to_string()
        } else {
            format!("{}%", w.topline * 100 / nlines.saturating_sub(w.height).max(1))
        };
        format!("{:<14}{}", format!("{},{}", w.cursor.line + 1, col), pct)
    }

    fn draw_cmdline_area(&mut self, g: &mut Grid) {
        let normal = self.group("Normal");
        let msgarea = self.resolve(self.hl.attr("Normal").combine(&self.hl.attr("MsgArea")));
        let ch = self.cmd_height();
        let top = self.rows - ch;
        // "Press ENTER" output
        if !self.output.is_empty() {
            let more = self.group("MoreMsg");
            let mut lines: Vec<(String, Attr)> = Vec::new();
            for m in &self.output {
                let a = if m.group.is_empty() { msgarea } else { self.group(m.group) };
                // wrap long lines
                let mut cur = String::new();
                let mut w = 0;
                for c in m.text.chars() {
                    let cw = char_width(c, false).max(1);
                    if w + cw > self.cols {
                        lines.push((std::mem::take(&mut cur), a));
                        w = 0;
                    }
                    cur.push(c);
                    w += cw;
                }
                lines.push((cur, a));
            }
            lines.push(("Press ENTER or type command to continue".into(), more));
            let n = lines.len().min(self.rows);
            let start = lines.len() - n;
            for (i, (t, a)) in lines[start..].iter().enumerate() {
                let r = self.rows - n + i;
                g.fill(r, 0, self.cols, msgarea);
                g.put(r, 0, t, *a, self.cols);
            }
            g.cursor = (self.rows - 1, ("Press ENTER or type command to continue".len()).min(self.cols - 1));
            g.cursor_shape = CursorShape::Block;
            return;
        }
        for r in top..self.rows {
            g.fill(r, 0, self.cols, msgarea);
        }
        let cl = self.expr_prompt.as_ref().map(|e| (e.clone(), true)).or_else(|| self.cmdline.as_ref().map(|c| (c.clone(), false)));
        if let Some((c, is_expr)) = cl {
            let prefix = if is_expr { "=".to_string() } else { c.kind.to_string() };
            let text = format!("{}{}", prefix, c.text);
            let shown: String = keys_visible(&text);
            // scroll horizontally if needed
            let cur_w = super::statusline::display_width(&keys_visible(&format!("{}{}", prefix, &c.text[..c.pos])));
            let off = if cur_w >= self.cols { cur_w - self.cols + 1 } else { 0 };
            let mut skipped = 0;
            let visible: String = shown
                .chars()
                .skip_while(|ch| {
                    if skipped < off {
                        skipped += char_width(*ch, false).max(1);
                        true
                    } else {
                        false
                    }
                })
                .collect();
            g.put(top, 0, &visible, msgarea, self.cols);
            g.cursor = (top, (cur_w - off).min(self.cols - 1));
            g.cursor_shape = CursorShape::Vertical(25);
            // wildmenu
            if let Some(w) = &c.wild {
                if self.opt_global("wildmenu").as_bool() && w.items.len() > 1 && top > 0 {
                    let r = top - 1;
                    let sl = self.group("StatusLine");
                    let wm = self.group("WildMenu");
                    g.fill(r, 0, self.cols, sl);
                    let mut col = 0;
                    let sel = w.idx.unwrap_or(usize::MAX);
                    // keep the selected item visible
                    let mut first = 0;
                    if sel != usize::MAX {
                        let mut width_until = 0;
                        for (i, it) in w.items.iter().enumerate() {
                            width_until += it.chars().count() + 2;
                            if i == sel {
                                break;
                            }
                        }
                        while width_until > self.cols.saturating_sub(4) && first < sel {
                            width_until -= w.items[first].chars().count() + 2;
                            first += 1;
                        }
                    }
                    if first > 0 {
                        col = g.put(r, col, "< ", sl, self.cols);
                    }
                    for (i, it) in w.items.iter().enumerate().skip(first) {
                        let name = it.trim_end_matches('/').rsplit('/').next().unwrap_or(it);
                        let name = if it.ends_with('/') { format!("{}/", name) } else { name.to_string() };
                        if col + name.chars().count() + 2 > self.cols {
                            g.put(r, self.cols - 1, ">", sl, self.cols);
                            break;
                        }
                        col = g.put(r, col, &name, if i == sel { wm } else { sl }, self.cols);
                        col = g.put(r, col, "  ", sl, self.cols);
                    }
                }
            }
            if is_expr {
                return;
            }
            return;
        }
        // mode message / message line
        let mut col = 0;
        let mode_msg = self.mode_message();
        if let Some(m) = self.msg_line.clone() {
            let a = if m.group.is_empty() { msgarea } else { self.group(m.group) };
            col = g.put(top, 0, &m.text, a, self.cols);
        } else if !mode_msg.is_empty() && self.opt_global("showmode").as_bool() {
            let a = self.group("ModeMsg");
            col = g.put(top, 0, &mode_msg, a, self.cols);
        }
        let _ = col;
        // showcmd and ruler
        let ruler_on = self.opt_global("ruler").as_bool() && !self.win().has_status;
        if self.opt_global("showcmd").as_bool() {
            let mut sc = keys_visible(&super::keys::keys_notation(&self.pending).replace("<Space>", " "));
            if let Some((s, e, kind)) = self.visual_range() {
                sc = match kind {
                    VisualKind::Line => (e.line - s.line + 1).to_string(),
                    VisualKind::Char => {
                        if s.line == e.line {
                            let t = self.line(s.line);
                            let end = super::motion::next_char_end(t, e.col).min(t.len());
                            t[s.col.min(end)..end].chars().count().to_string()
                        } else {
                            (e.line - s.line + 1).to_string()
                        }
                    }
                    VisualKind::Block => {
                        let r = self.visual_op_range();
                        format!("{}x{}", e.line - s.line + 1, r.vcols.1 + 1 - r.vcols.0)
                    }
                };
            }
            if !sc.is_empty() {
                let sc: String = sc.chars().rev().take(10).collect::<Vec<_>>().into_iter().rev().collect();
                let x = self.cols.saturating_sub(if ruler_on { 11 + 18 } else { 11 });
                g.put(self.rows - 1, x, &sc, normal, self.cols);
            }
        }
        if ruler_on {
            let id = self.cur_win_id();
            let ruler = self.ruler_text(id);
            let x = self.cols.saturating_sub(18);
            g.put(self.rows - 1, x, &ruler, msgarea, self.cols);
        }
        g.cursor_shape = match self.mode {
            Mode::Insert => CursorShape::Vertical(25),
            Mode::Replace => CursorShape::Horizontal(20),
            _ if self.pending_state == super::normal::PendingState::Operator => CursorShape::Horizontal(50),
            _ => CursorShape::Block,
        };
        let cattr = self.hl.attr("Cursor");
        g.cursor_attr = self.resolve(cattr);
        if self.ime_preedit.is_empty() {
            return;
        }
        // IME pre-edit text over the cursor position
        let (r, c) = g.cursor;
        let a = Attr { underline: true, ..self.group("Normal") };
        g.put(r, c, &self.ime_preedit.clone(), a, self.cols);
    }

    pub fn mode_message(&self) -> String {
        let rec = self.recording.as_ref().map(|(r, _)| format!("recording @{}", r)).unwrap_or_default();
        let m = match self.mode {
            Mode::Insert => {
                if self.restart_insert {
                    "-- (insert) --".to_string()
                } else if self.completion.is_some() {
                    "-- Keyword completion (^N^P)".to_string()
                } else {
                    "-- INSERT --".to_string()
                }
            }
            Mode::Replace => "-- REPLACE --".to_string(),
            Mode::Visual(VisualKind::Char) => format!("-- {}VISUAL --", if self.restart_insert { "(insert) " } else { "" }),
            Mode::Visual(VisualKind::Line) => format!("-- {}VISUAL LINE --", if self.restart_insert { "(insert) " } else { "" }),
            Mode::Visual(VisualKind::Block) => format!("-- {}VISUAL BLOCK --", if self.restart_insert { "(insert) " } else { "" }),
            Mode::Normal if self.restart_insert => "-- (insert) --".to_string(),
            _ => String::new(),
        };
        match (m.is_empty(), rec.is_empty()) {
            (true, _) => rec,
            (false, true) => m,
            (false, false) => format!("{}{}", m, rec),
        }
    }

    fn draw_popup(&mut self, g: &mut Grid) {
        let Some(c) = self.completion.clone() else {
            return;
        };
        let pmenu = self.group("Pmenu");
        let sel = self.group("PmenuSel");
        let sbar = self.group("PmenuSbar");
        let thumb = self.group("PmenuThumb");
        let maxh = match self.opt_global("pumheight").as_num() {
            0 => 15,
            n => n as usize,
        };
        let n = c.items.len();
        let (crow, ccol) = g.cursor;
        let below = self.rows.saturating_sub(crow + 1 + self.cmd_height());
        let above = crow;
        let h = n.min(maxh).min(below.max(above)).max(1);
        let width = c.items.iter().map(|s| super::statusline::display_width(s)).max().unwrap_or(1).max(15) + 2;
        let start_row = if below >= h { crow + 1 } else { crow.saturating_sub(h) };
        // column: align with the start of the completed word
        let wordw = super::statusline::display_width(&c.base);
        let col = ccol.saturating_sub(wordw).min(self.cols.saturating_sub(width + 1));
        let first = match c.selected {
            Some(s) if s >= h => s + 1 - h,
            _ => 0,
        };
        for i in 0..h {
            let idx = first + i;
            let r = start_row + i;
            let a = if c.selected == Some(idx) { sel } else { pmenu };
            g.fill(r, col, col + width, a);
            if let Some(it) = c.items.get(idx) {
                g.put(r, col + 1, it, a, col + width);
            }
            if n > h {
                let pos = i * n / h;
                let thumb_on = pos >= first && pos < first + h && (first * h / n..=(first + h) * h / n).contains(&i);
                g.set(r, col + width, Cell { ch: ' ', attr: if thumb_on { thumb } else { sbar }, width: 1 });
            }
        }
    }
}

fn normal_partial(a: Attr) -> Attr {
    Attr { fg: None, ..a }
}

/// Show control characters on the command line as ^X.
fn keys_visible(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if (c as u32) < 0x20 {
            out.push('^');
            out.push(((c as u8) + 0x40) as char);
        } else if (0xF000..0xF200).contains(&(c as u32)) {
            // encoded special key
        } else {
            out.push(c);
        }
    }
    out
}
