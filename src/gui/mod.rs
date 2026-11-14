//! GPUI front end: one window showing the editor's cell grid.

mod font;
mod keymap;

use std::ops::Range;
use std::time::Duration;

use gpui::{
    App, Application, Bounds, ClipboardItem, Context, CursorStyle, Element, ElementId, ElementInputHandler, Entity, EntityInputHandler,
    ExternalPaths, FocusHandle, Focusable, GlobalElementId, Hsla, InspectorElementId, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, Render, ScrollDelta, ScrollWheelEvent,
    ShapedLine, SharedString, StrikethroughStyle, Style, Task, TextRun, TitlebarOptions, UTF16Selection, UnderlineStyle, Window,
    WindowBounds, WindowOptions, div, fill, outline, point, prelude::*, px, relative, size,
};

use crate::editor::highlight::{Attr, Color};
use crate::editor::mouse::MouseState;
use crate::editor::screen::{CursorShape, Grid};
use crate::editor::{Editor, GuiRequest};
use crate::StartOptions;

use font::FontSet;

fn hsla(c: Color) -> Hsla {
    gpui::rgb(c.0).into()
}

fn f(p: Pixels) -> f32 {
    f32::from(p)
}

pub fn run(opts: StartOptions) {
    Application::new().run(move |cx: &mut App| {
        let mut ed = crate::new_editor();
        ed.gui_running = true;
        crate::load_config(&mut ed, &opts);
        let fonts = FontSet::from_options(&ed, cx.text_system());
        let cols = ed.opt_global("columns").as_num().clamp(12, 1000) as f32;
        let lines = ed.opt_global("lines").as_num().clamp(3, 500) as f32;
        let win_size = size(px(f(fonts.cell_w) * cols + 2.0), px(f(fonts.cell_h) * lines + 2.0));
        let bounds = match ed.win_pos {
            (x, y) if x > 0 && y > 0 => Bounds::new(point(px(x as f32), px(y as f32)), win_size),
            _ => Bounds::centered(None, win_size, cx),
        };
        ed.gui_requests.clear();
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions { title: Some("FeVim".into()), ..Default::default() }),
                    window_min_size: Some(size(px(200.), px(120.))),
                    app_id: Some("fevim".into()),
                    ..Default::default()
                },
                |window, cx| {
                    let view = cx.new(|cx| FeVim::new(ed, fonts, cx));
                    view.update(cx, |v, cx| v.start(&opts, window, cx));
                    view
                },
            )
            .expect("failed to open the FeVim window");
        window
            .update(cx, |view, window, cx| {
                window.focus(&view.focus);
                cx.activate(true);
            })
            .ok();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}

pub struct FeVim {
    ed: Editor,
    focus: FocusHandle,
    fonts: FontSet,
    /// Grid painted last; also used for mouse hit testing and the IME position.
    grid: Option<Grid>,
    bounds: Option<Bounds<Pixels>>,
    mouse: MouseState,
    map_timer: Option<Task<()>>,
    lua_timer: Option<Task<()>>,
    guifont: String,
    title: String,
}

impl Focusable for FeVim {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FeVim {
    fn new(ed: Editor, fonts: FontSet, cx: &mut Context<Self>) -> FeVim {
        let guifont = ed.opt_global("guifont").as_string();
        FeVim {
            ed,
            focus: cx.focus_handle(),
            fonts,
            grid: None,
            bounds: None,
            mouse: MouseState::default(),
            map_timer: None,
            lua_timer: None,
            guifont,
            title: String::new(),
        }
    }

    fn start(&mut self, opts: &StartOptions, window: &mut Window, cx: &mut Context<Self>) {
        // size the editor to the window before files are opened
        let vs = window.viewport_size();
        let (rows, cols) = self.fonts.grid_size(vs);
        self.ed.resize(rows, cols);
        crate::open_files(&mut self.ed, opts);
        // like gvim, show errors from the start-up files in a hit-enter prompt
        if self.ed.messages.iter().any(|m| m.group == "ErrorMsg") {
            self.ed.output = self.ed.messages.clone();
        }
        window.on_window_should_close(cx, {
            let entity = cx.entity();
            move |_window, cx| {
                entity.update(cx, |v, cx| {
                    let modified: Vec<String> = v.ed.buffers.values().filter(|b| b.modified && !b.scratch).map(|b| b.display_name()).collect();
                    if modified.is_empty() {
                        true
                    } else {
                        v.ed.emsg(format!("E37: No write since last change: {} (use :qa! to quit)", modified.join(", ")));
                        cx.notify();
                        false
                    }
                })
            }
        });
        self.after_input(window, cx);
    }

    fn sync_clipboard_in(&mut self, cx: &mut Context<Self>) {
        self.ed.clip_in = cx.read_from_clipboard().and_then(|c| c.text());
    }

    /// Feed keys to the editor and handle everything that follows.
    fn send_keys(&mut self, keys: &[crate::editor::keys::Key], window: &mut Window, cx: &mut Context<Self>) {
        self.map_timer = None;
        self.sync_clipboard_in(cx);
        self.ed.input(keys);
        self.after_input(window, cx);
    }

    fn after_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(t) = self.ed.clip_out.take() {
            cx.write_to_clipboard(ClipboardItem::new_string(t.clone()));
            self.ed.clip_in = Some(t);
        }
        let reqs = std::mem::take(&mut self.ed.gui_requests);
        for r in reqs {
            match r {
                GuiRequest::FontChanged => self.reload_font(window, cx),
                GuiRequest::Resize(lines, cols) => {
                    let w = f(self.fonts.cell_w) * cols as f32 + 2.0;
                    let h = f(self.fonts.cell_h) * lines as f32 + 2.0;
                    window.resize(size(px(w), px(h)));
                    self.ed.resize(lines, cols);
                }
                GuiRequest::Move(..) | GuiRequest::Bell | GuiRequest::Title => {}
            }
        }
        if self.ed.quit {
            window.remove_window();
            cx.quit();
            return;
        }
        // mapping timeout ('timeout' / 'timeoutlen')
        if self.ed.waiting_for_map() && self.ed.opt_global("timeout").as_bool() {
            let ms = self.ed.opt_global("timeoutlen").as_num().max(1) as u64;
            self.map_timer = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms)).await;
                this.update_in(cx, |v, window, cx| {
                    v.map_timer = None;
                    v.ed.map_timeout();
                    v.after_input(window, cx);
                })
                .ok();
            }));
        }
        // Lua timers (vim.defer_fn / timer_start)
        if self.lua_timer.is_none() {
            let pending = self.ed.script.clone().map(|h| h.tick(&mut self.ed)).unwrap_or(false);
            if pending {
                self.lua_timer = Some(cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(Duration::from_millis(20)).await;
                    this.update_in(cx, |v, window, cx| {
                        v.lua_timer = None;
                        v.ed.after_input();
                        v.after_input(window, cx);
                    })
                    .ok();
                }));
            }
        }
        let title = self.ed.window_title();
        if title != self.title {
            window.set_window_title(&title);
            self.title = title;
        }
        cx.notify();
    }

    fn reload_font(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gf = self.ed.opt_global("guifont").as_string();
        self.guifont = gf;
        self.fonts = FontSet::from_options(&self.ed, window.text_system());
        // keep 'lines' and 'columns': resize the window like gvim does
        let (rows, cols) = (self.ed.rows, self.ed.cols);
        let w = f(self.fonts.cell_w) * cols as f32 + 2.0;
        let h = f(self.fonts.cell_h) * rows as f32 + 2.0;
        window.resize(size(px(w), px(h)));
        cx.notify();
    }

    fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize) {
        let origin = self.bounds.map(|b| b.origin).unwrap_or_default();
        let x = (f(pos.x) - f(origin.x)).max(0.0);
        let y = (f(pos.y) - f(origin.y)).max(0.0);
        let col = (x / f(self.fonts.cell_w)) as usize;
        let row = (y / f(self.fonts.cell_h)) as usize;
        (row.min(self.ed.rows.saturating_sub(1)), col.min(self.ed.cols.saturating_sub(1)))
    }

    // ------------------------------------------------------------ events

    fn on_key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if std::env::var("FEVIM_DEBUG_KEYS").is_ok() {
            eprintln!("keydown key={:?} key_char={:?} mods={:?} text={}", ks.key, ks.key_char, ks.modifiers, keymap::is_text_input(ks));
        }
        // Printable text is delivered through the input handler (IME aware).
        if keymap::is_text_input(ks) {
            return;
        }
        if let Some(k) = keymap::to_key(ks) {
            cx.stop_propagation();
            self.send_keys(&[k], window, cx);
        }
    }

    fn on_mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus);
        let (row, col) = self.cell_at(ev.position);
        self.sync_clipboard_in(cx);
        let mut ms = self.mouse;
        self.ed.mouse_down(row, col, ev.click_count, ev.modifiers.shift, &mut ms);
        self.mouse = ms;
        self.ed.after_input();
        self.after_input(window, cx);
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
        if ev.pressed_button != Some(MouseButton::Left) || (!self.mouse.dragging && self.mouse.resizing.is_none()) {
            return;
        }
        let (row, col) = self.cell_at(ev.position);
        let mut ms = self.mouse;
        self.ed.mouse_drag(row, col, &mut ms);
        self.mouse = ms;
        self.after_input(window, cx);
    }

    fn on_mouse_up(&mut self, _ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mut ms = self.mouse;
        self.ed.mouse_up(&mut ms);
        self.mouse = ms;
        // with 'clipboard' containing "autoselect" Vim copies the selection; we
        // only need to keep the screen up to date
        self.after_input(window, cx);
    }

    fn on_scroll(&mut self, ev: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let lines = match ev.delta {
            ScrollDelta::Lines(p) => -(p.y * 3.0) as isize,
            ScrollDelta::Pixels(p) => -(f(p.y) / f(self.fonts.cell_h)).round() as isize,
        };
        if lines == 0 {
            return;
        }
        let (row, col) = self.cell_at(ev.position);
        self.ed.mouse_scroll(row, col, lines);
        self.ed.after_input();
        self.after_input(window, cx);
    }

    fn on_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        for p in paths.paths() {
            let path = p.to_string_lossy().replace('\\', "/").replace(' ', "\\ ");
            self.ed.exec_ex_reporting(&format!("drop {}", path).replace("drop", if p.is_dir() { "cd" } else { "edit" }));
        }
        self.ed.after_input();
        self.after_input(window, cx);
    }
}

impl Render for FeVim {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = hsla(self.ed.hl.normal_bg());
        div()
            .size_full()
            .bg(bg)
            .track_focus(&self.focus)
            .key_context("FeVim")
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .on_drop(cx.listener(Self::on_drop))
            .child(GridElement { view: cx.entity() })
    }
}

// ------------------------------------------------------------ IME input

impl EntityInputHandler for FeVim {
    fn text_for_range(&mut self, _range: Range<usize>, actual: &mut Option<Range<usize>>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<String> {
        *actual = Some(0..0);
        Some(String::new())
    }

    fn selected_text_range(&mut self, _ignore: bool, _w: &mut Window, _cx: &mut Context<Self>) -> Option<UTF16Selection> {
        let n = self.ed.ime_preedit.encode_utf16().count();
        Some(UTF16Selection { range: n..n, reversed: false })
    }

    fn marked_text_range(&self, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        if self.ed.ime_preedit.is_empty() {
            None
        } else {
            Some(0..self.ed.ime_preedit.encode_utf16().count())
        }
    }

    fn unmark_text(&mut self, _w: &mut Window, cx: &mut Context<Self>) {
        self.ed.ime_preedit.clear();
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _range: Option<Range<usize>>, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.ed.ime_preedit.clear();
        if std::env::var("FEVIM_DEBUG_KEYS").is_ok() {
            eprintln!("text input {:?}", text);
        }
        if text.is_empty() {
            cx.notify();
            return;
        }
        let keys = keymap::text_to_keys(text);
        self.send_keys(&keys, window, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _sel: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ed.ime_preedit = text.to_string();
        cx.notify();
    }

    fn bounds_for_range(&mut self, _range: Range<usize>, bounds: Bounds<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        let (row, col) = self.grid.as_ref().map(|g| g.cursor).unwrap_or((0, 0));
        let x = f(bounds.origin.x) + col as f32 * f(self.fonts.cell_w);
        let y = f(bounds.origin.y) + row as f32 * f(self.fonts.cell_h);
        Some(Bounds::new(point(px(x), px(y)), size(self.fonts.cell_w, self.fonts.cell_h)))
    }

    fn character_index_for_point(&mut self, _p: Point<Pixels>, _w: &mut Window, _cx: &mut Context<Self>) -> Option<usize> {
        None
    }
}

// ------------------------------------------------------------ grid element

struct GridElement {
    view: Entity<FeVim>,
}

impl IntoElement for GridElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

struct Prepaint {
    quads: Vec<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Vec<PaintQuad>,
    cursor_text: Option<(Point<Pixels>, ShapedLine)>,
    line_height: Pixels,
}

/// A run of cells with the same attributes.
struct Seg {
    col: usize,
    text: String,
    attr: Attr,
    wide: bool,
    cells: usize,
}

fn segments(g: &Grid, row: usize) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    for c in 0..g.cols {
        let cell = g.cell(row, c);
        if cell.width == 0 {
            continue;
        }
        let wide = cell.width == 2 || (cell.ch as u32) > 0x2000 && !cell.ch.is_ascii();
        match out.last_mut() {
            Some(s) if !wide && !s.wide && s.attr == cell.attr && s.col + s.cells == c => {
                s.text.push(cell.ch);
                s.cells += 1;
            }
            _ => out.push(Seg { col: c, text: cell.ch.to_string(), attr: cell.attr, wide, cells: cell.width.max(1) as usize }),
        }
    }
    out
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _id: Option<&GlobalElementId>, _iid: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _iid: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _rl: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let (grid, fonts, focused) = self.view.update(cx, |v, _cx| {
            if v.ed.opt_global("guifont").as_string() != v.guifont {
                v.guifont = v.ed.opt_global("guifont").as_string();
                v.fonts = FontSet::from_options(&v.ed, window.text_system());
            }
            let (rows, cols) = v.fonts.grid_size(bounds.size);
            if (rows, cols) != (v.ed.rows, v.ed.cols) {
                v.ed.resize(rows, cols);
                v.ed.do_autocmd("VimResized", "");
            }
            v.bounds = Some(bounds);
            let g = v.ed.render();
            let focused = v.focus.is_focused(window);
            (g, v.fonts.clone(), focused)
        });
        let cw = fonts.cell_w;
        let ch = fonts.cell_h;
        let origin = bounds.origin;
        let mut quads = Vec::new();
        let mut lines = Vec::new();
        let font_size = fonts.size;
        let shape = |window: &mut Window, text: &str, attr: &Attr, wide: bool, cells: usize| -> ShapedLine {
            let fg = hsla(attr.fg.unwrap_or(grid.default_fg));
            let font = fonts.font_for(attr, wide);
            let run = TextRun {
                len: text.len(),
                font,
                color: fg,
                background_color: None,
                underline: if attr.underline || attr.undercurl {
                    Some(UnderlineStyle { color: Some(attr.sp.map(hsla).unwrap_or(fg)), thickness: px(1.), wavy: attr.undercurl })
                } else {
                    None
                },
                strikethrough: if attr.strikethrough { Some(StrikethroughStyle { color: Some(fg), thickness: px(1.) }) } else { None },
            };
            let force = if wide { px(f(cw) * cells as f32) } else { cw };
            window.text_system().shape_line(SharedString::from(text.to_string()), font_size, &[run], Some(force))
        };
        for row in 0..grid.rows {
            let y = f(origin.y) + row as f32 * f(ch);
            for seg in segments(&grid, row) {
                let x = f(origin.x) + seg.col as f32 * f(cw);
                let bg = seg.attr.bg.unwrap_or(grid.default_bg);
                if bg != grid.default_bg {
                    quads.push(fill(Bounds::new(point(px(x), px(y)), size(px(f(cw) * seg.cells as f32), ch)), hsla(bg)));
                }
                if seg.text.chars().all(|c| c == ' ') && !seg.attr.underline && !seg.attr.undercurl {
                    continue;
                }
                let line = shape(window, &seg.text, &seg.attr, seg.wide, seg.cells);
                lines.push((point(px(x), px(y)), line));
            }
        }
        // cursor
        let mut cursor = Vec::new();
        let mut cursor_text = None;
        let (crow, ccol) = grid.cursor;
        if crow < grid.rows && ccol < grid.cols {
            let cell = *grid.cell(crow, ccol);
            let cells = if cell.width == 2 { 2 } else { 1 };
            let x = f(origin.x) + ccol as f32 * f(cw);
            let y = f(origin.y) + crow as f32 * f(ch);
            let ca = grid.cursor_attr;
            // the Cursor group is "reverse" by default: use the cell's colours swapped
            let (cbg, cfg) = if ca.bg.is_some() && ca.bg != Some(grid.default_bg) && ca.fg != ca.bg {
                (ca.bg.unwrap(), ca.fg.unwrap_or(grid.default_bg))
            } else {
                (cell.attr.fg.unwrap_or(grid.default_fg), cell.attr.bg.unwrap_or(grid.default_bg))
            };
            let full = Bounds::new(point(px(x), px(y)), size(px(f(cw) * cells as f32), ch));
            if !focused {
                cursor.push(outline(full, hsla(cbg), gpui::BorderStyle::Solid));
            } else {
                match grid.cursor_shape {
                    CursorShape::Block => {
                        cursor.push(fill(full, hsla(cbg)));
                        if cell.ch != ' ' {
                            let attr = Attr { fg: Some(cfg), bg: None, ..cell.attr };
                            let mut buf = String::new();
                            buf.push(cell.ch);
                            cursor_text = Some((point(px(x), px(y)), shape(window, &buf, &attr, cells == 2, cells)));
                        }
                    }
                    CursorShape::Vertical(pct) => {
                        let w = (f(cw) * pct as f32 / 100.0).max(2.0);
                        cursor.push(fill(Bounds::new(point(px(x), px(y)), size(px(w), ch)), hsla(cbg)));
                    }
                    CursorShape::Horizontal(pct) => {
                        let h = (f(ch) * pct as f32 / 100.0).max(2.0);
                        cursor.push(fill(Bounds::new(point(px(x), px(y + f(ch) - h)), size(px(f(cw) * cells as f32), px(h))), hsla(cbg)));
                    }
                }
            }
        }
        self.view.update(cx, |v, _| v.grid = Some(grid));
        Prepaint { quads, lines, cursor, cursor_text, line_height: ch }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _iid: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _rl: &mut (),
        pp: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.read(cx).focus.clone();
        window.handle_input(&focus, ElementInputHandler::new(bounds, self.view.clone()), cx);
        for q in pp.quads.drain(..) {
            window.paint_quad(q);
        }
        for (origin, line) in pp.lines.iter() {
            line.paint(*origin, pp.line_height, window, cx).ok();
        }
        for q in pp.cursor.drain(..) {
            window.paint_quad(q);
        }
        if let Some((origin, line)) = &pp.cursor_text {
            line.paint(*origin, pp.line_height, window, cx).ok();
        }
    }
}
