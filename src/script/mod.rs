//! Lua scripting host: the `vim.*` API, Vim script translation and the
//! bridge between Lua values and editor values.

pub mod builtins;
pub mod vimscript;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use mlua::{Function, Lua, MultiValue, Table, Value as LV};

use crate::editor::excmds::{expand_env, intern_group, modes_from_str};
use crate::editor::keys;
use crate::editor::options::{self, OptKind, OptVal};
use crate::editor::{AutoAction, Editor, MapRhs, Mapping, ScriptHost, UserCmd, Value};

thread_local! {
    static ED: Cell<*mut Editor> = const { Cell::new(std::ptr::null_mut()) };
}

/// Makes the editor reachable from Lua callbacks while Lua code runs.
struct EdGuard(*mut Editor);

impl EdGuard {
    fn new(ed: &mut Editor) -> EdGuard {
        let old = ED.with(|c| c.replace(ed as *mut Editor));
        EdGuard(old)
    }
}

impl Drop for EdGuard {
    fn drop(&mut self) {
        ED.with(|c| c.set(self.0));
    }
}

fn with_ed<R>(f: impl FnOnce(&mut Editor) -> R) -> mlua::Result<R> {
    let p = ED.with(|c| c.get());
    if p.is_null() {
        return Err(mlua::Error::external("fevim: editor is not available here"));
    }
    // SAFETY: the pointer is set by `EdGuard` from a live `&mut Editor` whose
    // owner does not touch it while Lua code (and therefore this callback) runs.
    Ok(f(unsafe { &mut *p }))
}

fn ext(msg: impl Into<String>) -> mlua::Error {
    mlua::Error::external(msg.into())
}

/// Extract a readable message from a Lua error.
pub fn lua_err(e: &mlua::Error) -> String {
    match e {
        mlua::Error::CallbackError { cause, .. } => lua_err(cause),
        mlua::Error::RuntimeError(m) => m.split("\nstack traceback:").next().unwrap_or(m).to_string(),
        mlua::Error::SyntaxError { message, .. } => format!("E5107: Lua syntax error: {}", message),
        mlua::Error::ExternalError(x) => x.to_string(),
        other => other.to_string().split("\nstack traceback:").next().unwrap_or("").to_string(),
    }
}

// ---------------------------------------------------------- conversions

pub fn to_lua(lua: &Lua, v: &Value) -> mlua::Result<LV> {
    Ok(match v {
        Value::Nil => LV::Nil,
        Value::Bool(b) => LV::Boolean(*b),
        Value::Int(n) => LV::Integer(*n),
        Value::Float(f) => LV::Number(*f),
        Value::Str(s) => LV::String(lua.create_string(s)?),
        Value::List(l) => {
            let t = lua.create_table_with_capacity(l.len(), 0)?;
            for (i, x) in l.iter().enumerate() {
                t.raw_set(i + 1, to_lua(lua, x)?)?;
            }
            if let Ok(mt) = lua.named_registry_value::<Table>("fevim_list_mt") {
                t.set_metatable(Some(mt))?;
            }
            LV::Table(t)
        }
        Value::Dict(d) => {
            let t = lua.create_table_with_capacity(0, d.len())?;
            for (k, x) in d {
                t.raw_set(k.as_str(), to_lua(lua, x)?)?;
            }
            if let Ok(mt) = lua.named_registry_value::<Table>("fevim_dict_mt") {
                t.set_metatable(Some(mt))?;
            }
            LV::Table(t)
        }
    })
}

pub fn from_lua(lua: &Lua, v: &LV) -> Value {
    from_lua_depth(lua, v, 0)
}

fn from_lua_depth(lua: &Lua, v: &LV, depth: usize) -> Value {
    if depth > 100 {
        return Value::Nil;
    }
    match v {
        LV::Nil => Value::Nil,
        LV::Boolean(b) => Value::Bool(*b),
        LV::Integer(n) => Value::Int(*n),
        LV::Number(f) => Value::Float(*f),
        LV::String(s) => Value::Str(s.to_string_lossy()),
        LV::Table(t) => {
            let mt = t.metatable();
            let list_mt = lua.named_registry_value::<Table>("fevim_list_mt").ok();
            let dict_mt = lua.named_registry_value::<Table>("fevim_dict_mt").ok();
            if let Some(m) = &mt {
                if m.raw_get::<String>("__name").map(|n| n == "vim.funcref").unwrap_or(false) {
                    return Value::Str(t.raw_get::<String>("name").unwrap_or_default());
                }
                if m.raw_get::<String>("__name").map(|n| n == "vim.NIL").unwrap_or(false) {
                    return Value::Nil;
                }
            }
            let is_list = match (&mt, &list_mt, &dict_mt) {
                (Some(m), Some(l), _) if m == l => true,
                (Some(m), _, Some(d)) if m == d => false,
                _ => t.raw_len() > 0 || t.clone().pairs::<LV, LV>().next().is_none(),
            };
            if is_list {
                let mut out = Vec::new();
                for x in t.clone().sequence_values::<LV>().flatten() {
                    out.push(from_lua_depth(lua, &x, depth + 1));
                }
                Value::List(out)
            } else {
                let mut out = Vec::new();
                for (k, x) in t.clone().pairs::<LV, LV>().flatten() {
                    let key = match &k {
                        LV::String(s) => s.to_string_lossy(),
                        LV::Integer(n) => n.to_string(),
                        LV::Number(f) => f.to_string(),
                        _ => continue,
                    };
                    out.push((key, from_lua_depth(lua, &x, depth + 1)));
                }
                out.sort_by(|a, b| a.0.cmp(&b.0));
                Value::Dict(out)
            }
        }
        LV::Function(_) => Value::Str("<lambda>".into()),
        _ => Value::Nil,
    }
}

fn optval_to_lua(lua: &Lua, v: &OptVal) -> mlua::Result<LV> {
    Ok(match v {
        OptVal::Bool(b) => LV::Boolean(*b),
        OptVal::Num(n) => LV::Integer(*n),
        OptVal::Str(s) => LV::String(lua.create_string(s)?),
    })
}

fn lua_to_optval(v: &LV) -> OptVal {
    match v {
        LV::Boolean(b) => OptVal::Bool(*b),
        LV::Integer(n) => OptVal::Num(*n),
        LV::Number(f) => OptVal::Num(*f as i64),
        LV::String(s) => OptVal::Str(s.to_string_lossy()),
        LV::Nil => OptVal::Bool(false),
        _ => OptVal::Str(String::new()),
    }
}

fn lv_str(v: &LV) -> Option<String> {
    match v {
        LV::String(s) => Some(s.to_string_lossy()),
        LV::Integer(n) => Some(n.to_string()),
        LV::Number(f) => Some(f.to_string()),
        _ => None,
    }
}

fn lv_bool(v: &LV) -> bool {
    !matches!(v, LV::Nil | LV::Boolean(false))
}

fn lv_int(v: &LV) -> Option<i64> {
    match v {
        LV::Integer(n) => Some(*n),
        LV::Number(f) => Some(*f as i64),
        LV::String(s) => s.to_string_lossy().trim().parse().ok(),
        _ => None,
    }
}

fn arg(args: &MultiValue, i: usize) -> LV {
    args.get(i).cloned().unwrap_or(LV::Nil)
}

// ---------------------------------------------------------------- host

pub struct LuaHost {
    lua: Lua,
    vim_cache: RefCell<HashMap<String, Function>>,
}

const PRELUDE: &str = include_str!("prelude.lua");

impl LuaHost {
    pub fn new() -> Result<LuaHost, String> {
        let lua = Lua::new();
        let host = LuaHost { lua, vim_cache: RefCell::new(HashMap::new()) };
        host.install().map_err(|e| lua_err(&e))?;
        Ok(host)
    }

    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    fn install(&self) -> mlua::Result<()> {
        let lua = &self.lua;
        let vim = lua.create_table()?;
        let core = lua.create_table()?;
        vim.set("_core", core.clone())?;
        lua.globals().set("vim", vim)?;
        register_core(lua, &core)?;
        // package.path: <config>/lua/?.lua
        let cfg = crate::config_dir().join("lua");
        let cfg = cfg.to_string_lossy().replace('\\', "/");
        let package: Table = lua.globals().get("package")?;
        let path: String = package.get("path")?;
        package.set("path", format!("{0}/?.lua;{0}/?/init.lua;{1}", cfg, path))?;
        lua.load(PRELUDE).set_name("=fevim/prelude.lua").exec()?;
        Ok(())
    }

    fn vs(&self) -> mlua::Result<Table> {
        let vim: Table = self.lua.globals().get("vim")?;
        vim.get("_vs")
    }

    /// Call `f` through `vim._pcall` so that Lua error objects become strings.
    fn protected_call(&self, f: Function, args: MultiValue) -> Result<MultiValue, String> {
        let vim: Table = self.lua.globals().get("vim").map_err(|e| lua_err(&e))?;
        let pc: Function = vim.get("_pcall").map_err(|e| lua_err(&e))?;
        let mut all = MultiValue::new();
        all.push_back(LV::Function(f));
        for a in args {
            all.push_back(a);
        }
        pc.call::<MultiValue>(all).map_err(|e| lua_err(&e))
    }

    fn run_chunk(&self, ed: &mut Editor, code: &str, name: &str) -> Result<(), String> {
        let _g = EdGuard::new(ed);
        let f = self.lua.load(code).set_name(format!("@{}", name)).into_function().map_err(|e| lua_err(&e))?;
        self.protected_call(f, MultiValue::new()).map(|_| ())
    }

    /// Run scheduled callbacks and due timers.  Returns true if timers remain.
    pub fn tick_impl(&self, ed: &mut Editor) -> bool {
        let _g = EdGuard::new(ed);
        let Ok(vim) = self.lua.globals().get::<Table>("vim") else {
            return false;
        };
        let Ok(t) = vim.get::<Function>("_tick") else {
            return false;
        };
        t.call::<bool>(()).unwrap_or(false)
    }
}

impl ScriptHost for LuaHost {
    fn eval(&self, ed: &mut Editor, expr: &str) -> Result<Value, String> {
        let _g = EdGuard::new(ed);
        let vs = self.vs().map_err(|e| lua_err(&e))?;
        let f: Function = vs.get("eval").map_err(|e| lua_err(&e))?;
        let mut args = MultiValue::new();
        args.push_back(LV::String(self.lua.create_string(expr).map_err(|e| lua_err(&e))?));
        let r = self.protected_call(f, args)?;
        Ok(r.front().map(|v| from_lua(&self.lua, v)).unwrap_or(Value::Nil))
    }

    fn exec_vim(&self, ed: &mut Editor, src: &str, name: &str) -> Result<(), String> {
        let is_file = name.contains('/') || name.contains('\\') || name.ends_with("vimrc");
        if is_file {
            let code = vimscript::transpile(src, Some(name), true)?;
            if std::env::var("FEVIM_DEBUG_TRANSPILE").is_ok() {
                eprintln!("---- {}\n{}", name, code);
            }
            return self.run_chunk(ed, &code, name);
        }
        let cached = self.vim_cache.borrow().get(src).cloned();
        let f = match cached {
            Some(f) => f,
            None => {
                let code = vimscript::transpile(src, None, false)?;
                let f = self.lua.load(&code).set_name("=vimscript").into_function().map_err(|e| format!("{} (translated: {})", lua_err(&e), code))?;
                let mut c = self.vim_cache.borrow_mut();
                if c.len() > 1000 {
                    c.clear();
                }
                c.insert(src.to_string(), f.clone());
                f
            }
        };
        let _g = EdGuard::new(ed);
        self.protected_call(f, MultiValue::new()).map(|_| ())
    }

    fn exec_lua(&self, ed: &mut Editor, src: &str, name: &str) -> Result<(), String> {
        self.run_chunk(ed, src, name)
    }

    fn call_ref(&self, ed: &mut Editor, id: usize, args: Vec<Value>) -> Result<Value, String> {
        let _g = EdGuard::new(ed);
        let vim: Table = self.lua.globals().get("vim").map_err(|e| lua_err(&e))?;
        let f: Function = vim.get("_call_ref").map_err(|e| lua_err(&e))?;
        let mut mv = MultiValue::new();
        mv.push_back(LV::Integer(id as i64));
        for a in &args {
            mv.push_back(to_lua(&self.lua, a).map_err(|e| lua_err(&e))?);
        }
        let r = self.protected_call(f, mv)?;
        Ok(r.front().map(|v| from_lua(&self.lua, v)).unwrap_or(Value::Nil))
    }

    fn get_global(&self, ed: &mut Editor, name: &str) -> Option<Value> {
        let _g = EdGuard::new(ed);
        let vim: Table = self.lua.globals().get("vim").ok()?;
        let g: Table = vim.get("g").ok()?;
        let v: LV = g.get(name).ok()?;
        if v.is_nil() {
            return None;
        }
        Some(from_lua(&self.lua, &v))
    }

    fn set_global(&self, ed: &mut Editor, name: &str, value: Value) {
        let _g = EdGuard::new(ed);
        if let Ok(vim) = self.lua.globals().get::<Table>("vim") {
            if let Ok(g) = vim.get::<Table>("g") {
                if let Ok(v) = to_lua(&self.lua, &value) {
                    let _ = g.set(name, v);
                }
            }
        }
    }

    fn function_exists(&self, ed: &mut Editor, name: &str) -> bool {
        let _g = EdGuard::new(ed);
        self.vs().and_then(|vs| vs.get::<Table>("funcs")).and_then(|f| f.get::<LV>(name)).map(|v| !v.is_nil()).unwrap_or(false)
    }

    fn transpile(&self, src: &str, name: &str) -> Result<String, String> {
        vimscript::transpile(src, Some(name), true)
    }

    fn tick(&self, ed: &mut Editor) -> bool {
        self.tick_impl(ed)
    }
}

// ------------------------------------------------------------ vim._core

fn register_core(lua: &Lua, core: &Table) -> mlua::Result<()> {
    macro_rules! f {
        ($name:expr, $body:expr) => {
            core.set($name, lua.create_function($body)?)?;
        };
    }

    f!("set_metatables", |lua, (l, d): (Table, Table)| {
        lua.set_named_registry_value("fevim_list_mt", l)?;
        lua.set_named_registry_value("fevim_dict_mt", d)?;
        Ok(())
    });

    f!("cmd", |_, s: String| {
        with_ed(|ed| {
            let r = ed.exec_lines(&s, "lua");
            r
        })?
        .map_err(ext)
    });

    f!("capture", |_, s: String| with_ed(|ed| ed.capture(&s))?.map_err(ext));

    f!("getopt", |lua, (name, scope): (String, Option<String>)| {
        let v = with_ed(|ed| {
            let def = options::find_def(&name)?;
            Some(match scope.as_deref() {
                Some("g") => ed.opt_global(def.name),
                _ => ed.opt(def.name),
            })
        })?;
        match v {
            Some(v) => optval_to_lua(lua, &v),
            None => Err(ext(format!("E518: Unknown option: {}", name))),
        }
    });

    f!("setopt", |_, (name, value, scope, op): (String, LV, Option<String>, Option<String>)| {
        with_ed(|ed| -> Result<(), String> {
            let def = options::find_def(&name).ok_or_else(|| format!("E518: Unknown option: {}", name))?;
            let scope = match scope.as_deref() {
                Some("l") => 'l',
                Some("g") => 'g',
                _ => 'b',
            };
            let mut v = lua_to_optval(&value);
            if def.kind == OptKind::Str {
                if let OptVal::Str(s) = &v {
                    if def.expand {
                        v = OptVal::Str(expand_env(s));
                    }
                }
            }
            if def.kind == OptKind::Bool {
                if let OptVal::Str(s) = &v {
                    v = OptVal::Bool(!(s.is_empty() || s == "0"));
                }
            }
            let op = op.unwrap_or_else(|| "=".into());
            if op != "=" {
                let cur = if scope == 'g' { ed.opt_global(def.name) } else { ed.opt(def.name) };
                let add = match (&cur, &v) {
                    (OptVal::Num(_), OptVal::Str(s)) => OptVal::Num(s.trim().parse().unwrap_or(0)),
                    (OptVal::Str(_), OptVal::Num(n)) => OptVal::Str(n.to_string()),
                    _ => v.clone(),
                };
                v = match op.as_str() {
                    "+" => options::append_value(def, &cur, &add),
                    "-" => options::remove_value(def, &cur, &add),
                    _ => options::prepend_value(def, &cur, &add),
                };
            }
            ed.set_opt(def, v, scope)
        })?
        .map_err(ext)
    });

    f!("optinfo", |lua, name: String| {
        let Some(d) = options::find_def(&name) else {
            return Ok(LV::Nil);
        };
        let t = lua.create_table()?;
        t.set("name", d.name)?;
        t.set(
            "kind",
            match d.kind {
                OptKind::Bool => "boolean",
                OptKind::Num => "number",
                OptKind::Str => "string",
            },
        )?;
        t.set("comma", d.comma)?;
        t.set("flaglist", d.flaglist)?;
        t.set(
            "scope",
            match d.scope {
                options::OptScope::Global => "global",
                options::OptScope::Buf => "buf",
                options::OptScope::Win => "win",
            },
        )?;
        Ok(LV::Table(t))
    });

    f!("fn", |lua, args: MultiValue| {
        let mut it = args.into_iter();
        let name = it.next().and_then(|v| lv_str(&v)).unwrap_or_default();
        let vals: Vec<Value> = it.map(|v| from_lua(lua, &v)).collect();
        let r = with_ed(|ed| builtins::call(ed, &name, vals))?;
        match r {
            Some(Ok(v)) => to_lua(lua, &v),
            Some(Err(e)) => Err(ext(e)),
            None => Err(ext(format!("E117: Unknown function: {}", name))),
        }
    });

    f!("is_builtin", |_, name: String| Ok(builtins::is_builtin(&name)));
    f!("exists", |_, what: String| with_ed(|ed| builtins::exists(ed, &what)));

    f!("regex_match", |_, (s, pat, ic): (String, String, Option<bool>)| {
        with_ed(|ed| -> Result<bool, String> {
            let icv = ic.unwrap_or_else(|| ed.opt_global("ignorecase").as_bool());
            let re = crate::editor::vregex::VimRegex::new(&pat, icv, false)?;
            Ok(re.is_match(&s))
        })?
        .map_err(ext)
    });

    f!("transpile_expr", |_, s: String| vimscript::transpile_expr(&s).map_err(ext));
    f!("transpile", |_, (s, name): (String, Option<String>)| vimscript::transpile(&s, name.as_deref(), name.is_some()).map_err(ext));

    f!("echo", |_, (text, hl, history, append): (LV, Option<String>, Option<bool>, Option<bool>)| {
        let text = lv_str(&text).unwrap_or_default();
        with_ed(|ed| {
            let group: &'static str = match hl {
                Some(h) if !h.is_empty() && !h.eq_ignore_ascii_case("none") => intern_group(&h),
                _ => ed.echohl,
            };
            if append.unwrap_or(false) {
                if ed.silent_depth > 0 {
                    return;
                }
                let prev = ed.msg_line.take().map(|m| m.text).unwrap_or_default();
                ed.msg_line = Some(crate::editor::Msg { text: format!("{}{}", prev, text), group });
            } else if history.unwrap_or(false) {
                ed.msg_history(text, group);
            } else {
                ed.msg_hl(text, group);
            }
        })
    });

    f!("silent", |_, (enter, bang): (bool, Option<bool>)| {
        with_ed(|ed| {
            let n = if bang.unwrap_or(false) { 2 } else { 1 };
            if enter {
                ed.silent_depth += n;
            } else {
                ed.silent_depth = ed.silent_depth.saturating_sub(n);
            }
        })
    });

    f!("cursor_line", |_, ()| with_ed(|ed| ed.cursor().line as i64 + 1));
    f!("curbuf", |_, ()| with_ed(|ed| ed.cur_buf_id() as i64));
    f!("curwin", |_, ()| with_ed(|ed| ed.cur_win_id() as i64));
    f!("curtab", |_, ()| with_ed(|ed| ed.cur_tab as i64 + 1));
    f!("bufnr", |lua, v: LV| {
        let val = from_lua(lua, &v);
        with_ed(|ed| match builtins::call(ed, "bufnr", vec![val]) {
            Some(Ok(v)) => v.as_int(),
            _ => -1,
        })
    });
    f!("winid", |lua, v: LV| {
        let val = from_lua(lua, &v);
        with_ed(|ed| match builtins::call(ed, "win_getid", vec![val]) {
            Some(Ok(v)) => v.as_int(),
            _ => 0,
        })
    });
    f!("buf_list", |lua, ()| {
        let ids = with_ed(|ed| ed.buffers.keys().map(|k| Value::Int(*k as i64)).collect::<Vec<_>>())?;
        to_lua(lua, &Value::List(ids))
    });
    f!("win_list", |lua, ()| {
        let ids = with_ed(|ed| ed.tab_wins().into_iter().map(|k| Value::Int(k as i64)).collect::<Vec<_>>())?;
        to_lua(lua, &Value::List(ids))
    });
    f!("buf_valid", |_, b: i64| with_ed(|ed| ed.buffers.contains_key(&(b as usize))));
    f!("buf_name", |_, b: i64| {
        with_ed(|ed| ed.buffers.get(&(b as usize)).and_then(|x| x.path.as_ref().map(|p| ed.full_path(p).to_string_lossy().replace('\\', "/"))).unwrap_or_default())
    });
    f!("buf_line_count", |_, b: i64| with_ed(|ed| ed.buffers.get(&(b as usize)).map(|x| x.line_count() as i64).unwrap_or(0)));
    f!("buf_get_lines", |lua, (b, s, e): (i64, i64, i64)| {
        let lines = with_ed(|ed| -> Result<Vec<Value>, String> {
            let buf = ed.buffers.get(&(b as usize)).ok_or("Invalid buffer id")?;
            let n = buf.line_count() as i64;
            let norm = |x: i64| if x < 0 { (n + 1 + x).max(0) } else { x.min(n) };
            let (s, e) = (norm(s) as usize, norm(e) as usize);
            Ok(buf.lines[s.min(e)..e].iter().map(|l| Value::Str(l.clone())).collect())
        })?
        .map_err(ext)?;
        to_lua(lua, &Value::List(lines))
    });
    f!("buf_set_lines", |lua, (b, s, e, lines): (i64, i64, i64, LV)| {
        let lines: Vec<String> = match from_lua(lua, &lines) {
            Value::List(v) => v.into_iter().map(|x| x.to_display()).collect(),
            Value::Nil => Vec::new(),
            other => vec![other.to_display()],
        };
        with_ed(|ed| -> Result<(), String> {
            let buf = ed.buffers.get_mut(&(b as usize)).ok_or("Invalid buffer id")?;
            let n = buf.line_count() as i64;
            let norm = |x: i64| if x < 0 { (n + 1 + x).max(0) } else { x.min(n) };
            let (s, e) = (norm(s) as usize, norm(e) as usize);
            // replacing the whole content of an empty buffer drops the placeholder line
            let empty = buf.is_empty();
            buf.splice(s, e.max(s), lines);
            if empty && s == 0 && e == 0 && buf.line_count() > 1 && buf.lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                let last = buf.line_count() - 1;
                buf.splice(last, last + 1, Vec::new());
            }
            ed.check_cursor();
            Ok(())
        })?
        .map_err(ext)
    });
    f!("win_buf", |_, w: i64| with_ed(|ed| ed.windows.get(&(w as usize)).map(|x| x.buf as i64).unwrap_or(0)));
    f!("get_cursor", |_, w: i64| {
        with_ed(|ed| {
            let c = ed.windows.get(&(w as usize)).map(|x| x.cursor).unwrap_or_default();
            (c.line as i64 + 1, c.col as i64)
        })
    });
    f!("set_cursor", |_, (w, l, c): (i64, i64, i64)| {
        with_ed(|ed| {
            let id = w as usize;
            if let Some(win) = ed.windows.get(&id) {
                let b = &ed.buffers[&win.buf];
                let p = b.clamp(crate::editor::Pos::new((l.max(1) - 1) as usize, c.max(0) as usize));
                ed.windows.get_mut(&id).unwrap().cursor = p;
            }
        })
    });

    f!("map", |_, args: MultiValue| {
        let mode = lv_str(&arg(&args, 0)).unwrap_or_default();
        let lhs = lv_str(&arg(&args, 1)).unwrap_or_default();
        let rhs = lv_str(&arg(&args, 2));
        let fnid = lv_int(&arg(&args, 3));
        let noremap = lv_bool(&arg(&args, 4));
        let silent = lv_bool(&arg(&args, 5));
        let expr = lv_bool(&arg(&args, 6));
        let nowait = lv_bool(&arg(&args, 7));
        let buffer = lv_int(&arg(&args, 8)).map(|b| b as usize);
        with_ed(|ed| {
            let modes = if mode.is_empty() { modes_from_str("") } else { modes_from_str(&mode) };
            let leader = ed.leader();
            let lhs_k = keys::parse_keys(&crate::editor::excmds::replace_leader(&lhs, &leader));
            let rhs_text = rhs.map(|r| crate::editor::excmds::replace_leader(&r, &leader)).unwrap_or_default();
            let rhs_v = match fnid {
                Some(id) => MapRhs::Lua(id as usize),
                None => MapRhs::Keys(if expr { Vec::new() } else { keys::parse_keys(&rhs_text) }),
            };
            ed.add_mapping(Mapping { modes, lhs: lhs_k, rhs: rhs_v, rhs_text, noremap, silent, expr, nowait, buffer });
        })
    });
    f!("unmap", |_, (mode, lhs, buffer): (String, String, Option<i64>)| {
        with_ed(|ed| {
            let modes = modes_from_str(&mode);
            let leader = ed.leader();
            let k = keys::parse_keys(&crate::editor::excmds::replace_leader(&lhs, &leader));
            if ed.del_mapping(modes, &k, buffer.map(|b| b as usize)) { Ok(()) } else { Err("E31: No such mapping".to_string()) }
        })?
        .map_err(ext)
    });

    f!("augroup", |_, (name, clear): (String, Option<bool>)| {
        with_ed(|ed| {
            if clear.unwrap_or(true) {
                ed.clear_autocmds(None, None, Some(&name));
            }
            if !ed.augroups.contains(&name) {
                ed.augroups.push(name.clone());
            }
            ed.augroups.iter().position(|g| *g == name).unwrap() as i64 + 1
        })
    });
    f!("del_augroup", |_, name: String| {
        with_ed(|ed| {
            ed.clear_autocmds(None, None, Some(&name));
            ed.augroups.retain(|g| *g != name);
        })
    });
    f!("autocmd", |lua, args: MultiValue| {
        let events: Vec<String> = match from_lua(lua, &arg(&args, 0)) {
            Value::List(v) => v.iter().map(|x| x.to_display()).collect(),
            v => vec![v.to_display()],
        };
        let patterns: Vec<String> = match from_lua(lua, &arg(&args, 1)) {
            Value::List(v) => v.iter().map(|x| x.to_display()).collect(),
            Value::Nil => vec!["*".into()],
            v => vec![v.to_display()],
        };
        let group = lv_str(&arg(&args, 2));
        let command = lv_str(&arg(&args, 3));
        let fnid = lv_int(&arg(&args, 4));
        let once = lv_bool(&arg(&args, 5));
        let buffer = lv_int(&arg(&args, 6)).map(|b| b as usize);
        with_ed(|ed| {
            if let Some(g) = &group {
                if !ed.augroups.contains(g) {
                    ed.augroups.push(g.clone());
                }
            }
            let mut first = 0;
            for e in &events {
                for p in &patterns {
                    let action = match (fnid, &command) {
                        (Some(id), _) => AutoAction::Lua(id as usize),
                        (None, Some(c)) => AutoAction::Ex(c.clone()),
                        (None, None) => AutoAction::Ex(String::new()),
                    };
                    let id = ed.add_autocmd(e, if buffer.is_some() { "*" } else { p }, group.clone(), action, once);
                    if let Some(b) = buffer {
                        if let Some(a) = ed.autocmds.last_mut() {
                            a.buffer = Some(b);
                        }
                    }
                    if first == 0 {
                        first = id;
                    }
                }
            }
            first as i64
        })
    });
    f!("del_autocmd", |_, id: i64| with_ed(|ed| ed.autocmds.retain(|a| a.id != id as usize)));
    f!("clear_autocmds", |_, (event, pattern, group): (Option<String>, Option<String>, Option<String>)| {
        with_ed(|ed| ed.clear_autocmds(event.as_deref(), pattern.as_deref(), group.as_deref()))
    });
    f!("user_command", |_, args: MultiValue| {
        let name = lv_str(&arg(&args, 0)).unwrap_or_default();
        let text = lv_str(&arg(&args, 1));
        let fnid = lv_int(&arg(&args, 2));
        let nargs = lv_str(&arg(&args, 3)).and_then(|s| s.chars().next()).unwrap_or('0');
        let bang = lv_bool(&arg(&args, 4));
        let range = lv_bool(&arg(&args, 5));
        let bar = lv_bool(&arg(&args, 6));
        if !name.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            return Err(ext("E183: User defined commands must start with an uppercase letter"));
        }
        with_ed(|ed| {
            let repl = match fnid {
                Some(id) => AutoAction::Lua(id as usize),
                None => AutoAction::Ex(text.unwrap_or_default()),
            };
            ed.user_cmds.insert(name.clone(), UserCmd { name, repl, nargs, bang, range, bar });
        })
    });
    f!("set_hl", |_, (name, spec, link, default): (String, Option<Table>, Option<String>, Option<bool>)| {
        let mut args: Vec<(String, String)> = Vec::new();
        if let Some(t) = &spec {
            for (k, v) in t.clone().pairs::<String, LV>().flatten() {
                if let Some(s) = lv_str(&v) {
                    args.push((k, s));
                }
            }
        }
        with_ed(|ed| {
            let default = default.unwrap_or(false);
            if default && ed.hl.exists(&name) {
                return;
            }
            match link {
                Some(l) => ed.hl.link(&name, &l, true),
                None => {
                    ed.hl.clear_group(&name);
                    ed.hl.set(&name, &args, false);
                }
            }
        })
    });
    f!("get_hl", |lua, name: String| {
        let a = with_ed(|ed| ed.hl.attr(&name))?;
        let t = lua.create_table()?;
        if let Some(c) = a.fg {
            t.set("fg", c.0 as i64)?;
        }
        if let Some(c) = a.bg {
            t.set("bg", c.0 as i64)?;
        }
        if let Some(c) = a.sp {
            t.set("sp", c.0 as i64)?;
        }
        for (k, v) in [("bold", a.bold), ("italic", a.italic), ("underline", a.underline), ("undercurl", a.undercurl), ("reverse", a.reverse), ("strikethrough", a.strikethrough)] {
            if v {
                t.set(k, true)?;
            }
        }
        Ok(t)
    });

    f!("setenv", |_, (k, v): (String, Option<String>)| {
        // SAFETY: fevim's editor runs on a single thread.
        unsafe {
            match v {
                Some(v) => std::env::set_var(&k, v),
                None => std::env::remove_var(&k),
            }
        }
        Ok(())
    });
    f!("environ", |lua, ()| {
        let t = lua.create_table()?;
        for (k, v) in std::env::vars() {
            t.set(k, v)?;
        }
        Ok(t)
    });
    f!("system", |_, (cmd, input): (String, Option<String>)| {
        with_ed(|ed| ed.run_shell_status(&cmd, input.as_deref()))?.map_err(ext)
    });
    f!("keycode", |_, s: String| Ok(keys::keycode(&s)));
    // autoload: foo#bar#baz -> autoload/foo/bar.vim in 'runtimepath'
    f!("autoload", |_, name: String| {
        let Some((path, _)) = name.rsplit_once('#') else {
            return Ok(false);
        };
        let rel = format!("autoload/{}.vim", path.replace('#', "/"));
        with_ed(|ed| {
            for dir in ed.runtime_dirs.clone() {
                let p = dir.join(&rel);
                if p.is_file() {
                    if let Err(e) = ed.source_file(&p) {
                        ed.emsg(e);
                    }
                    return true;
                }
            }
            false
        })
    });
    f!("now_ms", |_, ()| {
        Ok(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0))
    });
    f!("runtime_paths", |lua, ()| {
        let v = with_ed(|ed| ed.runtime_dirs.iter().map(|p| Value::Str(p.to_string_lossy().replace('\\', "/"))).collect::<Vec<_>>())?;
        to_lua(lua, &Value::List(v))
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    fn editor() -> Editor {
        let mut ed = Editor::new();
        ed.script = Some(Rc::new(LuaHost::new().unwrap()));
        ed
    }

    #[test]
    fn lua_api_basics() {
        let mut ed = editor();
        let h = ed.script.clone().unwrap();
        h.exec_lua(&mut ed, "vim.opt.number = true; vim.opt.tabstop = 4; vim.g.foo = 'bar'", "t").unwrap();
        assert!(ed.opt_bool("number"));
        assert_eq!(ed.opt_num("tabstop"), 4);
        assert_eq!(h.get_global(&mut ed, "foo"), Some(Value::Str("bar".into())));
        h.exec_lua(&mut ed, "vim.opt.formatoptions = 'tcqro'; vim.opt.formatoptions:remove('ro')", "t").unwrap();
        assert_eq!(ed.opt_str("formatoptions"), "tcq");
        h.exec_lua(&mut ed, "vim.keymap.set('n', 'Y', 'y$')", "t").unwrap();
        assert_eq!(ed.maps.len(), 1);
        assert_eq!(h.eval(&mut ed, "1 + 2 * 3").unwrap(), Value::Int(7));
        assert_eq!(h.eval(&mut ed, "'a' . 1 . 'b'").unwrap(), Value::Str("a1b".into()));
        assert_eq!(h.eval(&mut ed, "[1, 2, 3][-1]").unwrap(), Value::Int(3));
        assert_eq!(h.eval(&mut ed, "len(split('a b  c'))").unwrap(), Value::Int(3));
        assert_eq!(h.eval(&mut ed, "substitute('hello', 'l\\+', 'L', '')").unwrap(), Value::Str("heLo".into()));
        assert_eq!(h.eval(&mut ed, "map([1, 2], 'v:val * 10')").unwrap(), Value::List(vec![Value::Int(10), Value::Int(20)]));
        assert_eq!(h.eval(&mut ed, "map([1, 2], {k, v -> v + k})").unwrap(), Value::List(vec![Value::Int(1), Value::Int(3)]));
        assert_eq!(h.eval(&mut ed, "'abc' =~ 'B'").unwrap(), Value::Bool(false));
        assert_eq!(h.eval(&mut ed, "'abc' =~? 'B'").unwrap(), Value::Bool(true));
    }

    #[test]
    fn vim_functions_and_control_flow() {
        let mut ed = editor();
        let h = ed.script.clone().unwrap();
        let src = "\
function! Fib(n)
  if a:n < 2
    return a:n
  endif
  return Fib(a:n - 1) + Fib(a:n - 2)
endfunction
let g:r = Fib(10)
let s:acc = []
for i in range(3)
  if i == 1 | continue | endif
  call add(s:acc, i)
endfor
let g:acc = join(s:acc, ',')
try
  call Nonexistent()
catch /E117/
  let g:caught = 1
finally
  let g:fin = 1
endtry
let g:d = {'a': 1, 'b': [1, 2]}
let g:d.c = 3
let g:keys = join(keys(g:d), '')
";
        h.exec_vim(&mut ed, src, "C:/test/x.vim").unwrap();
        assert_eq!(h.get_global(&mut ed, "r"), Some(Value::Int(55)));
        assert_eq!(h.get_global(&mut ed, "acc"), Some(Value::Str("0,2".into())));
        assert_eq!(h.get_global(&mut ed, "caught"), Some(Value::Int(1)));
        assert_eq!(h.get_global(&mut ed, "fin"), Some(Value::Int(1)));
        assert_eq!(h.get_global(&mut ed, "keys"), Some(Value::Str("abc".into())));
        assert!(ed.messages.iter().all(|m| m.group != "ErrorMsg"), "{:?}", ed.messages);
    }
}
