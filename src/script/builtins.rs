//! Vim built-in functions implemented in Rust (string, buffer, window,
//! file system ...).  Functions working on lists/dicts/funcrefs live in the
//! Lua prelude.

use std::path::{Path, PathBuf};

use crate::editor::buffer::Pos;
use crate::editor::excmds::{expand_env, expand_replacement, fnamemodify};
use crate::editor::keys;
use crate::editor::motion::{char_at, line_width, vcol_of};
use crate::editor::options::{self, OptVal};
use crate::editor::{Editor, MapRhs, Mode, RegKind, Value, VisualKind};

type R = Result<Value, String>;

fn s(args: &[Value], i: usize) -> String {
    args.get(i).map(|v| v.to_display()).unwrap_or_default()
}

fn n(args: &[Value], i: usize) -> i64 {
    args.get(i).map(|v| v.as_int()).unwrap_or(0)
}

fn has(args: &[Value], i: usize) -> bool {
    !matches!(args.get(i), None | Some(Value::Nil))
}

fn list_of_strings(v: Vec<String>) -> Value {
    Value::List(v.into_iter().map(Value::Str).collect())
}

fn b(v: bool) -> Value {
    Value::Int(v as i64)
}

/// Resolve a line specification ("." "$" "'x" "w0" "w$" "v" or a number) to a 0-based line.
fn lnum_arg(ed: &mut Editor, v: Option<&Value>) -> Option<usize> {
    let v = v?;
    let last = ed.buf().last_line();
    match v {
        Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
            let n = v.as_int();
            if n < 1 || n as usize > last + 1 { None } else { Some(n as usize - 1) }
        }
        Value::Str(t) => match t.as_str() {
            "." => Some(ed.cursor().line),
            "$" => Some(last),
            "w0" => Some(ed.win().topline),
            "w$" => {
                let w = ed.win();
                Some(ed.botline(w))
            }
            "v" => Some(if matches!(ed.mode, Mode::Visual(_)) { ed.visual_start.line } else { ed.cursor().line }),
            t if t.starts_with('\'') => {
                let c = t[1..].chars().next()?;
                ed.get_mark(c).map(|p| p.line)
            }
            t => {
                let n: i64 = t.trim().parse().ok()?;
                if n < 1 || n as usize > last + 1 { None } else { Some(n as usize - 1) }
            }
        },
        _ => None,
    }
}

fn pos_arg(ed: &mut Editor, v: Option<&Value>) -> Option<Pos> {
    match v? {
        Value::List(l) => {
            let line = l.first()?.as_int().max(1) as usize - 1;
            let col = l.get(1).map(|c| c.as_int()).unwrap_or(1).max(1) as usize - 1;
            Some(Pos::new(line, col))
        }
        Value::Str(t) => match t.as_str() {
            "." => Some(ed.cursor()),
            "$" => {
                let l = ed.cursor().line;
                Some(Pos::new(l, ed.line(l).len()))
            }
            "v" => Some(if matches!(ed.mode, Mode::Visual(_)) { ed.visual_start } else { ed.cursor() }),
            t if t.starts_with('\'') => {
                let c = t[1..].chars().next()?;
                ed.get_mark(c)
            }
            _ => None,
        },
        _ => None,
    }
}

fn buf_arg(ed: &Editor, v: Option<&Value>) -> Option<usize> {
    match v {
        None => Some(ed.cur_buf_id()),
        Some(Value::Int(n)) => {
            if *n == 0 {
                Some(ed.cur_buf_id())
            } else {
                ed.buffers.contains_key(&(*n as usize)).then_some(*n as usize)
            }
        }
        Some(Value::Str(t)) => match t.as_str() {
            "" | "%" => Some(ed.cur_buf_id()),
            "#" => ed.win().alt_buf,
            "$" => ed.buffers.keys().next_back().copied(),
            t => {
                if let Ok(n) = t.parse::<usize>() {
                    return ed.buffers.contains_key(&n).then_some(n);
                }
                let t = t.replace('\\', "/");
                ed.buffers.values().find(|b| b.display_name() == t).or_else(|| ed.buffers.values().find(|b| b.display_name().ends_with(&t))).map(|b| b.id)
            }
        },
        Some(v) => buf_arg(ed, Some(&Value::Int(v.as_int()))),
    }
}

fn win_arg(ed: &Editor, v: Option<&Value>) -> Option<usize> {
    let wins = ed.tab_wins();
    match v {
        None => Some(ed.cur_win_id()),
        Some(v) => {
            let n = v.as_int();
            if n == 0 {
                Some(ed.cur_win_id())
            } else if n >= 1000 {
                ed.windows.contains_key(&(n as usize)).then_some(n as usize)
            } else {
                wins.get(n as usize - 1).copied()
            }
        }
    }
}

/// Simple glob with `*`, `?`, `**` and `[...]`.
pub fn glob(pattern: &str) -> Vec<String> {
    let pat = expand_env(pattern).replace('\\', "/");
    if !pat.contains(['*', '?', '[']) {
        return if Path::new(&pat).exists() { vec![pat] } else { Vec::new() };
    }
    let (root, rest) = if pat.starts_with('/') {
        ("/".to_string(), pat[1..].to_string())
    } else if pat.len() >= 3 && pat.as_bytes()[1] == b':' && pat.as_bytes()[2] == b'/' {
        (pat[..3].to_string(), pat[3..].to_string())
    } else {
        (String::new(), pat.clone())
    };
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let mut out = Vec::new();
    walk(&root, &parts, &mut out);
    out.sort();
    out
}

fn walk(base: &str, parts: &[&str], out: &mut Vec<String>) {
    if out.len() > 10_000 {
        return;
    }
    let Some(first) = parts.first() else {
        out.push(base.trim_end_matches('/').to_string());
        return;
    };
    let dir = if base.is_empty() { "." } else { base };
    let join = |name: &str| if base.is_empty() { name.to_string() } else if base.ends_with('/') { format!("{}{}", base, name) } else { format!("{}/{}", base, name) };
    if *first == "**" {
        walk(base, &parts[1..], out);
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    let name = e.file_name().to_string_lossy().to_string();
                    if !name.starts_with('.') {
                        walk(&join(&name), parts, out);
                    }
                }
            }
        }
        return;
    }
    if !first.contains(['*', '?', '[']) {
        let p = join(first);
        if Path::new(&p).exists() {
            walk(&p, &parts[1..], out);
        }
        return;
    }
    let re = crate::editor::vregex::glob_to_regex(first);
    let re = if cfg!(windows) { format!("(?i){}", re) } else { re };
    let Ok(re) = regex::Regex::new(&re) else {
        return;
    };
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        for name in names {
            if name.starts_with('.') && !first.starts_with('.') {
                continue;
            }
            if re.is_match(&name) {
                walk(&join(&name), &parts[1..], out);
            }
        }
    }
}

pub fn executable(name: &str) -> bool {
    let p = Path::new(name);
    if p.components().count() > 1 {
        return p.is_file();
    }
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into()).split(';').map(|e| e.to_lowercase()).collect()
    } else {
        vec![String::new()]
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() && (!cfg!(windows) || name.contains('.')) {
            return true;
        }
        for e in &exts {
            if e.is_empty() {
                continue;
            }
            let mut c = cand.clone().into_os_string();
            c.push(e);
            if PathBuf::from(c).is_file() {
                return true;
            }
        }
    }
    false
}

/// Format like C printf (subset used by Vim scripts).
pub fn printf(fmt: &str, args: &[Value]) -> Result<String, String> {
    let mut out = String::new();
    let chars: Vec<char> = fmt.chars().collect();
    let mut i = 0;
    let mut ai = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut flags = String::new();
        while i < chars.len() && "-+ 0#".contains(chars[i]) {
            flags.push(chars[i]);
            i += 1;
        }
        let mut width: Option<usize> = None;
        if i < chars.len() && chars[i] == '*' {
            width = Some(args.get(ai).map(|v| v.as_int()).unwrap_or(0).max(0) as usize);
            ai += 1;
            i += 1;
        } else {
            let mut w = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                w.push(chars[i]);
                i += 1;
            }
            if !w.is_empty() {
                width = w.parse().ok();
            }
        }
        let mut prec: Option<usize> = None;
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            let mut p = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                p.push(chars[i]);
                i += 1;
            }
            prec = Some(p.parse().unwrap_or(0));
        }
        while i < chars.len() && "hlL".contains(chars[i]) {
            i += 1;
        }
        let Some(&conv) = chars.get(i) else {
            break;
        };
        i += 1;
        if conv == '%' {
            out.push('%');
            continue;
        }
        let arg = args.get(ai).cloned().unwrap_or(Value::Int(0));
        ai += 1;
        let body = match conv {
            's' | 'S' => {
                let t = arg.to_display();
                match prec {
                    Some(p) => t.chars().take(p).collect(),
                    None => t,
                }
            }
            'd' | 'i' | 'u' => {
                let v = arg.as_int();
                let mut t = v.abs().to_string();
                if let Some(p) = prec {
                    while t.len() < p {
                        t.insert(0, '0');
                    }
                }
                if v < 0 {
                    t.insert(0, '-');
                } else if flags.contains('+') {
                    t.insert(0, '+');
                } else if flags.contains(' ') {
                    t.insert(0, ' ');
                }
                t
            }
            'x' => format!("{}{:x}", if flags.contains('#') { "0x" } else { "" }, arg.as_int()),
            'X' => format!("{}{:X}", if flags.contains('#') { "0X" } else { "" }, arg.as_int()),
            'o' => format!("{:o}", arg.as_int()),
            'b' | 'B' => format!("{:b}", arg.as_int()),
            'c' => char::from_u32(arg.as_int() as u32).map(|c| c.to_string()).unwrap_or_default(),
            'f' | 'F' | 'e' | 'E' | 'g' | 'G' => {
                let f = match &arg {
                    Value::Float(f) => *f,
                    v => v.as_int() as f64,
                };
                match conv {
                    'e' | 'E' => format!("{:.*e}", prec.unwrap_or(6), f),
                    'g' | 'G' => format!("{}", f),
                    _ => format!("{:.*}", prec.unwrap_or(6), f),
                }
            }
            other => return Err(format!("E766: Invalid printf format: %{}", other)),
        };
        let w = width.unwrap_or(0);
        let len = body.chars().count();
        if len < w {
            let pad = w - len;
            if flags.contains('-') {
                out.push_str(&body);
                out.push_str(&" ".repeat(pad));
            } else if flags.contains('0') && !matches!(conv, 's' | 'S' | 'c') {
                let (sign, digits) = if body.starts_with(['-', '+']) { body.split_at(1) } else { ("", body.as_str()) };
                out.push_str(sign);
                out.push_str(&"0".repeat(pad));
                out.push_str(digits);
            } else {
                out.push_str(&" ".repeat(pad));
                out.push_str(&body);
            }
        } else {
            out.push_str(&body);
        }
    }
    Ok(out)
}

/// strftime() for the most common conversions.  Uses UTC unless the TZ
/// offset is known through the environment (FEVIM_TZ_OFFSET minutes).
pub fn strftime(fmt: &str, t: i64) -> String {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    let off: i64 = *OFFSET.get_or_init(|| std::env::var("FEVIM_TZ_OFFSET").ok().and_then(|v| v.parse().ok()).unwrap_or_else(local_offset_minutes));
    let t = t + off * 60;
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    let (h, mi, se) = (secs / 3600, secs % 3600 / 60, secs % 60);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let wday = (days + 4).rem_euclid(7);
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mut out = String::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('Y') => out.push_str(&y.to_string()),
            Some('y') => out.push_str(&format!("{:02}", y % 100)),
            Some('m') => out.push_str(&format!("{:02}", m)),
            Some('d') => out.push_str(&format!("{:02}", d)),
            Some('e') => out.push_str(&format!("{:2}", d)),
            Some('H') => out.push_str(&format!("{:02}", h)),
            Some('I') => out.push_str(&format!("{:02}", if h % 12 == 0 { 12 } else { h % 12 })),
            Some('M') => out.push_str(&format!("{:02}", mi)),
            Some('S') => out.push_str(&format!("{:02}", se)),
            Some('p') => out.push_str(if h < 12 { "AM" } else { "PM" }),
            Some('a') => out.push_str(WD[wday as usize]),
            Some('A') => out.push_str(["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][wday as usize]),
            Some('b') | Some('h') => out.push_str(MO[(m - 1) as usize]),
            Some('j') => out.push_str(&format!("{:03}", doy_from(y, m, d))),
            Some('c') => out.push_str(&format!("{} {} {:2} {:02}:{:02}:{:02} {}", WD[wday as usize], MO[(m - 1) as usize], d, h, mi, se, y)),
            Some('x') => out.push_str(&format!("{:02}/{:02}/{:02}", m, d, y % 100)),
            Some('X') => out.push_str(&format!("{:02}:{:02}:{:02}", h, mi, se)),
            Some('%') => out.push('%'),
            Some(o) => {
                out.push('%');
                out.push(o);
            }
            None => out.push('%'),
        }
    }
    out
}

fn doy_from(y: i64, m: i64, d: i64) -> i64 {
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let cum = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    cum[(m - 1) as usize] + d + if leap && m > 2 { 1 } else { 0 }
}

#[cfg(windows)]
fn local_offset_minutes() -> i64 {
    #[repr(C)]
    struct SystemTime {
        w: [u16; 8],
    }
    #[repr(C)]
    struct TimeZoneInformation {
        bias: i32,
        standard_name: [u16; 32],
        standard_date: SystemTime,
        standard_bias: i32,
        daylight_name: [u16; 32],
        daylight_date: SystemTime,
        daylight_bias: i32,
    }
    unsafe extern "system" {
        fn GetTimeZoneInformation(tz: *mut TimeZoneInformation) -> u32;
    }
    let mut tz: TimeZoneInformation = unsafe { std::mem::zeroed() };
    let r = unsafe { GetTimeZoneInformation(&mut tz) };
    let mut bias = tz.bias as i64;
    if r == 2 {
        bias += tz.daylight_bias as i64;
    }
    -bias
}

#[cfg(not(windows))]
fn local_offset_minutes() -> i64 {
    // `date +%z` is available on macOS and Linux
    std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1 } else { 1 };
                let h: i64 = s[1..3].parse().ok()?;
                let m: i64 = s[3..5].parse().ok()?;
                Some(sign * (h * 60 + m))
            } else {
                None
            }
        })
        .unwrap_or(0)
}

pub fn localtime() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn has_feature(ed: &Editor, f: &str) -> bool {
    let f = f.to_ascii_lowercase();
    if let Some(p) = f.strip_prefix("patch") {
        // patch-8.2.1234 or patch123
        return !p.is_empty();
    }
    match f.as_str() {
        "win32" | "win64" | "win95" | "win32unix" if f == "win32unix" => false,
        "win32" | "win64" | "win95" | "gui_win32" | "gui_win64" | "mswin" => cfg!(windows),
        "unix" => cfg!(unix),
        "linux" => cfg!(target_os = "linux"),
        "mac" | "macunix" | "osx" | "osxdarwin" | "gui_mac" => cfg!(target_os = "macos"),
        "bsd" => cfg!(target_os = "freebsd") || cfg!(target_os = "openbsd") || cfg!(target_os = "netbsd"),
        "gui" | "gui_running" => ed.gui_running,
        "fevim" | "lua" | "eval" | "autocmd" | "syntax" | "multi_byte" | "multi_byte_encoding" | "cmdline_hist" | "cmdline_compl"
        | "comments" | "cindent" | "smartindent" | "autochdir" | "clipboard" | "clipboard_working" | "unnamedplus" | "file_in_path"
        | "find_in_path" | "insert_expand" | "jumplist" | "linebreak" | "listcmds" | "localmap" | "mouse" | "modify_fname"
        | "path_extra" | "quickfix" | "reltime" | "scrollbind" | "smartcase" | "statusline" | "tag_binary" | "textobjects"
        | "title" | "user_commands" | "vertsplit" | "virtualedit" | "visual" | "visualextra" | "vreplace" | "wildignore"
        | "wildmenu" | "windows" | "writebackup" | "extra_search" | "byte_offset" | "float" | "lambda" | "num64" | "timers"
        | "vim_starting" | "multi_byte_ime" | "iconv" | "termguicolors" | "gui_gpui" => {
            if f == "vim_starting" {
                ed.starting
            } else if f == "multi_byte_ime" {
                ed.gui_running
            } else {
                true
            }
        }
        _ => false,
    }
}

/// Call a builtin.  `None` means "no such function".
pub fn call(ed: &mut Editor, name: &str, args: Vec<Value>) -> Option<R> {
    Some(call_inner(ed, name, args)?)
}

pub fn exists_rust(ed: &mut Editor, name: &str) -> bool {
    is_builtin(name) || ed.user_cmds.contains_key(name)
}

pub fn is_builtin(name: &str) -> bool {
    BUILTINS.binary_search(&name).is_ok()
}

/// Sorted list of the functions implemented here.
pub static BUILTINS: &[&str] = &[
    "append", "browse", "bufexists", "buflisted", "bufloaded", "bufname", "bufnr", "bufwinid", "bufwinnr", "byte2line", "byteidx",
    "changenr", "char2nr", "chdir", "col", "confirm", "cursor", "delete", "escape", "eventhandler", "executable", "exepath",
    "expand", "feedkeys", "filereadable", "filewritable", "finddir", "findfile", "fnameescape", "fnamemodify", "foldclosed",
    "foldclosedend", "foldlevel", "getbufline", "getchar", "getcharstr", "getcmdline", "getcmdpos", "getcmdtype", "getcurpos",
    "getcwd", "getenv", "getfontname", "getfperm", "getfsize", "getftime", "getftype", "getline", "getloclist", "getpid",
    "getpos", "getqflist", "getreg", "getregtype", "getwinposx", "getwinposy", "glob", "glob2regpat", "globpath", "has",
    "hasmapto", "histadd", "histget", "hlID", "hlexists", "hostname", "iconv", "indent", "input", "inputlist", "isdirectory",
    "line", "line2byte", "localtime", "maparg", "mapcheck", "match", "matchadd", "matchdelete", "matchend", "matchlist",
    "matchstr", "matchstrpos", "mkdir", "mode", "nr2char", "pathshorten", "printf", "pumvisible", "readfile", "reg_executing",
    "reg_recording", "rename", "repeat", "resolve", "screencol", "screenrow", "search", "searchpos", "setcmdpos", "setline",
    "setloclist", "setpos", "setqflist", "setreg", "shellescape", "simplify", "split", "stdpath", "str2nr", "strcharpart",
    "strchars", "strdisplaywidth", "strftime", "strgetchar", "stridx", "strlen", "strpart", "strridx", "strtrans", "strwidth",
    "substitute", "synID", "synIDattr", "synIDtrans", "tabpagebuflist", "tabpagenr", "tabpagewinnr",
    "tempname", "tolower", "toupper", "tr", "trim", "undofile", "virtcol", "visualmode", "win_getid", "win_gotoid", "winbufnr",
    "wincol", "winheight", "winline", "winnr", "winrestview", "winsaveview", "winwidth", "writefile",
];

fn call_inner(ed: &mut Editor, name: &str, a: Vec<Value>) -> Option<R> {
    let r: R = match name {
        // ------------------------------------------------------------ strings
        "strlen" => Ok(Value::Int(s(&a, 0).len() as i64)),
        "strchars" => Ok(Value::Int(s(&a, 0).chars().count() as i64)),
        "strwidth" | "strdisplaywidth" => Ok(Value::Int(line_width(&s(&a, 0), ed.opt_num("tabstop").max(1) as usize, ed.ambi_double()) as i64)),
        "strpart" => {
            let t = s(&a, 0);
            let start = n(&a, 1);
            let len = if has(&a, 2) { n(&a, 2) } else { t.len() as i64 };
            let (st, en) = (start.max(0) as usize, (start + len).max(0) as usize);
            let st = st.min(t.len());
            let en = en.min(t.len()).max(st);
            Ok(Value::Str(String::from_utf8_lossy(&t.as_bytes()[st..en]).into_owned()))
        }
        "strcharpart" => {
            let t = s(&a, 0);
            let start = n(&a, 1).max(0) as usize;
            let len = if has(&a, 2) { n(&a, 2).max(0) as usize } else { usize::MAX };
            Ok(Value::Str(t.chars().skip(start).take(len).collect()))
        }
        "strgetchar" => Ok(Value::Int(s(&a, 0).chars().nth(n(&a, 1).max(0) as usize).map(|c| c as i64).unwrap_or(-1))),
        "stridx" => {
            let (h, nd) = (s(&a, 0), s(&a, 1));
            let start = n(&a, 2).max(0) as usize;
            Ok(Value::Int(h.get(start..).and_then(|x| x.find(&nd)).map(|i| (i + start) as i64).unwrap_or(-1)))
        }
        "strridx" => {
            let (h, nd) = (s(&a, 0), s(&a, 1));
            let end = if has(&a, 2) { (n(&a, 2).max(0) as usize + nd.len()).min(h.len()) } else { h.len() };
            Ok(Value::Int(h.get(..end).and_then(|x| x.rfind(&nd)).map(|i| i as i64).unwrap_or(-1)))
        }
        "tolower" => Ok(Value::Str(s(&a, 0).to_lowercase())),
        "toupper" => Ok(Value::Str(s(&a, 0).to_uppercase())),
        "trim" => {
            let t = s(&a, 0);
            let mask: Vec<char> = if has(&a, 1) && !s(&a, 1).is_empty() { s(&a, 1).chars().collect() } else { vec![' ', '\t', '\n', '\r', '\x0b', '\x0c', '\u{a0}', '\0'] };
            let dir = n(&a, 2);
            let r = match dir {
                1 => t.trim_start_matches(&mask[..]).to_string(),
                2 => t.trim_end_matches(&mask[..]).to_string(),
                _ => t.trim_matches(&mask[..]).to_string(),
            };
            Ok(Value::Str(r))
        }
        "tr" => {
            let t = s(&a, 0);
            let from: Vec<char> = s(&a, 1).chars().collect();
            let to: Vec<char> = s(&a, 2).chars().collect();
            if from.len() != to.len() {
                return Some(Err("E475: Invalid argument: tr()".into()));
            }
            Ok(Value::Str(t.chars().map(|c| from.iter().position(|f| *f == c).map(|i| to[i]).unwrap_or(c)).collect()))
        }
        "repeat" => Ok(Value::Str(s(&a, 0).repeat(n(&a, 1).max(0) as usize))),
        "escape" => {
            let t = s(&a, 0);
            let chars = s(&a, 1);
            let mut out = String::new();
            for c in t.chars() {
                if chars.contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            Ok(Value::Str(out))
        }
        "fnameescape" => {
            let t = s(&a, 0);
            let mut out = String::new();
            for (i, c) in t.chars().enumerate() {
                if " \t\n*?[{`$\\%#'\"|!<".contains(c) || (i == 0 && (c == '+' || c == '>' || c == '-')) {
                    out.push('\\');
                }
                out.push(c);
            }
            Ok(Value::Str(out))
        }
        "shellescape" => {
            let t = s(&a, 0);
            Ok(Value::Str(if cfg!(windows) && !ed.opt_global("shellslash").as_bool() || cfg!(windows) && ed.opt_global("shell").as_string().contains("cmd") {
                format!("\"{}\"", t.replace('"', "\"\""))
            } else {
                format!("'{}'", t.replace('\'', "'\\''"))
            }))
        }
        "nr2char" => Ok(Value::Str(char::from_u32(n(&a, 0) as u32).map(|c| c.to_string()).unwrap_or_default())),
        "char2nr" => Ok(Value::Int(s(&a, 0).chars().next().map(|c| c as i64).unwrap_or(0))),
        "str2nr" => {
            let t = s(&a, 0);
            let t = t.trim_start();
            let base = if has(&a, 1) { n(&a, 1) as u32 } else { 10 };
            let (neg, t) = match t.strip_prefix('-') {
                Some(r) => (true, r),
                None => (false, t.strip_prefix('+').unwrap_or(t)),
            };
            let t = match base {
                16 => t.trim_start_matches("0x").trim_start_matches("0X"),
                2 => t.trim_start_matches("0b").trim_start_matches("0B"),
                8 => t.trim_start_matches("0o").trim_start_matches("0O"),
                _ => t,
            };
            let digits: String = t.chars().take_while(|c| c.is_digit(base)).collect();
            let v = i64::from_str_radix(&digits, base).unwrap_or(0);
            Ok(Value::Int(if neg { -v } else { v }))
        }
        "byteidx" => {
            let t = s(&a, 0);
            let i = n(&a, 1);
            if i < 0 {
                Ok(Value::Int(-1))
            } else {
                let i = i as usize;
                let cnt = t.chars().count();
                Ok(Value::Int(if i == cnt { t.len() as i64 } else { t.char_indices().nth(i).map(|(b, _)| b as i64).unwrap_or(-1) }))
            }
        }
        "strtrans" => Ok(Value::Str(s(&a, 0).chars().map(|c| if (c as u32) < 0x20 { format!("^{}", ((c as u8) + 0x40) as char) } else { c.to_string() }).collect())),
        "iconv" => Ok(Value::Str(s(&a, 0))),
        "printf" => printf(&s(&a, 0), &a[1.min(a.len())..]).map(Value::Str),
        "split" => {
            let t = s(&a, 0);
            let pat = if has(&a, 1) && !s(&a, 1).is_empty() { s(&a, 1) } else { "\\s\\+".to_string() };
            let keep = n(&a, 2) != 0;
            let re = match crate::editor::vregex::VimRegex::new(&pat, false, false) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            let mut out = Vec::new();
            let mut last = 0;
            let mut at = 0;
            while let Some(m) = re.find_at(&t, at) {
                if m.end == m.start {
                    // split between characters
                    if m.start >= t.len() {
                        break;
                    }
                    let nx = crate::editor::motion::next_char_end(&t, m.start);
                    if m.start > last || keep {
                        if m.start > 0 {
                            out.push(t[last..m.start].to_string());
                            last = m.start;
                        }
                    }
                    at = nx;
                    continue;
                }
                let piece = &t[last..m.start];
                if !piece.is_empty() || keep {
                    out.push(piece.to_string());
                }
                last = m.end;
                at = m.end;
            }
            let piece = &t[last.min(t.len())..];
            if !piece.is_empty() || keep {
                out.push(piece.to_string());
            }
            Ok(list_of_strings(out))
        }
        "match" | "matchend" | "matchstr" | "matchlist" | "matchstrpos" => {
            let t = s(&a, 0);
            let pat = s(&a, 1);
            let start = n(&a, 2).max(0) as usize;
            let ic = ed.opt_global("ignorecase").as_bool();
            let re = match crate::editor::vregex::VimRegex::new(&pat, ic, false) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            let start = start.min(t.len());
            let found = re.captures_at(&t, start);
            Ok(match (name, found) {
                ("match", Some((m, _))) => Value::Int(m.start as i64),
                ("matchend", Some((m, _))) => Value::Int(m.end as i64),
                ("matchstr", Some((m, _))) => Value::Str(t[m.start..m.end].to_string()),
                ("matchstrpos", Some((m, _))) => Value::List(vec![Value::Str(t[m.start..m.end].to_string()), Value::Int(m.start as i64), Value::Int(m.end as i64)]),
                ("matchlist", Some((_, groups))) => {
                    let mut v: Vec<Value> = groups.iter().map(|g| Value::Str(g.unwrap_or("").to_string())).collect();
                    while v.len() < 10 {
                        v.push(Value::Str(String::new()));
                    }
                    Value::List(v)
                }
                ("match", None) | ("matchend", None) => Value::Int(-1),
                ("matchstr", None) => Value::Str(String::new()),
                ("matchstrpos", None) => Value::List(vec![Value::Str(String::new()), Value::Int(-1), Value::Int(-1)]),
                _ => Value::List(Vec::new()),
            })
        }
        "substitute" => {
            let t = s(&a, 0);
            let pat = s(&a, 1);
            let rep = s(&a, 2);
            let flags = s(&a, 3);
            let ic = ed.opt_global("ignorecase").as_bool();
            let re = match crate::editor::vregex::VimRegex::new(&pat, ic, false) {
                Ok(r) => r,
                Err(e) => return Some(Err(e)),
            };
            let global = flags.contains('g');
            let mut out = String::new();
            let mut at = 0;
            let mut from = 0;
            while let Some((m, groups)) = re.captures_at(&t, from) {
                out.push_str(&t[at..m.start]);
                out.push_str(&expand_replacement(&rep, &groups).replace('\0', "\n"));
                at = m.end;
                if !global {
                    break;
                }
                if m.end == m.start {
                    match t[m.end..].chars().next() {
                        Some(c) => {
                            out.push(c);
                            at = m.end + c.len_utf8();
                            from = at;
                        }
                        None => break,
                    }
                } else {
                    from = m.end;
                }
            }
            out.push_str(&t[at.min(t.len())..]);
            Ok(Value::Str(out))
        }
        "glob2regpat" => Ok(Value::Str(crate::editor::vregex::glob_to_regex(&s(&a, 0)))),
        // -------------------------------------------------------- buffer text
        "getline" => {
            let Some(l) = lnum_arg(ed, a.first()) else {
                return Some(Ok(if has(&a, 1) { Value::List(Vec::new()) } else { Value::Str(String::new()) }));
            };
            if has(&a, 1) {
                let e = lnum_arg(ed, a.get(1)).unwrap_or(ed.buf().last_line());
                Ok(list_of_strings((l..=e).map(|i| ed.line(i).to_string()).collect()))
            } else {
                Ok(Value::Str(ed.line(l).to_string()))
            }
        }
        "getbufline" => {
            let Some(bid) = buf_arg(ed, a.first()) else {
                return Some(Ok(Value::List(Vec::new())));
            };
            let b = &ed.buffers[&bid];
            let last = b.last_line();
            let st = match a.get(1) {
                Some(Value::Str(x)) if x == "$" => last,
                Some(v) => (v.as_int().max(1) - 1) as usize,
                None => 0,
            };
            let en = match a.get(2) {
                Some(Value::Str(x)) if x == "$" => last,
                Some(v) => (v.as_int().max(1) - 1) as usize,
                None => st,
            };
            Ok(list_of_strings((st..=en.min(last)).map(|i| b.line(i).to_string()).collect()))
        }
        "setline" | "append" => {
            let lnum = match a.first() {
                Some(Value::Int(0)) if name == "append" => Some(usize::MAX),
                v => lnum_arg(ed, v).or_else(|| if name == "setline" && a.first().map(|v| v.as_int() as usize == ed.buf().line_count() + 1).unwrap_or(false) { Some(ed.buf().line_count()) } else { None }),
            };
            let Some(l) = lnum else {
                return Some(Ok(Value::Int(1)));
            };
            let lines: Vec<String> = match a.get(1) {
                Some(Value::List(v)) => v.iter().map(|x| x.to_display()).collect(),
                Some(v) => vec![v.to_display()],
                None => Vec::new(),
            };
            if name == "append" {
                let at = if l == usize::MAX { 0 } else { l + 1 };
                ed.buf_mut().insert_lines(at, lines);
            } else {
                for (i, t) in lines.into_iter().enumerate() {
                    let ln = l + i;
                    if ln >= ed.buf().line_count() {
                        let at = ed.buf().line_count();
                        ed.buf_mut().insert_lines(at, vec![t]);
                    } else {
                        ed.buf_mut().set_line(ln, t);
                    }
                }
            }
            ed.check_cursor();
            Ok(Value::Int(0))
        }
        "line" => {
            let r = lnum_arg(ed, a.first());
            Ok(Value::Int(r.map(|x| x as i64 + 1).unwrap_or(0)))
        }
        "col" => {
            let p = pos_arg(ed, a.first());
            Ok(Value::Int(match (a.first(), p) {
                (Some(Value::Str(x)), Some(p)) if x == "$" => p.col as i64 + 1,
                (_, Some(p)) => {
                    let l = ed.line(p.line.min(ed.buf().last_line()));
                    if l.is_empty() && !matches!(ed.mode, Mode::Insert) { 1 } else { p.col as i64 + 1 }
                }
                _ => 0,
            }))
        }
        "virtcol" => {
            let p = pos_arg(ed, a.first());
            let ts = ed.opt_num("tabstop").max(1) as usize;
            Ok(Value::Int(match p {
                Some(p) => {
                    let l = ed.line(p.line.min(ed.buf().last_line())).to_string();
                    let v = vcol_of(&l, p.col, ts, ed.ambi_double());
                    let w = char_at(&l, p.col).map(|c| if c == '\t' { ts - v % ts } else { crate::editor::motion::char_width(c, ed.ambi_double()) }).unwrap_or(1);
                    (v + w) as i64
                }
                None => 0,
            }))
        }
        "indent" => {
            let Some(l) = lnum_arg(ed, a.first()) else {
                return Some(Ok(Value::Int(-1)));
            };
            let ts = ed.opt_num("tabstop").max(1) as usize;
            let line = ed.line(l).to_string();
            Ok(Value::Int(line_width(crate::editor::motion::indent_of(&line), ts, false) as i64))
        }
        "line2byte" => {
            let Some(l) = lnum_arg(ed, a.first()) else {
                return Some(Ok(Value::Int(-1)));
            };
            Ok(Value::Int(ed.buf().lines[..l].iter().map(|x| x.len() as i64 + 1).sum::<i64>() + 1))
        }
        "byte2line" => {
            let mut off = n(&a, 0);
            for (i, l) in ed.buf().lines.iter().enumerate() {
                off -= l.len() as i64 + 1;
                if off <= 0 {
                    return Some(Ok(Value::Int(i as i64 + 1)));
                }
            }
            Ok(Value::Int(-1))
        }
        "cursor" => {
            let (l, c) = match a.first() {
                Some(Value::List(v)) => (v.first().map(|x| x.as_int()).unwrap_or(1), v.get(1).map(|x| x.as_int()).unwrap_or(1)),
                _ => (n(&a, 0), if has(&a, 1) { n(&a, 1) } else { ed.cursor().col as i64 + 1 }),
            };
            let line = if l == 0 { ed.cursor().line } else { (l.max(1) - 1) as usize };
            let col = (c.max(1) - 1) as usize;
            ed.set_cursor(Pos::new(line, col));
            ed.check_cursor();
            Ok(Value::Int(0))
        }
        "getpos" | "getcurpos" => {
            let p = if name == "getcurpos" { Some(ed.cursor()) } else { pos_arg(ed, a.first()) };
            let bufn = if name == "getpos" && s(&a, 0).starts_with('\'') { 0 } else { 0 };
            Ok(match p {
                Some(p) => Value::List(vec![Value::Int(bufn), Value::Int(p.line as i64 + 1), Value::Int(p.col as i64 + 1), Value::Int(0)]),
                None => Value::List(vec![Value::Int(0); 4]),
            })
        }
        "setpos" => {
            let spec = s(&a, 0);
            let Some(Value::List(v)) = a.get(1) else {
                return Some(Ok(Value::Int(-1)));
            };
            let l = v.get(1).map(|x| x.as_int()).unwrap_or(1).max(1) as usize - 1;
            let c = v.get(2).map(|x| x.as_int()).unwrap_or(1).max(1) as usize - 1;
            let p = Pos::new(l, c);
            if spec == "." {
                ed.set_cursor(p);
            } else if let Some(m) = spec.strip_prefix('\'').and_then(|x| x.chars().next()) {
                ed.set_mark(m, p);
            }
            Ok(Value::Int(0))
        }
        "search" | "searchpos" => {
            let pat = s(&a, 0);
            let flags = s(&a, 1);
            let back = flags.contains('b');
            let cur = ed.cursor();
            let saved_ws = ed.opt_global("wrapscan");
            if flags.contains('W') {
                ed.opts.global.insert("wrapscan", OptVal::Bool(false));
            } else if flags.contains('w') {
                ed.opts.global.insert("wrapscan", OptVal::Bool(true));
            }
            let from = if flags.contains('c') {
                if back { Pos::new(cur.line, cur.col + 1) } else if cur.col == 0 { Pos::new(cur.line.saturating_sub(1), usize::MAX / 2) } else { Pos::new(cur.line, cur.col - 1) }
            } else {
                cur
            };
            let from = if flags.contains('c') && !back && cur.col == 0 && cur.line == 0 {
                let last = ed.buf().last_line();
                Pos::new(last, ed.line(last).len())
            } else {
                from
            };
            let r = ed.search(&pat, !back, from, 1, false);
            ed.opts.global.insert("wrapscan", saved_ws);
            if let Some(p) = r {
                if !flags.contains('n') {
                    if flags.contains('s') {
                        ed.set_mark('\'', cur);
                    }
                    ed.set_cursor(p);
                }
                if name == "searchpos" {
                    Ok(Value::List(vec![Value::Int(p.line as i64 + 1), Value::Int(p.col as i64 + 1)]))
                } else {
                    Ok(Value::Int(p.line as i64 + 1))
                }
            } else if name == "searchpos" {
                Ok(Value::List(vec![Value::Int(0), Value::Int(0)]))
            } else {
                Ok(Value::Int(0))
            }
        }
        // --------------------------------------------------- buffers/windows
        "bufnr" => Ok(Value::Int(buf_arg(ed, a.first().or(Some(&Value::Str("%".into())))).map(|x| x as i64).unwrap_or(-1))),
        "bufname" => Ok(Value::Str(
            buf_arg(ed, a.first())
                .and_then(|id| ed.buffers.get(&id))
                .and_then(|b| b.path.as_ref().map(|_| b.display_name()))
                .unwrap_or_default(),
        )),
        "bufexists" | "buflisted" | "bufloaded" => Ok(b(buf_arg(ed, a.first()).map(|id| if name == "buflisted" { ed.buffers[&id].listed } else { true }).unwrap_or(false))),
        "bufwinnr" | "bufwinid" => {
            let bid = buf_arg(ed, a.first());
            let wins = ed.tab_wins();
            let r = bid.and_then(|bid| wins.iter().position(|w| ed.windows[w].buf == bid)).map(|i| if name == "bufwinid" { wins[i] as i64 } else { i as i64 + 1 }).unwrap_or(-1);
            Ok(Value::Int(r))
        }
        "winnr" => {
            let wins = ed.tab_wins();
            let arg = s(&a, 0);
            Ok(Value::Int(match arg.as_str() {
                "$" => wins.len() as i64,
                "#" => wins.iter().position(|w| *w == ed.tabs[ed.cur_tab].prev).map(|i| i as i64 + 1).unwrap_or(0),
                d if d.len() == 1 && "hjkl".contains(d) => {
                    let id = ed.window_in_direction(d.chars().next().unwrap(), 1);
                    wins.iter().position(|w| *w == id).map(|i| i as i64 + 1).unwrap_or(0)
                }
                _ => wins.iter().position(|w| *w == ed.cur_win_id()).map(|i| i as i64 + 1).unwrap_or(1),
            }))
        }
        "win_getid" => Ok(Value::Int(win_arg(ed, a.first()).map(|x| x as i64).unwrap_or(0))),
        "win_gotoid" => {
            let id = n(&a, 0) as usize;
            if ed.tab_wins().contains(&id) {
                ed.goto_window(id);
                Ok(Value::Int(1))
            } else {
                Ok(Value::Int(0))
            }
        }
        "winbufnr" => Ok(Value::Int(win_arg(ed, a.first()).map(|w| ed.windows[&w].buf as i64).unwrap_or(-1))),
        "winwidth" => Ok(Value::Int(win_arg(ed, a.first()).map(|w| ed.windows[&w].width as i64).unwrap_or(-1))),
        "winheight" => Ok(Value::Int(win_arg(ed, a.first()).map(|w| ed.windows[&w].height as i64).unwrap_or(-1))),
        "winline" => {
            let w = ed.win();
            Ok(Value::Int((w.cursor.line.saturating_sub(w.topline) + 1) as i64))
        }
        "wincol" => {
            let w = ed.win();
            let ts = ed.tabstop_of(w);
            let v = vcol_of(ed.cur_line(), w.cursor.col, ts, ed.ambi_double());
            Ok(Value::Int((v.saturating_sub(w.leftcol) + ed.number_width(w) + 1) as i64))
        }
        "winsaveview" => {
            let w = ed.win();
            Ok(Value::Dict(vec![
                ("lnum".into(), Value::Int(w.cursor.line as i64 + 1)),
                ("col".into(), Value::Int(w.cursor.col as i64)),
                ("coladd".into(), Value::Int(0)),
                ("curswant".into(), Value::Int(if w.curswant == usize::MAX { i32::MAX as i64 } else { w.curswant as i64 })),
                ("topline".into(), Value::Int(w.topline as i64 + 1)),
                ("topfill".into(), Value::Int(0)),
                ("leftcol".into(), Value::Int(w.leftcol as i64)),
                ("skipcol".into(), Value::Int(0)),
            ]))
        }
        "winrestview" => {
            if let Some(Value::Dict(d)) = a.first() {
                let get = |k: &str| d.iter().find(|(x, _)| x == k).map(|(_, v)| v.as_int());
                let cur = ed.cursor();
                let l = get("lnum").map(|x| (x.max(1) - 1) as usize).unwrap_or(cur.line);
                let c = get("col").map(|x| x.max(0) as usize).unwrap_or(cur.col);
                ed.set_cursor(Pos::new(l, c));
                if let Some(t) = get("topline") {
                    ed.win_mut().topline = (t.max(1) - 1) as usize;
                }
                if let Some(lc) = get("leftcol") {
                    ed.win_mut().leftcol = lc.max(0) as usize;
                }
                if let Some(cw) = get("curswant") {
                    ed.win_mut().curswant = if cw >= i32::MAX as i64 { usize::MAX } else { cw.max(0) as usize };
                }
            }
            Ok(Value::Int(0))
        }
        "tabpagenr" => Ok(Value::Int(if s(&a, 0) == "$" { ed.tabs.len() as i64 } else if s(&a, 0) == "#" { ed.prev_tab as i64 + 1 } else { ed.cur_tab as i64 + 1 })),
        "tabpagewinnr" => {
            let t = (n(&a, 0).max(1) - 1) as usize;
            let Some(tab) = ed.tabs.get(t) else {
                return Some(Ok(Value::Int(0)));
            };
            let leaves = tab.layout.leaves();
            Ok(Value::Int(if s(&a, 1) == "$" { leaves.len() as i64 } else { leaves.iter().position(|w| *w == tab.cur).map(|i| i as i64 + 1).unwrap_or(1) }))
        }
        "tabpagebuflist" => {
            let t = if has(&a, 0) { (n(&a, 0).max(1) - 1) as usize } else { ed.cur_tab };
            let Some(tab) = ed.tabs.get(t) else {
                return Some(Ok(Value::Int(0)));
            };
            Ok(Value::List(tab.layout.leaves().iter().map(|w| Value::Int(ed.windows[w].buf as i64)).collect()))
        }
        "mode" => Ok(Value::Str(
            match (ed.mode, ed.cmdline.is_some()) {
                (_, true) => "c",
                (Mode::Insert, _) => "i",
                (Mode::Replace, _) => "R",
                (Mode::Visual(VisualKind::Char), _) => "v",
                (Mode::Visual(VisualKind::Line), _) => "V",
                (Mode::Visual(VisualKind::Block), _) => "\x16",
                (Mode::Cmdline, _) => "c",
                (Mode::Normal, _) => "n",
            }
            .to_string(),
        )),
        "visualmode" => Ok(Value::Str(
            match ed.last_visual.map(|v| v.0) {
                Some(VisualKind::Char) => "v",
                Some(VisualKind::Line) => "V",
                Some(VisualKind::Block) => "\x16",
                None => "",
            }
            .to_string(),
        )),
        "pumvisible" => Ok(b(ed.completion.is_some())),
        "getcmdline" => Ok(Value::Str(ed.cmdline.as_ref().map(|c| c.text.clone()).unwrap_or_default())),
        "getcmdpos" => Ok(Value::Int(ed.cmdline.as_ref().map(|c| c.pos as i64 + 1).unwrap_or(0))),
        "getcmdtype" => Ok(Value::Str(ed.cmdline.as_ref().map(|c| c.kind.to_string()).unwrap_or_default())),
        "setcmdpos" => {
            if let Some(c) = &mut ed.cmdline {
                c.pos = ((n(&a, 0).max(1) - 1) as usize).min(c.text.len());
            }
            Ok(Value::Int(0))
        }
        "screenrow" => Ok(Value::Int(1)),
        "screencol" => Ok(Value::Int(1)),
        "getwinposx" => Ok(Value::Int(ed.win_pos.0 as i64)),
        "getwinposy" => Ok(Value::Int(ed.win_pos.1 as i64)),
        "getfontname" => Ok(Value::Str(ed.opt_global("guifont").as_string().split(',').next().unwrap_or("").to_string())),
        "reg_recording" => Ok(Value::Str(ed.recording.as_ref().map(|r| r.0.to_string()).unwrap_or_default())),
        "reg_executing" => Ok(Value::Str(String::new())),
        "eventhandler" | "changenr" | "screenrow_" => Ok(Value::Int(0)),
        "foldlevel" => Ok(Value::Int(0)),
        "foldclosed" | "foldclosedend" => Ok(Value::Int(-1)),
        "synID" | "synIDtrans" | "hlID" => Ok(Value::Int(if name == "hlID" && ed.hl.exists(&s(&a, 0)) { 1 } else { 0 })),
        "synIDattr" => Ok(Value::Str(String::new())),
        "hlexists" => Ok(b(ed.hl.exists(&s(&a, 0)))),
        "matchadd" => {
            let group = s(&a, 0);
            let pattern = s(&a, 1);
            let id = ed.win().matches.iter().map(|m| m.id).max().unwrap_or(3).max(3) + 1;
            ed.win_mut().matches.push(crate::editor::window::WinMatch { id, group, pattern });
            Ok(Value::Int(id as i64))
        }
        "matchdelete" => {
            let id = n(&a, 0) as usize;
            ed.win_mut().matches.retain(|m| m.id != id);
            Ok(Value::Int(0))
        }
        "getqflist" | "getloclist" => Ok(Value::List(Vec::new())),
        "setqflist" | "setloclist" => Ok(Value::Int(0)),
        // -------------------------------------------------------- registers
        "getreg" => {
            let r = s(&a, 0).chars().next().unwrap_or('"');
            Ok(Value::Str(ed.reg_get(r).map(|x| if x.kind == RegKind::Line { format!("{}\n", x.text) } else { x.text }).unwrap_or_default()))
        }
        "getregtype" => {
            let r = s(&a, 0).chars().next().unwrap_or('"');
            Ok(Value::Str(
                match ed.reg_get(r).map(|x| x.kind) {
                    Some(RegKind::Char) => "v".to_string(),
                    Some(RegKind::Line) => "V".to_string(),
                    Some(RegKind::Block) => "\x16".to_string(),
                    None => String::new(),
                },
            ))
        }
        "setreg" => {
            let r = s(&a, 0).chars().next().unwrap_or('"');
            let opt = s(&a, 2);
            let (text, kind) = match a.get(1) {
                Some(Value::List(v)) => (v.iter().map(|x| x.to_display()).collect::<Vec<_>>().join("\n"), RegKind::Line),
                Some(v) => {
                    let t = v.to_display();
                    if let Some(st) = t.strip_suffix('\n') {
                        (st.to_string(), RegKind::Line)
                    } else {
                        (t, RegKind::Char)
                    }
                }
                None => (String::new(), RegKind::Char),
            };
            let kind = if opt.contains('l') || opt.contains('V') {
                RegKind::Line
            } else if opt.contains('c') || opt.contains('v') {
                RegKind::Char
            } else if opt.contains('b') {
                RegKind::Block
            } else {
                kind
            };
            let r = if opt.contains('a') { r.to_ascii_uppercase() } else { r };
            ed.reg_set(r, text, kind);
            Ok(Value::Int(0))
        }
        // --------------------------------------------------------- mappings
        "maparg" | "mapcheck" => {
            let lhs = keys::parse_keys(&s(&a, 0));
            let modes = crate::editor::excmds::modes_from_str(&s(&a, 1));
            let modes = if s(&a, 1).is_empty() { crate::editor::MODE_N | crate::editor::MODE_X | crate::editor::MODE_O } else { modes };
            let found = ed.maps.iter().find(|m| m.modes & modes != 0 && if name == "maparg" { m.lhs == lhs } else { m.lhs.starts_with(&lhs) || lhs.starts_with(&m.lhs) });
            Ok(Value::Str(found.map(|m| match m.rhs {
                MapRhs::Lua(_) => "<Lua function>".to_string(),
                _ => m.rhs_text.clone(),
            }).unwrap_or_default()))
        }
        "hasmapto" => {
            let what = s(&a, 0);
            let modes = crate::editor::excmds::modes_from_str(&s(&a, 1));
            Ok(b(ed.maps.iter().any(|m| m.modes & modes != 0 && m.rhs_text.contains(&what))))
        }
        "feedkeys" => {
            let ks = keys::parse_keys_raw(&s(&a, 0));
            let mode = s(&a, 1);
            let remap = !mode.contains('n');
            if mode.contains('x') {
                ed.run_keys(&ks, remap);
            } else if mode.contains('i') {
                ed.feed_front_keys(&ks, remap);
            } else {
                ed.feed(&ks, remap);
            }
            Ok(Value::Int(0))
        }
        // -------------------------------------------------------- files
        "expand" => {
            let t = s(&a, 0);
            let list = n(&a, 2) != 0;
            let e = ed.expand_filename(&t);
            if e.contains(['*', '?']) {
                let g = glob(&e);
                return Some(Ok(if list { list_of_strings(g) } else { Value::Str(g.join("\n")) }));
            }
            Ok(if list { list_of_strings(vec![e]) } else { Value::Str(e) })
        }
        "fnamemodify" => Ok(Value::Str(fnamemodify(&s(&a, 0), &s(&a, 1)))),
        "simplify" => Ok(Value::Str(s(&a, 0))),
        "resolve" => Ok(Value::Str(std::fs::canonicalize(s(&a, 0)).map(|p| p.to_string_lossy().trim_start_matches("\\\\?\\").replace('\\', "/")).unwrap_or_else(|_| s(&a, 0)))),
        "pathshorten" => {
            let t = s(&a, 0).replace('\\', "/");
            let parts: Vec<&str> = t.split('/').collect();
            let n = parts.len();
            Ok(Value::Str(parts.iter().enumerate().map(|(i, p)| if i + 1 < n { p.chars().next().map(|c| c.to_string()).unwrap_or_default() } else { p.to_string() }).collect::<Vec<_>>().join("/")))
        }
        "filereadable" => Ok(b(Path::new(&expand_env(&s(&a, 0))).is_file())),
        "filewritable" => {
            let p = PathBuf::from(expand_env(&s(&a, 0)));
            Ok(Value::Int(if p.is_dir() { 2 } else if p.is_file() && !std::fs::metadata(&p).map(|m| m.permissions().readonly()).unwrap_or(true) { 1 } else { 0 }))
        }
        "isdirectory" => Ok(b(Path::new(&expand_env(&s(&a, 0))).is_dir())),
        "executable" => Ok(b(executable(&s(&a, 0)))),
        "exepath" => Ok(Value::Str(if executable(&s(&a, 0)) { s(&a, 0) } else { String::new() })),
        "getftype" => {
            let p = expand_env(&s(&a, 0));
            Ok(Value::Str(
                match std::fs::symlink_metadata(&p) {
                    Ok(m) if m.file_type().is_symlink() => "link",
                    Ok(m) if m.is_dir() => "dir",
                    Ok(_) => "file",
                    Err(_) => "",
                }
                .to_string(),
            ))
        }
        "getfsize" => Ok(Value::Int(std::fs::metadata(expand_env(&s(&a, 0))).map(|m| if m.is_dir() { 0 } else { m.len() as i64 }).unwrap_or(-1))),
        "getftime" => Ok(Value::Int(
            std::fs::metadata(expand_env(&s(&a, 0))).and_then(|m| m.modified()).ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(-1),
        )),
        "getfperm" => Ok(Value::Str(match std::fs::metadata(expand_env(&s(&a, 0))) {
            Ok(m) => if m.permissions().readonly() { "r--r--r--" } else { "rw-rw-rw-" }.to_string(),
            Err(_) => String::new(),
        })),
        "delete" => {
            let p = expand_env(&s(&a, 0));
            let f = s(&a, 1);
            let r = if f.contains("rf") { std::fs::remove_dir_all(&p) } else if f.contains('d') { std::fs::remove_dir(&p) } else { std::fs::remove_file(&p) };
            Ok(Value::Int(if r.is_ok() { 0 } else { -1 }))
        }
        "rename" => Ok(Value::Int(if std::fs::rename(expand_env(&s(&a, 0)), expand_env(&s(&a, 1))).is_ok() { 0 } else { -1 })),
        "mkdir" => {
            let p = expand_env(&s(&a, 0));
            let r = if s(&a, 1).contains('p') { std::fs::create_dir_all(&p) } else { std::fs::create_dir(&p) };
            Ok(b(r.is_ok() || (s(&a, 1).contains('p') && Path::new(&p).is_dir())))
        }
        "readfile" => {
            let p = expand_env(&s(&a, 0));
            match std::fs::read(&p) {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
                    let mut lines: Vec<String> = text.split('\n').map(|x| x.to_string()).collect();
                    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
                        lines.pop();
                    }
                    let max = n(&a, 2);
                    if max > 0 {
                        lines.truncate(max as usize);
                    } else if max < 0 {
                        let k = lines.len().saturating_sub((-max) as usize);
                        lines.drain(..k);
                    }
                    Ok(list_of_strings(lines))
                }
                Err(e) => Err(format!("E484: Can't open file {}: {}", p, e)),
            }
        }
        "writefile" => {
            let lines: Vec<String> = match a.first() {
                Some(Value::List(v)) => v.iter().map(|x| x.to_display()).collect(),
                Some(v) => vec![v.to_display()],
                None => Vec::new(),
            };
            let p = expand_env(&s(&a, 1));
            let flags = s(&a, 2);
            let mut text = lines.join("\n");
            if !flags.contains('b') {
                text.push('\n');
            }
            let r = if flags.contains('a') {
                use std::io::Write;
                std::fs::OpenOptions::new().append(true).create(true).open(&p).and_then(|mut f| f.write_all(text.as_bytes()))
            } else {
                std::fs::write(&p, text)
            };
            Ok(Value::Int(if r.is_ok() { 0 } else { -1 }))
        }
        "glob" => {
            let g = glob(&s(&a, 0));
            Ok(if n(&a, 2) != 0 { list_of_strings(g) } else { Value::Str(g.join("\n")) })
        }
        "globpath" => {
            let mut all = Vec::new();
            for dir in s(&a, 0).split(',') {
                if dir.is_empty() {
                    continue;
                }
                all.extend(glob(&format!("{}/{}", dir.trim_end_matches(['/', '\\']), s(&a, 1))));
            }
            Ok(if n(&a, 3) != 0 { list_of_strings(all) } else { Value::Str(all.join("\n")) })
        }
        "findfile" | "finddir" => {
            let what = s(&a, 0);
            let found = if Path::new(&what).exists() { what } else { String::new() };
            Ok(Value::Str(found))
        }
        "getcwd" => Ok(Value::Str(std::env::current_dir().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default())),
        "chdir" => {
            let old = std::env::current_dir().map(|p| p.to_string_lossy().replace('\\', "/")).unwrap_or_default();
            Ok(Value::Str(if std::env::set_current_dir(expand_env(&s(&a, 0))).is_ok() { old } else { String::new() }))
        }
        "tempname" => {
            let mut p = std::env::temp_dir();
            p.push(format!("fevim{}_{}.tmp", std::process::id(), localtime() % 100000 + rand_small()));
            Ok(Value::Str(p.to_string_lossy().replace('\\', "/")))
        }
        "undofile" => Ok(Value::Str(String::new())),
        "stdpath" => {
            let base = crate::config_dir();
            Ok(Value::Str(
                match s(&a, 0).as_str() {
                    "data" => base.join("data"),
                    "cache" => std::env::temp_dir().join("fevim"),
                    "state" => base.join("state"),
                    _ => base,
                }
                .to_string_lossy()
                .replace('\\', "/"),
            ))
        }
        // ----------------------------------------------------------- system
        "has" => Ok(b(has_feature(ed, &s(&a, 0)))),
        "getenv" => Ok(std::env::var(s(&a, 0)).map(Value::Str).unwrap_or(Value::Nil)),
        "hostname" => Ok(Value::Str(std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "localhost".into()))),
        "getpid" => Ok(Value::Int(std::process::id() as i64)),
        "localtime" => Ok(Value::Int(localtime())),
        "strftime" => Ok(Value::Str(strftime(&s(&a, 0), if has(&a, 1) { n(&a, 1) } else { localtime() }))),
        "input" | "browse" => Ok(Value::Str(if has(&a, 1) { s(&a, 1) } else { String::new() })),
        "inputlist" | "getchar" => Ok(Value::Int(0)),
        "getcharstr" => Ok(Value::Str(String::new())),
        "confirm" => Ok(Value::Int(n(&a, 3).max(1))),
        "histadd" => {
            let t = s(&a, 1);
            match s(&a, 0).as_str() {
                ":" | "cmd" => ed.cmd_history.push(t),
                "/" | "search" => ed.search_history.push(t),
                _ => {}
            }
            Ok(Value::Int(1))
        }
        "histget" => {
            let h = if matches!(s(&a, 0).as_str(), "/" | "search") { &ed.search_history } else { &ed.cmd_history };
            let i = if has(&a, 1) { n(&a, 1) } else { -1 };
            let idx = if i < 0 { h.len() as i64 + i } else { i - 1 };
            Ok(Value::Str(h.get(idx.max(0) as usize).cloned().unwrap_or_default()))
        }
        _ => return None,
    };
    Some(r)
}

fn rand_small() -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u64(localtime() as u64);
    (h.finish() % 10000) as i64
}

/// `exists()` checks that need editor state.
pub fn exists(ed: &mut Editor, what: &str) -> bool {
    if let Some(o) = what.strip_prefix('&').or_else(|| what.strip_prefix('+')) {
        let o = o.trim_start_matches("l:").trim_start_matches("g:");
        return options::find_def(o).is_some();
    }
    if let Some(c) = what.strip_prefix(':') {
        if ed.user_cmds.contains_key(c) {
            return true;
        }
        return crate::editor::ex::full_command_name(c).is_some();
    }
    if let Some(e) = what.strip_prefix("##") {
        return !e.is_empty();
    }
    if let Some(e) = what.strip_prefix('#') {
        let (group_or_event, rest) = e.split_once('#').unwrap_or((e, ""));
        let ev = crate::editor::canon_event(group_or_event);
        if ed.augroups.iter().any(|g| g == group_or_event) {
            if rest.is_empty() {
                return true;
            }
            let ev2 = crate::editor::canon_event(rest.split('#').next().unwrap_or(""));
            return ed.autocmds.iter().any(|a| a.group.as_deref() == Some(group_or_event) && a.event == ev2);
        }
        return ed.autocmds.iter().any(|a| a.event == ev);
    }
    if let Some(f) = what.strip_prefix('*') {
        return is_builtin(f);
    }
    if let Some(v) = what.strip_prefix('$') {
        return std::env::var(v).is_ok();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printf_and_time() {
        assert_eq!(printf("%3d|%-4s|%05.1f|%x", &[Value::Int(5), Value::Str("ab".into()), Value::Float(3.14159), Value::Int(255)]).unwrap(), "  5|ab  |003.1|ff");
        assert_eq!(strftime("%Y-%m-%d %H:%M:%S", 0).len(), 19);
        let mut v = BUILTINS.to_vec();
        v.sort();
        assert_eq!(v, BUILTINS.to_vec(), "BUILTINS must be sorted");
    }
}
