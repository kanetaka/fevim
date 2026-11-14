//! A small table-driven syntax highlighter and filetype detection.
//!
//! This is not Vim's regex based syntax engine; it is a generic lexer that
//! knows comments, strings, numbers and keyword classes per language and maps
//! them to the standard highlight groups (Comment, String, Statement, ...).

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum LineState {
    #[default]
    Normal,
    /// Inside block comment number N of the language.
    Block(u8),
    /// Inside multi-line string number N of the language.
    Str(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub group: &'static str,
}

pub struct StrDef {
    pub open: &'static str,
    pub close: &'static str,
    pub escape: bool,
    pub multiline: bool,
}

pub struct Lang {
    pub line_comments: &'static [&'static str],
    pub block_comments: &'static [(&'static str, &'static str)],
    pub strings: &'static [StrDef],
    pub statements: &'static [&'static str],
    pub types: &'static [&'static str],
    pub constants: &'static [&'static str],
    /// Lines starting with this character are preprocessor lines.
    pub preproc: Option<char>,
    pub ignore_case: bool,
    /// Extra characters allowed in identifiers.
    pub ident_extra: &'static str,
}

const fn s(open: &'static str, close: &'static str, escape: bool, multiline: bool) -> StrDef {
    StrDef { open, close, escape, multiline }
}

const DQ: StrDef = s("\"", "\"", true, false);
const SQ: StrDef = s("'", "'", true, false);

static C_LIKE: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[DQ, SQ],
    statements: &[
        "if", "else", "for", "while", "do", "switch", "case", "default", "break", "continue", "return", "goto", "sizeof",
        "try", "catch", "throw", "new", "delete", "using", "namespace", "operator", "typeid", "co_await", "co_return",
        "co_yield", "public", "private", "protected", "friend", "this", "typename", "template", "alignof", "decltype",
        "static_cast", "dynamic_cast", "const_cast", "reinterpret_cast", "discard",
    ],
    types: &[
        "int", "char", "short", "long", "float", "double", "void", "unsigned", "signed", "bool", "struct", "union", "enum",
        "class", "typedef", "static", "extern", "const", "constexpr", "volatile", "inline", "register", "auto", "virtual",
        "explicit", "mutable", "override", "final", "noexcept", "size_t", "ssize_t", "uint8_t", "uint16_t", "uint32_t",
        "uint64_t", "int8_t", "int16_t", "int32_t", "int64_t", "wchar_t", "char16_t", "char32_t", "FILE", "float2",
        "float3", "float4", "float4x4", "float3x3", "half", "half2", "half3", "half4", "matrix", "sampler", "sampler2D",
        "Texture2D", "SamplerState", "cbuffer", "technique", "pass", "in", "out", "inout", "uniform", "vec2", "vec3",
        "vec4", "mat3", "mat4",
    ],
    constants: &["true", "false", "NULL", "nullptr", "TRUE", "FALSE"],
    preproc: Some('#'),
    ignore_case: false,
    ident_extra: "",
};

static RUST: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[s("\"", "\"", true, true)],
    statements: &[
        "as", "async", "await", "break", "continue", "crate", "dyn", "else", "extern", "fn", "for", "if", "impl", "in",
        "let", "loop", "match", "mod", "move", "pub", "ref", "return", "super", "trait", "type", "unsafe", "use", "where",
        "while", "macro_rules",
    ],
    types: &[
        "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
        "f32", "f64", "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "Self", "struct", "enum", "union", "const",
        "static", "mut", "HashMap", "HashSet",
    ],
    constants: &["true", "false", "None", "Some", "Ok", "Err", "self"],
    preproc: None,
    ignore_case: false,
    ident_extra: "",
};

static PYTHON: Lang = Lang {
    line_comments: &["#"],
    block_comments: &[],
    strings: &[s("\"\"\"", "\"\"\"", true, true), s("'''", "'''", true, true), DQ, SQ],
    statements: &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except",
        "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise",
        "return", "try", "while", "with", "yield", "match", "case",
    ],
    types: &["int", "float", "str", "bool", "list", "dict", "set", "tuple", "bytes", "object", "type"],
    constants: &["True", "False", "None", "self", "cls"],
    preproc: Some('@'),
    ignore_case: false,
    ident_extra: "",
};

static JAVASCRIPT: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[DQ, SQ, s("`", "`", true, true)],
    statements: &[
        "break", "case", "catch", "class", "const", "continue", "debugger", "default", "delete", "do", "else", "export",
        "extends", "finally", "for", "function", "if", "import", "in", "instanceof", "let", "new", "of", "return", "static",
        "super", "switch", "this", "throw", "try", "typeof", "var", "void", "while", "with", "yield", "async", "await",
        "from", "as", "interface", "type", "enum", "implements", "namespace", "declare", "readonly", "public", "private",
        "protected", "abstract",
    ],
    types: &[
        "number", "string", "boolean", "any", "unknown", "never", "object", "symbol", "bigint", "Array", "Promise", "Map",
        "Set", "Object", "String", "Number", "Boolean", "Date", "RegExp", "Error",
    ],
    constants: &["true", "false", "null", "undefined", "NaN", "Infinity"],
    preproc: None,
    ignore_case: false,
    ident_extra: "$",
};

static LUA: Lang = Lang {
    line_comments: &["--"],
    block_comments: &[("--[[", "]]")],
    strings: &[DQ, SQ, s("[[", "]]", false, true)],
    statements: &[
        "and", "break", "do", "else", "elseif", "end", "for", "function", "goto", "if", "in", "local", "not", "or",
        "repeat", "return", "then", "until", "while",
    ],
    types: &[],
    constants: &["true", "false", "nil", "self"],
    preproc: None,
    ignore_case: false,
    ident_extra: "",
};

static VIM: Lang = Lang {
    line_comments: &[],
    block_comments: &[],
    strings: &[s("'", "'", false, false)],
    statements: &[
        "if", "else", "elseif", "endif", "while", "endwhile", "for", "endfor", "in", "function", "endfunction", "return",
        "let", "unlet", "set", "setlocal", "setl", "setglobal", "call", "execute", "exec", "exe", "echo", "echom",
        "echomsg", "echoerr", "echohl", "try", "catch", "finally", "endtry", "throw", "autocmd", "au", "augroup",
        "highlight", "hi", "syntax", "syn", "colorscheme", "map", "nmap", "vmap", "imap", "cmap", "omap", "xmap",
        "noremap", "nnoremap", "vnoremap", "inoremap", "cnoremap", "onoremap", "xnoremap", "command", "source", "runtime",
        "filetype", "normal", "silent", "end", "finish", "break", "continue", "def", "enddef", "const", "var", "match",
        "2match", "abort",
    ],
    types: &[],
    constants: &[],
    preproc: None,
    ignore_case: false,
    ident_extra: ":#",
};

static SHELL: Lang = Lang {
    line_comments: &["#"],
    block_comments: &[],
    strings: &[DQ, s("'", "'", false, true)],
    statements: &[
        "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac", "in", "function",
        "return", "exit", "local", "export", "readonly", "set", "unset", "shift", "echo", "cd", "source", "break",
        "continue",
    ],
    types: &[],
    constants: &["true", "false"],
    preproc: None,
    ignore_case: false,
    ident_extra: "",
};

static JSON: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[DQ],
    statements: &[],
    types: &[],
    constants: &["true", "false", "null"],
    preproc: None,
    ignore_case: false,
    ident_extra: "",
};

static TOML: Lang = Lang {
    line_comments: &["#"],
    block_comments: &[],
    strings: &[s("\"\"\"", "\"\"\"", true, true), DQ, SQ],
    statements: &[],
    types: &[],
    constants: &["true", "false"],
    preproc: Some('['),
    ignore_case: false,
    ident_extra: "-",
};

static GO: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[DQ, SQ, s("`", "`", false, true)],
    statements: &[
        "break", "case", "chan", "const", "continue", "default", "defer", "else", "fallthrough", "for", "func", "go",
        "goto", "if", "import", "interface", "map", "package", "range", "return", "select", "struct", "switch", "type",
        "var",
    ],
    types: &[
        "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int", "int8", "int16", "int32", "int64",
        "rune", "string", "uint", "uint8", "uint16", "uint32", "uint64", "uintptr", "any",
    ],
    constants: &["true", "false", "nil", "iota"],
    preproc: None,
    ignore_case: false,
    ident_extra: "",
};

static JAVA: Lang = Lang {
    line_comments: &["//"],
    block_comments: &[("/*", "*/")],
    strings: &[s("\"\"\"", "\"\"\"", true, true), DQ, SQ],
    statements: &[
        "abstract", "assert", "break", "case", "catch", "class", "continue", "default", "do", "else", "enum", "extends",
        "finally", "for", "if", "implements", "import", "instanceof", "interface", "new", "package", "return", "super",
        "switch", "this", "throw", "throws", "try", "while", "using", "namespace", "foreach", "in", "is", "as", "get",
        "set", "await", "async", "yield", "lock",
    ],
    types: &[
        "boolean", "byte", "char", "double", "float", "int", "long", "short", "void", "final", "private", "protected",
        "public", "static", "synchronized", "transient", "volatile", "native", "String", "Object", "var", "bool",
        "string", "object", "decimal", "uint", "ulong", "readonly", "const", "override", "virtual", "sealed", "struct",
    ],
    constants: &["true", "false", "null"],
    preproc: Some('#'),
    ignore_case: false,
    ident_extra: "",
};

static SQL: Lang = Lang {
    line_comments: &["--"],
    block_comments: &[("/*", "*/")],
    strings: &[SQ, DQ],
    statements: &[
        "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create", "table", "drop",
        "alter", "index", "join", "left", "right", "inner", "outer", "on", "group", "by", "order", "having", "limit",
        "union", "and", "or", "not", "as", "in", "is", "like", "between", "case", "when", "then", "else", "end",
        "primary", "key", "foreign", "references", "begin", "commit", "rollback", "distinct",
    ],
    types: &["int", "integer", "varchar", "char", "text", "date", "datetime", "timestamp", "float", "double", "boolean"],
    constants: &["null", "true", "false"],
    preproc: None,
    ignore_case: true,
    ident_extra: "",
};

static INI: Lang = Lang {
    line_comments: &[";", "#"],
    block_comments: &[],
    strings: &[DQ],
    statements: &[],
    types: &[],
    constants: &["true", "false", "yes", "no", "on", "off"],
    preproc: Some('['),
    ignore_case: true,
    ident_extra: "-",
};

static MARKUP: Lang = Lang {
    line_comments: &[],
    block_comments: &[("<!--", "-->")],
    strings: &[DQ, SQ],
    statements: &[],
    types: &[],
    constants: &[],
    preproc: None,
    ignore_case: false,
    ident_extra: "-",
};

static BATCH: Lang = Lang {
    line_comments: &["rem ", "REM ", "::"],
    block_comments: &[],
    strings: &[DQ],
    statements: &[
        "if", "else", "for", "in", "do", "goto", "call", "set", "echo", "exit", "not", "exist", "defined", "setlocal",
        "endlocal", "pause", "shift",
    ],
    types: &[],
    constants: &[],
    preproc: Some('@'),
    ignore_case: true,
    ident_extra: "",
};

pub fn lang_for(filetype: &str) -> Option<&'static Lang> {
    Some(match filetype {
        "c" | "cpp" | "objc" | "hlsl" | "glsl" | "cuda" => &C_LIKE,
        "rust" => &RUST,
        "python" => &PYTHON,
        "javascript" | "typescript" | "javascriptreact" | "typescriptreact" => &JAVASCRIPT,
        "lua" => &LUA,
        "vim" => &VIM,
        "sh" | "bash" | "zsh" | "fish" | "make" | "yaml" | "conf" | "ps1" | "cmake" | "dockerfile" | "ruby" | "perl" => &SHELL,
        "json" | "jsonc" => &JSON,
        "toml" => &TOML,
        "go" => &GO,
        "java" | "cs" | "kotlin" | "swift" | "scala" | "dart" | "php" => &JAVA,
        "sql" => &SQL,
        "dosini" | "gitconfig" => &INI,
        "html" | "xml" | "markdown" | "css" | "svg" => &MARKUP,
        "dosbatch" => &BATCH,
        _ => return None,
    })
}

/// Detect the filetype from a file name (and optionally the first line).
pub fn detect_filetype(path: &Path, first_line: &str) -> Option<&'static str> {
    let name = path.file_name()?.to_string_lossy().to_string();
    let lower = name.to_ascii_lowercase();
    let by_name = match lower.as_str() {
        "makefile" | "gnumakefile" => Some("make"),
        "cmakelists.txt" => Some("cmake"),
        "dockerfile" => Some("dockerfile"),
        "_vimrc" | ".vimrc" | "_gvimrc" | ".gvimrc" | "vimrc" | "gvimrc" => Some("vim"),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" => Some("sh"),
        ".gitconfig" => Some("gitconfig"),
        "cargo.lock" => Some("toml"),
        _ => None,
    };
    if by_name.is_some() {
        return by_name;
    }
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    let by_ext = match ext {
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "inl" | "ipp" => Some("cpp"),
        "m" | "mm" => Some("objc"),
        "rs" => Some("rust"),
        "py" | "pyw" | "pyi" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "jsx" => Some("javascriptreact"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("typescriptreact"),
        "lua" => Some("lua"),
        "vim" => Some("vim"),
        "sh" | "bash" => Some("sh"),
        "zsh" => Some("zsh"),
        "json" => Some("json"),
        "jsonc" => Some("jsonc"),
        "toml" => Some("toml"),
        "yaml" | "yml" => Some("yaml"),
        "go" => Some("go"),
        "java" => Some("java"),
        "cs" => Some("cs"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "php" => Some("php"),
        "rb" => Some("ruby"),
        "pl" | "pm" => Some("perl"),
        "sql" => Some("sql"),
        "ini" | "cfg" => Some("dosini"),
        "html" | "htm" => Some("html"),
        "xml" | "xaml" | "csproj" | "vcxproj" => Some("xml"),
        "svg" => Some("svg"),
        "css" | "scss" | "less" => Some("css"),
        "md" | "markdown" => Some("markdown"),
        "bat" | "cmd" => Some("dosbatch"),
        "ps1" => Some("ps1"),
        "hlsl" | "fx" | "fxh" => Some("hlsl"),
        "glsl" | "vert" | "frag" | "comp" => Some("glsl"),
        "cmake" => Some("cmake"),
        "mk" => Some("make"),
        "txt" => Some("text"),
        "diff" | "patch" => Some("diff"),
        _ => None,
    };
    if by_ext.is_some() {
        return by_ext;
    }
    if let Some(rest) = first_line.strip_prefix("#!") {
        if rest.contains("python") {
            return Some("python");
        }
        if rest.contains("sh") {
            return Some("sh");
        }
        if rest.contains("node") {
            return Some("javascript");
        }
        if rest.contains("lua") {
            return Some("lua");
        }
        if rest.contains("perl") {
            return Some("perl");
        }
        if rest.contains("ruby") {
            return Some("ruby");
        }
    }
    None
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn find_close(line: &str, from: usize, close: &str, escape: bool) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        if escape && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if line[i..].starts_with(close) {
            return Some(i + close.len());
        }
        i += 1;
        while i < bytes.len() && !line.is_char_boundary(i) {
            i += 1;
        }
    }
    None
}

fn todo_spans(line: &str, start: usize, end: usize, out: &mut Vec<Span>) {
    for word in ["TODO", "FIXME", "XXX", "NOTE"] {
        let mut at = start;
        while let Some(p) = line[at..end].find(word) {
            out.push(Span { start: at + p, end: at + p + word.len(), group: "Todo" });
            at += p + word.len();
        }
    }
}

/// Highlight one line.  Returns the spans (non-overlapping except for Todo,
/// which is layered on comments) and the state at the start of the next line.
pub fn highlight_line(lang: &Lang, filetype: &str, line: &str, state: LineState) -> (Vec<Span>, LineState) {
    let mut spans = Vec::new();
    let mut i = 0;
    let len = line.len();
    match state {
        LineState::Block(n) => {
            let (_, close) = lang.block_comments[n as usize];
            match line.find(close) {
                Some(p) => {
                    spans.push(Span { start: 0, end: p + close.len(), group: "Comment" });
                    todo_spans(line, 0, p, &mut spans);
                    i = p + close.len();
                }
                None => {
                    spans.push(Span { start: 0, end: len, group: "Comment" });
                    todo_spans(line, 0, len, &mut spans);
                    return (spans, state);
                }
            }
        }
        LineState::Str(n) => {
            let d = &lang.strings[n as usize];
            match find_close(line, 0, d.close, d.escape) {
                Some(e) => {
                    spans.push(Span { start: 0, end: e, group: "String" });
                    i = e;
                }
                None => {
                    spans.push(Span { start: 0, end: len, group: "String" });
                    return (spans, state);
                }
            }
        }
        LineState::Normal => {}
    }
    let trimmed = line.trim_start();
    let indent = len - trimmed.len();
    if i <= indent {
        if let Some(p) = lang.preproc {
            if trimmed.starts_with(p) && !(p == '[' && filetype == "json") {
                let end = if p == '#' || p == '@' && filetype == "dosbatch" {
                    // keep trailing comments on preprocessor lines highlighted as comments
                    lang.line_comments.iter().filter_map(|c| line.find(c)).min().unwrap_or(len)
                } else if p == '[' {
                    line.find(']').map(|x| x + 1).unwrap_or(len)
                } else {
                    // decorators: the identifier only
                    indent
                        + 1
                        + trimmed[1..].find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).unwrap_or(trimmed.len() - 1)
                };
                if p != '@' || filetype != "dosbatch" {
                    spans.push(Span { start: indent, end, group: "PreProc" });
                    i = end;
                }
            }
        }
        if filetype == "vim" && trimmed.starts_with('"') {
            spans.push(Span { start: indent, end: len, group: "Comment" });
            return (spans, LineState::Normal);
        }
        if filetype == "markdown" && trimmed.starts_with('#') {
            spans.push(Span { start: indent, end: len, group: "Title" });
            return (spans, LineState::Normal);
        }
        if filetype == "diff" {
            let g = match line.chars().next() {
                Some('+') => "Identifier",
                Some('-') => "Special",
                Some('@') => "Statement",
                _ => "",
            };
            if !g.is_empty() {
                spans.push(Span { start: 0, end: len, group: g });
            }
            return (spans, LineState::Normal);
        }
    }
    let mut prev_ident = false;
    'outer: while i < len {
        let rest = &line[i..];
        let c = rest.chars().next().unwrap();
        if !prev_ident {
            for lc in lang.line_comments {
                if rest.starts_with(lc) {
                    // Lua: "--[[" is a block comment, not a line comment.
                    if lang.block_comments.iter().any(|(o, _)| o.len() > lc.len() && rest.starts_with(o)) {
                        break;
                    }
                    spans.push(Span { start: i, end: len, group: "Comment" });
                    todo_spans(line, i, len, &mut spans);
                    return (spans, LineState::Normal);
                }
            }
            for (n, (open, close)) in lang.block_comments.iter().enumerate() {
                if rest.starts_with(open) {
                    match line[i + open.len()..].find(close) {
                        Some(p) => {
                            let end = i + open.len() + p + close.len();
                            spans.push(Span { start: i, end, group: "Comment" });
                            todo_spans(line, i, end, &mut spans);
                            i = end;
                            continue 'outer;
                        }
                        None => {
                            spans.push(Span { start: i, end: len, group: "Comment" });
                            todo_spans(line, i, len, &mut spans);
                            return (spans, LineState::Block(n as u8));
                        }
                    }
                }
            }
            for (n, d) in lang.strings.iter().enumerate() {
                if rest.starts_with(d.open) {
                    // Rust lifetimes / char literals: only treat 'x' as a string when closed nearby.
                    match find_close(line, i + d.open.len(), d.close, d.escape) {
                        Some(end) => {
                            spans.push(Span { start: i, end, group: "String" });
                            i = end;
                            continue 'outer;
                        }
                        None => {
                            if d.multiline {
                                spans.push(Span { start: i, end: len, group: "String" });
                                return (spans, LineState::Str(n as u8));
                            }
                            if d.open == "\"" && filetype == "vim" {
                                spans.push(Span { start: i, end: len, group: "Comment" });
                                return (spans, LineState::Normal);
                            }
                            if d.open == "\"" {
                                spans.push(Span { start: i, end: len, group: "String" });
                                return (spans, LineState::Normal);
                            }
                        }
                    }
                }
            }
        }
        if c.is_ascii_digit() && !prev_ident {
            let end = i + rest
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                .unwrap_or(rest.len());
            spans.push(Span { start: i, end, group: "Number" });
            i = end;
            prev_ident = false;
            continue;
        }
        if is_ident_start(c) || lang.ident_extra.contains(c) && c != '-' && c != ':' && c != '#' {
            let end = i + rest
                .find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || lang.ident_extra.contains(ch)))
                .unwrap_or(rest.len());
            let word = &line[i..end];
            let has = |list: &[&str]| {
                if lang.ignore_case {
                    list.iter().any(|w| w.eq_ignore_ascii_case(word))
                } else {
                    list.contains(&word)
                }
            };
            let group = if has(lang.statements) {
                "Statement"
            } else if has(lang.types) {
                "Type"
            } else if has(lang.constants) {
                "Constant"
            } else {
                ""
            };
            if !group.is_empty() {
                spans.push(Span { start: i, end, group });
            }
            i = end;
            prev_ident = false;
            continue;
        }
        prev_ident = false;
        i += c.len_utf8();
    }
    (spans, LineState::Normal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_line() {
        let lang = lang_for("c").unwrap();
        let (spans, st) = highlight_line(lang, "c", "int x = 10; // TODO fix", LineState::Normal);
        assert_eq!(st, LineState::Normal);
        assert_eq!(spans[0], Span { start: 0, end: 3, group: "Type" });
        assert!(spans.iter().any(|s| s.group == "Number" && s.start == 8));
        assert!(spans.iter().any(|s| s.group == "Comment" && s.start == 12));
        assert!(spans.iter().any(|s| s.group == "Todo"));
        let (_, st) = highlight_line(lang, "c", "a /* open", LineState::Normal);
        assert_eq!(st, LineState::Block(0));
        let (spans, st) = highlight_line(lang, "c", "still */ int", st);
        assert_eq!(st, LineState::Normal);
        assert_eq!(spans[0].group, "Comment");
    }

    #[test]
    fn filetypes() {
        assert_eq!(detect_filetype(Path::new("a/b.rs"), ""), Some("rust"));
        assert_eq!(detect_filetype(Path::new("_vimrc"), ""), Some("vim"));
        assert_eq!(detect_filetype(Path::new("x"), "#!/usr/bin/env python"), Some("python"));
    }
}
