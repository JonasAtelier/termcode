//! Explorer file icons: VS Code's default file icon theme (Seti) mapped to the
//! Nerd Font's `nf-seti-*` glyphs (assets/seti.json, from `cargo run --example gen_seti`),
//! with per-name overrides from `herdr-code.fileIcons`.

use crate::theme::{icons, Rgba, P};
use ratatui::style::Color;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::OnceLock;

/// A user override; either half may be left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Over {
    pub glyph: Option<String>,
    pub color: Option<Rgba>,
}

impl Over {
    pub fn parse(v: &Value) -> Over {
        Over { glyph: v["glyph"].as_str().and_then(glyph), color: v["color"].as_str().and_then(Rgba::parse) }
    }
}

/// A user glyph as one cell: its first non-control char, if that is exactly one
/// column wide (a wide or zero-width one would shift the row; the default stays).
fn glyph(s: &str) -> Option<String> {
    let c = s.chars().find(|c| !c.is_control())?;
    (unicode_width::UnicodeWidthChar::width(c) == Some(1)).then(|| c.to_string())
}

/// `herdr-code.fileIcons`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileIcons {
    pub extensions: HashMap<String, Over>,
    pub file_names: HashMap<String, Over>,
    pub folder_names: HashMap<String, Over>,
    pub folder: Over,
    pub folder_open: Over,
}

impl FileIcons {
    pub fn parse(v: &Value) -> FileIcons {
        let map = |k: &str| {
            v[k].as_object()
                .into_iter()
                .flatten()
                .map(|(name, o)| (name.trim_start_matches('.').to_lowercase(), Over::parse(o)))
                .collect()
        };
        FileIcons {
            extensions: map("extensions"),
            file_names: map("fileNames"),
            folder_names: map("folderNames"),
            folder: Over::parse(&v["folder"]),
            folder_open: Over::parse(&v["folderOpen"]),
        }
    }
}

struct Seti {
    default: usize,
    names: HashMap<String, usize>,
    exts: HashMap<String, usize>,
    lnames: HashMap<String, usize>,
    lexts: HashMap<String, usize>,
    icons: Vec<(String, Option<Rgba>)>,
}

fn seti() -> &'static Seti {
    static S: OnceLock<Seti> = OnceLock::new();
    S.get_or_init(|| {
        let v: Value = serde_json::from_str(include_str!("../assets/seti.json")).unwrap_or_default();
        let map = |k: &str| {
            v[k].as_object()
                .into_iter()
                .flatten()
                .filter_map(|(n, i)| Some((n.clone(), i.as_u64()? as usize)))
                .collect()
        };
        Seti {
            default: v["default"].as_u64().unwrap_or(0) as usize,
            names: map("names"),
            exts: map("exts"),
            lnames: map("lnames"),
            lexts: map("lexts"),
            icons: v["icons"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|i| (i[0].as_str().unwrap_or(" ").to_string(), i[1].as_str().and_then(Rgba::parse)))
                .collect(),
        }
    })
}

/// Extension candidates, longest first: "a.spec.ts" -> ["spec.ts", "ts"].
fn exts(lower: &str) -> impl Iterator<Item = &str> {
    lower.char_indices().filter(|(_, c)| *c == '.').map(move |(i, _)| &lower[i + 1..]).filter(|e| !e.is_empty())
}

fn color(c: Option<Rgba>) -> Color {
    c.map_or(P.icon_fg, |c| c.over(P.sidebar_bg))
}

/// Glyph and colour for a file, in VS Code's order: file name, extension
/// (longest first), then the language the name or extension maps to.
pub fn file_icon(name: &str, o: &FileIcons) -> (String, Color) {
    if crate::theme::ascii() {
        return (icons().file.to_string(), P.icon_fg);
    }
    let s = seti();
    let lower = name.to_lowercase();
    let found = s
        .names
        .get(&lower)
        .or_else(|| exts(&lower).find_map(|e| s.exts.get(e)))
        .or_else(|| s.lnames.get(&lower))
        .or_else(|| exts(&lower).find_map(|e| s.lexts.get(e)))
        .copied()
        .unwrap_or(s.default);
    let (glyph, c) = s.icons.get(found).cloned().unwrap_or((" ".into(), None));
    let user = o.file_names.get(&lower).or_else(|| exts(&lower).find_map(|e| o.extensions.get(e)));
    match user {
        Some(u) => (u.glyph.clone().unwrap_or(glyph), color(u.color.or(c))),
        None => (glyph, color(c)),
    }
}

pub fn folder_icon(name: &str, open: bool, o: &FileIcons) -> (String, Color) {
    let ic = icons();
    let base = if open { &o.folder_open } else { &o.folder };
    let named = o.folder_names.get(&name.to_lowercase());
    let glyph = named.and_then(|n| n.glyph.clone()).or_else(|| base.glyph.clone());
    let glyph = match glyph {
        Some(g) if !crate::theme::ascii() => g,
        _ => (if open { ic.folder_open } else { ic.folder }).to_string(),
    };
    (glyph, color(named.and_then(|n| n.color).or(base.color)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp(name: &str) -> (u32, Color) {
        let (g, c) = file_icon(name, &FileIcons::default());
        (g.chars().next().unwrap() as u32, c)
    }

    #[test]
    fn seti_mapping_matches_vscode_defaults() {
        let rgb = |h: u32| Rgba((h >> 16) as u8, (h >> 8) as u8, h as u8, 255).over(Color::Rgb(0, 0, 0));
        // nf-seti-c, blue for .c and purple for .h (Seti's _c / _c_1).
        assert_eq!(cp("main.c"), (0xE649, rgb(0x519ABA)));
        assert_eq!(cp("mxl.h"), (0xE649, rgb(0xA074C4)));
        assert_eq!(cp("lib.rs"), (0xE68B, rgb(0x6D8086)));
        assert_eq!(cp("NOTES.md"), (0xE609, rgb(0x519ABA)));
        // nf-seti-json shares its glyph with seti-less (U+E60B).
        assert_eq!(cp("package.json"), (0xE60B, rgb(0xCBCB41)));
        // Makefile: no Seti file name; the makefile language's file name.
        assert_eq!(cp("Makefile"), (0xE673, rgb(0xE37933)));
        assert_eq!(cp("README.md").0, 0xE66A, "file name beats extension");
    }

    #[test]
    fn user_overrides_replace_glyph_or_colour() {
        let v: Value = serde_json::json!({
            "extensions": { "c": { "color": "#ff0000" } },
            "fileNames": { "Makefile": { "glyph": "M" } },
            "folder": { "glyph": "D", "color": "#00ff00" },
        });
        let o = FileIcons::parse(&v);
        let (g, c) = file_icon("x.c", &o);
        assert_eq!((g.as_str(), c), ("\u{e649}", Color::Rgb(255, 0, 0)));
        assert_eq!(file_icon("makefile", &o).0, "M");
        assert_eq!(folder_icon("src", false, &o), ("D".to_string(), Color::Rgb(0, 255, 0)));
        // One cell only: control chars are dropped, a wide glyph falls back to Seti's.
        let o = FileIcons::parse(&serde_json::json!({ "extensions": { "c": { "glyph": "\u{1b}[31mX" }, "h": { "glyph": "中" } } }));
        assert_eq!(file_icon("x.c", &o).0, "[");
        assert_eq!(file_icon("x.h", &o).0, "\u{e649}");
    }
}
