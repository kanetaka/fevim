//! Option definitions and values.  The table of names comes from Vim's
//! `optiondefs.h` (see `tools/gen_options.py`).

use std::collections::HashMap;
use std::sync::OnceLock;

pub use super::options_table::OPTION_DEFS;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptKind {
    Bool,
    Num,
    Str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OptScope {
    Global,
    Buf,
    Win,
}

#[derive(Debug)]
pub struct OptDef {
    pub name: &'static str,
    pub short: &'static str,
    pub kind: OptKind,
    pub scope: OptScope,
    pub comma: bool,
    pub flaglist: bool,
    pub expand: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum OptVal {
    Bool(bool),
    Num(i64),
    Str(String),
}

impl OptVal {
    pub fn as_bool(&self) -> bool {
        match self {
            OptVal::Bool(b) => *b,
            OptVal::Num(n) => *n != 0,
            OptVal::Str(s) => !s.is_empty(),
        }
    }
    pub fn as_num(&self) -> i64 {
        match self {
            OptVal::Bool(b) => *b as i64,
            OptVal::Num(n) => *n,
            OptVal::Str(s) => s.parse().unwrap_or(0),
        }
    }
    pub fn as_string(&self) -> String {
        match self {
            OptVal::Bool(b) => (*b as i64).to_string(),
            OptVal::Num(n) => n.to_string(),
            OptVal::Str(s) => s.clone(),
        }
    }
}

pub type LocalOpts = HashMap<&'static str, OptVal>;

fn index() -> &'static HashMap<&'static str, &'static OptDef> {
    static IDX: OnceLock<HashMap<&'static str, &'static OptDef>> = OnceLock::new();
    IDX.get_or_init(|| {
        let mut m = HashMap::new();
        for d in OPTION_DEFS {
            m.insert(d.name, d);
            if !d.short.is_empty() {
                m.insert(d.short, d);
            }
        }
        m
    })
}

pub fn find_def(name: &str) -> Option<&'static OptDef> {
    index().get(name).copied()
}

fn default_for(d: &OptDef) -> OptVal {
    let win = cfg!(windows);
    let s = |v: &str| OptVal::Str(v.to_string());
    match d.name {
        // numbers
        "tabstop" => OptVal::Num(8),
        "shiftwidth" => OptVal::Num(8),
        "scrolloff" => OptVal::Num(5),
        "sidescroll" => OptVal::Num(1),
        "laststatus" => OptVal::Num(1),
        "showtabline" => OptVal::Num(1),
        "cmdheight" => OptVal::Num(1),
        "numberwidth" => OptVal::Num(4),
        "history" => OptVal::Num(200),
        "timeoutlen" => OptVal::Num(1000),
        "ttimeoutlen" => OptVal::Num(100),
        "updatetime" => OptVal::Num(4000),
        "undolevels" => OptVal::Num(1000),
        "lines" => OptVal::Num(40),
        "columns" => OptVal::Num(120),
        "pumheight" => OptVal::Num(15),
        "matchtime" => OptVal::Num(5),
        "report" => OptVal::Num(2),
        "scroll" => OptVal::Num(0),
        "textwidth" => OptVal::Num(0),
        "linespace" => OptVal::Num(if win { 1 } else { 0 }),
        "maxmapdepth" => OptVal::Num(1000),
        "tabpagemax" => OptVal::Num(10),
        "winheight" | "winwidth" | "winminheight" | "winminwidth" => OptVal::Num(1),
        // booleans that default to on
        "wrap" | "wrapscan" | "magic" | "ruler" | "showcmd" | "showmode" | "incsearch" | "timeout" | "ttimeout"
        | "modifiable" | "modeline" | "buflisted" | "endofline" | "fixendofline" | "equalalways" | "startofline"
        | "joinspaces" | "smarttab" | "wildmenu" | "esckeys" | "swapfile" | "writebackup" | "loadplugins" | "remap"
        | "hidden" | "cursorbind" | "splitbelow" | "splitright" | "langremap" | "ttyfast" | "tildeop"
            if !matches!(
                d.name,
                "joinspaces" | "smarttab" | "hidden" | "cursorbind" | "splitbelow" | "splitright" | "tildeop" | "langremap"
            ) =>
        {
            OptVal::Bool(true)
        }
        // strings
        "backspace" => s("indent,eol,start"),
        "encoding" => s("utf-8"),
        "fileencodings" => s("ucs-bom,utf-8,cp932,euc-jp,latin1"),
        "fileformat" => s(if win { "dos" } else { "unix" }),
        "fileformats" => s(if win { "dos,unix" } else { "unix,dos" }),
        "formatoptions" => s("tcq"),
        "iskeyword" => s("@,48-57,_,192-255"),
        "matchpairs" => s("(:),{:},[:]"),
        "listchars" => s("eol:$"),
        "fillchars" => s("vert:|,fold:-,eob:~"),
        "background" => s("dark"),
        "selection" => s("inclusive"),
        "mouse" => s("a"),
        "guioptions" => s("egmrLT"),
        "guicursor" => s("n-v-c:block,o:hor50,i-ci:ver25,r-cr:hor20,sm:block"),
        "shortmess" => s("filnxtToOS"),
        "whichwrap" => s("b,s"),
        "nrformats" => s("bin,hex"),
        "cinkeys" => s("0{,0},0),0],:,0#,!^F,o,O,e"),
        "cinwords" => s("if,else,while,do,for,switch"),
        "comments" => s("s1:/*,mb:*,ex:*/,://,b:#,:%,:XCOMM,n:>,fb:-"),
        "commentstring" => s("/* %s */"),
        "virtualedit" => s(""),
        "shell" => s(if win { "cmd.exe" } else { "sh" }),
        "shellcmdflag" => s(if win { "/c" } else { "-c" }),
        "grepprg" => s(if win { "findstr /n" } else { "grep -n $* /dev/null" }),
        "viminfo" => s("'100,<50,s10,h"),
        "wildmode" => s("full"),
        "switchbuf" => s(""),
        "sessionoptions" => s("blank,buffers,curdir,folds,help,options,tabpages,winsize,terminal"),
        "display" => s("truncate"),
        "complete" => s(".,w,b,u,t,i"),
        "completeopt" => s("menu,preview"),
        "breakat" => s(" \t!@*-+;:,./?"),
        "highlight" => s(""),
        _ => match d.kind {
            OptKind::Bool => OptVal::Bool(false),
            OptKind::Num => OptVal::Num(0),
            OptKind::Str => OptVal::Str(String::new()),
        },
    }
}

/// Global option values.
pub struct Options {
    pub global: HashMap<&'static str, OptVal>,
}

impl Default for Options {
    fn default() -> Self {
        Self::new()
    }
}

impl Options {
    pub fn new() -> Options {
        let mut global = HashMap::new();
        for d in OPTION_DEFS {
            global.insert(d.name, default_for(d));
        }
        Options { global }
    }

    pub fn default_value(d: &OptDef) -> OptVal {
        default_for(d)
    }
}

/// Convert a textual value (as written after `:set opt=`) into a typed value.
pub fn parse_value(d: &OptDef, text: &str) -> Result<OptVal, String> {
    match d.kind {
        OptKind::Bool => Err(format!("E474: Invalid argument: {}={}", d.name, text)),
        OptKind::Num => {
            let t = text.trim();
            let v = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).ok()
            } else {
                t.parse::<i64>().ok()
            };
            v.map(OptVal::Num).ok_or_else(|| format!("E521: Number required after =: {}={}", d.name, text))
        }
        OptKind::Str => Ok(OptVal::Str(text.to_string())),
    }
}

/// `opt+=value`
pub fn append_value(d: &OptDef, cur: &OptVal, add: &OptVal) -> OptVal {
    match (cur, add) {
        (OptVal::Num(a), OptVal::Num(b)) => OptVal::Num(a + b),
        (OptVal::Str(a), OptVal::Str(b)) => {
            if d.comma {
                if a.split(',').any(|x| x == b) {
                    OptVal::Str(a.clone())
                } else if a.is_empty() {
                    OptVal::Str(b.clone())
                } else {
                    OptVal::Str(format!("{},{}", a, b))
                }
            } else if d.flaglist {
                let mut out = a.clone();
                for c in b.chars() {
                    if !out.contains(c) {
                        out.push(c);
                    }
                }
                OptVal::Str(out)
            } else {
                OptVal::Str(format!("{}{}", a, b))
            }
        }
        _ => cur.clone(),
    }
}

/// `opt^=value`
pub fn prepend_value(d: &OptDef, cur: &OptVal, add: &OptVal) -> OptVal {
    match (cur, add) {
        (OptVal::Num(a), OptVal::Num(b)) => OptVal::Num(a * b),
        (OptVal::Str(a), OptVal::Str(b)) => {
            if d.comma && !a.is_empty() {
                OptVal::Str(format!("{},{}", b, a))
            } else {
                OptVal::Str(format!("{}{}", b, a))
            }
        }
        _ => cur.clone(),
    }
}

/// `opt-=value`
pub fn remove_value(d: &OptDef, cur: &OptVal, rem: &OptVal) -> OptVal {
    match (cur, rem) {
        (OptVal::Num(a), OptVal::Num(b)) => OptVal::Num(a - b),
        (OptVal::Str(a), OptVal::Str(b)) => {
            if d.comma {
                OptVal::Str(a.split(',').filter(|x| x != b).collect::<Vec<_>>().join(","))
            } else if d.flaglist {
                OptVal::Str(a.chars().filter(|c| !b.contains(*c)).collect())
            } else {
                OptVal::Str(a.replacen(b.as_str(), "", 1))
            }
        }
        _ => cur.clone(),
    }
}
