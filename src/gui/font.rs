//! 'guifont' / 'guifontwide' handling and cell metrics.

use gpui::{Font, FontStyle, FontWeight, Pixels, Size, TextSystem, font, px};

use crate::editor::Editor;
use crate::editor::highlight::Attr;

#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: String,
    /// Size in points.
    pub points: f32,
    pub bold: bool,
    pub italic: bool,
}

/// Parse one entry of 'guifont' in Vim's format: `Consolas:h12:b`,
/// `Lucida_Console:h12:w5`, `Menlo Regular:h14`, `DejaVu Sans Mono 12` (GTK).
pub fn parse_font(entry: &str) -> Option<FontSpec> {
    let e = entry.trim();
    if e.is_empty() || e == "*" {
        return None;
    }
    let mut parts = e.split(':');
    let mut family = parts.next()?.replace('_', " ").replace("\\ ", " ");
    let mut spec = FontSpec { family: String::new(), points: 0.0, bold: false, italic: false };
    for p in parts {
        let mut cs = p.chars();
        match cs.next() {
            Some('h') => spec.points = cs.as_str().parse().unwrap_or(0.0),
            Some('b') => spec.bold = true,
            Some('i') => spec.italic = true,
            _ => {}
        }
    }
    // GTK style: "Family Name 12"
    if spec.points == 0.0 {
        if let Some((name, size)) = family.rsplit_once(' ') {
            if let Ok(n) = size.parse::<f32>() {
                spec.points = n;
                family = name.to_string();
            }
        }
    }
    spec.family = family.trim().to_string();
    Some(spec)
}

fn default_family() -> &'static str {
    if cfg!(windows) {
        "Consolas"
    } else if cfg!(target_os = "macos") {
        "Menlo"
    } else {
        "DejaVu Sans Mono"
    }
}

/// Vim sizes are in points; GPUI uses logical pixels (96 dpi on Windows and
/// Linux, 72 dpi "points" on macOS).
fn points_to_px(pt: f32) -> f32 {
    if cfg!(target_os = "macos") { pt } else { pt * 96.0 / 72.0 }
}

#[derive(Clone)]
pub struct FontSet {
    pub normal: Font,
    pub wide: Option<Font>,
    pub size: Pixels,
    pub cell_w: Pixels,
    pub cell_h: Pixels,
}

impl FontSet {
    pub fn from_options(ed: &Editor, ts: &TextSystem) -> FontSet {
        let names = ts.all_font_names();
        let exists = |fam: &str| names.iter().any(|n| n.eq_ignore_ascii_case(fam));
        let pick = |opt: &str| -> Option<FontSpec> {
            for entry in opt.split(',') {
                if let Some(s) = parse_font(entry) {
                    if exists(&s.family) {
                        return Some(s);
                    }
                }
            }
            None
        };
        let gf = ed.opt_global("guifont").as_string();
        let spec = pick(&gf).unwrap_or_else(|| {
            let size = gf.split(',').filter_map(parse_font).map(|s| s.points).find(|p| *p > 0.0).unwrap_or(if cfg!(target_os = "macos") { 13.0 } else { 11.0 });
            FontSpec { family: default_family().into(), points: size, bold: false, italic: false }
        });
        let points = if spec.points > 0.0 { spec.points } else if cfg!(target_os = "macos") { 13.0 } else { 11.0 };
        let size = px(points_to_px(points));
        let mut normal = font(spec.family.clone());
        if spec.bold {
            normal.weight = FontWeight::BOLD;
        }
        if spec.italic {
            normal.style = FontStyle::Italic;
        }
        let wide = pick(&ed.opt_global("guifontwide").as_string()).map(|w| font(w.family));
        // cell metrics from the normal font
        let id = ts.resolve_font(&normal);
        let adv = ts.advance(id, size, 'm').map(|s| s.width).unwrap_or(px(points_to_px(points) * 0.6));
        let ascent = ts.ascent(id, size);
        let descent = ts.descent(id, size);
        let h = f32::from(ascent) + f32::from(descent).abs();
        let linespace = ed.opt_global("linespace").as_num().max(0) as f32;
        let cell_h = px((h * 1.08).ceil() + linespace);
        FontSet { normal, wide, size, cell_w: adv, cell_h }
    }

    pub fn font_for(&self, attr: &Attr, wide: bool) -> Font {
        let mut f = if wide { self.wide.clone().unwrap_or_else(|| self.normal.clone()) } else { self.normal.clone() };
        if attr.bold {
            f.weight = FontWeight::BOLD;
        }
        if attr.italic {
            f.style = FontStyle::Italic;
        }
        f
    }

    /// Rows and columns that fit in `size`.
    pub fn grid_size(&self, size: Size<Pixels>) -> (usize, usize) {
        let rows = (f32::from(size.height) / f32::from(self.cell_h)).floor().max(3.0) as usize;
        let cols = (f32::from(size.width) / f32::from(self.cell_w)).floor().max(12.0) as usize;
        (rows, cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guifont_entries() {
        let s = parse_font("Consolas:h12").unwrap();
        assert_eq!((s.family.as_str(), s.points), ("Consolas", 12.0));
        let s = parse_font("Lucida_Console:h12:w5").unwrap();
        assert_eq!(s.family, "Lucida Console");
        let s = parse_font("MS_Gothic:h12:b").unwrap();
        assert_eq!((s.family.as_str(), s.bold), ("MS Gothic", true));
        let s = parse_font("DejaVu Sans Mono 13").unwrap();
        assert_eq!((s.family.as_str(), s.points), ("DejaVu Sans Mono", 13.0));
    }
}
