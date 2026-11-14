//! FeVim: a reimplementation of gvim in Rust.
//!
//! * `editor` – the editing core (buffers, windows, modes, Ex commands, rendering grid)
//! * `script` – Lua scripting (`vim.*` API) and the Vim script → Lua translator
//! * `gui`    – the GPUI front end (feature `gui`)

pub mod editor;
pub mod script;

#[cfg(feature = "gui")]
pub mod gui;

use std::path::PathBuf;
use std::rc::Rc;

use editor::{Editor, Value};

/// Directory holding fevim's own configuration (`init.lua`, `ginit.lua`,
/// `colors/`, `lua/` ...).
///
/// * Windows: `%APPDATA%\fevim`
/// * macOS / Linux: `$XDG_CONFIG_HOME/fevim` or `~/.config/fevim`
///
/// `$FEVIM_CONFIG` overrides the location.
pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("FEVIM_CONFIG") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if cfg!(windows) {
        if let Ok(a) = std::env::var("APPDATA") {
            return PathBuf::from(a).join("fevim");
        }
    }
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("fevim");
        }
    }
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".config").join("fevim")
}

/// Home directory.
pub fn home_dir() -> Option<PathBuf> {
    editor::cmdline::home_dir().map(PathBuf::from)
}

/// Candidate locations of the user's Vim configuration files, in Vim's order.
pub fn vimrc_candidates(gui: bool) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let names: &[&str] = if gui { &["_gvimrc", ".gvimrc"] } else { &["_vimrc", ".vimrc"] };
    if let Some(h) = home_dir() {
        for n in names {
            v.push(h.join(n));
        }
        v.push(h.join(if cfg!(windows) { "vimfiles" } else { ".vim" }).join(if gui { "gvimrc" } else { "vimrc" }));
    }
    if let Ok(vim) = std::env::var("VIM") {
        for n in names {
            v.push(PathBuf::from(&vim).join(n));
        }
    }
    v
}

/// Command line options.
#[derive(Default, Debug, Clone)]
pub struct StartOptions {
    pub files: Vec<PathBuf>,
    /// `-u {vimrc}` (`NONE` skips all initialisation files).
    pub vimrc: Option<String>,
    /// `-U {gvimrc}`.
    pub gvimrc: Option<String>,
    /// `-c {cmd}` / `+{cmd}`.
    pub commands: Vec<String>,
    /// `--cmd {cmd}`: executed before any vimrc.
    pub pre_commands: Vec<String>,
    pub gui: bool,
    /// `--headless`: run the commands and exit (used for scripting/tests).
    pub headless: bool,
    /// `--transpile {file}`: print the Lua translation of a Vim script.
    pub transpile: Option<PathBuf>,
}

const USAGE: &str = "usage: fevim [options] [file ...]
  -u {vimrc}          use this vimrc/init.lua (NONE: skip all)
  -U {gvimrc}         use this gvimrc/ginit.lua (NONE: skip)
  -c {cmd}, +{cmd}    execute {cmd} after loading the first file
  --cmd {cmd}         execute {cmd} before loading any vimrc
  --clean             start without any configuration
  --headless          run the commands without a window and exit
  --transpile {file}  print the Lua translation of a Vim script

Configuration: init.lua (or your _vimrc / .vimrc) and ginit.lua (or _gvimrc)
in the fevim config directory.  :ImportVimrc {file} converts a vimrc into init.lua.";

impl StartOptions {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<StartOptions, String> {
        let mut o = StartOptions { gui: cfg!(feature = "gui"), ..Default::default() };
        let mut it = args.into_iter();
        let mut only_files = false;
        while let Some(a) = it.next() {
            if only_files {
                o.files.push(PathBuf::from(a));
                continue;
            }
            match a.as_str() {
                "--" => only_files = true,
                "-u" => o.vimrc = Some(it.next().ok_or("-u requires an argument")?),
                "-U" => o.gvimrc = Some(it.next().ok_or("-U requires an argument")?),
                "-c" => o.commands.push(it.next().ok_or("-c requires an argument")?),
                "--cmd" => o.pre_commands.push(it.next().ok_or("--cmd requires an argument")?),
                "--clean" => o.vimrc = Some("NONE".into()),
                "--headless" | "-e" | "-s" | "-es" => {
                    o.headless = true;
                    o.gui = false;
                }
                "--transpile" => o.transpile = Some(PathBuf::from(it.next().ok_or("--transpile requires a file")?)),
                "--version" | "-v" | "--help" | "-h" => {
                    return Err(format!(
                        "FeVim {} - gvim reimplemented in Rust (GPUI front end, Lua scripting)\nconfig directory: {}\n\n{}",
                        env!("CARGO_PKG_VERSION"),
                        config_dir().display(),
                        USAGE
                    ));
                }
                s if s.starts_with('+') => {
                    let c = &s[1..];
                    o.commands.push(if c.is_empty() { "$".into() } else { c.to_string() });
                }
                s if s.starts_with('-') && s.len() > 1 => {} // accepted for compatibility
                _ => o.files.push(PathBuf::from(a)),
            }
        }
        Ok(o)
    }
}

/// Create an editor with the Lua host attached and runtime paths set.
pub fn new_editor() -> Editor {
    let mut ed = Editor::new();
    match script::LuaHost::new() {
        Ok(h) => ed.script = Some(Rc::new(h)),
        Err(e) => ed.emsg(format!("fevim: Lua initialisation failed: {}", e)),
    }
    let mut dirs = vec![config_dir()];
    if let Some(h) = home_dir() {
        dirs.push(h.join(if cfg!(windows) { "vimfiles" } else { ".vim" }));
    }
    if let Ok(v) = std::env::var("VIM") {
        dirs.push(PathBuf::from(v).join("vimfiles"));
    }
    if let Ok(v) = std::env::var("VIMRUNTIME") {
        dirs.push(PathBuf::from(v));
    }
    ed.runtime_dirs = dirs;
    let rtp: Vec<String> = ed.runtime_dirs.iter().map(|p| p.to_string_lossy().replace('\\', "/")).collect();
    ed.opts.global.insert("runtimepath", editor::options::OptVal::Str(rtp.join(",")));
    ed
}

fn source_reporting(ed: &mut Editor, p: &std::path::Path) {
    if let Err(e) = ed.source_file(p) {
        ed.emsg(format!("Error detected while processing {}:\n{}", p.display(), e));
    }
}

/// Load the user's configuration: `init.lua` (or vimrc) and, for the GUI,
/// `ginit.lua` (or gvimrc).  Returns the files that were read.
pub fn load_config(ed: &mut Editor, o: &StartOptions) -> Vec<PathBuf> {
    let mut loaded = Vec::new();
    for c in &o.pre_commands {
        ed.exec_ex_reporting(c);
    }
    let pick = |explicit: &Option<String>, lua_name: &str, gui: bool| -> Option<PathBuf> {
        match explicit.as_deref() {
            Some("NONE") | Some("NORC") => None,
            Some(p) => Some(PathBuf::from(p)),
            None => {
                let lua = config_dir().join(lua_name);
                if lua.is_file() {
                    return Some(lua);
                }
                vimrc_candidates(gui).into_iter().find(|p| p.is_file())
            }
        }
    };
    let skip_all = matches!(o.vimrc.as_deref(), Some("NONE"));
    if let Some(p) = pick(&o.vimrc, "init.lua", false) {
        if std::env::var("VIM").is_err() {
            if let Some(dir) = p.parent() {
                // SAFETY: called once at start up, before any other thread exists.
                unsafe { std::env::set_var("VIM", dir) };
            }
        }
        if let Some(h) = ed.script.clone() {
            h.set_global(ed, "fevim_vimrc", Value::Str(p.to_string_lossy().replace('\\', "/")));
        }
        source_reporting(ed, &p);
        loaded.push(p);
    }
    // plugins: opt-in, because fevim only understands part of Vim script
    let load_plugins = !skip_all
        && ed.opt_global("loadplugins").as_bool()
        && ed.get_var("fevim_load_plugins").map(|v| v.truthy()).unwrap_or(false);
    if load_plugins {
        for dir in ed.runtime_dirs.clone() {
            let mut files = Vec::new();
            collect_plugins(&dir.join("plugin"), &mut files);
            files.sort();
            for f in files {
                source_reporting(ed, &f);
            }
        }
    }
    if o.gui && !skip_all {
        if let Some(p) = pick(&o.gvimrc, "ginit.lua", true) {
            source_reporting(ed, &p);
            loaded.push(p);
        }
    }
    loaded
}

fn collect_plugins(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_plugins(&p, out);
        } else if p.extension().map(|x| x == "vim" || x == "lua").unwrap_or(false) {
            out.push(p);
        }
    }
}

/// Open the files given on the command line and run the start-up events.
pub fn open_files(ed: &mut Editor, o: &StartOptions) {
    let mut first = true;
    for f in &o.files {
        if first {
            if let Err(e) = ed.edit_file(f, true) {
                ed.emsg(e);
            }
            first = false;
        } else {
            // further files become listed buffers, like Vim's argument list
            let id = ed.new_buffer();
            let fencs = ed.opt_global("fileencodings").as_string();
            let b = ed.buffers.get_mut(&id).unwrap();
            b.path = Some(f.clone());
            if let Ok(r) = editor::buffer::Buffer::read_file(f, &fencs) {
                b.lines = if r.lines.is_empty() { vec![String::new()] } else { r.lines };
                b.modified = false;
                b.opts.insert("fileformat", editor::options::OptVal::Str(r.fileformat.into()));
                b.opts.insert("fileencoding", editor::options::OptVal::Str(r.encoding));
            }
        }
    }
    if o.gui {
        ed.do_autocmd("GUIEnter", "");
    }
    ed.do_autocmd("VimEnter", "");
    ed.starting = false;
    for c in &o.commands {
        ed.exec_ex_reporting(c);
        if o.headless {
            flush_messages(ed);
        }
        if ed.quit {
            break;
        }
    }
    ed.after_input();
}

/// Print and clear pending messages (headless mode).
fn flush_messages(ed: &mut Editor) {
    for m in ed.output.drain(..) {
        eprintln!("{}", m.text);
    }
    if let Some(m) = ed.msg_line.take() {
        eprintln!("{}", m.text);
    }
}

/// Entry point used by `main`.
pub fn run() {
    let opts = match StartOptions::parse(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(msg) => {
            println!("{}", msg);
            return;
        }
    };
    if let Some(f) = &opts.transpile {
        match std::fs::read(f) {
            Ok(bytes) => {
                let text = editor::buffer::decode_bytes(&bytes, "ucs-bom,utf-8,cp932,latin1").lines.join("\n");
                match script::vimscript::transpile(&text, Some(&f.to_string_lossy().replace('\\', "/")), true) {
                    Ok(lua) => print!("{}", lua),
                    Err(e) => eprintln!("{}", e),
                }
            }
            Err(e) => eprintln!("{}: {}", f.display(), e),
        }
        return;
    }
    if opts.headless || !cfg!(feature = "gui") {
        let mut o = opts.clone();
        o.gui = false;
        o.headless = true;
        let mut ed = new_editor();
        load_config(&mut ed, &o);
        for m in ed.messages.drain(..) {
            eprintln!("{}", m.text);
        }
        ed.msg_line = None;
        open_files(&mut ed, &o);
        flush_messages(&mut ed);
        std::process::exit(ed.exit_code);
    }
    #[cfg(feature = "gui")]
    gui::run(opts);
}
