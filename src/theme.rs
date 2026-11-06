//! VS Code "Visual Studio Dark - C++" look: workbench palette plus a syntect
//! theme converted from the embedded cpptools token colours.

use ratatui::style::Color;
use std::str::FromStr;
use std::sync::OnceLock;
use syntect::highlighting::{
    Color as SColor, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};

/// `#RRGGBB` as a terminal truecolor.
const fn rgb(hex: u32) -> Color {
    Color::Rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// VS Code `#RRGGBBAA` composited over an opaque `#RRGGBB` background.
const fn blend(rgba: u32, bg: u32) -> Color {
    Color::Rgb(mix(rgba, bg, 16), mix(rgba, bg, 8), mix(rgba, bg, 0))
}

const fn mix(rgba: u32, bg: u32, shift: u32) -> u8 {
    let a = rgba & 0xff;
    let f = (rgba >> (shift + 8)) & 0xff;
    let b = (bg >> shift) & 0xff;
    ((f * a + b * (255 - a)) / 255) as u8
}

/// Workbench colours. Values come from cpptools_dark_vs_new.json where it sets
/// them, else from VS Code's dark_vs.json, else VS Code's built-in dark defaults.
pub struct Pal {
    pub editor_bg: Color,
    pub editor_fg: Color,
    pub line_nr: Color,
    pub line_nr_active: Color,
    pub cur_line: Color,
    pub selection: Color,
    pub sel_inactive: Color,
    pub find_match: Color,
    pub find_other: Color,
    pub activity_bg: Color,
    pub activity_fg: Color,
    pub activity_inactive: Color,
    pub sidebar_bg: Color,
    pub sidebar_fg: Color,
    pub sidebar_title: Color,
    pub dim: Color,
    pub tabs_bg: Color,
    pub tab_active_bg: Color,
    pub tab_active_fg: Color,
    pub tab_unfocused_fg: Color,
    pub tab_inactive_bg: Color,
    pub tab_inactive_fg: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub list_active_bg: Color,
    pub list_inactive_bg: Color,
    pub list_hover_bg: Color,
    pub input_bg: Color,
    pub input_fg: Color,
    pub placeholder: Color,
    pub button_bg: Color,
    pub button_fg: Color,
    pub widget_bg: Color,
    pub widget_border: Color,
    pub focus_border: Color,
    pub drop_bg: Color,
    pub title_bg: Color,
    pub git_modified: Color,
    pub git_untracked: Color,
    pub git_deleted: Color,
    pub git_conflict: Color,
    pub diff_ins_bg: Color,
    pub diff_del_bg: Color,
    pub diff_filler: Color,
}

pub const P: Pal = Pal {
    editor_bg: rgb(0x1E1E1E),
    editor_fg: rgb(0xDADADA),
    line_nr: rgb(0x2B91AF),
    line_nr_active: rgb(0x2B91AF),
    cur_line: rgb(0x282828),
    selection: rgb(0x264F78),
    sel_inactive: rgb(0x3A3D41),
    find_match: rgb(0x515C6A),
    find_other: blend(0xEA5C0055, 0x1E1E1E),
    activity_bg: rgb(0x333333),
    activity_fg: rgb(0xFFFFFF),
    activity_inactive: blend(0xFFFFFF66, 0x333333),
    sidebar_bg: rgb(0x252526),
    sidebar_fg: rgb(0xCCCCCC),
    sidebar_title: rgb(0xBBBBBB),
    dim: blend(0xCCCCCCB3, 0x252526),
    tabs_bg: rgb(0x252526),
    tab_active_bg: rgb(0x1E1E1E),
    tab_active_fg: rgb(0xFFFFFF),
    tab_unfocused_fg: blend(0xFFFFFF80, 0x1E1E1E),
    tab_inactive_bg: rgb(0x2D2D2D),
    tab_inactive_fg: rgb(0xA6A6A6),
    status_bg: rgb(0x007ACC),
    status_fg: rgb(0xFFFFFF),
    list_active_bg: rgb(0x04395E),
    list_inactive_bg: rgb(0x37373D),
    list_hover_bg: rgb(0x2A2D2E),
    input_bg: rgb(0x3C3C3C),
    input_fg: rgb(0xCCCCCC),
    placeholder: rgb(0xA6A6A6),
    button_bg: rgb(0x0E639C),
    button_fg: rgb(0xFFFFFF),
    widget_bg: rgb(0x252526),
    widget_border: rgb(0x454545),
    focus_border: rgb(0x007FD4),
    drop_bg: rgb(0x383B3D),
    title_bg: rgb(0x3C3C3C),
    git_modified: rgb(0xE2C08D),
    git_untracked: rgb(0x73C991),
    git_deleted: rgb(0xC74E39),
    git_conflict: rgb(0xE4676B),
    diff_ins_bg: blend(0x9BB95533, 0x1E1E1E),
    diff_del_bg: blend(0xFF000033, 0x1E1E1E),
    diff_filler: rgb(0x3A3A3A),
};

/// UI symbols. Codicons sit at VS Code's own codepoints (Nerd Font `nf-cod-*`,
/// rendered through fontconfig fallback); every symbol is one cell wide.
pub struct Icons {
    pub files: &'static str,
    pub search: &'static str,
    pub scm: &'static str,
    pub history: &'static str,
    pub chev_right: &'static str,
    pub chev_down: &'static str,
    pub folder: &'static str,
    pub folder_open: &'static str,
    pub file: &'static str,
    pub close: &'static str,
    pub dirty: &'static str,
    pub split: &'static str,
    pub check: &'static str,
    pub add: &'static str,
    pub remove: &'static str,
    pub discard: &'static str,
    pub open_file: &'static str,
    pub refresh: &'static str,
    pub case: &'static str,
    pub regex: &'static str,
    pub word: &'static str,
    pub up: &'static str,
    pub down: &'static str,
    pub replace: &'static str,
    pub replace_all: &'static str,
    pub branch: &'static str,
    pub lock: &'static str,
}

/// Codepoints taken from VS Code's codicon registry (`codiconsLibrary`).
const CODICONS: Icons = Icons {
    files: "\u{eaf0}",
    search: "\u{ea6d}",
    scm: "\u{ea68}",
    history: "\u{ea82}",
    chev_right: "\u{eab6}",
    chev_down: "\u{eab4}",
    folder: "\u{ea83}",
    folder_open: "\u{eaf7}",
    file: "\u{ea7b}",
    close: "\u{ea76}",
    dirty: "\u{ea71}",
    split: "\u{eb56}",
    check: "\u{eab2}",
    add: "\u{ea60}",
    remove: "\u{eb3b}",
    discard: "\u{eae2}",
    open_file: "\u{ea94}",
    refresh: "\u{eb37}",
    case: "\u{eab1}",
    regex: "\u{eb38}",
    word: "\u{eb7e}",
    up: "\u{eaa1}",
    down: "\u{ea9a}",
    replace: "\u{eb3d}",
    replace_all: "\u{eb3c}",
    branch: "\u{ea68}",
    lock: "\u{ea75}",
};

/// HERDR_CODE_ICONS=0: ASCII and narrow Unicode only (no East Asian ambiguous widths).
const PLAIN: Icons = Icons {
    files: "F",
    search: "S",
    scm: "G",
    history: "H",
    chev_right: ">",
    chev_down: "v",
    folder: " ",
    folder_open: " ",
    file: " ",
    close: "x",
    dirty: "*",
    split: "|",
    check: "+",
    add: "+",
    remove: "-",
    discard: "u",
    open_file: "o",
    refresh: "r",
    case: "c",
    regex: "*",
    word: "w",
    up: "^",
    down: "v",
    replace: "R",
    replace_all: "A",
    branch: "",
    lock: "!",
};

pub fn icons() -> &'static Icons {
    static I: OnceLock<bool> = OnceLock::new();
    if *I.get_or_init(|| std::env::var("HERDR_CODE_ICONS").as_deref() == Ok("0")) {
        &PLAIN
    } else {
        &CODICONS
    }
}

/// VS Code colour for a git status letter.
pub fn git_color(c: char) -> Color {
    match c {
        'M' | 'R' | 'C' | 'T' => P.git_modified,
        'U' | 'A' | '?' => P.git_untracked,
        'D' => P.git_deleted,
        '!' => P.git_conflict,
        _ => P.sidebar_fg,
    }
}

/// Drop `//` and `/* */` comments (outside strings) so VS Code's JSONC parses.
fn strip_comments(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            in_str = c == b'"';
            out.push(c);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// JSONC (comments, trailing commas) to plain JSON.
pub fn jsonc_to_json(s: &str) -> String {
    let s = strip_comments(s);
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut in_str = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 1;
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b',' {
            let next = b[i + 1..].iter().find(|c| !c.is_ascii_whitespace());
            if !matches!(next, Some(b']' | b'}')) {
                out.push(c);
            }
        } else {
            in_str = c == b'"';
            out.push(c);
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

fn scolor(hex: &str) -> Option<SColor> {
    let h = hex.strip_prefix('#')?;
    let v = u32::from_str_radix(h.get(..6)?, 16).ok()?;
    Some(SColor { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8, a: 0xff })
}

/// Convert VS Code `tokenColors` into a syntect theme.
pub fn convert(theme_jsonc: &str) -> Theme {
    let v: serde_json::Value = serde_json::from_str(&jsonc_to_json(theme_jsonc)).unwrap_or_default();
    let mut t = Theme {
        name: v["name"].as_str().map(String::from),
        settings: ThemeSettings {
            foreground: scolor("#DADADA"),
            background: scolor("#1E1E1E"),
            ..Default::default()
        },
        ..Default::default()
    };
    for rule in v["tokenColors"].as_array().into_iter().flatten() {
        let scope = match &rule["scope"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(a) => a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            _ => continue,
        };
        let Ok(sel) = ScopeSelectors::from_str(&scope) else { continue };
        let s = &rule["settings"];
        let font_style = s["fontStyle"].as_str().map(|fs| {
            let mut f = FontStyle::empty();
            for w in fs.split_whitespace() {
                match w {
                    "bold" => f |= FontStyle::BOLD,
                    "italic" => f |= FontStyle::ITALIC,
                    "underline" => f |= FontStyle::UNDERLINE,
                    _ => {}
                }
            }
            f
        });
        t.scopes.push(ThemeItem {
            scope: sel,
            style: StyleModifier { foreground: s["foreground"].as_str().and_then(scolor), background: None, font_style },
        });
    }
    // VS Code lets the later of two equally specific rules win; syntect keeps the
    // first, so hand it the rules in reverse.
    t.scopes.reverse();
    t
}

pub fn syntax_theme() -> &'static Theme {
    static T: OnceLock<Theme> = OnceLock::new();
    T.get_or_init(|| convert(include_str!("../assets/cpptools_dark_vs_new.json")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::highlighting::Highlighter;
    use syntect::parsing::{Scope, ScopeStack};

    #[test]
    fn jsonc_strips_comments_and_trailing_commas() {
        let j = jsonc_to_json("{\"a\": \"x//y\", // c\n \"b\": [1, 2, /* z */ ],\n}");
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["a"], "x//y");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
    }

    #[test]
    fn embedded_theme_maps_vscode_scopes() {
        let t = syntax_theme();
        assert!(t.scopes.len() > 50, "{}", t.scopes.len());
        let h = Highlighter::new(t);
        let fg = |scope: &str| {
            let st = ScopeStack::from_vec(vec![Scope::new("source.c").unwrap(), Scope::new(scope).unwrap()]);
            let c = h.style_for_stack(st.as_slice()).foreground;
            (c.r, c.g, c.b)
        };
        assert_eq!(fg("comment.line.double-slash"), (0x57, 0xA6, 0x4A));
        assert_eq!(fg("string.quoted.double"), (0xCE, 0x91, 0x78));
        assert_eq!(fg("entity.name.function"), (0xDC, 0xDC, 0xAA));
        assert_eq!(fg("keyword.control"), (0xD8, 0xA0, 0xDF));
    }

    #[test]
    fn blend_matches_vscode_alpha() {
        assert_eq!(blend(0xFFFFFF80, 0x000000), Color::Rgb(128, 128, 128));
        assert_eq!(blend(0x12345600, 0xABCDEF), Color::Rgb(0xAB, 0xCD, 0xEF));
    }
}
