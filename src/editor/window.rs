//! Windows, split layout, tab pages and buffer/file management.

use std::path::{Path, PathBuf};

use super::buffer::{Buffer, Pos};
use super::motion::{line_width, vcol_of};
use super::options::{LocalOpts, OptVal};
use super::syntax;
use super::{BufId, Editor, Mode, WinId};

#[derive(Clone, Debug)]
pub struct WinMatch {
    pub id: usize,
    pub group: String,
    pub pattern: String,
}

pub struct Window {
    pub id: WinId,
    pub buf: BufId,
    pub cursor: Pos,
    /// Wanted display column for vertical movement (`usize::MAX` = end of line).
    pub curswant: usize,
    pub topline: usize,
    pub leftcol: usize,
    pub opts: LocalOpts,
    // geometry in cells, assigned by `Editor::layout`
    pub row: usize,
    pub col: usize,
    pub width: usize,
    pub height: usize,
    pub has_status: bool,
    pub matches: Vec<WinMatch>,
    pub alt_buf: Option<BufId>,
}

#[derive(Clone, Debug)]
pub enum Layout {
    Leaf(WinId),
    Split {
        /// true: children are side by side (`:vsplit`); false: stacked.
        vertical: bool,
        children: Vec<Layout>,
        weights: Vec<f64>,
        /// Cell sizes from the last layout pass.
        sizes: Vec<usize>,
    },
}

pub struct TabPage {
    pub layout: Layout,
    pub cur: WinId,
    pub prev: WinId,
}

impl Layout {
    pub fn leaves(&self) -> Vec<WinId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<WinId>) {
        match self {
            Layout::Leaf(id) => out.push(*id),
            Layout::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }

    fn contains(&self, id: WinId) -> bool {
        match self {
            Layout::Leaf(x) => *x == id,
            Layout::Split { children, .. } => children.iter().any(|c| c.contains(id)),
        }
    }

    /// Insert `new` next to `target`.
    fn split_leaf(&mut self, target: WinId, new: WinId, vertical: bool, before: bool) -> bool {
        match self {
            Layout::Leaf(x) => {
                if *x != target {
                    return false;
                }
                let children = if before { vec![Layout::Leaf(new), Layout::Leaf(target)] } else { vec![Layout::Leaf(target), Layout::Leaf(new)] };
                *self = Layout::Split { vertical, children, weights: vec![1.0, 1.0], sizes: vec![0, 0] };
                true
            }
            Layout::Split { vertical: v, children, weights, sizes } => {
                if let Some(i) = children.iter().position(|c| matches!(c, Layout::Leaf(x) if *x == target)) {
                    if *v == vertical {
                        let w = weights[i] / 2.0;
                        weights[i] = w;
                        let at = if before { i } else { i + 1 };
                        children.insert(at, Layout::Leaf(new));
                        weights.insert(at, w);
                        sizes.insert(at, 0);
                        return true;
                    }
                }
                children.iter_mut().any(|c| c.split_leaf(target, new, vertical, before))
            }
        }
    }

    /// Remove a leaf; returns true if found.  Single-child splits collapse.
    fn remove_leaf(&mut self, id: WinId) -> bool {
        let Layout::Split { children, weights, sizes, .. } = self else {
            return false;
        };
        if let Some(i) = children.iter().position(|c| matches!(c, Layout::Leaf(x) if *x == id)) {
            children.remove(i);
            let w = weights.remove(i);
            sizes.remove(i);
            // give the space to a neighbour
            if !weights.is_empty() {
                let n = if i > 0 { i - 1 } else { 0 };
                weights[n] += w;
            }
        } else if !children.iter_mut().any(|c| c.remove_leaf(id)) {
            return false;
        }
        if let Layout::Split { children, .. } = self {
            if children.len() == 1 {
                *self = children.pop().unwrap();
            }
        }
        true
    }

    fn equalize(&mut self) {
        if let Layout::Split { children, weights, .. } = self {
            for w in weights.iter_mut() {
                *w = 1.0;
            }
            children.iter_mut().for_each(|c| c.equalize());
        }
    }

    /// Change the size of the split containing `id` along one axis.
    fn resize(&mut self, id: WinId, vertical: bool, delta: isize, absolute: Option<usize>) -> bool {
        let Layout::Split { vertical: v, children, weights, sizes } = self else {
            return false;
        };
        let Some(i) = children.iter().position(|c| c.contains(id)) else {
            return false;
        };
        // Prefer the innermost matching split.
        if children[i].resize(id, vertical, delta, absolute) {
            return true;
        }
        if *v != vertical || children.len() < 2 {
            return false;
        }
        let total: usize = sizes.iter().sum();
        let cur = sizes[i] as isize;
        let max = total as isize - (children.len() as isize - 1);
        let want = match absolute {
            Some(a) => a as isize,
            None => cur + delta,
        }
        .clamp(1, max.max(1));
        let diff = want - cur;
        sizes[i] = want as usize;
        // take from / give to the following siblings first, then the preceding ones
        let mut remaining = diff;
        let order: Vec<usize> = (i + 1..children.len()).chain((0..i).rev()).collect();
        for j in order {
            if remaining == 0 {
                break;
            }
            let s = sizes[j] as isize;
            let take = if remaining > 0 { remaining.min(s - 1).max(0) } else { remaining };
            sizes[j] = (s - take) as usize;
            remaining -= take;
        }
        for (w, s) in weights.iter_mut().zip(sizes.iter()) {
            *w = (*s).max(1) as f64;
        }
        true
    }

    fn rotate(&mut self, id: WinId, backward: bool) -> bool {
        let Layout::Split { children, weights, .. } = self else {
            return false;
        };
        if children.iter().any(|c| matches!(c, Layout::Leaf(x) if *x == id)) {
            if children.iter().all(|c| matches!(c, Layout::Leaf(_))) {
                if backward {
                    children.rotate_left(1);
                    weights.rotate_left(1);
                } else {
                    children.rotate_right(1);
                    weights.rotate_right(1);
                }
            }
            return true;
        }
        children.iter_mut().any(|c| c.rotate(id, backward))
    }

    fn exchange(&mut self, a: WinId, b: WinId) {
        match self {
            Layout::Leaf(x) => {
                if *x == a {
                    *x = b;
                } else if *x == b {
                    *x = a;
                }
            }
            Layout::Split { children, .. } => children.iter_mut().for_each(|c| c.exchange(a, b)),
        }
    }
}

fn distribute(total: usize, weights: &[f64], min: usize) -> Vec<usize> {
    let n = weights.len();
    let sum: f64 = weights.iter().sum::<f64>().max(1e-9);
    let mut sizes: Vec<usize> = weights.iter().map(|w| ((w / sum) * total as f64).floor() as usize).map(|s| s.max(min)).collect();
    let mut used: usize = sizes.iter().sum();
    let mut i = 0;
    while used < total {
        sizes[i % n] += 1;
        used += 1;
        i += 1;
    }
    let mut guard = 0;
    while used > total && guard < 10000 {
        let j = (0..n).max_by_key(|j| sizes[*j]).unwrap();
        if sizes[j] <= 1 {
            break;
        }
        sizes[j] -= 1;
        used -= 1;
        guard += 1;
    }
    sizes
}

impl Editor {
    pub fn new_window(&mut self, buf: BufId) -> WinId {
        let id = self.next_win_id;
        self.next_win_id += 1;
        self.windows.insert(
            id,
            Window {
                id,
                buf,
                cursor: Pos::default(),
                curswant: 0,
                topline: 0,
                leftcol: 0,
                opts: LocalOpts::new(),
                row: 0,
                col: 0,
                width: self.cols,
                height: self.rows.saturating_sub(2),
                has_status: false,
                matches: Vec::new(),
                alt_buf: None,
            },
        );
        id
    }

    pub fn tab_wins(&self) -> Vec<WinId> {
        self.tabs[self.cur_tab].layout.leaves()
    }

    pub fn tabline_height(&self) -> usize {
        match self.opt_global("showtabline").as_num() {
            0 => 0,
            1 => (self.tabs.len() > 1) as usize,
            _ => 1,
        }
    }

    pub fn cmd_height(&self) -> usize {
        (self.opt_global("cmdheight").as_num().max(1) as usize).min(self.rows.saturating_sub(2).max(1))
    }

    /// Vertical separators of the current tab: (row, col, height).
    pub fn layout(&mut self) -> Vec<(usize, usize, usize)> {
        let top = self.tabline_height();
        let height = self.rows.saturating_sub(top + self.cmd_height()).max(1);
        let ls = self.opt_global("laststatus").as_num();
        let nwin = self.tabs[self.cur_tab].layout.leaves().len();
        let status = ls >= 2 || (ls == 1 && nwin > 1);
        let mut seps = Vec::new();
        let mut layout = std::mem::replace(&mut self.tabs[self.cur_tab].layout, Layout::Leaf(0));
        self.assign(&mut layout, top, 0, self.cols, height, status, &mut seps);
        self.tabs[self.cur_tab].layout = layout;
        seps
    }

    #[allow(clippy::too_many_arguments)]
    fn assign(&mut self, l: &mut Layout, row: usize, col: usize, width: usize, height: usize, status: bool, seps: &mut Vec<(usize, usize, usize)>) {
        match l {
            Layout::Leaf(id) => {
                if let Some(w) = self.windows.get_mut(id) {
                    w.row = row;
                    w.col = col;
                    w.width = width.max(1);
                    w.has_status = status && height > 1;
                    w.height = (height - w.has_status as usize).max(1);
                }
            }
            Layout::Split { vertical, children, weights, sizes } => {
                let n = children.len();
                if *vertical {
                    let avail = width.saturating_sub(n - 1).max(n);
                    let s = distribute(avail, weights, 1);
                    let mut c = col;
                    for (i, child) in children.iter_mut().enumerate() {
                        self.assign(child, row, c, s[i], height, true, seps);
                        c += s[i];
                        if i + 1 < n {
                            seps.push((row, c, height));
                            c += 1;
                        }
                    }
                    *sizes = s;
                } else {
                    let s = distribute(height.max(n), weights, 1);
                    let mut r = row;
                    for (i, child) in children.iter_mut().enumerate() {
                        let st = status || i + 1 < n;
                        self.assign(child, r, col, width, s[i], st, seps);
                        r += s[i];
                    }
                    *sizes = s;
                }
            }
        }
    }

    // --------------------------------------------------------------- splits

    /// Split the current window.  Returns the new window (which becomes current).
    pub fn split_window(&mut self, vertical: bool, buf: Option<BufId>) -> WinId {
        let cur = self.cur_win_id();
        let buf = buf.unwrap_or(self.win().buf);
        let new = self.new_window(buf);
        let (cursor, topline, opts, curswant) = {
            let w = self.win();
            (w.cursor, w.topline, w.opts.clone(), w.curswant)
        };
        let same = buf == self.windows[&cur].buf;
        {
            let w = self.windows.get_mut(&new).unwrap();
            w.opts = opts;
            if same {
                w.cursor = cursor;
                w.topline = topline;
                w.curswant = curswant;
            }
        }
        let before = if vertical { !self.opt_global("splitright").as_bool() } else { !self.opt_global("splitbelow").as_bool() };
        let tab = &mut self.tabs[self.cur_tab];
        tab.layout.split_leaf(cur, new, vertical, before);
        self.do_autocmd("WinLeave", "");
        let tab = &mut self.tabs[self.cur_tab];
        tab.prev = cur;
        tab.cur = new;
        self.layout();
        self.do_autocmd("WinNew", "");
        self.do_autocmd("WinEnter", "");
        new
    }

    pub fn goto_window(&mut self, id: WinId) {
        if id == self.cur_win_id() || !self.windows.contains_key(&id) {
            return;
        }
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.do_autocmd("WinLeave", "");
        let old_buf = self.cur_buf_id();
        let tab = &mut self.tabs[self.cur_tab];
        if !tab.layout.contains(id) {
            return;
        }
        tab.prev = tab.cur;
        tab.cur = id;
        self.do_autocmd("WinEnter", "");
        if self.cur_buf_id() != old_buf {
            self.do_autocmd("BufEnter", "");
        }
    }

    /// Window in direction `dir` ('h','j','k','l') from the current one.
    pub fn window_in_direction(&self, dir: char, count: usize) -> WinId {
        let mut cur = self.cur_win_id();
        for _ in 0..count.max(1) {
            let w = &self.windows[&cur];
            let crow = w.row + (w.cursor.line.saturating_sub(w.topline)).min(w.height.saturating_sub(1));
            let ccol = w.col;
            let mut best: Option<(usize, WinId)> = None;
            for id in self.tab_wins() {
                if id == cur {
                    continue;
                }
                let o = &self.windows[&id];
                let (adjacent, overlap, dist) = match dir {
                    'h' => (o.col + o.width < w.col + 1, o.row <= crow && crow < o.row + o.height + 1, w.col - (o.col + o.width).min(w.col)),
                    'l' => (o.col > w.col + w.width.saturating_sub(1), o.row <= crow && crow < o.row + o.height + 1, o.col - w.col),
                    'k' => (o.row + o.height < w.row + 1, o.col <= ccol && ccol < o.col + o.width + 1, w.row - (o.row + o.height).min(w.row)),
                    _ => (o.row > w.row + w.height.saturating_sub(1), o.col <= ccol && ccol < o.col + o.width + 1, o.row - w.row),
                };
                if adjacent && overlap && best.map(|b| dist < b.0).unwrap_or(true) {
                    best = Some((dist, id));
                }
            }
            match best {
                Some((_, id)) => cur = id,
                None => break,
            }
        }
        cur
    }

    pub fn buf_window_count(&self, buf: BufId) -> usize {
        self.windows.values().filter(|w| w.buf == buf).count()
    }

    /// Check whether the current buffer may be abandoned by this window.
    pub fn can_abandon(&self, force: bool) -> Result<(), String> {
        let b = self.buf();
        if force || !b.modified || self.opt_global("hidden").as_bool() || self.buf_window_count(b.id) > 1 || b.scratch {
            Ok(())
        } else {
            Err("E37: No write since last change (add ! to override)".into())
        }
    }

    /// Close a window of the current tab.  Closing the last window of the last
    /// tab is refused (use `:quit`).
    pub fn close_window(&mut self, id: WinId) -> Result<(), String> {
        let wins = self.tab_wins();
        if wins.len() <= 1 {
            if self.tabs.len() <= 1 {
                return Err("E444: Cannot close last window".into());
            }
            return self.close_tab(self.cur_tab);
        }
        let was_cur = self.cur_win_id() == id;
        if was_cur {
            self.do_autocmd("WinLeave", "");
        }
        let pos = self.windows[&id].cursor;
        let bid = self.windows[&id].buf;
        if let Some(b) = self.buffers.get_mut(&bid) {
            b.last_cursor = pos;
        }
        let tab = &mut self.tabs[self.cur_tab];
        tab.layout.remove_leaf(id);
        self.windows.remove(&id);
        let tab = &mut self.tabs[self.cur_tab];
        if was_cur {
            let leaves = tab.layout.leaves();
            let next = if leaves.contains(&tab.prev) {
                tab.prev
            } else {
                let idx = wins.iter().position(|w| *w == id).unwrap_or(0);
                *wins.get(idx + 1).filter(|w| leaves.contains(w)).or_else(|| leaves.get(idx.saturating_sub(1))).unwrap_or(&leaves[0])
            };
            tab.cur = next;
            tab.prev = next;
        } else if tab.prev == id {
            tab.prev = tab.cur;
        }
        self.layout();
        if was_cur {
            self.do_autocmd("WinEnter", "");
            self.do_autocmd("BufEnter", "");
        }
        Ok(())
    }

    pub fn only_window(&mut self) {
        let cur = self.cur_win_id();
        for id in self.tab_wins() {
            if id != cur {
                self.windows.remove(&id);
            }
        }
        let tab = &mut self.tabs[self.cur_tab];
        tab.layout = Layout::Leaf(cur);
        tab.prev = cur;
        self.layout();
    }

    pub fn resize_window(&mut self, vertical: bool, delta: isize, absolute: Option<usize>) {
        let id = self.cur_win_id();
        // account for the status line when setting an absolute height
        let absolute = absolute.map(|a| if vertical { a } else { a + self.win().has_status as usize });
        self.tabs[self.cur_tab].layout.resize(id, vertical, delta, absolute);
        self.layout();
    }

    pub fn equalize_windows(&mut self) {
        self.tabs[self.cur_tab].layout.equalize();
        self.layout();
    }

    pub fn rotate_windows(&mut self, backward: bool) {
        let id = self.cur_win_id();
        self.tabs[self.cur_tab].layout.rotate(id, backward);
        self.layout();
    }

    pub fn exchange_window(&mut self) {
        let wins = self.tab_wins();
        let cur = self.cur_win_id();
        if let Some(i) = wins.iter().position(|w| *w == cur) {
            let other = wins[(i + 1) % wins.len()];
            if other != cur {
                self.tabs[self.cur_tab].layout.exchange(cur, other);
                self.layout();
            }
        }
    }

    /// Move the current window to the far edge (`<C-w>H/J/K/L`).
    pub fn move_window_far(&mut self, dir: char) {
        let cur = self.cur_win_id();
        let tab = &mut self.tabs[self.cur_tab];
        if tab.layout.leaves().len() < 2 {
            return;
        }
        tab.layout.remove_leaf(cur);
        let rest = std::mem::replace(&mut tab.layout, Layout::Leaf(cur));
        let vertical = matches!(dir, 'H' | 'L');
        let children = if matches!(dir, 'H' | 'K') { vec![Layout::Leaf(cur), rest] } else { vec![rest, Layout::Leaf(cur)] };
        tab.layout = Layout::Split { vertical, children, weights: vec![1.0, 1.0], sizes: vec![0, 0] };
        self.layout();
    }

    // ----------------------------------------------------------------- tabs

    pub fn new_tab(&mut self, buf: Option<BufId>, at: Option<usize>) {
        self.do_autocmd("WinLeave", "");
        self.do_autocmd("TabLeave", "");
        let buf = buf.unwrap_or_else(|| self.new_buffer());
        let win = self.new_window(buf);
        let idx = at.unwrap_or(self.cur_tab + 1).min(self.tabs.len());
        self.tabs.insert(idx, TabPage { layout: Layout::Leaf(win), cur: win, prev: win });
        self.prev_tab = self.cur_tab;
        self.cur_tab = idx;
        self.layout();
        self.do_autocmd("TabNew", "");
        self.do_autocmd("WinEnter", "");
        self.do_autocmd("TabEnter", "");
        self.do_autocmd("BufEnter", "");
    }

    pub fn goto_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.cur_tab {
            return;
        }
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.do_autocmd("WinLeave", "");
        self.do_autocmd("TabLeave", "");
        self.prev_tab = self.cur_tab;
        self.cur_tab = idx;
        self.layout();
        self.do_autocmd("TabEnter", "");
        self.do_autocmd("WinEnter", "");
        self.do_autocmd("BufEnter", "");
    }

    pub fn close_tab(&mut self, idx: usize) -> Result<(), String> {
        if self.tabs.len() <= 1 {
            return Err("E784: Cannot close last tab page".into());
        }
        if idx >= self.tabs.len() {
            return Err("E475: Invalid argument".into());
        }
        let tab = self.tabs.remove(idx);
        for id in tab.layout.leaves() {
            self.windows.remove(&id);
        }
        if self.cur_tab > idx || self.cur_tab >= self.tabs.len() {
            self.cur_tab = self.cur_tab.saturating_sub(1);
        }
        self.cur_tab = self.cur_tab.min(self.tabs.len() - 1);
        self.prev_tab = self.cur_tab;
        self.layout();
        self.do_autocmd("TabEnter", "");
        self.do_autocmd("WinEnter", "");
        self.do_autocmd("BufEnter", "");
        Ok(())
    }

    // -------------------------------------------------------------- buffers

    /// Show `buf` in the current window.
    pub fn set_cur_buf(&mut self, buf: BufId) {
        if !self.buffers.contains_key(&buf) {
            return;
        }
        let old = self.cur_buf_id();
        if old == buf {
            return;
        }
        if matches!(self.mode, Mode::Visual(_)) {
            self.mode = Mode::Normal;
        }
        self.do_autocmd("BufLeave", "");
        let pos = self.cursor();
        if let Some(b) = self.buffers.get_mut(&old) {
            b.last_cursor = pos;
        }
        let last = self.buffers[&buf].last_cursor;
        let w = self.win_mut();
        w.alt_buf = Some(old);
        w.buf = buf;
        w.cursor = last;
        w.topline = last.line.saturating_sub(w.height / 2);
        w.leftcol = 0;
        w.curswant = 0;
        self.check_cursor();
        self.do_autocmd("BufEnter", "");
        self.do_autocmd("BufWinEnter", "");
        self.apply_autochdir();
    }

    fn apply_autochdir(&mut self) {
        if self.opt_global("autochdir").as_bool() {
            if let Some(p) = self.buf().path.clone() {
                let full = self.full_path(&p);
                if let Some(dir) = full.parent() {
                    if dir.is_dir() {
                        // keep the buffer name absolute so it survives the chdir
                        let id = self.cur_buf_id();
                        for b in self.buffers.values_mut() {
                            if let Some(bp) = &b.path {
                                if !bp.is_absolute() {
                                    let abs = std::env::current_dir().map(|d| d.join(bp)).unwrap_or_else(|_| bp.clone());
                                    b.path = Some(abs);
                                }
                            }
                        }
                        let _ = id;
                        let _ = std::env::set_current_dir(dir);
                    }
                }
            }
        }
    }

    /// Read a file into buffer `id` (which must be the current buffer).
    fn load_file(&mut self, id: BufId, path: &Path) {
        let subject = self.full_path(path).to_string_lossy().replace('\\', "/");
        let exists = path.exists();
        if exists {
            self.do_autocmd("BufReadPre", &subject);
            let fencs = self.opt_global("fileencodings").as_string();
            match Buffer::read_file(path, &fencs) {
                Ok(r) => {
                    let nlines = r.lines.len();
                    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                    let b = self.buffers.get_mut(&id).unwrap();
                    b.lines = if r.lines.is_empty() { vec![String::new()] } else { r.lines };
                    b.modified = false;
                    b.clear_undo();
                    b.syn_states.clear();
                    b.bomb = r.bomb;
                    b.mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
                    b.opts.insert("fileformat", OptVal::Str(r.fileformat.to_string()));
                    b.opts.insert("fileencoding", OptVal::Str(r.encoding.clone()));
                    b.opts.insert("endofline", OptVal::Bool(r.eol));
                    b.opts.insert("bomb", OptVal::Bool(r.bomb));
                    let ro = std::fs::metadata(path).map(|m| m.permissions().readonly()).unwrap_or(false);
                    if ro {
                        b.opts.insert("readonly", OptVal::Bool(true));
                    }
                    let name = self.buffers[&id].display_name();
                    self.msg(format!(
                        "\"{}\" {}{}L, {}B",
                        name,
                        if ro { "[readonly] " } else { "" },
                        nlines,
                        bytes
                    ));
                }
                Err(e) => {
                    self.emsg(format!("E484: Can't open file {}: {}", path.display(), e));
                }
            }
            self.do_autocmd("BufReadPost", &subject);
        } else {
            let name = self.buffers[&id].display_name();
            self.msg(format!("\"{}\" [New]", name));
            self.do_autocmd("BufNewFile", &subject);
        }
        self.detect_filetype();
    }

    pub fn detect_filetype(&mut self) {
        if !self.filetype_on || !self.opt_str("filetype").is_empty() {
            return;
        }
        let Some(path) = self.buf().path.clone() else {
            return;
        };
        let first = self.buf().line(0).to_string();
        if let Some(ft) = syntax::detect_filetype(&path, &first) {
            let _ = self.set_opt_by_name("filetype", OptVal::Str(ft.to_string()), 'l');
        }
    }

    /// `:edit file`
    pub fn edit_file(&mut self, path: &Path, force: bool) -> Result<(), String> {
        if let Some(id) = self.find_buffer_by_path(path) {
            if id != self.cur_buf_id() {
                self.can_abandon(force)?;
                self.set_cur_buf(id);
            }
            return Ok(());
        }
        // Reuse an empty, unnamed, unmodified buffer.
        let reuse = {
            let b = self.buf();
            b.path.is_none() && !b.modified && b.is_empty() && self.buf_window_count(b.id) == 1
        };
        if !reuse {
            self.can_abandon(force)?;
        }
        let id = if reuse { self.cur_buf_id() } else { self.new_buffer() };
        self.buffers.get_mut(&id).unwrap().path = Some(path.to_path_buf());
        if !reuse {
            self.do_autocmd("BufLeave", "");
            let old = self.cur_buf_id();
            let pos = self.cursor();
            self.buffers.get_mut(&old).unwrap().last_cursor = pos;
            let w = self.win_mut();
            w.alt_buf = Some(old);
            w.buf = id;
        }
        let w = self.win_mut();
        w.cursor = Pos::default();
        w.topline = 0;
        w.leftcol = 0;
        w.curswant = 0;
        self.do_autocmd("BufAdd", "");
        self.load_file(id, path);
        self.do_autocmd("BufEnter", "");
        self.do_autocmd("BufWinEnter", "");
        self.apply_autochdir();
        Ok(())
    }

    /// `:edit!` – reload the current file from disk.
    pub fn reload_buffer(&mut self) -> Result<(), String> {
        let Some(path) = self.buf().path.clone() else {
            return Err("E32: No file name".into());
        };
        let id = self.cur_buf_id();
        let pos = self.cursor();
        self.load_file(id, &path);
        self.set_cursor(pos);
        Ok(())
    }

    /// Write lines `range` (or the whole buffer) to `path`.
    pub fn write_buffer(&mut self, id: BufId, path: Option<PathBuf>, range: Option<(usize, usize)>, force: bool, append: bool) -> Result<(), String> {
        let target = match path.clone().or_else(|| self.buffers[&id].path.clone()) {
            Some(p) => p,
            None => return Err("E32: No file name".into()),
        };
        let is_own = path.is_none() || self.buffers[&id].path.is_none() || self.buffers[&id].path.as_deref() == path.as_deref();
        if path.is_some() && !is_own && target.exists() && !force && !append {
            return Err("E13: File exists (add ! to override)".into());
        }
        let cur = self.cur_buf_id();
        let whole = range.is_none();
        let subject = self.full_path(&target).to_string_lossy().replace('\\', "/");
        if id == cur && whole {
            if self.opt_bool("readonly") && !force && is_own {
                return Err("E45: 'readonly' option is set (add ! to override)".into());
            }
            self.do_autocmd("BufWritePre", &subject);
        }
        let (bytes, nlines) = {
            let b = &self.buffers[&id];
            let w = self.windows.values().find(|w| w.buf == id);
            let ff = self.opt_for(w, Some(b), "fileformat").as_string();
            let fenc = self.opt_for(w, Some(b), "fileencoding").as_string();
            let bomb = self.opt_for(w, Some(b), "bomb").as_bool();
            let eol = self.opt_for(w, Some(b), "endofline").as_bool() || self.opt_for(w, Some(b), "fixendofline").as_bool();
            let lines: &[String] = match range {
                Some((s, e)) => &b.lines[s.min(b.lines.len() - 1)..(e + 1).min(b.lines.len())],
                None => &b.lines,
            };
            (Buffer::encode(lines, &ff, &fenc, bomb, eol), lines.len())
        };
        let res = if append {
            use std::io::Write;
            std::fs::OpenOptions::new().append(true).create(true).open(&target).and_then(|mut f| f.write_all(&bytes))
        } else {
            std::fs::write(&target, &bytes)
        };
        if let Err(e) = res {
            return Err(format!("E212: Can't open file for writing: {} ({})", target.display(), e));
        }
        let b = self.buffers.get_mut(&id).unwrap();
        let was_new = b.path.is_none();
        if was_new && whole {
            b.path = Some(target.clone());
        }
        if whole && (is_own || was_new) {
            b.modified = false;
            b.mtime = std::fs::metadata(&target).and_then(|m| m.modified()).ok();
        }
        let name = target.to_string_lossy().replace('\\', "/");
        self.msg(format!("\"{}\" {}L, {}B written", name, nlines, bytes.len()));
        if id == cur && whole {
            if was_new {
                self.detect_filetype();
            }
            self.do_autocmd("BufWritePost", &subject);
        }
        Ok(())
    }

    /// Remove a buffer from the list, closing or replacing its windows.
    pub fn delete_buffer(&mut self, id: BufId, force: bool) -> Result<(), String> {
        let Some(b) = self.buffers.get(&id) else {
            return Err(format!("E516: No buffers were deleted: {}", id));
        };
        if b.modified && !force && !b.scratch {
            return Err(format!("E89: No write since last change for buffer {} (add ! to override)", id));
        }
        // Pick a replacement buffer for windows showing it.
        let replacement = self
            .win()
            .alt_buf
            .filter(|a| *a != id && self.buffers.contains_key(a))
            .or_else(|| self.buffers.keys().copied().find(|k| *k != id && self.buffers[k].listed));
        let replacement = match replacement {
            Some(r) => r,
            None => self.new_buffer(),
        };
        let wins: Vec<WinId> = self.windows.values().filter(|w| w.buf == id).map(|w| w.id).collect();
        for wid in wins {
            let last = self.buffers[&replacement].last_cursor;
            let w = self.windows.get_mut(&wid).unwrap();
            w.buf = replacement;
            w.cursor = last;
            w.topline = last.line.saturating_sub(w.height / 2);
            w.alt_buf = None;
        }
        for w in self.windows.values_mut() {
            if w.alt_buf == Some(id) {
                w.alt_buf = None;
            }
        }
        self.buffers.remove(&id);
        self.jumplist.retain(|j| j.0 != id);
        self.check_cursor();
        self.do_autocmd("BufEnter", "");
        Ok(())
    }

    // ------------------------------------------------------------ scrolling

    /// Width of the line number column of a window (0 when disabled).
    pub fn number_width(&self, w: &Window) -> usize {
        let b = &self.buffers[&w.buf];
        let nu = self.opt_for(Some(w), Some(b), "number").as_bool();
        let rnu = self.opt_for(Some(w), Some(b), "relativenumber").as_bool();
        if !nu && !rnu {
            return 0;
        }
        let digits = b.line_count().to_string().len();
        let nuw = self.opt_for(Some(w), Some(b), "numberwidth").as_num().max(1) as usize;
        (digits + 1).max(nuw)
    }

    pub fn text_width(&self, w: &Window) -> usize {
        w.width.saturating_sub(self.number_width(w)).max(1)
    }

    pub fn tabstop_of(&self, w: &Window) -> usize {
        self.opt_for(Some(w), self.buffers.get(&w.buf), "tabstop").as_num().max(1) as usize
    }

    pub fn ambi_double(&self) -> bool {
        self.opt_global("ambiwidth").as_string() == "double"
    }

    /// Number of screen rows a buffer line occupies in a window.
    pub fn line_rows(&self, w: &Window, lnum: usize) -> usize {
        let b = &self.buffers[&w.buf];
        if !self.opt_for(Some(w), Some(b), "wrap").as_bool() {
            return 1;
        }
        let ts = self.tabstop_of(w);
        let list = self.opt_for(Some(w), Some(b), "list").as_bool();
        let mut width = line_width(b.line(lnum), ts, self.ambi_double());
        if list && self.opt_global("listchars").as_string().split(',').any(|x| x.starts_with("eol:")) {
            width += 1;
        }
        let tw = self.text_width(w);
        width.max(1).div_ceil(tw)
    }

    /// Adjust topline/leftcol so the cursor is visible.
    pub fn scroll_to_cursor(&mut self) {
        let id = self.cur_win_id();
        let (mut topline, mut leftcol) = {
            let w = &self.windows[&id];
            (w.topline, w.leftcol)
        };
        let w = &self.windows[&id];
        let b = &self.buffers[&w.buf];
        let cur = w.cursor;
        let height = w.height.max(1);
        let so = (self.opt_for(Some(w), Some(b), "scrolloff").as_num().max(0) as usize).min((height.saturating_sub(1)) / 2);
        let last = b.last_line();
        topline = topline.min(last);
        if cur.line < topline + so {
            topline = cur.line.saturating_sub(so);
        } else {
            // rows from topline through the cursor line (+ scrolloff lines below)
            let bottom = (cur.line + so).min(last);
            let rows_between = |top: usize| -> usize { (top..=bottom).map(|l| self.line_rows(w, l)).sum() };
            if bottom.saturating_sub(topline) > height * 2 {
                topline = bottom.saturating_sub(height * 2);
            }
            let mut guard = 0;
            while topline < cur.line && rows_between(topline) > height && guard < 100_000 {
                topline += 1;
                guard += 1;
            }
        }
        let wrap = self.opt_for(Some(w), Some(b), "wrap").as_bool();
        if wrap {
            leftcol = 0;
        } else {
            let ts = self.tabstop_of(w);
            let vcol = vcol_of(b.line(cur.line), cur.col, ts, self.ambi_double());
            let tw = self.text_width(w);
            let siso = self.opt_global("sidescrolloff").as_num().max(0) as usize;
            let siso = siso.min(tw.saturating_sub(1) / 2);
            if vcol < leftcol + siso {
                leftcol = vcol.saturating_sub(siso);
            } else if vcol + siso + 1 > leftcol + tw {
                leftcol = vcol + siso + 1 - tw;
            }
        }
        let w = self.windows.get_mut(&id).unwrap();
        w.topline = topline;
        w.leftcol = leftcol;
    }

    /// Last buffer line (at least partly) visible in the window.
    pub fn botline(&self, w: &Window) -> usize {
        let b = &self.buffers[&w.buf];
        let mut rows = 0;
        let mut l = w.topline.min(b.last_line());
        loop {
            rows += self.line_rows(w, l);
            if rows >= w.height || l >= b.last_line() {
                return l;
            }
            l += 1;
        }
    }
}
