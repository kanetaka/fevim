//! Translation of (legacy) Vim script into Lua.
//!
//! The generated code uses fevim's `vim.*` Lua API where a direct
//! equivalent exists (`vim.opt`, `vim.keymap.set`, `vim.api.nvim_create_autocmd`,
//! `vim.g`, `vim.fn`, ...) and the helper runtime `vim._vs` (`V`) for Vim
//! script semantics (number/string coercion, 0-based lists, `=~`, ...).
//!
//! The same translator is used for
//! * sourcing a vimrc / Vim script file directly,
//! * `:ImportVimrc`, which writes the result to `init.lua` so it can be edited,
//! * Vim script typed on the command line, in mappings and autocommands,
//! * expressions (`%{}` in 'statusline', `<expr>` mappings, `<C-r>=`).

use crate::editor::ex;
use crate::editor::options;

// ------------------------------------------------------------ Lua helpers

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil", "not", "or",
    "repeat", "return", "then", "true", "until", "while",
];

pub fn is_lua_ident(s: &str) -> bool {
    let mut cs = s.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    cs.all(|c| c.is_ascii_alphanumeric() || c == '_') && !LUA_KEYWORDS.contains(&s)
}

/// `base.name` or `base['name']`.
pub fn field(base: &str, name: &str) -> String {
    if is_lua_ident(name) {
        format!("{}.{}", base, name)
    } else {
        format!("{}[{}]", base, lua_str(name))
    }
}

/// A Lua string literal for `s`, preferring readable forms.
pub fn lua_str(s: &str) -> String {
    let plain = !s.chars().any(|c| c == '\\' || c == '\'' || (c as u32) < 0x20 || c == '\x7f');
    if plain {
        return format!("'{}'", s);
    }
    let has_ctrl = s.chars().any(|c| ((c as u32) < 0x20 && c != '\t') || c == '\x7f' || (0xF000..0xF200).contains(&(c as u32)));
    if !has_ctrl && !s.starts_with('\n') {
        // long bracket string
        for level in 0..5 {
            let eq = "=".repeat(level);
            let close = format!("]{}]", eq);
            if !s.contains(&close) && !s.ends_with(&format!("]{}", eq)) {
                return format!("[{}[{}]{}]", eq, s, eq);
            }
        }
    }
    let mut out = String::from("'");
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\x7f' => out.push_str(&format!("\\{}", c as u32)),
            c if (0xF000..0xF200).contains(&(c as u32)) => out.push_str(&format!("\\u{{{:X}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

// ------------------------------------------------------------ expressions

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A Lua boolean (comparison, logical operator).
    Bool,
    /// A function call (valid as a Lua statement).
    Call,
    Other,
}

#[derive(Clone, Debug)]
struct E {
    code: String,
    kind: Kind,
}

impl E {
    fn new(code: impl Into<String>) -> E {
        E { code: code.into(), kind: Kind::Other }
    }
    fn b(code: impl Into<String>) -> E {
        E { code: code.into(), kind: Kind::Bool }
    }
    fn call(code: impl Into<String>) -> E {
        E { code: code.into(), kind: Kind::Call }
    }
    /// As a Lua condition.
    fn cond(&self) -> String {
        if self.kind == Kind::Bool { self.code.clone() } else { format!("V.truthy({})", self.code) }
    }
}

/// Function names that are part of Vim's built-in function set (used to
/// decide `vim.fn.x` vs a user-function call).
fn is_builtin_name(n: &str) -> bool {
    n.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false) && !n.contains('#') && !n.contains(':')
}

struct PCtx {
    in_func: usize,
    funcrefs: Vec<String>,
}

struct Parser {
    s: Vec<char>,
    i: usize,
    tr: PCtx,
    /// Lambda parameters in scope.
    lambda: Vec<Vec<String>>,
}

impl Parser {
    fn new(text: &str, tr: &Translator) -> Parser {
        let ctx = PCtx { in_func: tr.in_func, funcrefs: tr.func_locals.last().cloned().unwrap_or_default() };
        Parser { s: text.chars().collect(), i: 0, tr: ctx, lambda: Vec::new() }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn peek_at(&self, k: usize) -> Option<char> {
        self.s.get(self.i + k).copied()
    }
    fn starts(&self, t: &str) -> bool {
        let tc: Vec<char> = t.chars().collect();
        self.s.len() >= self.i + tc.len() && self.s[self.i..self.i + tc.len()] == tc[..]
    }
    fn ws(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.i += 1;
        }
    }
    fn rest(&self) -> String {
        self.s[self.i.min(self.s.len())..].iter().collect()
    }
    fn at_end(&mut self) -> bool {
        self.ws();
        self.i >= self.s.len()
    }
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("E15: Invalid expression: \"{}\" ({})", self.s.iter().collect::<String>(), msg))
    }

    // expr1: ternary
    fn expr(&mut self) -> Result<E, String> {
        let c = self.expr2()?;
        self.ws();
        if self.peek() == Some('?') {
            self.i += 1;
            let a = self.expr()?;
            self.ws();
            if self.peek() != Some(':') {
                return self.err("missing ':'");
            }
            self.i += 1;
            let b = self.expr()?;
            let lazy = a.code.contains('(') || b.code.contains('(');
            if lazy {
                return Ok(E::new(format!("V.ternl({}, function() return {} end, function() return {} end)", c.cond(), a.code, b.code)));
            }
            return Ok(E::new(format!("V.tern({}, {}, {})", c.cond(), a.code, b.code)));
        }
        Ok(c)
    }

    fn expr2(&mut self) -> Result<E, String> {
        let mut l = self.expr3()?;
        loop {
            self.ws();
            if self.starts("||") {
                self.i += 2;
                let r = self.expr3()?;
                l = E::b(format!("({} or {})", l.cond(), r.cond()));
            } else {
                return Ok(l);
            }
        }
    }

    fn expr3(&mut self) -> Result<E, String> {
        let mut l = self.expr4()?;
        loop {
            self.ws();
            if self.starts("&&") {
                self.i += 2;
                let r = self.expr4()?;
                l = E::b(format!("({} and {})", l.cond(), r.cond()));
            } else {
                return Ok(l);
            }
        }
    }

    fn expr4(&mut self) -> Result<E, String> {
        let l = self.expr5()?;
        self.ws();
        let ops: &[(&str, &str)] = &[
            ("==", "eq"),
            ("!=", "ne"),
            (">=", "ge"),
            ("<=", "le"),
            ("=~", "match"),
            ("!~", "nomatch"),
            (">", "gt"),
            ("<", "lt"),
        ];
        let mut found = None;
        for (op, f) in ops {
            if self.starts(op) {
                found = Some((op.len(), *f));
                break;
            }
        }
        if found.is_none() {
            for (op, f) in [("isnot", "isnot"), ("is", "is")] {
                if self.starts(op) {
                    let after = self.peek_at(op.len());
                    if !after.map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) || after == Some('#') || after == Some('?') {
                        found = Some((op.len(), f));
                        break;
                    }
                }
            }
        }
        let Some((n, f)) = found else {
            return Ok(l);
        };
        self.i += n;
        let ic = match self.peek() {
            Some('#') => {
                self.i += 1;
                ", false"
            }
            Some('?') => {
                self.i += 1;
                ", true"
            }
            _ => "",
        };
        let r = self.expr5()?;
        Ok(E::b(format!("V.{}({}, {}{})", f, l.code, r.code, ic)))
    }

    fn expr5(&mut self) -> Result<E, String> {
        let mut l = self.expr6()?;
        let mut concat: Vec<String> = Vec::new();
        loop {
            self.ws();
            let c = self.peek();
            if c == Some('+') || (c == Some('-') && self.peek_at(1) != Some('>')) {
                if !concat.is_empty() {
                    l = E::new(format!("V.concat({})", std::mem::take(&mut concat).join(", ")));
                }
                self.i += 1;
                let r = self.expr6()?;
                l = E::new(format!("V.{}({}, {})", if c == Some('+') { "add" } else { "sub" }, l.code, r.code));
            } else if c == Some('.') && self.peek_at(1) != Some('=') && !(self.peek_at(1) == Some('.') && self.peek_at(2) == Some('=')) {
                self.i += 1;
                if self.peek() == Some('.') {
                    self.i += 1;
                }
                let r = self.expr6()?;
                if concat.is_empty() {
                    concat.push(l.code.clone());
                }
                concat.push(r.code);
            } else {
                break;
            }
        }
        if !concat.is_empty() {
            l = E::new(format!("V.concat({})", concat.join(", ")));
        }
        Ok(l)
    }

    fn expr6(&mut self) -> Result<E, String> {
        let mut l = self.expr7()?;
        loop {
            self.ws();
            let f = match self.peek() {
                Some('*') => "mul",
                Some('/') => "div",
                Some('%') => "mod",
                _ => return Ok(l),
            };
            self.i += 1;
            let r = self.expr7()?;
            l = E::new(format!("V.{}({}, {})", f, l.code, r.code));
        }
    }

    fn expr7(&mut self) -> Result<E, String> {
        self.ws();
        match self.peek() {
            Some('!') => {
                self.i += 1;
                let e = self.expr7()?;
                Ok(E::b(format!("not {}", e.cond())))
            }
            Some('-') => {
                self.i += 1;
                let e = self.expr7()?;
                if e.code.chars().all(|c| c.is_ascii_digit() || c == '.') {
                    return Ok(E::new(format!("-{}", e.code)));
                }
                Ok(E::new(format!("V.neg({})", e.code)))
            }
            Some('+') => {
                self.i += 1;
                let e = self.expr7()?;
                Ok(E::new(format!("V.num({})", e.code)))
            }
            _ => self.expr8(),
        }
    }

    fn args(&mut self, close: char) -> Result<Vec<String>, String> {
        let mut out = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some(close) {
                self.i += 1;
                return Ok(out);
            }
            let e = self.expr()?;
            out.push(e.code);
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some(c) if c == close => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return self.err("expected , or closing bracket"),
            }
        }
    }

    fn expr8(&mut self) -> Result<E, String> {
        let (mut e, name_like) = self.expr9()?;
        let mut name_like = name_like;
        loop {
            match self.peek() {
                Some('[') => {
                    self.i += 1;
                    self.ws();
                    let a = if self.peek() == Some(':') { None } else { Some(self.expr()?) };
                    self.ws();
                    if self.peek() == Some(':') {
                        self.i += 1;
                        self.ws();
                        let b = if self.peek() == Some(']') { None } else { Some(self.expr()?) };
                        self.ws();
                        if self.peek() != Some(']') {
                            return self.err("missing ]");
                        }
                        self.i += 1;
                        e = E::new(format!(
                            "V.slice({}, {}, {})",
                            e.code,
                            a.map(|x| x.code).unwrap_or_else(|| "0".into()),
                            b.map(|x| x.code).unwrap_or_else(|| "nil".into())
                        ));
                    } else {
                        if self.peek() != Some(']') {
                            return self.err("missing ]");
                        }
                        self.i += 1;
                        e = E::new(format!("V.index({}, {})", e.code, a.unwrap().code));
                    }
                    name_like = true;
                }
                Some('(') if e.kind != Kind::Call || name_like => {
                    // calling a funcref value
                    self.i += 1;
                    let args = self.args(')')?;
                    let mut all = vec![e.code.clone()];
                    all.extend(args);
                    e = E::call(format!("V.call({})", all.join(", ")));
                    name_like = true;
                }
                Some('.') if name_like && self.peek_at(1).map(|c| c.is_ascii_alphabetic() || c == '_').unwrap_or(false) => {
                    self.i += 1;
                    let st = self.i;
                    while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                        self.i += 1;
                    }
                    let key: String = self.s[st..self.i].iter().collect();
                    let var = self.var_ref(&key);
                    e = E::new(format!("V.dot({}, {}, function() return {} end)", e.code, lua_str(&key), var));
                }
                Some('-') if self.peek_at(1) == Some('>') => {
                    self.i += 2;
                    self.ws();
                    if self.peek() == Some('{') {
                        // ->{lambda}(args)
                        let (lam, _) = self.expr9()?;
                        self.ws();
                        let args = if self.peek() == Some('(') {
                            self.i += 1;
                            self.args(')')?
                        } else {
                            Vec::new()
                        };
                        let mut all = vec![lam.code, e.code.clone()];
                        all.extend(args);
                        e = E::call(format!("V.call({})", all.join(", ")));
                        continue;
                    }
                    let st = self.i;
                    while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_' || c == '#' || c == ':').unwrap_or(false) {
                        self.i += 1;
                    }
                    let name: String = self.s[st..self.i].iter().collect();
                    if self.peek() != Some('(') {
                        return self.err("missing ( after method");
                    }
                    self.i += 1;
                    let mut args = vec![e.code.clone()];
                    args.extend(self.args(')')?);
                    e = E::call(self.call_code(&name, args));
                    name_like = true;
                }
                _ => return Ok(e),
            }
        }
    }

    fn ident(&mut self) -> String {
        let st = self.i;
        // scope prefix
        if let (Some(c), Some(':')) = (self.peek(), self.peek_at(1)) {
            if "gslabwtv".contains(c) && self.peek_at(2).map(|x| x.is_ascii_alphanumeric() || x == '_').unwrap_or(false) {
                self.i += 2;
            }
        }
        if self.starts("<SNR>") || self.starts("<SID>") {
            self.i += 5;
        }
        while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_' || c == '#').unwrap_or(false) {
            self.i += 1;
        }
        self.s[st..self.i].iter().collect()
    }

    fn var_ref(&self, name: &str) -> String {
        if let Some((scope, rest)) = name.split_once(':') {
            if scope.len() == 1 {
                return match scope {
                    "g" => field("vim.g", rest),
                    "s" => field("s", rest),
                    "l" => field("l", rest),
                    "a" => field("a", rest),
                    "b" => field("vim.b", rest),
                    "w" => field("vim.w", rest),
                    "t" => field("vim.t", rest),
                    "v" => match rest {
                        "true" => "true".into(),
                        "false" => "false".into(),
                        "null" | "none" => "nil".into(),
                        "t_number" => "0".into(),
                        "t_string" => "1".into(),
                        "t_func" => "2".into(),
                        "t_list" => "3".into(),
                        "t_dict" => "4".into(),
                        "t_float" => "5".into(),
                        "t_bool" => "6".into(),
                        _ => field("vim.v", rest),
                    },
                    _ => field("vim.g", name),
                };
            }
        }
        for params in self.lambda.iter().rev() {
            if params.iter().any(|p| p == name) {
                return lambda_param(name);
            }
        }
        if name == "count" || name == "version" {
            return field("vim.v", name);
        }
        if self.tr.in_func > 0 {
            field("l", name)
        } else {
            field("vim.g", name)
        }
    }

    fn call_code(&self, name: &str, args: Vec<String>) -> String {
        let a = args.join(", ");
        if let Some(r) = name.strip_prefix("s:").or_else(|| name.strip_prefix("<SID>")) {
            return format!("V.call(S .. {}{}{})", lua_str(r), if a.is_empty() { "" } else { ", " }, a);
        }
        if let Some((scope, rest)) = name.split_once(':') {
            if matches!(scope, "a" | "l" | "b" | "w" | "t") || (scope == "g" && !rest.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false)) {
                let var = self.var_ref(name);
                return format!("V.call({}{}{})", var, if a.is_empty() { "" } else { ", " }, a);
            }
            if scope == "g" {
                return format!("{}({})", field("vim.fn", rest), a);
            }
        }
        // lambda parameter holding a function
        for params in self.lambda.iter().rev() {
            if params.iter().any(|p| p == name) {
                return format!("V.call({}{}{})", lambda_param(name), if a.is_empty() { "" } else { ", " }, a);
            }
        }
        // a local variable holding a funcref (Vim requires capitals for these)
        if self.tr.in_func > 0 && !is_builtin_name(name) && !name.contains('#') && self.tr.funcrefs.iter().any(|x| x == name) {
            return format!("V.call({}, {})", field("l", name), a);
        }
        format!("{}({})", field("vim.fn", name), a)
    }

    /// Parse a double-quoted string body (after the opening quote).
    fn dq_string(&mut self) -> Result<String, String> {
        enum Seg {
            T(String),
            K(String),
        }
        let mut segs: Vec<Seg> = Vec::new();
        let mut cur = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("missing quote");
            };
            self.i += 1;
            match c {
                '"' => break,
                '\\' => {
                    let Some(n) = self.peek() else {
                        return self.err("missing quote");
                    };
                    self.i += 1;
                    match n {
                        'n' => cur.push('\n'),
                        't' => cur.push('\t'),
                        'r' => cur.push('\r'),
                        'e' => cur.push('\x1b'),
                        'b' => cur.push('\x08'),
                        'f' => cur.push('\x0c'),
                        '\\' => cur.push('\\'),
                        '"' => cur.push('"'),
                        'x' | 'X' | 'u' | 'U' => {
                            let max = match n {
                                'x' | 'X' => 2,
                                'u' => 4,
                                _ => 8,
                            };
                            let mut digits = String::new();
                            while digits.len() < max && self.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
                                digits.push(self.peek().unwrap());
                                self.i += 1;
                            }
                            match u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32) {
                                Some(ch) => cur.push(ch),
                                None => cur.push(n),
                            }
                        }
                        '0'..='7' => {
                            let mut v = n as u32 - '0' as u32;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d) if ('0'..='7').contains(&d) => {
                                        v = v * 8 + (d as u32 - '0' as u32);
                                        self.i += 1;
                                    }
                                    _ => break,
                                }
                            }
                            cur.push(char::from_u32(v).unwrap_or('?'));
                        }
                        '<' => {
                            // key notation \<C-W>
                            let rest = self.rest();
                            match rest.find('>') {
                                Some(end) if end > 0 && !rest[..end].contains(char::is_whitespace) => {
                                    let notation = format!("<{}>", &rest[..end]);
                                    if !cur.is_empty() {
                                        segs.push(Seg::T(std::mem::take(&mut cur)));
                                    }
                                    segs.push(Seg::K(notation));
                                    self.i += rest[..end].chars().count() + 1;
                                }
                                _ => cur.push('<'),
                            }
                        }
                        other => cur.push(other),
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            segs.push(Seg::T(cur));
        }
        if !segs.iter().any(|s| matches!(s, Seg::K(_))) {
            let t: String = segs.into_iter().map(|s| if let Seg::T(t) = s { t } else { String::new() }).collect();
            return Ok(lua_str(&t));
        }
        let mut notation = String::new();
        for s in segs {
            match s {
                Seg::T(t) => notation.push_str(&t.replace('<', "<lt>")),
                Seg::K(k) => notation.push_str(&k),
            }
        }
        Ok(format!("vim.keycode({})", lua_str(&notation)))
    }

    /// Atoms.  Returns the expression and whether it is "name like"
    /// (allows `.key` member access).
    fn expr9(&mut self) -> Result<(E, bool), String> {
        self.ws();
        let Some(c) = self.peek() else {
            return self.err("unexpected end");
        };
        match c {
            '0'..='9' => Ok((E::new(self.number()?), false)),
            '\'' => {
                self.i += 1;
                let mut s = String::new();
                loop {
                    match self.peek() {
                        None => return self.err("missing quote"),
                        Some('\'') => {
                            self.i += 1;
                            if self.peek() == Some('\'') {
                                s.push('\'');
                                self.i += 1;
                            } else {
                                break;
                            }
                        }
                        Some(ch) => {
                            s.push(ch);
                            self.i += 1;
                        }
                    }
                }
                Ok((E::new(lua_str(&s)), false))
            }
            '"' => {
                self.i += 1;
                Ok((E::new(self.dq_string()?), false))
            }
            '[' => {
                self.i += 1;
                let items = self.args(']')?;
                Ok((E::new(format!("V.list({{{}}})", items.join(", "))), false))
            }
            '#' if self.peek_at(1) == Some('{') => {
                self.i += 2;
                Ok((self.dict(true)?, false))
            }
            '{' => {
                // lambda or dict
                if let Some(params) = self.lambda_params() {
                    self.lambda.push(params.clone());
                    let body = self.expr();
                    self.lambda.pop();
                    let body = body?;
                    self.ws();
                    if self.peek() != Some('}') {
                        return self.err("missing } of lambda");
                    }
                    self.i += 1;
                    let ps: Vec<String> = params.iter().map(|p| lambda_param(p)).collect();
                    return Ok((E::new(format!("function({}) return {} end", ps.join(", "), body.code)), false));
                }
                self.i += 1;
                Ok((self.dict(false)?, false))
            }
            '(' => {
                self.i += 1;
                let e = self.expr()?;
                self.ws();
                if self.peek() != Some(')') {
                    return self.err("missing )");
                }
                self.i += 1;
                Ok((E { code: format!("({})", e.code), kind: if e.kind == Kind::Bool { Kind::Bool } else { Kind::Other } }, true))
            }
            '&' => {
                self.i += 1;
                let mut scope = "";
                if matches!(self.peek(), Some('l') | Some('g')) && self.peek_at(1) == Some(':') {
                    scope = if self.peek() == Some('l') { "l" } else { "g" };
                    self.i += 2;
                }
                let st = self.i;
                while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                    self.i += 1;
                }
                let name: String = self.s[st..self.i].iter().collect();
                if name.starts_with("t_") {
                    return Ok((E::new("''"), false));
                }
                let full = options::find_def(&name).map(|d| d.name).unwrap_or(&name);
                Ok((
                    E::new(match scope {
                        "l" => format!("V.getopt({}, 'l')", lua_str(full)),
                        "g" => format!("V.getopt({}, 'g')", lua_str(full)),
                        _ => field("vim.o", full),
                    }),
                    false,
                ))
            }
            '@' => {
                self.i += 1;
                let r = self.peek().unwrap_or('"');
                self.i += 1;
                let r = if r == '@' { '"' } else { r };
                Ok((E::call(format!("vim.fn.getreg({})", lua_str(&r.to_string()))), false))
            }
            '$' => {
                self.i += 1;
                let st = self.i;
                while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                    self.i += 1;
                }
                let name: String = self.s[st..self.i].iter().collect();
                Ok((E::new(field("vim.env", &name)), false))
            }
            c if c.is_ascii_alphabetic() || c == '_' || c == '<' => {
                let name = self.ident();
                if name.is_empty() {
                    return self.err("unexpected character");
                }
                if self.peek() == Some('(') {
                    self.i += 1;
                    let args = self.args(')')?;
                    // function('Name') / funcref('Name') keep the script context
                    if (name == "function" || name == "funcref") && !args.is_empty() {
                        return Ok((E::call(format!("V.funcref(S, {})", args.join(", "))), true));
                    }
                    return Ok((E::call(self.call_code(&name, args)), true));
                }
                Ok((E::new(self.var_ref(&name)), true))
            }
            _ => self.err("unexpected character"),
        }
    }

    fn lambda_params(&mut self) -> Option<Vec<String>> {
        // `{` [names] `->`
        let mut j = self.i + 1;
        let mut params = Vec::new();
        let mut cur = String::new();
        while j < self.s.len() {
            let c = self.s[j];
            if c == '-' && self.s.get(j + 1) == Some(&'>') {
                if !cur.is_empty() {
                    params.push(std::mem::take(&mut cur));
                }
                self.i = j + 2;
                return Some(params);
            }
            if c.is_ascii_alphanumeric() || c == '_' {
                cur.push(c);
            } else if c == ',' {
                if cur.is_empty() {
                    return None;
                }
                params.push(std::mem::take(&mut cur));
            } else if c == '.' && self.s.get(j + 1) == Some(&'.') && self.s.get(j + 2) == Some(&'.') {
                params.push("...".into());
                j += 2;
            } else if c != ' ' && c != '\t' {
                return None;
            }
            j += 1;
        }
        None
    }

    fn dict(&mut self, literal_keys: bool) -> Result<E, String> {
        let mut items = Vec::new();
        loop {
            self.ws();
            if self.peek() == Some('}') {
                self.i += 1;
                break;
            }
            let key = if literal_keys {
                let st = self.i;
                while self.peek().map(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-').unwrap_or(false) {
                    self.i += 1;
                }
                let k: String = self.s[st..self.i].iter().collect();
                lua_str(&k)
            } else {
                self.expr()?.code
            };
            self.ws();
            if self.peek() != Some(':') {
                return self.err("missing : in dict");
            }
            self.i += 1;
            let v = self.expr()?;
            // 'key' literal -> key = v
            let k = if key.starts_with('\'') && key.ends_with('\'') && is_lua_ident(&key[1..key.len() - 1]) {
                key[1..key.len() - 1].to_string()
            } else if key.starts_with('\'') || key.starts_with('[') {
                format!("[{}]", key)
            } else {
                format!("[V.str({})]", key)
            };
            items.push(format!("{} = {}", k, v.code));
            self.ws();
            match self.peek() {
                Some(',') => self.i += 1,
                Some('}') => {
                    self.i += 1;
                    break;
                }
                _ => return self.err("missing , in dict"),
            }
        }
        Ok(E::new(format!("V.dict({{ {} }})", items.join(", "))))
    }

    fn number(&mut self) -> Result<String, String> {
        let st = self.i;
        if self.peek() == Some('0') && matches!(self.peek_at(1), Some('x') | Some('X')) {
            self.i += 2;
            while self.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
                self.i += 1;
            }
            let t: String = self.s[st..self.i].iter().collect();
            return Ok(t);
        }
        if self.peek() == Some('0') && matches!(self.peek_at(1), Some('b') | Some('B')) {
            self.i += 2;
            let s2 = self.i;
            while matches!(self.peek(), Some('0') | Some('1')) {
                self.i += 1;
            }
            let t: String = self.s[s2..self.i].iter().collect();
            return Ok(i64::from_str_radix(&t, 2).unwrap_or(0).to_string());
        }
        if self.peek() == Some('0') && matches!(self.peek_at(1), Some('o') | Some('O')) {
            self.i += 2;
            let s2 = self.i;
            while self.peek().map(|c| ('0'..='7').contains(&c)).unwrap_or(false) {
                self.i += 1;
            }
            let t: String = self.s[s2..self.i].iter().collect();
            return Ok(i64::from_str_radix(&t, 8).unwrap_or(0).to_string());
        }
        while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.i += 1;
        }
        // float?
        if self.peek() == Some('.') && self.peek_at(1).map(|c| c.is_ascii_digit()).unwrap_or(false) {
            self.i += 1;
            while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                self.i += 1;
            }
            if matches!(self.peek(), Some('e') | Some('E')) {
                let save = self.i;
                self.i += 1;
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.i += 1;
                }
                if self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    while self.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                        self.i += 1;
                    }
                } else {
                    self.i = save;
                }
            }
            return Ok(self.s[st..self.i].iter().collect());
        }
        let t: String = self.s[st..self.i].iter().collect();
        // legacy octal: 017 == 15
        if t.len() > 1 && t.starts_with('0') && t.chars().all(|c| ('0'..='7').contains(&c)) {
            return Ok(i64::from_str_radix(&t, 8).unwrap_or(0).to_string());
        }
        Ok(t.trim_start_matches('0').to_string()).map(|x| if x.is_empty() { "0".into() } else { x })
    }

    /// After an expression: `|` continues with another command, `"` starts a comment.
    fn tail(&mut self) -> Result<Option<String>, String> {
        self.ws();
        match self.peek() {
            None => Ok(None),
            Some('|') => {
                self.i += 1;
                Ok(Some(self.rest()))
            }
            Some('"') => Ok(None),
            _ => self.err("trailing characters"),
        }
    }
}

fn lambda_param(p: &str) -> String {
    if p == "..." {
        return "...".into();
    }
    if is_lua_ident(p) { p.to_string() } else { format!("p_{}", p) }
}

// ------------------------------------------------------------- statements

#[derive(Debug)]
enum Block {
    If { wrapped: bool },
    Loop { id: usize, cont: bool, wrapped: bool },
    Func { close: &'static str },
    Try { wrapped: bool },
}

pub struct Translator {
    out: Vec<String>,
    indent: usize,
    blocks: Vec<Block>,
    in_func: usize,
    augroup: Option<String>,
    /// Wrap top-level statements so an error does not stop the whole script.
    wrap: bool,
    pub lnum: usize,
    next_id: usize,
    /// Local variables of the current function that hold funcrefs (capitalised `let`).
    func_locals: Vec<Vec<String>>,
}

impl Translator {
    pub fn new(wrap: bool) -> Translator {
        Translator {
            out: Vec::new(),
            indent: 0,
            blocks: Vec::new(),
            in_func: 0,
            augroup: None,
            wrap,
            lnum: 0,
            next_id: 0,
            func_locals: Vec::new(),
        }
    }

    fn emit(&mut self, line: impl AsRef<str>) {
        for l in line.as_ref().split('\n') {
            if l.is_empty() {
                self.out.push(String::new());
            } else {
                self.out.push(format!("{}{}", "  ".repeat(self.indent), l));
            }
        }
    }

    fn at_top(&self) -> bool {
        self.blocks.is_empty()
    }

    fn in_try_in_func(&self) -> bool {
        for b in self.blocks.iter().rev() {
            match b {
                Block::Func { .. } => return false,
                Block::Try { .. } => return true,
                _ => {}
            }
        }
        false
    }

    fn expr(&self, text: &str) -> Result<(E, Option<String>), String> {
        let mut p = Parser::new(text, self);
        let e = p.expr()?;
        let rest = p.tail()?;
        Ok((e, rest))
    }

    /// Emit a simple statement, wrapping it at top level when it may fail.
    fn simple(&mut self, code: String, risky: bool) {
        if risky && self.wrap && self.at_top() {
            let ln = self.lnum;
            if code.contains('\n') {
                self.emit(format!("at({}, function()", ln));
                self.indent += 1;
                self.emit(code);
                self.indent -= 1;
                self.emit("end)");
            } else {
                self.emit(format!("at({}, function() {} end)", ln, code));
            }
        } else {
            self.emit(code);
        }
    }

    fn open_block_wrap(&mut self) -> bool {
        if self.wrap && self.at_top() {
            let ln = self.lnum;
            self.emit(format!("at({}, function()", ln));
            self.indent += 1;
            true
        } else {
            false
        }
    }

    fn close_block_wrap(&mut self, wrapped: bool) {
        if wrapped {
            self.indent -= 1;
            self.emit("end)");
        }
    }

    /// Translate a whole script.
    pub fn script(&mut self, src: &str) -> Result<(), String> {
        let lines: Vec<&str> = src.split('\n').collect();
        let mut i = 0;
        while i < lines.len() {
            let mut line = lines[i].trim_end_matches('\r').to_string();
            let start = i;
            // line continuation
            while i + 1 < lines.len() {
                let next = lines[i + 1].trim_end_matches('\r');
                let t = next.trim_start();
                if let Some(rest) = t.strip_prefix('\\') {
                    line.push_str(rest);
                    i += 1;
                } else if t.starts_with("\"\\ ") {
                    i += 1;
                } else {
                    break;
                }
            }
            i += 1;
            self.lnum = start + 1;
            self.line(&line);
        }
        if !self.blocks.is_empty() {
            // close unterminated blocks so the Lua is valid
            while let Some(b) = self.blocks.pop() {
                self.indent = self.indent.saturating_sub(1);
                match b {
                    Block::Func { close } => {
                        self.in_func -= 1;
                        self.emit(close);
                    }
                    Block::Try { wrapped } => {
                        self.emit("end)");
                        self.close_block_wrap(wrapped);
                    }
                    Block::If { wrapped } | Block::Loop { wrapped, .. } => {
                        self.emit("end");
                        self.close_block_wrap(wrapped);
                    }
                }
            }
            self.emit("-- (fevim) missing :endif/:endfor/:endfunction added");
        }
        Ok(())
    }

    /// Translate one logical line.
    pub fn line(&mut self, line: &str) {
        let t = line.trim_start_matches([' ', '\t', ':']);
        if t.is_empty() {
            if !self.out.last().map(|l| l.is_empty()).unwrap_or(true) {
                self.out.push(String::new());
            }
            return;
        }
        if let Some(c) = t.strip_prefix('"') {
            self.emit(format!("--{}", c));
            return;
        }
        if t.starts_with("#!") {
            return;
        }
        let mut text = t.to_string();
        let mut guard = 0;
        loop {
            guard += 1;
            if guard > 100 {
                break;
            }
            match self.stmt(&text) {
                Ok(Some(rest)) => {
                    let r = rest.trim_start_matches([' ', '\t', ':']).to_string();
                    if r.is_empty() {
                        break;
                    }
                    text = r;
                }
                Ok(None) => break,
                Err(e) => {
                    // keep going: report the problem when the line runs
                    let ln = self.lnum;
                    let msg = format!("V.error({}, {})", ln, lua_str(&e));
                    self.simple(msg, false);
                    self.emit(format!("-- {}", text));
                    break;
                }
            }
        }
    }

    /// Translate one statement; returns the text after a `|`.
    fn stmt(&mut self, text: &str) -> Result<Option<String>, String> {
        let t = text.trim_start_matches([' ', '\t', ':']);
        if t.starts_with('"') {
            self.emit(format!("--{}", &t[1..]));
            return Ok(None);
        }
        if ex::is_script_line(t) {
            return self.script_stmt(t);
        }
        self.ex_stmt(t)
    }

    // ---------------------------------------------------------- Ex commands

    fn ex_stmt(&mut self, t: &str) -> Result<Option<String>, String> {
        let Some(p) = ex::parse_command(t) else {
            return Ok(None);
        };
        let raw = match &p.rest {
            Some(r) => t[..t.len() - r.len() - 1].trim_end().to_string(),
            None => t.trim_end().to_string(),
        };
        let rest = p.rest.clone();
        if p.name.is_empty() && p.range.is_empty() {
            return Ok(rest);
        }
        let code = self.ex_idiom(&p, &raw);
        match code {
            Some((c, risky)) => self.simple(c, risky),
            None => {
                let raw = self.sid_fix(&raw);
                self.simple(format!("vim.cmd({})", raw), true);
            }
        }
        Ok(rest)
    }

    /// Replace `<SID>` and `s:func(` so that commands run later find
    /// script-local functions.  Returns Lua code for the string.
    fn sid_fix(&self, raw: &str) -> String {
        let needs = raw.contains("<SID>") || raw.contains("s:");
        if !needs {
            return lua_str(raw);
        }
        format!("V.sid(S, {})", lua_str(raw))
    }

    /// Idiomatic Lua for common commands.  Returns (code, may_fail).
    fn ex_idiom(&mut self, p: &ex::ParsedCmd, raw: &str) -> Option<(String, bool)> {
        if !p.range.is_empty() {
            return None;
        }
        let full = p.full?;
        let arg = p.arg.trim();
        match full {
            "set" | "setlocal" | "setglobal" if !arg.is_empty() => {
                let base = match full {
                    "set" => "vim.opt",
                    "setlocal" => "vim.opt_local",
                    _ => "vim.opt_global",
                };
                let mut out = Vec::new();
                for a in split_set_args(arg) {
                    if a.starts_with('"') {
                        break;
                    }
                    out.push(set_arg_to_lua(base, &a)?);
                }
                Some((out.join("\n"), false))
            }
            "colorscheme" if !arg.is_empty() => Some((format!("vim.cmd.colorscheme({})", lua_str(arg)), true)),
            "augroup" => {
                if arg.eq_ignore_ascii_case("end") {
                    self.augroup = None;
                    Some(("-- augroup END".to_string(), false))
                } else if p.bang {
                    Some((format!("vim.api.nvim_del_augroup_by_name({})", lua_str(arg)), true))
                } else {
                    self.augroup = Some(arg.to_string());
                    Some((format!("vim.api.nvim_create_augroup({}, {{ clear = false }})", lua_str(arg)), false))
                }
            }
            "autocmd" => self.autocmd_idiom(arg, p.bang, raw),
            "syntax" if arg == "on" || arg == "enable" => Some(("vim.cmd.syntax('on')".into(), false)),
            "filetype" if !arg.is_empty() => Some((format!("vim.cmd.filetype({})", lua_str(arg)), false)),
            "nohlsearch" => Some(("vim.cmd.nohlsearch()".into(), false)),
            _ if ex::map_command(full) == Some(full) => self.map_idiom(full, p.bang, &p.arg),
            _ => None,
        }
    }

    fn map_idiom(&mut self, cmd: &str, bang: bool, arg: &str) -> Option<(String, bool)> {
        let (prefix, kind) = if let Some(b) = cmd.strip_suffix("noremap") {
            (b, "noremap")
        } else if let Some(b) = cmd.strip_suffix("unmap") {
            (b, "unmap")
        } else if cmd.ends_with("mapclear") {
            return None;
        } else {
            (cmd.strip_suffix("map")?, "map")
        };
        let modes: Vec<&str> = match (prefix, bang) {
            ("", true) => vec!["i", "c"],
            ("", false) => vec!["n", "v", "o"],
            ("l", _) => vec!["i", "c"],
            (m, _) => vec![m],
        };
        let mut rest = arg.trim_start();
        let mut opts: Vec<String> = Vec::new();
        loop {
            let lower = rest.to_ascii_lowercase();
            let mut hit = false;
            for (tag, opt) in [
                ("<silent>", "silent = true"),
                ("<expr>", "expr = true"),
                ("<buffer>", "buffer = true"),
                ("<nowait>", "nowait = true"),
                ("<unique>", ""),
                ("<script>", ""),
                ("<special>", ""),
            ] {
                if lower.starts_with(tag) {
                    if !opt.is_empty() {
                        opts.push(opt.to_string());
                    }
                    rest = rest[tag.len()..].trim_start();
                    hit = true;
                    break;
                }
            }
            if !hit {
                break;
            }
        }
        let (lhs, rhs) = match rest.find([' ', '\t']) {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None => (rest, ""),
        };
        if lhs.is_empty() {
            return None;
        }
        let modes_lua = if modes.len() == 1 { lua_str(modes[0]) } else { format!("{{ {} }}", modes.iter().map(|m| lua_str(m)).collect::<Vec<_>>().join(", ")) };
        if kind == "unmap" {
            let o = if opts.iter().any(|o| o.starts_with("buffer")) { ", { buffer = true }" } else { "" };
            return Some((format!("vim.keymap.del({}, {}{})", modes_lua, lua_str(lhs), o), true));
        }
        if rhs.is_empty() {
            return None; // listing
        }
        if kind == "map" {
            opts.push("remap = true".into());
        }
        let rhs_code = if rhs.contains("<SID>") || rhs.contains("s:") { format!("V.sid(S, {})", lua_str(rhs)) } else { lua_str(rhs) };
        let o = if opts.is_empty() { String::new() } else { format!(", {{ {} }}", opts.join(", ")) };
        Some((format!("vim.keymap.set({}, {}, {}{})", modes_lua, lua_str(lhs), rhs_code, o), false))
    }

    fn autocmd_idiom(&mut self, arg: &str, bang: bool, _raw: &str) -> Option<(String, bool)> {
        let mut rest = arg.trim_start();
        let mut group = self.augroup.clone();
        let first = rest.split_whitespace().next().unwrap_or("");
        let is_event = |s: &str| s == "*" || s.split(',').all(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphabetic()) && known_event(e));
        if !first.is_empty() && !is_event(first) {
            group = Some(first.to_string());
            rest = rest[first.len()..].trim_start();
        }
        let gopt = group.as_ref().map(|g| format!("group = {}", lua_str(g)));
        if rest.is_empty() {
            if bang {
                return Some((
                    match &gopt {
                        Some(g) => format!("vim.api.nvim_clear_autocmds({{ {} }})", g),
                        None => "vim.cmd('autocmd!')".into(),
                    },
                    false,
                ));
            }
            return None;
        }
        let (events, r) = match rest.find(char::is_whitespace) {
            Some(i) => (&rest[..i], rest[i..].trim_start()),
            None => (rest, ""),
        };
        if !is_event(events) || events == "*" {
            return None;
        }
        let (pattern, cmd) = match r.find(char::is_whitespace) {
            Some(i) => (&r[..i], r[i..].trim_start()),
            None => (r, ""),
        };
        let ev: Vec<String> = events.split(',').map(|e| lua_str(&canonical_event(e))).collect();
        let ev_code = if ev.len() == 1 { ev[0].clone() } else { format!("{{ {} }}", ev.join(", ")) };
        let mut clear = String::new();
        if bang {
            let mut fields = vec![format!("event = {}", ev_code)];
            if !pattern.is_empty() {
                fields.push(format!("pattern = {}", lua_str(pattern)));
            }
            if let Some(g) = &gopt {
                fields.push(g.clone());
            }
            clear = format!("vim.api.nvim_clear_autocmds({{ {} }})\n", fields.join(", "));
        }
        if pattern.is_empty() || cmd.is_empty() {
            if bang {
                return Some((clear.trim_end().to_string(), false));
            }
            return None;
        }
        let mut cmd = cmd;
        let mut extra = Vec::new();
        loop {
            if let Some(x) = cmd.strip_prefix("++once") {
                extra.push("once = true".to_string());
                cmd = x.trim_start();
            } else if let Some(x) = cmd.strip_prefix("++nested") {
                extra.push("nested = true".to_string());
                cmd = x.trim_start();
            } else if let Some(x) = cmd.strip_prefix("nested ") {
                extra.push("nested = true".to_string());
                cmd = x.trim_start();
            } else {
                break;
            }
        }
        let mut fields = Vec::new();
        if let Some(g) = gopt {
            fields.push(g);
        }
        if pattern.eq_ignore_ascii_case("<buffer>") {
            fields.push("buffer = 0".into());
        } else {
            fields.push(format!("pattern = {}", lua_str(pattern)));
        }
        fields.extend(extra);
        let cmd_code = if cmd.contains("<SID>") || cmd.contains("s:") { format!("V.sid(S, {})", lua_str(cmd)) } else { lua_str(cmd) };
        fields.push(format!("command = {}", cmd_code));
        Some((format!("{}vim.api.nvim_create_autocmd({}, {{ {} }})", clear, ev_code, fields.join(", ")), false))
    }

    // ------------------------------------------------------ script commands

    fn script_stmt(&mut self, t: &str) -> Result<Option<String>, String> {
        // modifiers before a script command
        let mut s = t;
        let mut silent: Option<bool> = None;
        loop {
            let n = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
            let word = &s[..n];
            if ex::script_keyword(word).is_some() {
                break;
            }
            match ex::full_command_name(word) {
                Some(full) => {
                    let mut r = &s[n..];
                    let bang = r.starts_with('!');
                    if bang {
                        r = &r[1..];
                    }
                    if full == "silent" {
                        silent = Some(bang);
                    }
                    s = r.trim_start_matches([' ', '\t', ':']);
                }
                None => break,
            }
        }
        if let Some(bang) = silent {
            // translate the inner statement into a closure
            let saved = std::mem::take(&mut self.out);
            let saved_indent = self.indent;
            self.indent = 0;
            // pretend not at top so the inner statement is not wrapped
            self.blocks.push(Block::If { wrapped: false });
            let r = self.script_cmd(s);
            self.blocks.pop();
            let inner = std::mem::replace(&mut self.out, saved);
            self.indent = saved_indent;
            let rest = r?;
            let body = inner.join("\n");
            let code = if body.contains('\n') {
                format!("V.silent({}, function()\n{}\nend)", bang, indent_text(&body))
            } else {
                format!("V.silent({}, function() {} end)", bang, body.trim())
            };
            self.simple(code, true);
            return Ok(rest);
        }
        self.script_cmd(s)
    }

    fn script_cmd(&mut self, s: &str) -> Result<Option<String>, String> {
        let n = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
        let word = &s[..n];
        let kw = ex::script_keyword(word).ok_or_else(|| format!("E492: Not an editor command: {}", s))?;
        let mut args = &s[n..];
        let bang = args.starts_with('!');
        if bang {
            args = &args[1..];
        }
        let args = args.trim_start();
        match kw {
            "let" | "const" => self.let_stmt(args),
            "unlet" | "lockvar" | "unlockvar" => {
                if kw != "unlet" {
                    return Ok(None);
                }
                let mut p = Parser::new(args, self);
                let mut codes = Vec::new();
                loop {
                    p.ws();
                    if p.peek().is_none() || p.peek() == Some('|') || p.peek() == Some('"') {
                        break;
                    }
                    let (lhs, _) = self.lvalue(&mut p)?;
                    codes.push(match lhs {
                        LValue::Var(v) => format!("{} = nil", v),
                        LValue::Index(b, i) => format!("V.remove({}, {})", b, i),
                        LValue::Env(name) => format!("{} = nil", field("vim.env", &name)),
                        _ => continue,
                    });
                }
                let rest = p.tail()?;
                self.simple(codes.join("\n"), false);
                Ok(rest)
            }
            "if" => {
                let (e, rest) = self.expr(args)?;
                let wrapped = self.open_block_wrap();
                self.emit(format!("if {} then", e.cond()));
                self.indent += 1;
                self.blocks.push(Block::If { wrapped });
                Ok(rest)
            }
            "elseif" => {
                let (e, rest) = self.expr(args)?;
                if !matches!(self.blocks.last(), Some(Block::If { .. })) {
                    return Err("E582: :elseif without :if".into());
                }
                self.indent -= 1;
                self.emit(format!("elseif {} then", e.cond()));
                self.indent += 1;
                Ok(rest)
            }
            "else" => {
                if !matches!(self.blocks.last(), Some(Block::If { .. })) {
                    return Err("E581: :else without :if".into());
                }
                self.indent -= 1;
                self.emit("else");
                self.indent += 1;
                Ok(after_bar(args))
            }
            "endif" => {
                let Some(Block::If { wrapped }) = self.blocks.pop() else {
                    return Err("E580: :endif without :if".into());
                };
                self.indent -= 1;
                self.emit("end");
                self.close_block_wrap(wrapped);
                Ok(after_bar(args))
            }
            "while" => {
                let (e, rest) = self.expr(args)?;
                let wrapped = self.open_block_wrap();
                self.next_id += 1;
                let id = self.next_id;
                self.emit(format!("while {} do", e.cond()));
                self.indent += 1;
                self.blocks.push(Block::Loop { id, cont: false, wrapped });
                Ok(rest)
            }
            "for" => {
                // for {var} in {list} / for [a, b] in {list}
                let in_pos = find_word(args, "in").ok_or("E690: Missing \"in\" after :for")?;
                let var = args[..in_pos].trim();
                let (e, rest) = self.expr(&args[in_pos + 2..])?;
                let wrapped = self.open_block_wrap();
                self.next_id += 1;
                let id = self.next_id;
                let tmp = format!("_v{}", id);
                self.emit(format!("for _, {} in V.iter({}) do", tmp, e.code));
                self.indent += 1;
                if var.starts_with('[') {
                    let names: Vec<&str> = var.trim_matches(|c| c == '[' || c == ']').split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
                    let p = Parser::new("", self);
                    for (i, n) in names.iter().enumerate() {
                        let v = p.var_ref(n);
                        self.emit(format!("{} = V.index({}, {})", v, tmp, i));
                    }
                } else {
                    let p = Parser::new("", self);
                    let v = p.var_ref(var);
                    self.emit(format!("{} = {}", v, tmp));
                }
                self.blocks.push(Block::Loop { id, cont: false, wrapped });
                Ok(rest)
            }
            "endwhile" | "endfor" => {
                let Some(Block::Loop { id, cont, wrapped }) = self.blocks.pop() else {
                    return Err(format!("E588: :{} without loop", kw));
                };
                if cont {
                    self.emit(format!("::continue{}::", id));
                }
                self.indent -= 1;
                self.emit("end");
                self.close_block_wrap(wrapped);
                Ok(after_bar(args))
            }
            "break" => {
                if self.in_try_in_loop() {
                    self.emit("error('E??? :break inside :try is not supported by fevim')");
                } else {
                    self.emit("break");
                }
                Ok(after_bar(args))
            }
            "continue" => {
                let mut found = None;
                for b in self.blocks.iter_mut().rev() {
                    if let Block::Loop { id, cont, .. } = b {
                        *cont = true;
                        found = Some(*id);
                        break;
                    }
                }
                match found {
                    Some(id) => self.emit(format!("goto continue{}", id)),
                    None => return Err("E586: :continue without :while or :for".into()),
                }
                Ok(after_bar(args))
            }
            "function" => self.function_stmt(args, bang),
            "endfunction" => {
                let Some(Block::Func { close }) = self.blocks.pop() else {
                    return Err("E193: :endfunction not inside a function".into());
                };
                self.in_func -= 1;
                self.func_locals.pop();
                self.indent -= 1;
                self.emit(close);
                Ok(None)
            }
            "return" => {
                let a = args.trim();
                let (code, rest) = if a.is_empty() || a.starts_with('|') {
                    ("0".to_string(), after_bar(a))
                } else {
                    let (e, rest) = self.expr(a)?;
                    (e.code, rest)
                };
                if self.in_func == 0 {
                    return Err("E133: :return not inside a function".into());
                }
                if self.in_try_in_func() {
                    self.emit(format!("do return V.ret({}) end", code));
                } else {
                    self.emit(format!("do return {} end", code));
                }
                Ok(rest)
            }
            "call" | "eval" => {
                let (e, rest) = self.expr(args)?;
                let code = if e.kind == Kind::Call { e.code } else { format!("V.discard({})", e.code) };
                self.simple(code, true);
                Ok(rest)
            }
            "execute" => {
                let (exprs, rest) = self.expr_list(args)?;
                let code = if exprs.len() == 1 { format!("vim.cmd({})", exprs[0]) } else { format!("V.execute({})", exprs.join(", ")) };
                self.simple(code, true);
                Ok(rest)
            }
            "echo" | "echon" | "echomsg" | "echoerr" | "echoconsole" => {
                let (exprs, rest) = self.expr_list(args)?;
                let f = match kw {
                    "echon" => "echon",
                    "echomsg" | "echoconsole" => "echomsg",
                    "echoerr" => "echoerr",
                    _ => "echo",
                };
                self.simple(format!("V.{}({})", f, exprs.join(", ")), true);
                Ok(rest)
            }
            "try" => {
                let wrapped = self.open_block_wrap();
                if self.in_func > 0 {
                    self.emit("do local _r = V.try(function()");
                } else {
                    self.emit("V.try(function()");
                }
                self.indent += 1;
                self.blocks.push(Block::Try { wrapped });
                Ok(after_bar(args))
            }
            "catch" => {
                if !matches!(self.blocks.last(), Some(Block::Try { .. })) {
                    return Err("E603: :catch without :try".into());
                }
                let a = args.trim();
                let pat = if a.is_empty() {
                    ".*".to_string()
                } else {
                    let d = a.chars().next().unwrap();
                    let body = &a[d.len_utf8()..];
                    body[..body.find(d).unwrap_or(body.len())].to_string()
                };
                self.indent -= 1;
                self.emit(format!("end, {}, function()", lua_str(&pat)));
                self.indent += 1;
                Ok(None)
            }
            "finally" => {
                if !matches!(self.blocks.last(), Some(Block::Try { .. })) {
                    return Err("E606: :finally without :try".into());
                }
                self.indent -= 1;
                self.emit("end, V.FINALLY, function()");
                self.indent += 1;
                Ok(None)
            }
            "endtry" => {
                let Some(Block::Try { wrapped }) = self.blocks.pop() else {
                    return Err("E602: :endtry without :try".into());
                };
                self.indent -= 1;
                if self.in_func > 0 {
                    self.emit("end) if _r ~= nil then return _r[1] end end");
                } else {
                    self.emit("end)");
                }
                self.close_block_wrap(wrapped);
                Ok(after_bar(args))
            }
            "throw" => {
                let (e, rest) = self.expr(args)?;
                self.emit(format!("V.throw({})", e.code));
                Ok(rest)
            }
            "finish" => {
                if self.at_top() {
                    self.emit("do return end");
                } else {
                    self.emit("V.finish()");
                }
                Ok(None)
            }
            _ => Err(format!("E492: Not an editor command: {}", s)),
        }
    }

    fn in_try_in_loop(&self) -> bool {
        for b in self.blocks.iter().rev() {
            match b {
                Block::Loop { .. } | Block::Func { .. } => return false,
                Block::Try { .. } => return true,
                _ => {}
            }
        }
        false
    }

    /// Whitespace separated expressions (`:echo`, `:execute`).
    fn expr_list(&self, args: &str) -> Result<(Vec<String>, Option<String>), String> {
        let mut p = Parser::new(args, self);
        let mut out = Vec::new();
        loop {
            p.ws();
            match p.peek() {
                None => return Ok((out, None)),
                Some('|') => {
                    p.i += 1;
                    return Ok((out, Some(p.rest())));
                }
                _ => {}
            }
            let e = p.expr()?;
            out.push(e.code);
        }
    }

    fn function_stmt(&mut self, args: &str, _bang: bool) -> Result<Option<String>, String> {
        let a = args.trim();
        let paren = a.find('(').ok_or("E124: Missing '('")?;
        let name = a[..paren].trim().to_string();
        let close = a[paren..].find(')').map(|x| x + paren).ok_or("E125: Illegal argument")?;
        let params: Vec<String> = a[paren + 1..close].split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        let attrs = &a[close + 1..];
        let attrs = attrs.split('"').next().unwrap_or("");
        let is_range = attrs.split_whitespace().any(|w| w == "range");
        let is_dict = attrs.split_whitespace().any(|w| w == "dict");
        let name_code = if let Some(r) = name.strip_prefix("s:").or_else(|| name.strip_prefix("<SID>")) {
            format!("S .. {}", lua_str(r))
        } else if let Some(r) = name.strip_prefix("g:") {
            lua_str(r)
        } else if name.contains('.') {
            // dict function: let d.f = function
            let (d, k) = name.split_once('.').unwrap();
            let p = Parser::new("", self);
            let dv = p.var_ref(d);
            let pl: Vec<String> = params.iter().filter(|p| *p != "...").map(|p| lua_str(p)).collect();
            self.emit(format!("V.setindex({}, {}, V.lambda({{ {} }}, {}, function(a, l)", dv, lua_str(k), pl.join(", "), params.iter().any(|p| p == "...")));
            self.indent += 1;
            self.blocks.push(Block::Func { close: "end))" });
            self.in_func += 1;
            self.func_locals.push(Vec::new());
            return Ok(None);
        } else {
            lua_str(&name)
        };
        let pl: Vec<String> = params.iter().filter(|p| *p != "...").map(|p| lua_str(p)).collect();
        let varargs = params.iter().any(|p| p == "...");
        let mut flags = Vec::new();
        if varargs {
            flags.push("varargs = true");
        }
        if is_range {
            flags.push("range = true");
        }
        if is_dict {
            flags.push("dict = true");
        }
        let fl = if flags.is_empty() { String::new() } else { format!(", {{ {} }}", flags.join(", ")) };
        self.emit(format!("V.func({}, {{ {} }}{}, function(a, l)", name_code, pl.join(", "), fl));
        self.indent += 1;
        self.blocks.push(Block::Func { close: "end)" });
        self.in_func += 1;
        self.func_locals.push(Vec::new());
        Ok(None)
    }

    fn let_stmt(&mut self, args: &str) -> Result<Option<String>, String> {
        let mut p = Parser::new(args, self);
        p.ws();
        if p.peek() == Some('[') {
            // let [a, b] = list
            p.i += 1;
            let mut targets = Vec::new();
            loop {
                p.ws();
                if p.peek() == Some(']') {
                    p.i += 1;
                    break;
                }
                if p.peek() == Some(';') {
                    p.i += 1;
                    continue;
                }
                let (lv, _) = self.lvalue(&mut p)?;
                targets.push(lv);
                p.ws();
                if p.peek() == Some(',') {
                    p.i += 1;
                }
            }
            p.ws();
            if p.peek() != Some('=') {
                return Err("E15: Invalid expression (let [...])".into());
            }
            p.i += 1;
            let e = p.expr()?;
            let rest = p.tail()?;
            self.next_id += 1;
            let tmp = format!("_t{}", self.next_id);
            let mut code = vec![format!("local {} = {}", tmp, e.code)];
            for (i, t) in targets.into_iter().enumerate() {
                code.push(assign_code(&t, &format!("V.index({}, {})", tmp, i)));
            }
            self.simple(format!("do\n{}\nend", indent_text(&code.join("\n"))), true);
            return Ok(rest);
        }
        let (lv, is_func_name) = self.lvalue(&mut p)?;
        p.ws();
        let op = if p.starts("..=") {
            p.i += 3;
            ".="
        } else {
            let two: String = p.s[p.i.min(p.s.len())..(p.i + 2).min(p.s.len())].iter().collect();
            match two.as_str() {
                ".=" | "+=" | "-=" | "*=" | "/=" | "%=" => {
                    p.i += 2;
                    if two == ".=" {
                        ".="
                    } else if two == "+=" {
                        "+="
                    } else if two == "-=" {
                        "-="
                    } else if two == "*=" {
                        "*="
                    } else if two == "/=" {
                        "/="
                    } else {
                        "%="
                    }
                }
                _ => {
                    if p.peek() == Some('=') {
                        p.i += 1;
                        "="
                    } else {
                        // `:let var` lists the variable
                        let v = match &lv {
                            LValue::Var(v) => v.clone(),
                            _ => "nil".into(),
                        };
                        self.simple(format!("V.echo({})", v), true);
                        return p.tail();
                    }
                }
            }
        };
        let e = p.expr()?;
        let rest = p.tail()?;
        // remember capitalised locals that hold funcrefs
        if self.in_func > 0 {
            if let LValue::Var(_) = &lv {
                if is_func_name {
                    let raw = args.trim_start().split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':')).next().unwrap_or("");
                    let n = raw.strip_prefix("l:").unwrap_or(raw).to_string();
                    if let Some(fl) = self.func_locals.last_mut() {
                        fl.push(n);
                    }
                }
            }
        }
        let cur = lvalue_read(&lv);
        let value = match op {
            "=" => e.code.clone(),
            ".=" => format!("V.concat({}, {})", cur, e.code),
            "+=" => format!("V.add({}, {})", cur, e.code),
            "-=" => format!("V.sub({}, {})", cur, e.code),
            "*=" => format!("V.mul({}, {})", cur, e.code),
            "/=" => format!("V.div({}, {})", cur, e.code),
            _ => format!("V.mod({}, {})", cur, e.code),
        };
        let risky = !(op == "=" && is_literal(&e.code) && matches!(lv, LValue::Var(_) | LValue::Opt(..)));
        self.simple(assign_code(&lv, &value), risky);
        Ok(rest)
    }

    fn lvalue(&self, p: &mut Parser) -> Result<(LValue, bool), String> {
        p.ws();
        match p.peek() {
            Some('&') => {
                p.i += 1;
                let mut scope = "";
                if matches!(p.peek(), Some('l') | Some('g')) && p.peek_at(1) == Some(':') {
                    scope = if p.peek() == Some('l') { "l" } else { "g" };
                    p.i += 2;
                }
                let st = p.i;
                while p.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                    p.i += 1;
                }
                let name: String = p.s[st..p.i].iter().collect();
                let full = options::find_def(&name).map(|d| d.name.to_string()).unwrap_or(name);
                Ok((LValue::Opt(full, scope.to_string()), false))
            }
            Some('@') => {
                p.i += 1;
                let r = p.peek().unwrap_or('"');
                p.i += 1;
                Ok((LValue::Reg(if r == '@' { '"' } else { r }), false))
            }
            Some('$') => {
                p.i += 1;
                let st = p.i;
                while p.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                    p.i += 1;
                }
                Ok((LValue::Env(p.s[st..p.i].iter().collect()), false))
            }
            _ => {
                let name = p.ident();
                if name.is_empty() {
                    return Err("E461: Illegal variable name".into());
                }
                let bare = name.split_once(':').map(|(_, r)| r).unwrap_or(&name);
                let funcish = bare.chars().next().map(|c| c.is_ascii_uppercase()).unwrap_or(false);
                let mut code = p.var_ref(&name);
                let mut lv = LValue::Var(code.clone());
                loop {
                    match p.peek() {
                        Some('[') => {
                            p.i += 1;
                            let idx = p.expr()?;
                            p.ws();
                            if p.peek() == Some(':') {
                                return Err("E709: list slice assignment is not supported".into());
                            }
                            if p.peek() != Some(']') {
                                return Err("E111: Missing ']'".into());
                            }
                            p.i += 1;
                            lv = LValue::Index(code.clone(), idx.code.clone());
                            code = format!("V.index({}, {})", code, idx.code);
                        }
                        Some('.') if p.peek_at(1).map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) => {
                            p.i += 1;
                            let st = p.i;
                            while p.peek().map(|c| c.is_ascii_alphanumeric() || c == '_').unwrap_or(false) {
                                p.i += 1;
                            }
                            let key: String = p.s[st..p.i].iter().collect();
                            lv = LValue::Index(code.clone(), lua_str(&key));
                            code = format!("V.index({}, {})", code, lua_str(&key));
                        }
                        _ => break,
                    }
                }
                Ok((lv, funcish))
            }
        }
    }
}

enum LValue {
    Var(String),
    Index(String, String),
    Opt(String, String),
    Reg(char),
    Env(String),
}

fn lvalue_read(lv: &LValue) -> String {
    match lv {
        LValue::Var(v) => v.clone(),
        LValue::Index(b, i) => format!("V.index({}, {})", b, i),
        LValue::Opt(n, s) => match s.as_str() {
            "" => field("vim.o", n),
            sc => format!("V.getopt({}, '{}')", lua_str(n), sc),
        },
        LValue::Reg(r) => format!("vim.fn.getreg({})", lua_str(&r.to_string())),
        LValue::Env(n) => field("vim.env", n),
    }
}

fn assign_code(lv: &LValue, value: &str) -> String {
    match lv {
        LValue::Var(v) => format!("{} = {}", v, value),
        LValue::Index(b, i) => format!("V.setindex({}, {}, {})", b, i, value),
        LValue::Opt(n, s) => match s.as_str() {
            "l" => format!("{} = {}", field("vim.opt_local", n), value),
            "g" => format!("{} = {}", field("vim.opt_global", n), value),
            _ => format!("{} = {}", field("vim.o", n), value),
        },
        LValue::Reg(r) => format!("vim.fn.setreg({}, {})", lua_str(&r.to_string()), value),
        LValue::Env(n) => format!("{} = {}", field("vim.env", n), value),
    }
}

fn is_literal(code: &str) -> bool {
    let c = code.trim();
    c.parse::<f64>().is_ok()
        || (c.starts_with('\'') && c.ends_with('\'') && !c[1..c.len() - 1].contains('\''))
        || (c.starts_with("[[") && c.ends_with("]]"))
        || c == "true"
        || c == "false"
}

fn indent_text(s: &str) -> String {
    s.lines().map(|l| if l.is_empty() { String::new() } else { format!("  {}", l) }).collect::<Vec<_>>().join("\n")
}

fn after_bar(args: &str) -> Option<String> {
    let a = args.trim_start();
    a.strip_prefix('|').map(|r| r.to_string())
}

fn find_word(s: &str, w: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while let Some(p) = s[i..].find(w) {
        let at = i + p;
        let before = at == 0 || !(b[at - 1].is_ascii_alphanumeric() || b[at - 1] == b'_');
        let after = at + w.len() >= b.len() || !(b[at + w.len()].is_ascii_alphanumeric() || b[at + w.len()] == b'_');
        if before && after && at > 0 {
            return Some(at);
        }
        i = at + w.len();
    }
    None
}

/// Split `:set` arguments on unescaped spaces, removing the escapes.
fn split_set_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(' ') | Some('\\') | Some('"') | Some('|') => cur.push(chars.next().unwrap()),
                _ => cur.push('\\'),
            }
        } else if c == ' ' || c == '\t' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One `:set` argument as a Lua statement; `None` if it needs `vim.cmd`.
fn set_arg_to_lua(base: &str, a: &str) -> Option<String> {
    if a.starts_with("t_") {
        return Some(format!("-- set {} (terminal option, not used by fevim)", a));
    }
    let name_end = a.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(a.len());
    let name = &a[..name_end];
    let tail = &a[name_end..];
    let (def, neg, inv) = match options::find_def(name) {
        Some(d) => (d, false, false),
        None => {
            if let Some(n) = name.strip_prefix("no").and_then(options::find_def) {
                (n, true, false)
            } else if let Some(n) = name.strip_prefix("inv").and_then(options::find_def) {
                (n, false, true)
            } else {
                return None;
            }
        }
    };
    let target = field(base, def.name);
    match def.kind {
        options::OptKind::Bool => {
            if inv || tail == "!" {
                return Some(format!("{} = not {}", target, field("vim.o", def.name)));
            }
            if !tail.is_empty() {
                return None;
            }
            Some(format!("{} = {}", target, !neg))
        }
        _ => {
            if neg || inv {
                return None;
            }
            let (op, val) = if let Some(v) = tail.strip_prefix("+=") {
                ("append", v)
            } else if let Some(v) = tail.strip_prefix("-=") {
                ("remove", v)
            } else if let Some(v) = tail.strip_prefix("^=") {
                ("prepend", v)
            } else if let Some(v) = tail.strip_prefix('=').or_else(|| tail.strip_prefix(':')) {
                ("set", v)
            } else {
                return None;
            };
            let lit = if def.kind == options::OptKind::Num {
                match val.parse::<i64>() {
                    Ok(n) => n.to_string(),
                    Err(_) => return None,
                }
            } else {
                lua_str(val)
            };
            Some(match op {
                "set" => format!("{} = {}", target, lit),
                op => format!("{}:{}({})", target, op, lit),
            })
        }
    }
}

fn known_event(e: &str) -> bool {
    const EVENTS: &[&str] = &[
        "bufadd", "bufcreate", "bufdelete", "bufenter", "buffilepost", "buffilepre", "bufhidden", "bufleave", "bufnew", "bufnewfile",
        "bufread", "bufreadcmd", "bufreadpost", "bufreadpre", "bufunload", "bufwinenter", "bufwinleave", "bufwipeout", "bufwrite",
        "bufwritecmd", "bufwritepost", "bufwritepre", "cmdlinechanged", "cmdlineenter", "cmdlineleave", "cmdwinenter", "cmdwinleave",
        "colorscheme", "colorschemepre", "completedone", "cursorhold", "cursorholdi", "cursormoved", "cursormovedi", "dirchanged",
        "encodingchanged", "exitpre", "filetype", "filereadpost", "filereadpre", "filewritepost", "filewritepre", "focusgained",
        "focuslost", "guienter", "insertchange", "insertcharpre", "insertenter", "insertleave", "insertleavepre", "modechanged",
        "optionset", "quickfixcmdpost", "quickfixcmdpre", "quitpre", "sourcepost", "sourcepre", "stdinreadpost", "syntax",
        "tabclosed", "tabenter", "tableave", "tabnew", "textchanged", "textchangedi", "textyankpost", "user", "vimenter",
        "vimleave", "vimleavepre", "vimresized", "winclosed", "winenter", "winleave", "winnew", "winscrolled",
    ];
    EVENTS.contains(&e.to_ascii_lowercase().as_str())
}

/// The CamelCase spelling Vim uses for an event name.
fn canonical_event(e: &str) -> String {
    const NAMES: &[&str] = &[
        "BufAdd", "BufCreate", "BufDelete", "BufEnter", "BufFilePost", "BufFilePre", "BufHidden", "BufLeave", "BufNew", "BufNewFile",
        "BufRead", "BufReadCmd", "BufReadPost", "BufReadPre", "BufUnload", "BufWinEnter", "BufWinLeave", "BufWipeout", "BufWrite",
        "BufWriteCmd", "BufWritePost", "BufWritePre", "CmdlineChanged", "CmdlineEnter", "CmdlineLeave", "CmdwinEnter",
        "CmdwinLeave", "ColorScheme", "ColorSchemePre", "CompleteDone", "CursorHold", "CursorHoldI", "CursorMoved",
        "CursorMovedI", "DirChanged", "EncodingChanged", "ExitPre", "FileType", "FileReadPost", "FileReadPre", "FileWritePost",
        "FileWritePre", "FocusGained", "FocusLost", "GUIEnter", "InsertChange", "InsertCharPre", "InsertEnter", "InsertLeave",
        "InsertLeavePre", "ModeChanged", "OptionSet", "QuickFixCmdPost", "QuickFixCmdPre", "QuitPre", "SourcePost", "SourcePre",
        "StdinReadPost", "Syntax", "TabClosed", "TabEnter", "TabLeave", "TabNew", "TextChanged", "TextChangedI", "TextYankPost",
        "User", "VimEnter", "VimLeave", "VimLeavePre", "VimResized", "WinClosed", "WinEnter", "WinLeave", "WinNew", "WinScrolled",
    ];
    NAMES.iter().find(|n| n.eq_ignore_ascii_case(e)).map(|n| n.to_string()).unwrap_or_else(|| e.to_string())
}

// ------------------------------------------------------------------ API

/// Translate a Vim script.  `name` is used in error messages; `wrap`
/// isolates errors per top-level statement (used for sourced files).
pub fn transpile(src: &str, name: Option<&str>, wrap: bool) -> Result<String, String> {
    let mut tr = Translator::new(wrap);
    tr.script(src)?;
    let mut out = String::new();
    if let Some(n) = name {
        out.push_str(&format!("-- Converted from {} by fevim's Vim script translator.\n", n));
    }
    out.push_str("local V = vim._vs\n");
    out.push_str(&format!("local S, s, at = V.script({})\n", name.map(lua_str).unwrap_or_else(|| "nil".into())));
    out.push('\n');
    out.push_str(&tr.out.join("\n"));
    out.push('\n');
    Ok(out)
}

/// Translate a single expression into a Lua chunk returning its value.
pub fn transpile_expr(expr: &str) -> Result<String, String> {
    let tr = Translator::new(false);
    let mut p = Parser::new(expr, &tr);
    let e = p.expr()?;
    if !p.at_end() {
        return Err(format!("E15: Invalid expression: \"{}\"", expr));
    }
    Ok(format!("local V = vim._vs\nlocal S, s = V.script(nil)\nreturn {}", e.code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(src: &str) -> String {
        let mut tr = Translator::new(false);
        tr.script(src).unwrap();
        tr.out.join("\n")
    }

    #[test]
    fn sets_and_maps() {
        assert_eq!(t("set nocompatible"), "vim.opt.compatible = false");
        assert_eq!(t("set fileformats=unix,dos,mac"), "vim.opt.fileformats = 'unix,dos,mac'");
        assert_eq!(t("set tabstop=4"), "vim.opt.tabstop = 4");
        assert_eq!(t("autocmd FileType * set formatoptions-=ro"), "vim.api.nvim_create_autocmd('FileType', { pattern = '*', command = 'set formatoptions-=ro' })");
        assert_eq!(t("set vb t_vb="), "vim.opt.visualbell = true\n-- set t_vb= (terminal option, not used by fevim)");
        assert_eq!(t("nnoremap Y y$"), "vim.keymap.set('n', 'Y', 'y$')");
        assert_eq!(t("nmap n nzz"), "vim.keymap.set('n', 'n', 'nzz', { remap = true })");
        assert_eq!(t("set grepprg=grep\\ -nH | cw"), "vim.opt.grepprg = 'grep -nH'\nvim.cmd('cw')");
        assert_eq!(t("setlocal statusline+=\\ %L"), "vim.opt_local.statusline:append(' %L')");
    }

    #[test]
    fn expressions() {
        assert_eq!(t("let g:x = 1"), "vim.g.x = 1");
        assert_eq!(t("let@/=@1"), "vim.fn.setreg('/', vim.fn.getreg('1'))");
        assert_eq!(t("if exists('&ambiwidth')\nset ambiwidth=double\nendif"), "if V.truthy(vim.fn.exists('&ambiwidth')) then\n  vim.opt.ambiwidth = 'double'\nend");
        let f = t("function! Foo(cmd, ...)\n  let x = a:cmd . 'a'\n  if a:0 > 0\n    return \"\\<S-CR>\"\n  end\nendfunction");
        assert!(f.contains("V.func('Foo', { 'cmd' }, { varargs = true }, function(a, l)"), "{}", f);
        assert!(f.contains("l.x = V.concat(a.cmd, 'a')"), "{}", f);
        assert!(f.contains("if V.gt(a['0'], 0) then"), "{}", f);
        assert!(f.contains("vim.keycode('<S-CR>')"), "{}", f);
        let s = t("set statusline=%<%f\\ %m%r%h%w%{'['.(&fenc!=''?&fenc:&enc).']['.&ff.']'}%=%l,%c%V%8P");
        assert!(s.starts_with("vim.opt.statusline = "), "{}", s);
        let e = transpile_expr("'['.(&fenc!=''?&fenc:&enc).']['.&ff.']'").unwrap();
        assert!(e.contains("V.concat('[', (V.tern(V.ne(vim.o.fileencoding, ''), vim.o.fileencoding, vim.o.encoding)), '][', vim.o.fileformat, ']')"), "{}", e);
    }

    #[test]
    fn autocmd_bar_split() {
        let s = t("autocmd BufReadPost * if &binary | silent %!xxd -g 1");
        assert_eq!(s, "vim.api.nvim_create_autocmd('BufReadPost', { pattern = '*', command = 'if &binary | silent %!xxd -g 1' })");
        let s = t("if &binary | silent %!xxd -g 1\nset ft=xxd | endif");
        assert_eq!(s, "if V.truthy(vim.o.binary) then\n  vim.cmd('silent %!xxd -g 1')\n  vim.opt.filetype = 'xxd'\nend");
    }
}
