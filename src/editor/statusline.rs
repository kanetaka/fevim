//! Evaluation of 'statusline' format strings (`%f %m %l,%c%V %P %{expr}` ...).

use super::{Editor, WinId};

/// A rendered status line: text plus highlight group changes at char offsets.
pub struct StlOut {
    pub text: String,
    /// (char offset, group) – group `""` means the default status line group.
    pub groups: Vec<(usize, String)>,
}

enum Item {
    Text(String),
    Group(String),
    Sep,
    Trunc,
}

impl Editor {
    pub fn eval_statusline(&mut self, fmt: &str, win: WinId, width: usize) -> StlOut {
        let Some(w) = self.windows.get(&win) else {
            return StlOut { text: String::new(), groups: Vec::new() };
        };
        let bid = w.buf;
        let cursor = w.cursor;
        let topline = w.topline;
        let b = &self.buffers[&bid];
        let name = b.path.as_ref().map(|_| b.display_name()).unwrap_or_else(|| "[No Name]".into());
        let full_name = b.path.as_ref().map(|p| self.full_path(p).to_string_lossy().replace('\\', "/")).unwrap_or_else(|| name.clone());
        let modified = b.modified;
        let nlines = b.line_count();
        let line_text = b.line(cursor.line).to_string();
        let ft = self.opt_for(Some(w), Some(b), "filetype").as_string();
        let ro = self.opt_for(Some(w), Some(b), "readonly").as_bool();
        let modifiable = self.opt_for(Some(w), Some(b), "modifiable").as_bool();
        let ts = self.tabstop_of(w);
        let ambi = self.ambi_double();
        let vcol = super::motion::vcol_of(&line_text, cursor.col, ts, ambi) + 1;
        let bytecol = if line_text.is_empty() { 0 } else { cursor.col + 1 };
        let bot = self.botline(w);
        let pct = if topline == 0 && bot + 1 >= nlines {
            "All".to_string()
        } else if topline == 0 {
            "Top".to_string()
        } else if bot + 1 >= nlines {
            "Bot".to_string()
        } else {
            format!("{}%", topline * 100 / (nlines.saturating_sub(w.height)).max(1))
        };
        let cur_win = self.cur_win_id();

        let mut items: Vec<Item> = Vec::new();
        let chars: Vec<char> = fmt.chars().collect();
        let mut i = 0;
        let mut lit = String::new();
        let flush = |lit: &mut String, items: &mut Vec<Item>| {
            if !lit.is_empty() {
                items.push(Item::Text(std::mem::take(lit)));
            }
        };
        while i < chars.len() {
            let c = chars[i];
            i += 1;
            if c != '%' {
                lit.push(c);
                continue;
            }
            if i >= chars.len() {
                break;
            }
            // flags: - 0 minwid .maxwid
            let mut left = false;
            if chars[i] == '-' {
                left = true;
                i += 1;
            }
            let mut zero = false;
            if i < chars.len() && chars[i] == '0' {
                zero = true;
                i += 1;
            }
            let mut minw = 0usize;
            while i < chars.len() && chars[i].is_ascii_digit() {
                minw = minw * 10 + (chars[i] as usize - '0' as usize);
                i += 1;
            }
            let mut maxw: Option<usize> = None;
            if i < chars.len() && chars[i] == '.' {
                i += 1;
                let mut m = 0;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    m = m * 10 + (chars[i] as usize - '0' as usize);
                    i += 1;
                }
                maxw = Some(m);
            }
            if i >= chars.len() {
                break;
            }
            let item = chars[i];
            i += 1;
            let val: Option<String> = match item {
                '%' => Some("%".into()),
                'f' => Some(name.clone()),
                'F' => Some(full_name.clone()),
                't' => Some(name.rsplit('/').next().unwrap_or(&name).to_string()),
                'm' => Some(if modified { "[+]".into() } else if !modifiable { "[-]".into() } else { String::new() }),
                'M' => Some(if modified { ",+".into() } else { String::new() }),
                'r' => Some(if ro { "[RO]".into() } else { String::new() }),
                'R' => Some(if ro { ",RO".into() } else { String::new() }),
                'h' | 'H' => Some(String::new()),
                'w' | 'W' => Some(String::new()),
                'q' => Some(String::new()),
                'y' => Some(if ft.is_empty() { String::new() } else { format!("[{}]", ft) }),
                'Y' => Some(if ft.is_empty() { String::new() } else { format!(",{}", ft.to_uppercase()) }),
                'n' => Some(bid.to_string()),
                'l' => Some((cursor.line + 1).to_string()),
                'L' => Some(nlines.to_string()),
                'c' => Some(bytecol.to_string()),
                'v' => Some(vcol.to_string()),
                'V' => Some(if vcol != bytecol && !line_text.is_empty() { format!("-{}", vcol) } else if line_text.is_empty() { "-1".into() } else { String::new() }),
                'p' => Some(((cursor.line + 1) * 100 / nlines.max(1)).to_string()),
                'P' => Some(pct.clone()),
                'b' => Some(super::motion::char_at(&line_text, cursor.col).map(|c| (c as u32).to_string()).unwrap_or_else(|| "0".into())),
                'B' => Some(super::motion::char_at(&line_text, cursor.col).map(|c| format!("{:x}", c as u32)).unwrap_or_else(|| "0".into())),
                'o' => {
                    let off: usize = self.buffers[&bid].lines[..cursor.line].iter().map(|l| l.len() + 1).sum::<usize>() + cursor.col + 1;
                    Some(off.to_string())
                }
                '=' => {
                    flush(&mut lit, &mut items);
                    items.push(Item::Sep);
                    None
                }
                '<' => {
                    flush(&mut lit, &mut items);
                    items.push(Item::Trunc);
                    None
                }
                '*' => {
                    flush(&mut lit, &mut items);
                    items.push(Item::Group(if minw == 0 { String::new() } else { format!("User{}", minw) }));
                    None
                }
                '#' => {
                    let end = chars[i..].iter().position(|c| *c == '#').map(|p| i + p).unwrap_or(chars.len());
                    let g: String = chars[i..end].iter().collect();
                    i = (end + 1).min(chars.len());
                    flush(&mut lit, &mut items);
                    items.push(Item::Group(g));
                    None
                }
                '{' => {
                    let end = chars[i..].iter().position(|c| *c == '}').map(|p| i + p).unwrap_or(chars.len());
                    let expr: String = chars[i..end].iter().collect();
                    i = (end + 1).min(chars.len());
                    // evaluate with the status line's window as current
                    let saved = self.tabs[self.cur_tab].cur;
                    self.tabs[self.cur_tab].cur = win;
                    let silent = self.silent_depth;
                    self.silent_depth = 2;
                    let v = self.eval_expr(&expr).map(|v| v.to_display()).unwrap_or_default();
                    self.silent_depth = silent;
                    self.tabs[self.cur_tab].cur = saved;
                    Some(v)
                }
                '(' | ')' | 'T' | 'X' | '@' => Some(String::new()),
                _ => Some(String::new()),
            };
            if let Some(mut v) = val {
                if let Some(m) = maxw {
                    let n = v.chars().count();
                    if n > m {
                        v = format!("<{}", v.chars().skip(n - m + 1).collect::<String>());
                    }
                }
                let n = v.chars().count();
                if n < minw {
                    let pad = minw - n;
                    let numeric = !v.is_empty() && v.chars().all(|c| c.is_ascii_digit());
                    if left {
                        v.push_str(&" ".repeat(pad));
                    } else if zero && numeric {
                        v = format!("{}{}", "0".repeat(pad), v);
                    } else {
                        v = format!("{}{}", " ".repeat(pad), v);
                    }
                }
                lit.push_str(&v);
            }
        }
        flush(&mut lit, &mut items);
        let _ = cur_win;

        // layout: split at separators and fill to width
        let total: usize = items.iter().map(|it| if let Item::Text(t) = it { display_width(t) } else { 0 }).sum();
        let seps = items.iter().filter(|it| matches!(it, Item::Sep)).count();
        let mut fill = width.saturating_sub(total);
        let mut text = String::new();
        let mut groups = Vec::new();
        let mut nchars = 0usize;
        let mut sep_idx = 0;
        let mut overflow = total.saturating_sub(width);
        for it in items {
            match it {
                Item::Text(t) => {
                    let t = if overflow > 0 {
                        // truncate from the start of the first long item
                        let mut out = String::new();
                        let mut dropped = 0;
                        for c in t.chars() {
                            if dropped < overflow {
                                dropped += super::motion::char_width(c, false);
                                continue;
                            }
                            out.push(c);
                        }
                        overflow -= dropped.min(overflow);
                        if !out.is_empty() || dropped > 0 {
                            format!("<{}", out.chars().skip(1).collect::<String>())
                        } else {
                            out
                        }
                    } else {
                        t
                    };
                    nchars += t.chars().count();
                    text.push_str(&t);
                }
                Item::Group(g) => groups.push((nchars, g)),
                Item::Sep => {
                    sep_idx += 1;
                    let share = if sep_idx == seps { fill } else { fill / (seps - sep_idx + 1) };
                    fill -= share;
                    text.push_str(&" ".repeat(share));
                    nchars += share;
                }
                Item::Trunc => {}
            }
        }
        StlOut { text, groups }
    }
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(|c| super::motion::char_width(c, false)).sum()
}
