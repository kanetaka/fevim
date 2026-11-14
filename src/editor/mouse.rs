//! Mouse support (the front end converts pixels to screen cells).

use super::buffer::Pos;
use super::motion::{col_at_vcol, last_char_start};
use super::{Editor, Mode, VisualKind, WinId};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MouseState {
    pub dragging: bool,
    /// Window the drag started in.
    pub win: Option<WinId>,
    /// Dragging a status line / vertical separator: (window, vertical).
    pub resizing: Option<(WinId, bool)>,
}

impl Editor {
    fn mouse_enabled(&self) -> bool {
        let m = self.opt_global("mouse").as_string();
        if m.is_empty() {
            return false;
        }
        if m.contains('a') {
            return true;
        }
        let need = match self.mode {
            Mode::Insert | Mode::Replace => 'i',
            Mode::Visual(_) => 'v',
            Mode::Cmdline => 'c',
            Mode::Normal => 'n',
        };
        m.contains(need)
    }

    /// The window whose text area or status line contains the cell.
    pub fn window_at(&self, row: usize, col: usize) -> Option<(WinId, bool)> {
        for id in self.tab_wins() {
            let w = &self.windows[&id];
            if col >= w.col && col < w.col + w.width && row >= w.row && row < w.row + w.height + w.has_status as usize {
                return Some((id, row >= w.row + w.height));
            }
        }
        None
    }

    /// Buffer position shown at a screen cell of a window.
    pub fn screen_to_pos(&self, id: WinId, row: usize, col: usize) -> Pos {
        let w = &self.windows[&id];
        let b = &self.buffers[&w.buf];
        let numw = self.number_width(w);
        let textw = self.text_width(w);
        let ts = self.tabstop_of(w);
        let ambi = self.ambi_double();
        let wrap = self.opt_for(Some(w), Some(b), "wrap").as_bool();
        let row = row.saturating_sub(w.row).min(w.height.saturating_sub(1));
        let x = col.saturating_sub(w.col + numw);
        let mut r = 0;
        let mut l = w.topline.min(b.last_line());
        loop {
            let rows = self.line_rows(w, l);
            if row < r + rows || l >= b.last_line() {
                let sub = row.saturating_sub(r).min(rows.saturating_sub(1));
                let vcol = if wrap { sub * textw + x } else { w.leftcol + x };
                let line = b.line(l);
                let c = col_at_vcol(line, vcol, ts, ambi);
                return Pos::new(l, c.min(line.len()));
            }
            r += rows;
            l += 1;
        }
    }

    fn tab_at(&self, col: usize) -> Option<usize> {
        let mut c = 0;
        for (i, t) in self.tabs.iter().enumerate() {
            let w = &self.windows[&t.cur];
            let b = &self.buffers[&w.buf];
            let name = b.path.as_ref().and_then(|p| p.file_name()).map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| "[No Name]".into());
            let nwin = t.layout.leaves().len();
            let modified = t.layout.leaves().iter().any(|id| self.buffers[&self.windows[id].buf].modified);
            let mut len = 2 + super::statusline::display_width(&name);
            if nwin > 1 {
                len += nwin.to_string().len();
            }
            if modified {
                len += 1;
            }
            if nwin > 1 || modified {
                len += 1;
            }
            if col >= c && col < c + len {
                return Some(i);
            }
            c += len;
        }
        None
    }

    /// Left button pressed.
    pub fn mouse_down(&mut self, row: usize, col: usize, clicks: usize, shift: bool, ms: &mut MouseState) {
        if !self.mouse_enabled() {
            return;
        }
        if !self.output.is_empty() {
            self.output.clear();
            return;
        }
        if self.cmdline.is_some() {
            return;
        }
        if row < self.tabline_height() {
            if let Some(t) = self.tab_at(col) {
                self.goto_tab(t);
            }
            return;
        }
        let Some((id, on_status)) = self.window_at(row, col) else {
            // vertical separator?
            for wid in self.tab_wins() {
                let w = &self.windows[&wid];
                if col == w.col + w.width && row >= w.row && row < w.row + w.height + 1 {
                    ms.resizing = Some((wid, true));
                    return;
                }
            }
            return;
        };
        if on_status {
            ms.resizing = Some((id, false));
            self.goto_window(id);
            return;
        }
        if id != self.cur_win_id() {
            if matches!(self.mode, Mode::Visual(_)) {
                self.exit_visual();
            }
            self.goto_window(id);
        }
        let pos = self.screen_to_pos(id, row, col);
        ms.dragging = true;
        ms.win = Some(id);
        match self.mode {
            Mode::Insert | Mode::Replace => {
                let cur = self.cursor();
                self.buf_mut().close_undo(cur);
                self.set_cursor(pos);
                self.buf_mut().undo_cursor = pos;
                self.ins.start = pos;
            }
            _ => {
                if shift && matches!(self.mode, Mode::Normal | Mode::Visual(_)) {
                    if self.mode == Mode::Normal {
                        self.visual_start = self.cursor();
                        self.mode = Mode::Visual(VisualKind::Char);
                    }
                    self.set_cursor(pos);
                } else if clicks >= 3 {
                    self.set_cursor(Pos::new(pos.line, 0));
                    self.visual_start = self.cursor();
                    self.mode = Mode::Visual(VisualKind::Line);
                } else if clicks == 2 {
                    self.set_cursor(pos);
                    if let Some((s, e)) = self.word_under_cursor() {
                        self.visual_start = Pos::new(pos.line, s);
                        let l = self.line(pos.line);
                        let end = super::motion::prev_char_start(l, e);
                        self.set_cursor(Pos::new(pos.line, end));
                        self.mode = Mode::Visual(VisualKind::Char);
                    }
                } else {
                    if matches!(self.mode, Mode::Visual(_)) {
                        self.exit_visual();
                    }
                    self.pending.clear();
                    self.set_cursor(pos);
                }
            }
        }
        self.update_curswant();
        self.check_cursor();
        self.scroll_to_cursor();
    }

    /// Mouse moved with the left button held.
    pub fn mouse_drag(&mut self, row: usize, col: usize, ms: &mut MouseState) {
        if let Some((id, vertical)) = ms.resizing {
            let Some(w) = self.windows.get(&id) else {
                return;
            };
            if vertical {
                let want = col.saturating_sub(w.col);
                let cur = self.cur_win_id();
                self.tabs[self.cur_tab].cur = id;
                self.resize_window(true, 0, Some(want.max(1)));
                self.tabs[self.cur_tab].cur = cur;
            } else {
                let want = row.saturating_sub(w.row);
                self.resize_window(false, 0, Some(want.max(1)));
            }
            return;
        }
        if !ms.dragging || !self.mouse_enabled() {
            return;
        }
        let Some(id) = ms.win else {
            return;
        };
        if id != self.cur_win_id() {
            return;
        }
        // scroll when dragging above/below the window
        let (wrow, wheight) = {
            let w = &self.windows[&id];
            (w.row, w.height)
        };
        if row < wrow {
            self.scroll_lines(-1);
        } else if row >= wrow + wheight {
            self.scroll_lines(1);
        }
        let pos = self.screen_to_pos(id, row.clamp(wrow, wrow + wheight.saturating_sub(1)), col);
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            return;
        }
        if pos == self.cursor() && !matches!(self.mode, Mode::Visual(_)) {
            return;
        }
        if self.mode == Mode::Normal {
            self.visual_start = self.cursor();
            self.mode = Mode::Visual(VisualKind::Char);
        }
        let line = self.line(pos.line);
        let p = if pos.col >= line.len() && !line.is_empty() { Pos::new(pos.line, last_char_start(line)) } else { pos };
        self.set_cursor(p);
        self.update_curswant();
        self.scroll_to_cursor();
    }

    pub fn mouse_up(&mut self, ms: &mut MouseState) {
        ms.dragging = false;
        ms.resizing = None;
        ms.win = None;
    }

    /// Scroll the window under the mouse.
    pub fn mouse_scroll(&mut self, row: usize, col: usize, lines: isize) {
        if lines == 0 {
            return;
        }
        let target = self.window_at(row, col).map(|(id, _)| id).unwrap_or(self.cur_win_id());
        let cur = self.cur_win_id();
        if target != cur {
            // scroll another window without making it current
            self.tabs[self.cur_tab].cur = target;
            self.scroll_lines(lines);
            self.tabs[self.cur_tab].cur = cur;
        } else {
            self.scroll_lines(lines);
        }
    }

    fn update_curswant(&mut self) {
        let ts = self.tabstop_of(self.win());
        let v = super::motion::vcol_of(self.cur_line(), self.cursor().col, ts, self.ambi_double());
        self.win_mut().curswant = v;
    }
}
