//! Highlight groups, colours and the built-in colour schemes.

use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Color(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub struct Attr {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub sp: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub undercurl: bool,
    pub strikethrough: bool,
    pub reverse: bool,
}

impl Attr {
    /// Layer `over` on top of `self`.
    pub fn combine(&self, over: &Attr) -> Attr {
        Attr {
            fg: over.fg.or(self.fg),
            bg: over.bg.or(self.bg),
            sp: over.sp.or(self.sp),
            bold: self.bold || over.bold,
            italic: self.italic || over.italic,
            underline: self.underline || over.underline,
            undercurl: self.undercurl || over.undercurl,
            strikethrough: self.strikethrough || over.strikethrough,
            reverse: self.reverse ^ over.reverse,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct HlGroup {
    pub name: String,
    pub attr: Attr,
    pub link: Option<String>,
    /// Raw key=value arguments that only matter for terminals; kept so that
    /// `:highlight` can show them.
    pub term_args: Vec<(String, String)>,
    pub cleared: bool,
}

pub struct Highlights {
    groups: HashMap<String, HlGroup>,
    /// Bumped on every change so renderers can invalidate caches.
    pub generation: u64,
}

const COLOR_NAMES: &[(&str, u32)] = &[
    ("black", 0x000000),
    ("white", 0xffffff),
    ("red", 0xff0000),
    ("green", 0x00ff00),
    ("blue", 0x0000ff),
    ("yellow", 0xffff00),
    ("cyan", 0x00ffff),
    ("magenta", 0xff00ff),
    ("gray", 0xbebebe),
    ("grey", 0xbebebe),
    ("darkgray", 0xa9a9a9),
    ("darkgrey", 0xa9a9a9),
    ("lightgray", 0xd3d3d3),
    ("lightgrey", 0xd3d3d3),
    ("darkred", 0x8b0000),
    ("darkgreen", 0x006400),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkmagenta", 0x8b008b),
    ("darkyellow", 0x8b8b00),
    ("brown", 0xa52a2a),
    ("lightred", 0xff8b8b),
    ("lightgreen", 0x90ee90),
    ("lightblue", 0xadd8e6),
    ("lightcyan", 0xe0ffff),
    ("lightmagenta", 0xff8bff),
    ("lightyellow", 0xffffe0),
    ("orange", 0xffa500),
    ("purple", 0xa020f0),
    ("violet", 0xee82ee),
    ("seagreen", 0x2e8b57),
    ("slateblue", 0x6a5acd),
    ("pink", 0xffc0cb),
    ("gold", 0xffd700),
    ("navy", 0x000080),
    ("grey10", 0x1a1a1a),
    ("grey20", 0x333333),
    ("grey30", 0x4d4d4d),
    ("grey40", 0x666666),
    ("grey50", 0x7f7f7f),
    ("grey60", 0x999999),
    ("grey70", 0xb3b3b3),
    ("grey80", 0xcccccc),
    ("grey90", 0xe5e5e5),
];

/// Parse a GUI colour (`#rrggbb` or a colour name).  Returns `None` for
/// `NONE`, `fg`, `bg` and unknown names.
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        if hex.len() == 6 {
            return u32::from_str_radix(hex, 16).ok().map(Color);
        }
        if hex.len() == 3 {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xf, (v >> 4) & 0xf, v & 0xf);
            return Some(Color((r * 17) << 16 | (g * 17) << 8 | b * 17));
        }
        return None;
    }
    let l: String = s.to_ascii_lowercase().chars().filter(|c| *c != ' ' && *c != '_').collect();
    let l = l.replace("gray", "grey");
    COLOR_NAMES
        .iter()
        .find(|(n, _)| n.replace("gray", "grey") == l)
        .map(|(_, v)| Color(*v))
}

fn canon(name: &str) -> String {
    name.to_ascii_lowercase()
}

impl Default for Highlights {
    fn default() -> Self {
        Self::new()
    }
}

impl Highlights {
    pub fn new() -> Highlights {
        let mut h = Highlights { groups: HashMap::new(), generation: 0 };
        h.load_builtin("default", true);
        h
    }

    pub fn exists(&self, name: &str) -> bool {
        self.groups.get(&canon(name)).map(|g| !g.cleared || g.link.is_some()).unwrap_or(false)
    }

    pub fn group(&self, name: &str) -> Option<&HlGroup> {
        self.groups.get(&canon(name))
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.groups.values().map(|g| g.name.clone()).collect();
        v.sort_by_key(|a| a.to_lowercase());
        v
    }

    /// Resolve a group (following links) to its attributes.
    pub fn attr(&self, name: &str) -> Attr {
        let mut key = canon(name);
        for _ in 0..10 {
            match self.groups.get(&key) {
                Some(g) => match &g.link {
                    Some(l) => key = canon(l),
                    None => return g.attr,
                },
                None => return Attr::default(),
            }
        }
        Attr::default()
    }

    pub fn normal_fg(&self) -> Color {
        self.attr("Normal").fg.unwrap_or(Color(0xc0c0c0))
    }

    pub fn normal_bg(&self) -> Color {
        self.attr("Normal").bg.unwrap_or(Color(0x000000))
    }

    pub fn clear_all(&mut self) {
        self.groups.clear();
        self.generation += 1;
    }

    pub fn link(&mut self, from: &str, to: &str, force: bool) {
        self.generation += 1;
        let g = self.groups.entry(canon(from)).or_insert_with(|| HlGroup { name: from.to_string(), ..Default::default() });
        if to.eq_ignore_ascii_case("NONE") {
            g.link = None;
            return;
        }
        if !force && !g.cleared && g.link.is_none() && g.attr != Attr::default() {
            return; // `:hi link` does not override existing settings without `!`
        }
        g.link = Some(to.to_string());
    }

    pub fn clear_group(&mut self, name: &str) {
        self.generation += 1;
        if let Some(g) = self.groups.get_mut(&canon(name)) {
            g.attr = Attr::default();
            g.link = None;
            g.term_args.clear();
            g.cleared = true;
        }
    }

    /// Apply `key=value` arguments of a `:highlight` command.
    pub fn set(&mut self, name: &str, args: &[(String, String)], default_only: bool) {
        self.generation += 1;
        let normal = self.attr("Normal");
        let key = canon(name);
        if default_only {
            if let Some(g) = self.groups.get(&key) {
                if !g.cleared || g.link.is_some() {
                    return;
                }
            }
        }
        let g = self.groups.entry(key).or_insert_with(|| HlGroup { name: name.to_string(), ..Default::default() });
        g.cleared = false;
        g.link = None;
        for (k, v) in args {
            let kl = k.to_ascii_lowercase();
            let special = |v: &str, which: u8| -> Option<Color> {
                match v.to_ascii_lowercase().as_str() {
                    "fg" | "foreground" => normal.fg,
                    "bg" | "background" => normal.bg,
                    _ => {
                        let _ = which;
                        parse_color(v)
                    }
                }
            };
            match kl.as_str() {
                "guifg" => g.attr.fg = special(v, 0),
                "guibg" => g.attr.bg = special(v, 1),
                "guisp" => g.attr.sp = special(v, 2),
                "gui" => {
                    g.attr.bold = false;
                    g.attr.italic = false;
                    g.attr.underline = false;
                    g.attr.undercurl = false;
                    g.attr.reverse = false;
                    g.attr.strikethrough = false;
                    for a in v.split(',') {
                        match a.to_ascii_lowercase().as_str() {
                            "bold" => g.attr.bold = true,
                            "italic" => g.attr.italic = true,
                            "underline" | "underdouble" | "underdotted" | "underdashed" => g.attr.underline = true,
                            "undercurl" => g.attr.undercurl = true,
                            "reverse" | "inverse" => g.attr.reverse = true,
                            "strikethrough" => g.attr.strikethrough = true,
                            "standout" => g.attr.reverse = true,
                            _ => {}
                        }
                    }
                }
                _ => {
                    g.term_args.retain(|(k2, _)| !k2.eq_ignore_ascii_case(k));
                    g.term_args.push((k.clone(), v.clone()));
                }
            }
        }
    }

    /// Text shown by `:highlight Name`.
    pub fn describe(&self, name: &str) -> Option<String> {
        let g = self.groups.get(&canon(name))?;
        let mut out = format!("{:<14} xxx", g.name);
        if let Some(l) = &g.link {
            out.push_str(&format!(" links to {}", l));
            return Some(out);
        }
        if g.cleared {
            out.push_str(" cleared");
            return Some(out);
        }
        for (k, v) in &g.term_args {
            out.push_str(&format!(" {}={}", k, v));
        }
        let mut gui = Vec::new();
        if g.attr.bold {
            gui.push("bold");
        }
        if g.attr.italic {
            gui.push("italic");
        }
        if g.attr.underline {
            gui.push("underline");
        }
        if g.attr.undercurl {
            gui.push("undercurl");
        }
        if g.attr.reverse {
            gui.push("reverse");
        }
        if g.attr.strikethrough {
            gui.push("strikethrough");
        }
        if !gui.is_empty() {
            out.push_str(&format!(" gui={}", gui.join(",")));
        }
        if let Some(c) = g.attr.fg {
            out.push_str(&format!(" guifg=#{:06x}", c.0));
        }
        if let Some(c) = g.attr.bg {
            out.push_str(&format!(" guibg=#{:06x}", c.0));
        }
        if let Some(c) = g.attr.sp {
            out.push_str(&format!(" guisp=#{:06x}", c.0));
        }
        Some(out)
    }

    fn def(&mut self, name: &str, spec: &str) {
        if let Some(target) = spec.strip_prefix("link:") {
            self.link(name, target, true);
            return;
        }
        let args: Vec<(String, String)> = spec
            .split_whitespace()
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        self.clear_group(name);
        self.set(name, &args, false);
    }

    pub fn has_builtin(name: &str) -> bool {
        matches!(name, "default" | "hybrid")
    }

    /// Load one of the colour schemes compiled into fevim.
    pub fn load_builtin(&mut self, name: &str, dark: bool) -> bool {
        match name {
            "default" => {
                self.clear_all();
                if dark {
                    self.def("Normal", "guifg=#d0d0d0 guibg=#000000");
                } else {
                    self.def("Normal", "guifg=#000000 guibg=#ffffff");
                }
                for (n, d, l) in DEFAULT_GROUPS {
                    self.def(n, if dark { d } else { l });
                }
                for (n, target) in DEFAULT_LINKS {
                    self.link(n, target, true);
                }
                true
            }
            "hybrid" => {
                self.load_builtin("default", true);
                for (n, spec) in HYBRID {
                    self.def(n, spec);
                }
                true
            }
            _ => false,
        }
    }
}

/// (name, dark background definition, light background definition)
const DEFAULT_GROUPS: &[(&str, &str, &str)] = &[
    ("SpecialKey", "guifg=#00ffff", "guifg=#0000ff"),
    ("EndOfBuffer", "link:NonText", "link:NonText"),
    ("NonText", "gui=bold guifg=#4040ff", "gui=bold guifg=#0000ff"),
    ("Directory", "guifg=#00ffff", "guifg=#0000ff"),
    ("ErrorMsg", "guifg=#ffffff guibg=#ff0000", "guifg=#ffffff guibg=#ff0000"),
    ("IncSearch", "gui=reverse", "gui=reverse"),
    ("Search", "guifg=#000000 guibg=#ffff00", "guibg=#ffff00"),
    ("CurSearch", "link:Search", "link:Search"),
    ("MoreMsg", "gui=bold guifg=#2e8b57", "gui=bold guifg=#2e8b57"),
    ("ModeMsg", "gui=bold", "gui=bold"),
    ("LineNr", "guifg=#ffff00", "guifg=#a52a2a"),
    ("CursorLineNr", "gui=bold guifg=#ffff00", "gui=bold guifg=#a52a2a"),
    ("Question", "gui=bold guifg=#00ff00", "gui=bold guifg=#2e8b57"),
    ("StatusLine", "gui=bold,reverse", "gui=bold,reverse"),
    ("StatusLineNC", "gui=reverse", "gui=reverse"),
    ("VertSplit", "gui=reverse", "gui=reverse"),
    ("Title", "gui=bold guifg=#ff00ff", "gui=bold guifg=#ff00ff"),
    ("Visual", "guifg=#000000 guibg=#a9a9a9", "guifg=#000000 guibg=#d3d3d3"),
    ("WarningMsg", "guifg=#ff0000", "guifg=#ff0000"),
    ("WildMenu", "guifg=#000000 guibg=#ffff00", "guifg=#000000 guibg=#ffff00"),
    ("Folded", "guifg=#00ffff guibg=#a9a9a9", "guifg=#00008b guibg=#d3d3d3"),
    ("FoldColumn", "guifg=#00ffff guibg=#bebebe", "guifg=#00008b guibg=#bebebe"),
    ("SignColumn", "guifg=#00ffff guibg=#bebebe", "guifg=#00008b guibg=#bebebe"),
    ("Conceal", "guifg=#d3d3d3 guibg=#a9a9a9", "guifg=#d3d3d3 guibg=#a9a9a9"),
    ("SpellBad", "gui=undercurl guisp=#ff0000", "gui=undercurl guisp=#ff0000"),
    ("Pmenu", "guibg=#ff00ff", "guibg=#ffbbff"),
    ("PmenuSel", "guibg=#a9a9a9", "guibg=#bebebe"),
    ("PmenuSbar", "guibg=#bebebe", "guibg=#bebebe"),
    ("PmenuThumb", "guibg=#ffffff", "guibg=#000000"),
    ("TabLine", "gui=underline guibg=#a9a9a9", "gui=underline guibg=#d3d3d3"),
    ("TabLineSel", "gui=bold", "gui=bold"),
    ("TabLineFill", "gui=reverse", "gui=reverse"),
    ("CursorColumn", "guibg=#666666", "guibg=#e5e5e5"),
    ("CursorLine", "guibg=#666666", "guibg=#e5e5e5"),
    ("ColorColumn", "guibg=#8b0000", "guibg=#ff8b8b"),
    ("QuickFixLine", "link:Search", "link:Search"),
    ("MatchParen", "guibg=#008b8b", "guibg=#00ffff"),
    ("Cursor", "gui=reverse", "gui=reverse"),
    ("lCursor", "gui=reverse", "gui=reverse"),
    ("CursorIM", "", ""),
    ("MsgArea", "", ""),
    // syntax groups
    ("Comment", "guifg=#80a0ff", "guifg=#0000ff"),
    ("Constant", "guifg=#ffa0a0", "guifg=#ff00ff"),
    ("Special", "guifg=#ffa500", "guifg=#6a5acd"),
    ("Identifier", "guifg=#40ffff", "guifg=#008b8b"),
    ("Statement", "gui=bold guifg=#ffff60", "gui=bold guifg=#a52a2a"),
    ("PreProc", "guifg=#ff80ff", "guifg=#6a0dad"),
    ("Type", "gui=bold guifg=#60ff60", "gui=bold guifg=#2e8b57"),
    ("Underlined", "gui=underline guifg=#80a0ff", "gui=underline guifg=#6a5acd"),
    ("Ignore", "guifg=#000000", "guifg=#ffffff"),
    ("Error", "guifg=#ffffff guibg=#ff0000", "guifg=#ffffff guibg=#ff0000"),
    ("Todo", "guifg=#0000ff guibg=#ffff00", "guifg=#0000ff guibg=#ffff00"),
];

const DEFAULT_LINKS: &[(&str, &str)] = &[
    ("String", "Constant"),
    ("Character", "Constant"),
    ("Number", "Constant"),
    ("Boolean", "Constant"),
    ("Float", "Number"),
    ("Function", "Identifier"),
    ("Conditional", "Statement"),
    ("Repeat", "Statement"),
    ("Label", "Statement"),
    ("Operator", "Statement"),
    ("Keyword", "Statement"),
    ("Exception", "Statement"),
    ("Include", "PreProc"),
    ("Define", "PreProc"),
    ("Macro", "PreProc"),
    ("PreCondit", "PreProc"),
    ("StorageClass", "Type"),
    ("Structure", "Type"),
    ("Typedef", "Type"),
    ("Tag", "Special"),
    ("SpecialChar", "Special"),
    ("Delimiter", "Special"),
    ("SpecialComment", "Special"),
    ("Debug", "Special"),
    ("VisualNOS", "Visual"),
    ("StatusLineTerm", "StatusLine"),
    ("StatusLineTermNC", "StatusLineNC"),
    ("LineNrAbove", "LineNr"),
    ("LineNrBelow", "LineNr"),
    ("CursorLineSign", "SignColumn"),
    ("CursorLineFold", "FoldColumn"),
    ("PmenuKind", "Pmenu"),
    ("PmenuKindSel", "PmenuSel"),
    ("PmenuExtra", "Pmenu"),
    ("PmenuExtraSel", "PmenuSel"),
];

/// The "hybrid" colour scheme palette (w0ng/vim-hybrid).
const HYBRID: &[(&str, &str)] = &[
    ("Normal", "guifg=#c5c8c6 guibg=#1d1f21"),
    ("ColorColumn", "guibg=#282a2e"),
    ("CursorColumn", "guibg=#282a2e"),
    ("CursorLine", "guibg=#282a2e"),
    ("Directory", "guifg=#81a2be"),
    ("ErrorMsg", "guifg=#1d1f21 guibg=#cc6666 gui=NONE"),
    ("VertSplit", "guifg=#303030 guibg=#1d1f21 gui=NONE"),
    ("Folded", "guifg=#707880 guibg=#1c1c1c"),
    ("FoldColumn", "guifg=#707880 guibg=#1c1c1c"),
    ("SignColumn", "guifg=#707880 guibg=#1c1c1c"),
    ("LineNr", "guifg=#373b41"),
    ("CursorLineNr", "guifg=#f0c674 gui=NONE"),
    ("MatchParen", "guifg=#1d1f21 guibg=#5f5f87"),
    ("ModeMsg", "guifg=#b5bd68 gui=NONE"),
    ("MoreMsg", "guifg=#b5bd68 gui=NONE"),
    ("NonText", "guifg=#373b41 gui=NONE"),
    ("Pmenu", "guifg=#c5c8c6 guibg=#373b41"),
    ("PmenuSbar", "guibg=#1c1c1c"),
    ("PmenuSel", "guifg=#c5c8c6 guibg=#373b41 gui=reverse"),
    ("PmenuThumb", "guibg=#707880"),
    ("Question", "guifg=#b5bd68 gui=NONE"),
    ("Search", "guifg=#1d1f21 guibg=#f0c674"),
    ("IncSearch", "guifg=#1d1f21 guibg=#de935f gui=NONE"),
    ("SpecialKey", "guifg=#373b41"),
    ("SpellBad", "guisp=#cc6666 gui=undercurl"),
    ("StatusLine", "guifg=#707880 guibg=#1d1f21 gui=reverse"),
    ("StatusLineNC", "guifg=#303030 guibg=#707880 gui=reverse"),
    ("TabLine", "guifg=#c5c8c6 guibg=#1c1c1c gui=reverse"),
    ("TabLineSel", "guifg=#c5c8c6 guibg=#1d1f21 gui=bold"),
    ("TabLineFill", "guifg=#303030 guibg=#1d1f21 gui=reverse"),
    ("Title", "guifg=#f0c674 gui=NONE"),
    ("Visual", "guifg=NONE guibg=#373b41"),
    ("WarningMsg", "guifg=#cc6666"),
    ("WildMenu", "guifg=#1d1f21 guibg=#f0c674"),
    ("Comment", "guifg=#707880"),
    ("Constant", "guifg=#cc6666"),
    ("String", "guifg=#b5bd68"),
    ("Identifier", "guifg=#b294bb"),
    ("Function", "guifg=#f0c674"),
    ("Statement", "guifg=#81a2be gui=NONE"),
    ("Operator", "guifg=#8abeb7"),
    ("PreProc", "guifg=#8abeb7"),
    ("Type", "guifg=#de935f gui=NONE"),
    ("Structure", "guifg=#8abeb7"),
    ("Special", "guifg=#b5bd68"),
    ("Underlined", "guifg=#81a2be gui=underline"),
    ("Ignore", "guifg=#1d1f21"),
    ("Error", "guifg=#cc6666 guibg=#5f0000"),
    ("Todo", "guifg=#5f875f guibg=#1d1f21 gui=bold"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(parse_color("#3d3d3d"), Some(Color(0x3d3d3d)));
        assert_eq!(parse_color("White"), Some(Color(0xffffff)));
        assert_eq!(parse_color("DarkGrey"), Some(Color(0xa9a9a9)));
        assert_eq!(parse_color("NONE"), None);
    }

    #[test]
    fn links_and_updates() {
        let mut h = Highlights::new();
        assert_eq!(h.attr("String"), h.attr("Constant"));
        h.set("Search", &[("guibg".into(), "#008db7".into()), ("guifg".into(), "White".into())], false);
        assert_eq!(h.attr("Search").bg, Some(Color(0x008db7)));
        h.load_builtin("hybrid", true);
        assert_eq!(h.normal_bg(), Color(0x1d1f21));
    }
}
