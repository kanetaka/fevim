//! The editor core.  Everything in here is independent of the GUI: the front
//! end feeds keys in and paints the cell grid produced by `screen.rs`.

pub mod buffer;
pub mod cmdline;
pub mod ex;
pub mod excmds;
pub mod highlight;
pub mod insert;
pub mod keys;
pub mod motion;
pub mod mouse;
pub mod normal;
pub mod options;
pub mod options_table;
pub mod screen;
pub mod statusline;
pub mod syntax;
pub mod vregex;
pub mod window;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

pub use buffer::{Buffer, Pos};
use highlight::Highlights;
use keys::{Key, KeyCode, Sp};
use options::{OptDef, OptKind, OptScope, OptVal, Options};
pub use window::{Layout, TabPage, Window};

pub type BufId = usize;
pub type WinId = usize;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VisualKind {
    Char,
    Line,
    Block,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Replace,
    Visual(VisualKind),
    Cmdline,
}

/// Values exchanged between the editor core and the script engine.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

impl Value {
    pub fn to_display(&self) -> String {
        match self {
            Value::Nil => String::new(),
            Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
            Value::Int(n) => n.to_string(),
            Value::Float(f) => format!("{}", f),
            Value::Str(s) => s.clone(),
            Value::List(l) => {
                format!("[{}]", l.iter().map(|v| v.to_repr()).collect::<Vec<_>>().join(", "))
            }
            Value::Dict(d) => {
                format!("{{{}}}", d.iter().map(|(k, v)| format!("'{}': {}", k, v.to_repr())).collect::<Vec<_>>().join(", "))
            }
        }
    }
    pub fn to_repr(&self) -> String {
        match self {
            Value::Str(s) => format!("'{}'", s.replace('\'', "''")),
            other => other.to_display(),
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => s.trim().parse::<i64>().map(|n| n != 0).unwrap_or(false),
            Value::List(l) => !l.is_empty(),
            Value::Dict(d) => !d.is_empty(),
        }
    }
    pub fn as_int(&self) -> i64 {
        match self {
            Value::Bool(b) => *b as i64,
            Value::Int(n) => *n,
            Value::Float(f) => *f as i64,
            Value::Str(s) => {
                let t = s.trim();
                let end = t.char_indices().find(|(i, c)| !(c.is_ascii_digit() || (*i == 0 && *c == '-'))).map(|(i, _)| i).unwrap_or(t.len());
                t[..end].parse().unwrap_or(0)
            }
            _ => 0,
        }
    }
}

/// Interface to the Lua/Vim script engine (implemented in `crate::script`).
pub trait ScriptHost {
    /// Evaluate a Vim script expression.
    fn eval(&self, ed: &mut Editor, expr: &str) -> Result<Value, String>;
    /// Execute Vim script source (one or more lines).
    fn exec_vim(&self, ed: &mut Editor, src: &str, name: &str) -> Result<(), String>;
    /// Execute Lua source.
    fn exec_lua(&self, ed: &mut Editor, src: &str, name: &str) -> Result<(), String>;
    /// Call a Lua function previously registered by the script side.
    fn call_ref(&self, ed: &mut Editor, id: usize, args: Vec<Value>) -> Result<Value, String>;
    fn get_global(&self, ed: &mut Editor, name: &str) -> Option<Value>;
    fn set_global(&self, ed: &mut Editor, name: &str, value: Value);
    fn function_exists(&self, ed: &mut Editor, name: &str) -> bool;
    /// Convert Vim script to Lua source (used by `:ImportVimrc`).
    fn transpile(&self, src: &str, name: &str) -> Result<String, String>;
    /// Run scheduled callbacks / due timers; returns true while timers are pending.
    fn tick(&self, _ed: &mut Editor) -> bool {
        false
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegKind {
    Char,
    Line,
    Block,
}

#[derive(Clone, Debug)]
pub struct Register {
    pub text: String,
    pub kind: RegKind,
}

pub const MODE_N: u8 = 1;
pub const MODE_X: u8 = 2;
pub const MODE_S: u8 = 4;
pub const MODE_O: u8 = 8;
pub const MODE_I: u8 = 16;
pub const MODE_C: u8 = 32;

#[derive(Clone, Debug)]
pub enum MapRhs {
    Keys(Vec<Key>),
    /// Lua function registered with the script host.
    Lua(usize),
}

#[derive(Clone, Debug)]
pub struct Mapping {
    pub modes: u8,
    pub lhs: Vec<Key>,
    pub rhs: MapRhs,
    /// Original right hand side text (for `:map` listings and `<expr>`).
    pub rhs_text: String,
    pub noremap: bool,
    pub silent: bool,
    pub expr: bool,
    pub nowait: bool,
    pub buffer: Option<BufId>,
}

#[derive(Clone, Debug)]
pub enum AutoAction {
    Ex(String),
    Lua(usize),
}

#[derive(Clone, Debug)]
pub struct AutoCmd {
    pub id: usize,
    pub event: String,
    pub pattern: String,
    pub group: Option<String>,
    pub action: AutoAction,
    pub once: bool,
    pub buffer: Option<BufId>,
}

#[derive(Clone, Debug)]
pub struct UserCmd {
    pub name: String,
    pub repl: AutoAction,
    pub nargs: char,
    pub bang: bool,
    pub range: bool,
    pub bar: bool,
}

#[derive(Clone, Debug)]
struct TKey {
    key: Key,
    remap: bool,
    silent: bool,
}

/// Requests from the core to the front end.
#[derive(Clone, Debug, PartialEq)]
pub enum GuiRequest {
    FontChanged,
    /// Resize the window to `lines` x `columns` cells.
    Resize(usize, usize),
    Move(i32, i32),
    Bell,
    Title,
}

#[derive(Clone, Debug)]
pub struct Msg {
    pub text: String,
    pub group: &'static str,
}

/// Popup-menu completion state.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub items: Vec<String>,
    pub selected: Option<usize>,
    /// Start of the word being completed.
    pub start: Pos,
    pub base: String,
}

/// Information needed to repeat the last change with `.`.
#[derive(Clone, Debug, Default)]
pub struct DotRepeat {
    pub keys: Vec<Key>,
    /// For visual-mode changes: the shape of the selection.
    pub visual: Option<(VisualKind, usize, usize)>,
}

pub struct Editor {
    pub buffers: BTreeMap<BufId, Buffer>,
    next_buf_id: BufId,
    pub windows: HashMap<WinId, Window>,
    next_win_id: WinId,
    pub tabs: Vec<TabPage>,
    pub cur_tab: usize,
    pub mode: Mode,
    pub opts: Options,
    pub hl: Highlights,
    pub registers: HashMap<char, Register>,
    pub maps: Vec<Mapping>,
    pub autocmds: Vec<AutoCmd>,
    pub augroups: Vec<String>,
    pub cur_augroup: Option<String>,
    next_autocmd_id: usize,
    autocmd_depth: usize,
    pub user_cmds: BTreeMap<String, UserCmd>,
    pub script: Option<Rc<dyn ScriptHost>>,

    // ---- screen
    pub rows: usize,
    pub cols: usize,

    // ---- input state
    typeahead: VecDeque<TKey>,
    map_depth: usize,
    /// Keys of the normal-mode command being typed.
    pub pending: Vec<Key>,
    pub pending_state: normal::PendingState,
    pub cmdline: Option<cmdline::CmdLine>,
    /// Expression prompt (`<C-r>=`) stacked on insert or cmdline mode.
    pub expr_prompt: Option<cmdline::CmdLine>,
    pub visual_start: Pos,
    pub visual_block_dollar: bool,
    pub last_visual: Option<(VisualKind, Pos, Pos)>,
    pub ins: insert::InsertState,
    pub completion: Option<Completion>,
    pub cur_key_silent: bool,
    /// Operator waiting for a `/` or `?` search to finish.
    pub pending_search_op: Option<Vec<Key>>,
    pub restart_insert: bool,

    // ---- search
    pub last_search: Option<String>,
    pub search_forward: bool,
    pub hlsearch_off: bool,
    pub incsearch_match: Option<(Pos, usize)>,
    pub last_sub: Option<(String, String, String)>,
    pub last_ft: Option<(char, char)>,
    regex_cache: HashMap<(String, bool, bool), Result<vregex::VimRegex, String>>,

    // ---- repeat / macros
    pub dot: DotRepeat,
    pub dot_recording: Option<Vec<Key>>,
    pub replaying_dot: bool,
    pub recording: Option<(char, Vec<Key>)>,
    pub last_macro: Option<char>,
    pub last_cmdline: Option<String>,
    pub last_insert: String,

    // ---- messages
    pub messages: Vec<Msg>,
    pub msg_line: Option<Msg>,
    /// Multi-line command output waiting for a key press.
    pub output: Vec<Msg>,
    collecting: Option<Vec<Msg>>,
    pub silent_depth: usize,
    pub echohl: &'static str,

    // ---- misc
    pub quit: bool,
    pub exit_code: i32,
    pub gui_requests: Vec<GuiRequest>,
    pub clip_in: Option<String>,
    pub clip_out: Option<String>,
    pub cmd_history: Vec<String>,
    pub search_history: Vec<String>,
    pub jumplist: Vec<(BufId, Pos)>,
    pub jumpidx: usize,
    pub gui_running: bool,
    pub filetype_on: bool,
    pub syntax_on: bool,
    pub colors_name: Option<String>,
    pub runtime_dirs: Vec<PathBuf>,
    pub win_pos: (i32, i32),
    /// Set when the next redraw must repaint everything (always true for now).
    pub starting: bool,
    pub ime_preedit: String,
    pub prev_tab: usize,
    pub dot_skip: bool,
    pub dot_visual_pending: Option<(VisualKind, usize, usize)>,
    pub cmdmod_vertical: bool,
    pub cmdmod_tab: bool,
    pub prev_cwd: Option<String>,
    pub last_bang: Option<String>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Editor {
        let mut ed = Editor {
            buffers: BTreeMap::new(),
            next_buf_id: 1,
            windows: HashMap::new(),
            next_win_id: 1000,
            tabs: Vec::new(),
            cur_tab: 0,
            mode: Mode::Normal,
            opts: Options::new(),
            hl: Highlights::new(),
            registers: HashMap::new(),
            maps: Vec::new(),
            autocmds: Vec::new(),
            augroups: Vec::new(),
            cur_augroup: None,
            next_autocmd_id: 1,
            autocmd_depth: 0,
            user_cmds: BTreeMap::new(),
            script: None,
            rows: 40,
            cols: 120,
            typeahead: VecDeque::new(),
            map_depth: 0,
            pending: Vec::new(),
            pending_state: normal::PendingState::Fresh,
            cmdline: None,
            expr_prompt: None,
            visual_start: Pos::default(),
            visual_block_dollar: false,
            last_visual: None,
            ins: insert::InsertState::default(),
            completion: None,
            cur_key_silent: false,
            pending_search_op: None,
            restart_insert: false,
            last_search: None,
            search_forward: true,
            hlsearch_off: false,
            incsearch_match: None,
            last_sub: None,
            last_ft: None,
            regex_cache: HashMap::new(),
            dot: DotRepeat::default(),
            dot_recording: None,
            replaying_dot: false,
            recording: None,
            last_macro: None,
            last_cmdline: None,
            last_insert: String::new(),
            messages: Vec::new(),
            msg_line: None,
            output: Vec::new(),
            collecting: None,
            silent_depth: 0,
            echohl: "",
            quit: false,
            exit_code: 0,
            gui_requests: Vec::new(),
            clip_in: None,
            clip_out: None,
            cmd_history: Vec::new(),
            search_history: Vec::new(),
            jumplist: Vec::new(),
            jumpidx: 0,
            gui_running: false,
            filetype_on: true,
            syntax_on: true,
            colors_name: None,
            runtime_dirs: Vec::new(),
            win_pos: (0, 0),
            starting: true,
            ime_preedit: String::new(),
            prev_tab: 0,
            dot_skip: false,
            dot_visual_pending: None,
            cmdmod_vertical: false,
            cmdmod_tab: false,
            prev_cwd: None,
            last_bang: None,
        };
        let buf = ed.new_buffer();
        let win = ed.new_window(buf);
        ed.tabs.push(TabPage { layout: Layout::Leaf(win), cur: win, prev: win });
        ed.layout();
        ed
    }

    // ------------------------------------------------------------ accessors

    pub fn new_buffer(&mut self) -> BufId {
        let id = self.next_buf_id;
        self.next_buf_id += 1;
        self.buffers.insert(id, Buffer::new(id));
        id
    }

    pub fn cur_win_id(&self) -> WinId {
        self.tabs[self.cur_tab].cur
    }

    pub fn win(&self) -> &Window {
        &self.windows[&self.cur_win_id()]
    }

    pub fn win_mut(&mut self) -> &mut Window {
        let id = self.cur_win_id();
        self.windows.get_mut(&id).unwrap()
    }

    pub fn cur_buf_id(&self) -> BufId {
        self.win().buf
    }

    pub fn buf(&self) -> &Buffer {
        &self.buffers[&self.win().buf]
    }

    pub fn buf_mut(&mut self) -> &mut Buffer {
        let id = self.win().buf;
        self.buffers.get_mut(&id).unwrap()
    }

    pub fn cursor(&self) -> Pos {
        self.win().cursor
    }

    pub fn set_cursor(&mut self, pos: Pos) {
        let pos = self.buf().clamp(pos);
        self.win_mut().cursor = pos;
    }

    pub fn line(&self, l: usize) -> &str {
        self.buf().line(l)
    }

    pub fn cur_line(&self) -> &str {
        self.buf().line(self.cursor().line)
    }

    // -------------------------------------------------------------- options

    /// Value of an option for a given window/buffer.
    pub fn opt_for(&self, win: Option<&Window>, buf: Option<&Buffer>, name: &str) -> OptVal {
        let Some(def) = options::find_def(name) else {
            return OptVal::Num(0);
        };
        match def.scope {
            OptScope::Win => {
                if let Some(v) = win.and_then(|w| w.opts.get(def.name)) {
                    return v.clone();
                }
            }
            OptScope::Buf => {
                if let Some(v) = buf.and_then(|b| b.opts.get(def.name)) {
                    return v.clone();
                }
            }
            OptScope::Global => {}
        }
        self.opts.global.get(def.name).cloned().unwrap_or(OptVal::Num(0))
    }

    pub fn opt(&self, name: &str) -> OptVal {
        self.opt_for(Some(self.win()), Some(self.buf()), name)
    }

    pub fn opt_bool(&self, name: &str) -> bool {
        self.opt(name).as_bool()
    }

    pub fn opt_num(&self, name: &str) -> i64 {
        self.opt(name).as_num()
    }

    pub fn opt_str(&self, name: &str) -> String {
        self.opt(name).as_string()
    }

    pub fn opt_global(&self, name: &str) -> OptVal {
        options::find_def(name).and_then(|d| self.opts.global.get(d.name).cloned()).unwrap_or(OptVal::Num(0))
    }

    /// Set an option.  `scope`: 'b' both (`:set`), 'l' local, 'g' global.
    pub fn set_opt(&mut self, def: &'static OptDef, val: OptVal, scope: char) -> Result<(), String> {
        let val = match (def.kind, val) {
            (OptKind::Bool, OptVal::Num(n)) => OptVal::Bool(n != 0),
            (OptKind::Bool, OptVal::Str(s)) => OptVal::Bool(!(s.is_empty() || s == "0")),
            (OptKind::Num, OptVal::Bool(b)) => OptVal::Num(b as i64),
            (OptKind::Num, OptVal::Str(s)) => options::parse_value(def, &s)?,
            (OptKind::Str, OptVal::Num(n)) => OptVal::Str(n.to_string()),
            (OptKind::Str, OptVal::Bool(b)) => OptVal::Str((b as i64).to_string()),
            (_, v) => v,
        };
        match def.name {
            "tabstop" if val.as_num() <= 0 => return Err("E487: Argument must be positive: tabstop".into()),
            "shiftwidth" | "softtabstop" | "scrolloff" | "textwidth" if val.as_num() < -1 => {
                return Err(format!("E487: Argument must be positive: {}", def.name));
            }
            "fileformat" if !matches!(val.as_string().as_str(), "unix" | "dos" | "mac") => {
                return Err("E474: Invalid argument: fileformat".into());
            }
            _ => {}
        }
        let old = self.opt(def.name);
        if scope != 'l' || def.scope == OptScope::Global {
            self.opts.global.insert(def.name, val.clone());
        }
        match def.scope {
            OptScope::Win => {
                if scope == 'l' {
                    self.win_mut().opts.insert(def.name, val.clone());
                } else if scope == 'b' {
                    self.win_mut().opts.remove(def.name);
                }
            }
            OptScope::Buf => {
                if scope == 'l' {
                    self.buf_mut().opts.insert(def.name, val.clone());
                } else if scope == 'b' {
                    // `:set` changes the local value too; buffer specific
                    // options such as 'filetype' never leak into other buffers.
                    if is_buffer_private(def.name) {
                        self.buf_mut().opts.insert(def.name, val.clone());
                        self.opts.global.insert(def.name, Options::default_value(def));
                    } else {
                        self.buf_mut().opts.remove(def.name);
                    }
                }
            }
            OptScope::Global => {}
        }
        if old != val || matches!(def.name, "filetype" | "syntax" | "hlsearch") {
            self.option_changed(def.name, &old);
        }
        Ok(())
    }

    pub fn set_opt_by_name(&mut self, name: &str, val: OptVal, scope: char) -> Result<(), String> {
        let def = options::find_def(name).ok_or_else(|| format!("E518: Unknown option: {}", name))?;
        self.set_opt(def, val, scope)
    }

    fn option_changed(&mut self, name: &str, _old: &OptVal) {
        match name {
            "filetype" => {
                let ft = self.opt_str("filetype");
                let id = self.cur_buf_id();
                self.buffers.get_mut(&id).unwrap().syn_states.clear();
                self.do_autocmd("FileType", &ft);
            }
            "syntax" => {
                self.buf_mut().syn_states.clear();
            }
            "guifont" | "guifontwide" | "linespace" => self.gui_requests.push(GuiRequest::FontChanged),
            "lines" | "columns" => {
                let l = self.opt_global("lines").as_num().max(3) as usize;
                let c = self.opt_global("columns").as_num().max(12) as usize;
                if (l, c) != (self.rows, self.cols) {
                    self.gui_requests.push(GuiRequest::Resize(l, c));
                    if !self.gui_running {
                        self.resize(l, c);
                    }
                }
            }
            "background" => {
                if self.colors_name.is_none() {
                    let dark = self.opt_str("background") != "light";
                    self.hl.load_builtin("default", dark);
                }
            }
            "hlsearch" => self.hlsearch_off = false,
            "ignorecase" | "smartcase" | "magic" => self.regex_cache.clear(),
            "laststatus" | "showtabline" | "cmdheight" => {
                self.layout();
            }
            "title" | "titlestring" => self.gui_requests.push(GuiRequest::Title),
            _ => {}
        }
    }

    pub fn resize(&mut self, rows: usize, cols: usize) {
        self.rows = rows.max(3);
        self.cols = cols.max(12);
        self.opts.global.insert("lines", OptVal::Num(self.rows as i64));
        self.opts.global.insert("columns", OptVal::Num(self.cols as i64));
        self.layout();
        self.scroll_to_cursor();
    }

    // ------------------------------------------------------------- messages

    pub fn msg(&mut self, text: impl Into<String>) {
        self.msg_hl(text, "");
    }

    pub fn emsg(&mut self, text: impl Into<String>) {
        let text = text.into();
        if self.silent_depth > 1 {
            return; // `:silent!`
        }
        self.push_msg(text, "ErrorMsg", true);
    }

    pub fn msg_hl(&mut self, text: impl Into<String>, group: &'static str) {
        if self.silent_depth > 0 {
            return;
        }
        self.push_msg(text.into(), group, false);
    }

    fn push_msg(&mut self, text: String, group: &'static str, history: bool) {
        for line in text.split('\n') {
            let m = Msg { text: line.replace('\t', "    "), group };
            if history {
                self.messages.push(m.clone());
                if self.messages.len() > 500 {
                    self.messages.remove(0);
                }
            }
            match &mut self.collecting {
                Some(c) => c.push(m),
                None => self.msg_line = Some(m),
            }
        }
    }

    /// Messages that should be remembered in `:messages` (`:echomsg`).
    pub fn msg_history(&mut self, text: impl Into<String>, group: &'static str) {
        if self.silent_depth > 0 {
            return;
        }
        self.push_msg(text.into(), group, true);
    }

    /// Start collecting messages produced by one command.
    pub fn begin_collect(&mut self) -> bool {
        if self.collecting.is_none() {
            self.collecting = Some(Vec::new());
            true
        } else {
            false
        }
    }

    /// Finish collecting: one message goes to the message line, several are
    /// shown as a "Press ENTER" block.
    pub fn end_collect(&mut self) {
        if let Some(msgs) = self.collecting.take() {
            let width = self.cols.saturating_sub(12).max(10);
            let long = msgs.len() == 1 && msgs[0].text.chars().count() > width && self.gui_running;
            if msgs.len() == 1 && !long {
                self.msg_line = msgs.into_iter().next();
            } else if !msgs.is_empty() {
                self.msg_line = None;
                self.output.extend(msgs);
            }
        }
    }

    pub fn clear_msg(&mut self) {
        self.msg_line = None;
    }

    pub fn bell(&mut self) {
        if !self.opt_bool("visualbell") || !self.opt_str("t_vb").is_empty() {
            self.gui_requests.push(GuiRequest::Bell);
        }
    }

    // ------------------------------------------------------------ registers

    fn clipboard_is_unnamed(&self) -> bool {
        let cb = self.opt_global("clipboard").as_string();
        cb.split(',').any(|x| x == "unnamed" || x == "unnamedplus")
    }

    pub fn reg_get(&mut self, name: char) -> Option<Register> {
        let name = if name.is_ascii_uppercase() { name.to_ascii_lowercase() } else { name };
        match name {
            '_' => None,
            '+' | '*' => self.clipboard_register(),
            '"' => {
                if self.clipboard_is_unnamed() {
                    if let Some(r) = self.clipboard_register() {
                        return Some(r);
                    }
                }
                self.registers.get(&'"').cloned()
            }
            '/' => self.last_search.clone().map(|t| Register { text: t, kind: RegKind::Char }),
            ':' => self.last_cmdline.clone().map(|t| Register { text: t, kind: RegKind::Char }),
            '.' => Some(Register { text: self.last_insert.clone(), kind: RegKind::Char }),
            '%' => self.buf().path.as_ref().map(|_| Register { text: self.buf().display_name(), kind: RegKind::Char }),
            '#' => {
                let alt = self.win().alt_buf?;
                let b = self.buffers.get(&alt)?;
                b.path.as_ref().map(|_| Register { text: b.display_name(), kind: RegKind::Char })
            }
            c => self.registers.get(&c).cloned(),
        }
    }

    fn clipboard_register(&mut self) -> Option<Register> {
        let text = self.clip_out.clone().or_else(|| self.clip_in.clone())?;
        // If the clipboard still holds what we put there, keep our register kind.
        if let Some(r) = self.registers.get(&'+') {
            let ours = if r.kind == RegKind::Line { format!("{}\n", r.text) } else { r.text.clone() };
            if ours.replace("\r\n", "\n") == text.replace("\r\n", "\n") {
                return Some(r.clone());
            }
        }
        let text = text.replace("\r\n", "\n");
        if let Some(t) = text.strip_suffix('\n') {
            Some(Register { text: t.to_string(), kind: RegKind::Line })
        } else {
            Some(Register { text, kind: RegKind::Char })
        }
    }

    pub fn reg_set(&mut self, name: char, text: String, kind: RegKind) {
        match name {
            '_' => {}
            '/' => {
                self.last_search = Some(text);
                self.hlsearch_off = false;
            }
            '+' | '*' => {
                let out = if kind == RegKind::Line { format!("{}\n", text) } else { text.clone() };
                self.clip_out = Some(out);
                self.registers.insert('+', Register { text, kind });
            }
            c if c.is_ascii_uppercase() => {
                let l = c.to_ascii_lowercase();
                let entry = self.registers.entry(l).or_insert(Register { text: String::new(), kind });
                if kind == RegKind::Line || entry.kind == RegKind::Line {
                    if !entry.text.is_empty() {
                        entry.text.push('\n');
                    }
                    entry.kind = RegKind::Line;
                }
                entry.text.push_str(&text);
            }
            '"' => {
                self.registers.insert('"', Register { text: text.clone(), kind });
                if self.clipboard_is_unnamed() {
                    self.reg_set('+', text, kind);
                }
            }
            c => {
                self.registers.insert(c, Register { text, kind });
            }
        }
    }

    /// Store text produced by a yank or delete following Vim's register rules.
    pub fn store_register(&mut self, named: Option<char>, text: String, kind: RegKind, is_delete: bool) {
        if named == Some('_') {
            return;
        }
        if let Some(n) = named {
            if n != '"' {
                self.reg_set(n, text.clone(), kind);
                let t = self.reg_get(n).map(|r| r.text).unwrap_or(text);
                self.registers.insert('"', Register { text: t, kind });
                return;
            }
        }
        if is_delete {
            if kind != RegKind::Char || text.contains('\n') {
                for i in (1..9).rev() {
                    let from = char::from_digit(i, 10).unwrap();
                    let to = char::from_digit(i + 1, 10).unwrap();
                    if let Some(r) = self.registers.get(&from).cloned() {
                        self.registers.insert(to, r);
                    }
                }
                self.registers.insert('1', Register { text: text.clone(), kind });
            } else {
                self.registers.insert('-', Register { text: text.clone(), kind });
            }
        } else {
            self.registers.insert('0', Register { text: text.clone(), kind });
        }
        self.reg_set('"', text, kind);
    }

    // --------------------------------------------------------------- regex

    pub fn compile_pattern(&mut self, pat: &str) -> Result<vregex::VimRegex, String> {
        let ic = self.opt_global("ignorecase").as_bool();
        let sc = self.opt_global("smartcase").as_bool();
        let key = (pat.to_string(), ic, sc);
        if let Some(r) = self.regex_cache.get(&key) {
            return r.clone();
        }
        let r = vregex::VimRegex::new(pat, ic, sc);
        if self.regex_cache.len() > 200 {
            self.regex_cache.clear();
        }
        self.regex_cache.insert(key, r.clone());
        r
    }

    // ------------------------------------------------------------- mappings

    pub fn mode_bits(&self) -> u8 {
        if self.expr_prompt.is_some() || self.cmdline.is_some() {
            return MODE_C;
        }
        match self.mode {
            Mode::Normal => match self.pending_state {
                normal::PendingState::Fresh => MODE_N,
                normal::PendingState::Operator => MODE_O,
                normal::PendingState::NoMap => 0,
            },
            Mode::Visual(_) => match self.pending_state {
                normal::PendingState::Fresh => MODE_X,
                normal::PendingState::Operator => MODE_O,
                normal::PendingState::NoMap => 0,
            },
            Mode::Insert | Mode::Replace => {
                if self.ins.literal_next || self.ins.pending_register {
                    0
                } else {
                    MODE_I
                }
            }
            Mode::Cmdline => MODE_C,
        }
    }

    pub fn add_mapping(&mut self, m: Mapping) {
        // A mapping replaces existing ones with the same lhs in its modes.
        let lhs = m.lhs.clone();
        let (modes, buffer) = (m.modes, m.buffer);
        for old in self.maps.iter_mut() {
            if old.lhs == lhs && old.buffer == buffer {
                old.modes &= !modes;
            }
        }
        self.maps.retain(|x| x.modes != 0);
        self.maps.push(m);
    }

    pub fn del_mapping(&mut self, modes: u8, lhs: &[Key], buffer: Option<BufId>) -> bool {
        let mut found = false;
        for old in self.maps.iter_mut() {
            if old.lhs == lhs && old.buffer == buffer && old.modes & modes != 0 {
                old.modes &= !modes;
                found = true;
            }
        }
        self.maps.retain(|x| x.modes != 0);
        found
    }

    /// Queue keys as if typed.
    pub fn feed(&mut self, keys: &[Key], remap: bool) {
        for k in keys {
            self.typeahead.push_back(TKey { key: *k, remap, silent: false });
        }
    }

    fn feed_front(&mut self, keys: &[Key], remap: bool, silent: bool, noremap_prefix: usize) {
        for (i, k) in keys.iter().enumerate().rev() {
            self.typeahead.push_front(TKey { key: *k, remap: remap && i >= noremap_prefix, silent });
        }
    }

    pub fn has_typeahead(&self) -> bool {
        !self.typeahead.is_empty()
    }

    /// True when keys are waiting for a mapping timeout.
    pub fn waiting_for_map(&self) -> bool {
        !self.typeahead.is_empty()
    }

    /// Feed keys and process them (the entry point for the front end).
    pub fn input(&mut self, keys: &[Key]) {
        self.feed(keys, true);
        self.process(false);
    }

    /// Called by the front end when 'timeoutlen' expired with keys pending.
    pub fn map_timeout(&mut self) {
        self.process(true);
    }

    /// Process queued keys.  With `flush`, ambiguous mappings are resolved now.
    pub fn process(&mut self, flush: bool) {
        self.map_depth = 0;
        let mut guard = 0usize;
        while !self.typeahead.is_empty() && !self.quit {
            guard += 1;
            if guard > 2_000_000 {
                self.typeahead.clear();
                self.emsg("E223: Recursive mapping");
                break;
            }
            match self.try_mapping(flush) {
                MapResult::Applied => {
                    self.map_depth += 1;
                    if self.map_depth > self.opt_global("maxmapdepth").as_num().max(10) as usize {
                        self.typeahead.clear();
                        self.emsg("E223: Recursive mapping");
                        break;
                    }
                    continue;
                }
                MapResult::Wait => break,
                MapResult::None => {}
            }
            self.map_depth = 0;
            let t = self.typeahead.pop_front().unwrap();
            self.cur_key_silent = t.silent;
            self.handle_key(t.key);
        }
        self.cur_key_silent = false;
        self.after_input();
        if let Some(h) = self.script.clone() {
            h.tick(self);
            self.after_input();
        }
    }

    /// Run Ex commands and return the messages they produce (`execute()`).
    pub fn capture(&mut self, cmd: &str) -> Result<String, String> {
        let saved = self.collecting.replace(Vec::new());
        let r = self.exec_lines(cmd, "capture");
        let msgs = std::mem::replace(&mut self.collecting, saved).unwrap_or_default();
        r?;
        let mut out = String::new();
        for m in msgs {
            out.push('\n');
            out.push_str(&m.text);
        }
        Ok(out)
    }

    fn try_mapping(&mut self, flush: bool) -> MapResult {
        let bits = self.mode_bits();
        if bits == 0 || self.maps.is_empty() {
            return MapResult::None;
        }
        if !self.typeahead.front().map(|t| t.remap).unwrap_or(false) {
            return MapResult::None;
        }
        let avail: Vec<Key> = self.typeahead.iter().take_while(|t| t.remap).map(|t| t.key).collect();
        let exhausted = avail.len() == self.typeahead.len();
        let cur_buf = self.cur_buf_id();
        let mut best: Option<usize> = None;
        let mut partial = false;
        for (i, m) in self.maps.iter().enumerate() {
            if m.modes & bits == 0 {
                continue;
            }
            if let Some(b) = m.buffer {
                if b != cur_buf {
                    continue;
                }
            }
            if m.lhs.len() <= avail.len() {
                if avail[..m.lhs.len()] == m.lhs[..] {
                    let better = match best {
                        None => true,
                        Some(b) => {
                            let bm = &self.maps[b];
                            m.lhs.len() > bm.lhs.len() || (m.lhs.len() == bm.lhs.len() && m.buffer.is_some())
                        }
                    };
                    if better {
                        best = Some(i);
                    }
                }
            } else if exhausted && m.lhs[..avail.len()] == avail[..] {
                partial = true;
            }
        }
        if partial && !flush {
            if let Some(b) = best {
                if !self.maps[b].nowait {
                    return MapResult::Wait;
                }
            } else {
                return MapResult::Wait;
            }
        }
        let Some(b) = best else {
            return MapResult::None;
        };
        let m = self.maps[b].clone();
        let silent = m.silent || self.typeahead.front().map(|t| t.silent).unwrap_or(false);
        for _ in 0..m.lhs.len() {
            self.typeahead.pop_front();
        }
        let rhs: Vec<Key> = match (&m.rhs, m.expr) {
            (MapRhs::Keys(k), false) => k.clone(),
            (MapRhs::Keys(_), true) => match self.eval_expr(&m.rhs_text) {
                Ok(v) => keys::parse_keys_raw(&v.to_display()),
                Err(e) => {
                    self.emsg(e);
                    Vec::new()
                }
            },
            (MapRhs::Lua(id), expr) => {
                let host = self.script.clone();
                match host {
                    Some(h) => match h.call_ref(self, *id, Vec::new()) {
                        Ok(v) if expr => keys::parse_keys_raw(&v.to_display()),
                        Ok(_) => Vec::new(),
                        Err(e) => {
                            self.emsg(e);
                            Vec::new()
                        }
                    },
                    None => Vec::new(),
                }
            }
        };
        let rhs: Vec<Key> = rhs.into_iter().filter(|k| k.code != KeyCode::Sp(Sp::Nop)).collect();
        let prefix = if !m.noremap && rhs.len() >= m.lhs.len() && rhs[..m.lhs.len()] == m.lhs[..] { m.lhs.len() } else { 0 };
        self.feed_front(&rhs, !m.noremap, silent, prefix);
        MapResult::Applied
    }

    /// Run keys to completion in a nested context (`:normal`, `feedkeys(..., 'x')`).
    pub fn run_keys(&mut self, keys: &[Key], remap: bool) {
        let saved: VecDeque<TKey> = std::mem::take(&mut self.typeahead);
        let saved_depth = self.map_depth;
        let saved_pending = std::mem::take(&mut self.pending);
        let saved_state = std::mem::replace(&mut self.pending_state, normal::PendingState::Fresh);
        self.feed(keys, remap);
        self.process_nested();
        // An incomplete command is aborted, like Vim does for `:normal`.
        if !self.pending.is_empty() {
            self.pending.clear();
            self.pending_state = normal::PendingState::Fresh;
        }
        if self.cmdline.is_some() || self.expr_prompt.is_some() {
            self.cmdline = None;
            self.expr_prompt = None;
            if self.mode == Mode::Cmdline {
                self.mode = Mode::Normal;
            }
        }
        if matches!(self.mode, Mode::Insert | Mode::Replace) && !self.restart_insert {
            self.handle_key(keys::ESC);
        }
        self.typeahead = saved;
        self.map_depth = saved_depth;
        self.pending = saved_pending;
        self.pending_state = saved_state;
    }

    fn process_nested(&mut self) {
        let mut guard = 0usize;
        while !self.typeahead.is_empty() && !self.quit {
            guard += 1;
            if guard > 2_000_000 {
                self.typeahead.clear();
                break;
            }
            match self.try_mapping(true) {
                MapResult::Applied => {
                    self.map_depth += 1;
                    if self.map_depth > 1000 {
                        self.typeahead.clear();
                        self.emsg("E223: Recursive mapping");
                    }
                    continue;
                }
                _ => {}
            }
            self.map_depth = 0;
            let t = self.typeahead.pop_front().unwrap();
            let saved = self.cur_key_silent;
            self.cur_key_silent = t.silent;
            self.handle_key(t.key);
            self.cur_key_silent = saved;
        }
    }

    /// Dispatch one (already mapped) key to the current mode.
    pub fn handle_key(&mut self, key: Key) {
        if let Some((_, rec)) = &mut self.recording {
            rec.push(key);
        }
        if !self.output.is_empty() {
            // "Press ENTER" prompt: any key dismisses it; ':' starts a command.
            self.output.clear();
            if key == keys::CR || key.is(' ') || key == keys::ESC {
                return;
            }
        }
        if self.expr_prompt.is_some() || self.cmdline.is_some() {
            self.cmdline_key(key);
            return;
        }
        match self.mode {
            Mode::Insert | Mode::Replace => self.insert_key(key),
            Mode::Normal | Mode::Visual(_) => self.normal_key(key),
            Mode::Cmdline => {
                self.mode = Mode::Normal;
                self.normal_key(key);
            }
        }
    }

    /// Housekeeping after a batch of input: close undo groups, fix the cursor
    /// and scroll it into view.
    pub fn after_input(&mut self) {
        let in_insert = matches!(self.mode, Mode::Insert | Mode::Replace);
        if !in_insert {
            let cur = self.cursor();
            for b in self.buffers.values_mut() {
                if b.has_open_undo() {
                    b.close_undo(cur);
                }
            }
        }
        self.check_cursor();
        self.scroll_to_cursor();
    }

    /// Keep the cursor on valid text for the current mode.
    pub fn check_cursor(&mut self) {
        let mut pos = self.buf().clamp(self.cursor());
        let line_len = self.buf().line(pos.line).len();
        let past_end_ok = matches!(self.mode, Mode::Insert | Mode::Replace)
            || (matches!(self.mode, Mode::Visual(_)) && self.opt_str("selection") != "old")
            || self.opt_str("virtualedit").contains("onemore")
            || self.cmdline.is_some() && matches!(self.mode, Mode::Insert | Mode::Replace);
        if !past_end_ok && line_len > 0 && pos.col >= line_len {
            pos.col = motion::prev_char_start(self.buf().line(pos.line), line_len);
        }
        self.win_mut().cursor = pos;
    }

    // --------------------------------------------------------------- script

    pub fn eval_expr(&mut self, expr: &str) -> Result<Value, String> {
        match self.script.clone() {
            Some(h) => h.eval(self, expr),
            None => Err("E319: scripting is not available".into()),
        }
    }

    pub fn get_var(&mut self, name: &str) -> Option<Value> {
        let h = self.script.clone()?;
        h.get_global(self, name)
    }

    // ------------------------------------------------------------ autocmds

    pub fn add_autocmd(&mut self, event: &str, pattern: &str, group: Option<String>, action: AutoAction, once: bool) -> usize {
        let id = self.next_autocmd_id;
        self.next_autocmd_id += 1;
        let mut buffer = None;
        let mut pattern = pattern.to_string();
        if pattern.eq_ignore_ascii_case("<buffer>") {
            buffer = Some(self.cur_buf_id());
            pattern = "*".into();
        }
        self.autocmds.push(AutoCmd { id, event: canon_event(event), pattern, group, action, once, buffer });
        id
    }

    /// Remove autocommands matching the given filters (`None` = any).
    pub fn clear_autocmds(&mut self, event: Option<&str>, pattern: Option<&str>, group: Option<&str>) {
        let event = event.map(canon_event);
        self.autocmds.retain(|a| {
            let m = event.as_ref().map(|e| *e == a.event).unwrap_or(true)
                && pattern.map(|p| p == a.pattern).unwrap_or(true)
                && match group {
                    Some(g) => a.group.as_deref() == Some(g),
                    None => true,
                };
            !m
        });
    }

    /// Fire an event.  `subject` is the file name (or filetype etc.) matched
    /// against the autocommand patterns; empty means the current buffer name.
    pub fn do_autocmd(&mut self, event: &str, subject: &str) {
        if self.autocmd_depth > 10 || self.autocmds.is_empty() {
            return;
        }
        let ei = self.opt_global("eventignore").as_string();
        if ei.split(',').any(|e| e.eq_ignore_ascii_case("all") || e.eq_ignore_ascii_case(event)) {
            return;
        }
        let ev = canon_event(event);
        let full = if subject.is_empty() {
            self.buf().path.as_ref().map(|p| self.full_path(p).to_string_lossy().replace('\\', "/")).unwrap_or_default()
        } else {
            subject.to_string()
        };
        let is_file_event = !matches!(
            ev.as_str(),
            "filetype" | "syntax" | "colorscheme" | "colorschemepre" | "quickfixcmdpost" | "quickfixcmdpre" | "optionset"
                | "user" | "termresponse" | "sourcepre" | "sourcepost" | "cmdlineenter" | "cmdlineleave"
        );
        let cur_buf = self.cur_buf_id();
        let matching: Vec<AutoCmd> = self
            .autocmds
            .iter()
            .filter(|a| a.event == ev)
            .filter(|a| match a.buffer {
                Some(b) => b == cur_buf,
                None => a.pattern.split(',').any(|p| pattern_matches(p, &full, is_file_event)),
            })
            .cloned()
            .collect();
        if matching.is_empty() {
            return;
        }
        self.autocmd_depth += 1;
        let once: Vec<usize> = matching.iter().filter(|a| a.once).map(|a| a.id).collect();
        self.autocmds.retain(|a| !once.contains(&a.id));
        // Consecutive Ex commands are run as one script so that `if ... |`
        // spread over several autocommands works like in Vim.
        let mut script = String::new();
        for a in &matching {
            match &a.action {
                AutoAction::Ex(cmd) => {
                    script.push_str(cmd);
                    script.push('\n');
                }
                AutoAction::Lua(id) => {
                    if !script.is_empty() {
                        let s = std::mem::take(&mut script);
                        self.exec_lines_reporting(&s, "autocmd");
                    }
                    if let Some(h) = self.script.clone() {
                        let arg = Value::Dict(vec![
                            ("event".into(), Value::Str(event.to_string())),
                            ("match".into(), Value::Str(full.clone())),
                            ("file".into(), Value::Str(full.clone())),
                            ("buf".into(), Value::Int(cur_buf as i64)),
                        ]);
                        if let Err(e) = h.call_ref(self, *id, vec![arg]) {
                            self.emsg(e);
                        }
                    }
                }
            }
        }
        if !script.is_empty() {
            self.exec_lines_reporting(&script, "autocmd");
        }
        self.autocmd_depth -= 1;
    }

    /// Execute Ex/Vim script lines, reporting (not propagating) errors.
    pub fn exec_lines_reporting(&mut self, src: &str, name: &str) {
        if let Err(e) = self.exec_lines(src, name) {
            self.emsg(e);
        }
    }

    /// Execute Ex/Vim script lines.  Plain Ex commands run natively; anything
    /// using script constructs goes through the script engine.
    pub fn exec_lines(&mut self, src: &str, name: &str) -> Result<(), String> {
        let needs_script = src.lines().any(ex::is_script_line) || src.lines().any(|l| l.trim_start().starts_with('\\'));
        if needs_script {
            match self.script.clone() {
                Some(h) => h.exec_vim(self, src, name),
                None => Err("E319: scripting is not available".into()),
            }
        } else {
            let mut first_err = None;
            for line in src.lines() {
                if let Err(e) = self.exec_ex(line) {
                    // Like Vim, keep going after an error in a script.
                    if first_err.is_none() {
                        first_err = Some(e.clone());
                    }
                    self.emsg(e);
                }
            }
            let _ = first_err;
            Ok(())
        }
    }

    // ---------------------------------------------------------------- paths

    pub fn full_path(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
        }
    }

    pub fn find_buffer_by_path(&self, p: &Path) -> Option<BufId> {
        let full = normalize_path(&self.full_path(p));
        self.buffers
            .values()
            .find(|b| b.path.as_ref().map(|bp| normalize_path(&self.full_path(bp)) == full).unwrap_or(false))
            .map(|b| b.id)
    }

    pub fn push_jump(&mut self) {
        let entry = (self.cur_buf_id(), self.cursor());
        self.jumplist.retain(|e| !(e.0 == entry.0 && e.1.line == entry.1.line));
        self.jumplist.push(entry);
        if self.jumplist.len() > 100 {
            self.jumplist.remove(0);
        }
        self.jumpidx = self.jumplist.len();
        let pos = self.cursor();
        self.buf_mut().marks.insert('\'', pos);
    }
}

enum MapResult {
    Applied,
    Wait,
    None,
}

fn is_buffer_private(name: &str) -> bool {
    matches!(
        name,
        "filetype" | "syntax" | "modified" | "readonly" | "buftype" | "bufhidden" | "buflisted" | "swapfile" | "fileformat"
            | "fileencoding" | "bomb" | "endofline" | "modifiable" | "binary"
    )
}

pub fn canon_event(e: &str) -> String {
    let l = e.to_ascii_lowercase();
    match l.as_str() {
        "bufread" => "bufreadpost".into(),
        "bufwrite" => "bufwritepre".into(),
        "bufcreate" => "bufadd".into(),
        "fileencoding" => "encodingchanged".into(),
        _ => l,
    }
}

pub fn normalize_path(p: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in p.components() {
        use std::path::Component::*;
        match c {
            CurDir => {}
            ParentDir => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            other => parts.push(other.as_os_str().to_string_lossy().replace('\\', "/").trim_end_matches('/').to_string()),
        }
    }
    let s = parts.join("/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// Match one autocommand pattern against a subject.
pub fn pattern_matches(pat: &str, subject: &str, is_file: bool) -> bool {
    if pat == "*" {
        return true;
    }
    let re = vregex::glob_to_regex(pat);
    let re = if cfg!(windows) && is_file { format!("(?i){}", re) } else { re };
    let Ok(re) = regex::Regex::new(&re) else {
        return false;
    };
    if !is_file {
        return re.is_match(subject);
    }
    let subject = subject.replace('\\', "/");
    if pat.contains('/') {
        re.is_match(&subject)
    } else {
        let tail = subject.rsplit('/').next().unwrap_or(&subject);
        re.is_match(tail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autocmd_patterns() {
        assert!(pattern_matches("*.js", "C:/x/foo.js", true));
        assert!(!pattern_matches("*.js", "C:/x/foo.jsx", true));
        assert!(pattern_matches("python", "python", false));
        assert!(pattern_matches("*", "", true));
    }
}
