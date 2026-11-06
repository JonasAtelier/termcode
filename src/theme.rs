//! Colours and icons. Every workbench colour is a VS Code colour key, resolved
//! like VS Code does: the active theme's `colors` (plus
//! `workbench.colorCustomizations`), else VS Code's dark colour-registry default
//! (assets/vscode_dark_defaults.json). Token colours become a syntect theme.
//! A settings reload swaps both at runtime (`install`).

use crate::semantic::{Sem, SEMS};
use ratatui::style::Color;
use serde_json::{Map, Value};
use std::ops::Deref;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use syntect::highlighting::{
    Color as SColor, FontStyle, Highlighter, ScopeSelectors, Style as SStyle, StyleModifier, Theme, ThemeItem,
    ThemeSettings,
};
use syntect::parsing::Scope;

/// A VS Code colour with alpha.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    /// `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, or CSS `rgb(r,g,b)` / `rgba(r,g,b,a)`.
    pub fn parse(s: &str) -> Option<Rgba> {
        let s = s.trim();
        if let Some(h) = s.strip_prefix('#') {
            let n = |i: usize, w: usize| u8::from_str_radix(h.get(i..i + w)?, 16).ok();
            let d = |i: usize| n(i, 1).map(|v| v * 17);
            return match h.len() {
                3 => Some(Rgba(d(0)?, d(1)?, d(2)?, 255)),
                4 => Some(Rgba(d(0)?, d(1)?, d(2)?, d(3)?)),
                6 => Some(Rgba(n(0, 2)?, n(2, 2)?, n(4, 2)?, 255)),
                8 => Some(Rgba(n(0, 2)?, n(2, 2)?, n(4, 2)?, n(6, 2)?)),
                _ => None,
            };
        }
        let inner = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb("))?.strip_suffix(')')?;
        let p: Vec<&str> = inner.split(',').map(str::trim).collect();
        if p.len() != 3 && p.len() != 4 {
            return None;
        }
        let c = |i: usize| p[i].parse::<f32>().ok().map(|v| v.clamp(0.0, 255.0).round() as u8);
        let a = match p.get(3) {
            Some(a) => (a.parse::<f32>().ok()?.clamp(0.0, 1.0) * 255.0).round() as u8,
            None => 255,
        };
        Some(Rgba(c(0)?, c(1)?, c(2)?, a))
    }

    /// VS Code's `transparent(f)`: scale the alpha.
    pub fn alpha(self, f: f64) -> Rgba {
        Rgba(self.0, self.1, self.2, (self.3 as f64 * f).round().clamp(0.0, 255.0) as u8)
    }

    /// Composite over an opaque background (a terminal cell has no alpha).
    pub fn over(self, bg: Color) -> Color {
        let (br, bgr, bb) = match bg {
            Color::Rgb(r, g, b) => (r, g, b),
            _ => (0, 0, 0),
        };
        let a = self.3 as u32;
        let m = |f: u8, b: u8| ((f as u32 * a + b as u32 * (255 - a)) / 255) as u8;
        Color::Rgb(m(self.0, br), m(self.1, bgr), m(self.2, bb))
    }

    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
}

fn defaults() -> &'static Map<String, Value> {
    static D: OnceLock<Map<String, Value>> = OnceLock::new();
    D.get_or_init(|| serde_json::from_str(include_str!("../assets/vscode_dark_defaults.json")).unwrap_or_default())
}

/// Resolve a colour key: theme (with customizations) first, then the registry
/// default, which may name another key (again theme first) or scale its alpha.
pub fn lookup(theme: &Map<String, Value>, key: &str) -> Option<Rgba> {
    fn go(theme: &Map<String, Value>, key: &str, depth: u8) -> Option<Rgba> {
        if depth > 8 {
            return None;
        }
        if let Some(c) = theme.get(key).and_then(Value::as_str).and_then(Rgba::parse) {
            return Some(c);
        }
        match defaults().get(key)? {
            Value::String(s) if s.starts_with('#') => Rgba::parse(s),
            Value::String(k) => go(theme, k, depth + 1),
            Value::Array(a) => Some(go(theme, a.first()?.as_str()?, depth + 1)?.alpha(a.get(1)?.as_f64()?)),
            _ => None,
        }
    }
    go(theme, key, 0)
}

/// Workbench colours, opaque (alpha already composited over what each is drawn on).
/// The VS Code key behind every field is in `build_pal_checked`.
#[derive(Clone, Copy, Debug)]
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
    pub cursor: Color,
    pub guide: Color,
    pub guide_active: Color,
    pub ruler: Color,
    pub gutter_added: Color,
    pub gutter_modified: Color,
    pub gutter_deleted: Color,
    pub ruler_added: Color,
    pub ruler_modified: Color,
    pub ruler_deleted: Color,
    pub ruler_find: Color,
    pub slider: Rgba,
    pub slider_hover: Rgba,
    pub slider_active: Rgba,
    pub activity_bg: Color,
    pub activity_fg: Color,
    pub activity_inactive: Color,
    pub activity_border: Color,
    pub activity_badge_bg: Color,
    pub activity_badge_fg: Color,
    pub sidebar_bg: Color,
    pub sidebar_fg: Color,
    pub sidebar_title: Color,
    pub section_bg: Color,
    pub section_fg: Color,
    pub dim: Color,
    pub icon_fg: Color,
    pub tabs_bg: Color,
    pub tab_active_bg: Color,
    pub tab_active_fg: Color,
    pub tab_unfocused_fg: Color,
    pub tab_inactive_bg: Color,
    pub tab_inactive_fg: Color,
    pub tab_border: Color,
    pub group_border: Color,
    pub drop_bg: Color,
    pub status_bg: Color,
    pub status_fg: Color,
    pub status_err_bg: Color,
    pub status_err_fg: Color,
    pub title_bg: Color,
    pub title_fg: Color,
    pub list_active_bg: Color,
    pub list_inactive_bg: Color,
    pub list_hover_bg: Color,
    /// Sticky scroll rows over the sidebar lists, and the line under them.
    pub sticky_bg: Color,
    pub sticky_shadow: Color,
    pub input_bg: Color,
    pub input_fg: Color,
    pub placeholder: Color,
    pub option_bg: Color,
    pub option_fg: Color,
    pub button_bg: Color,
    pub button_fg: Color,
    pub button2_bg: Color,
    pub button2_fg: Color,
    pub widget_bg: Color,
    pub widget_fg: Color,
    pub widget_border: Color,
    /// `sash.hoverBorder`: a splitter under the mouse or being dragged.
    pub sash_hover: Color,
    pub focus_border: Color,
    pub quick_bg: Color,
    pub quick_fg: Color,
    pub quick_sel_bg: Color,
    pub quick_sel_fg: Color,
    pub badge_bg: Color,
    pub badge_fg: Color,
    pub error_fg: Color,
    pub git_added: Color,
    pub git_modified: Color,
    pub git_deleted: Color,
    pub git_renamed: Color,
    pub git_untracked: Color,
    pub git_conflict: Color,
    pub diff_ins_bg: Color,
    pub diff_del_bg: Color,
    pub diff_filler: Color,
    pub diff_ins_line: Color,
    pub diff_del_line: Color,
    /// Word highlights, composited over the line backgrounds when drawn.
    pub diff_ins_text: Rgba,
    pub diff_del_text: Rgba,
    pub mini_bg: Color,
    /// `minimap.foregroundOpacity`'s alpha: scales the token colours.
    pub mini_fg_opacity: u8,
    pub mini_find: Color,
    pub mini_sel: Color,
    pub mini_added: Color,
    pub mini_modified: Color,
    pub mini_deleted: Color,
    pub mini_slider: Rgba,
    pub mini_slider_hover: Rgba,
    pub mini_slider_active: Rgba,
    pub peek_border: Color,
    pub peek_title_bg: Color,
    pub peek_title_fg: Color,
    pub peek_desc_fg: Color,
}

/// Build the palette; also returns the keys that resolved nowhere (a test keeps it empty).
pub fn build_pal_checked(t: &Map<String, Value>) -> (Pal, Vec<&'static str>) {
    let missing = std::cell::RefCell::new(Vec::new());
    let raw = |k: &'static str| {
        lookup(t, k).unwrap_or_else(|| {
            missing.borrow_mut().push(k);
            Rgba(255, 0, 255, 255)
        })
    };
    let black = Color::Rgb(0, 0, 0);
    let c = |k| raw(k).over(black);
    let on = |k, bg: Color| raw(k).over(bg);
    let editor_bg = c("editor.background");
    let activity_bg = c("activityBar.background");
    let sidebar_bg = c("sideBar.background");
    let tab_active_bg = on("tab.activeBackground", editor_bg);
    let tab_inactive_bg = on("tab.inactiveBackground", sidebar_bg);
    let input_bg = on("input.background", sidebar_bg);
    let widget_bg = on("editorWidget.background", editor_bg);
    let quick_bg = on("quickInput.background", editor_bg);
    let status_bg = c("statusBar.background");
    let title_bg = c("titleBar.activeBackground");
    let pal = Pal {
        editor_bg,
        editor_fg: on("editor.foreground", editor_bg),
        line_nr: on("editorLineNumber.foreground", editor_bg),
        line_nr_active: on("editorLineNumber.activeForeground", editor_bg),
        cur_line: on("editor.lineHighlightBackground", editor_bg),
        selection: on("editor.selectionBackground", editor_bg),
        sel_inactive: on("editor.inactiveSelectionBackground", editor_bg),
        find_match: on("editor.findMatchBackground", editor_bg),
        find_other: on("editor.findMatchHighlightBackground", editor_bg),
        cursor: on("editorCursor.foreground", editor_bg),
        guide: on("editorIndentGuide.background", editor_bg),
        guide_active: on("editorIndentGuide.activeBackground", editor_bg),
        ruler: on("editorRuler.foreground", editor_bg),
        gutter_added: on("editorGutter.addedBackground", editor_bg),
        gutter_modified: on("editorGutter.modifiedBackground", editor_bg),
        gutter_deleted: on("editorGutter.deletedBackground", editor_bg),
        ruler_added: on("editorOverviewRuler.addedForeground", editor_bg),
        ruler_modified: on("editorOverviewRuler.modifiedForeground", editor_bg),
        ruler_deleted: on("editorOverviewRuler.deletedForeground", editor_bg),
        ruler_find: on("editorOverviewRuler.findMatchForeground", editor_bg),
        slider: raw("scrollbarSlider.background"),
        slider_hover: raw("scrollbarSlider.hoverBackground"),
        slider_active: raw("scrollbarSlider.activeBackground"),
        activity_bg,
        activity_fg: on("activityBar.foreground", activity_bg),
        activity_inactive: on("activityBar.inactiveForeground", activity_bg),
        activity_border: on("activityBar.activeBorder", activity_bg),
        activity_badge_bg: on("activityBarBadge.background", activity_bg),
        activity_badge_fg: on("activityBarBadge.foreground", on("activityBarBadge.background", activity_bg)),
        sidebar_bg,
        sidebar_fg: on("sideBar.foreground", sidebar_bg),
        sidebar_title: on("sideBarTitle.foreground", sidebar_bg),
        section_bg: on("sideBarSectionHeader.background", sidebar_bg),
        section_fg: on("sideBarSectionHeader.foreground", sidebar_bg),
        dim: on("descriptionForeground", sidebar_bg),
        icon_fg: on("icon.foreground", sidebar_bg),
        tabs_bg: on("editorGroupHeader.tabsBackground", sidebar_bg),
        tab_active_bg,
        tab_active_fg: on("tab.activeForeground", tab_active_bg),
        tab_unfocused_fg: on("tab.unfocusedActiveForeground", tab_active_bg),
        tab_inactive_bg,
        tab_inactive_fg: on("tab.inactiveForeground", tab_inactive_bg),
        tab_border: on("tab.border", sidebar_bg),
        group_border: on("editorGroup.border", editor_bg),
        drop_bg: on("editorGroup.dropBackground", editor_bg),
        status_bg,
        status_fg: on("statusBar.foreground", status_bg),
        status_err_bg: on("statusBarItem.errorBackground", status_bg),
        status_err_fg: on("statusBarItem.errorForeground", on("statusBarItem.errorBackground", status_bg)),
        title_bg,
        title_fg: on("titleBar.activeForeground", title_bg),
        list_active_bg: on("list.activeSelectionBackground", sidebar_bg),
        list_inactive_bg: on("list.inactiveSelectionBackground", sidebar_bg),
        list_hover_bg: on("list.hoverBackground", sidebar_bg),
        sticky_bg: on("sideBarStickyScroll.background", sidebar_bg),
        sticky_shadow: on("sideBarStickyScroll.shadow", sidebar_bg),
        input_bg,
        input_fg: on("input.foreground", input_bg),
        placeholder: on("input.placeholderForeground", input_bg),
        option_bg: on("inputOption.activeBackground", input_bg),
        option_fg: on("inputOption.activeForeground", on("inputOption.activeBackground", input_bg)),
        button_bg: on("button.background", sidebar_bg),
        button_fg: on("button.foreground", on("button.background", sidebar_bg)),
        button2_bg: on("button.secondaryBackground", widget_bg),
        button2_fg: on("button.secondaryForeground", on("button.secondaryBackground", widget_bg)),
        widget_bg,
        widget_fg: on("editorWidget.foreground", widget_bg),
        widget_border: on("editorWidget.border", widget_bg),
        sash_hover: on("sash.hoverBorder", widget_bg),
        focus_border: on("focusBorder", sidebar_bg),
        quick_bg,
        quick_fg: on("quickInput.foreground", quick_bg),
        quick_sel_bg: on("quickInputList.focusBackground", quick_bg),
        quick_sel_fg: on("quickInputList.focusForeground", on("quickInputList.focusBackground", quick_bg)),
        badge_bg: on("badge.background", sidebar_bg),
        badge_fg: on("badge.foreground", on("badge.background", sidebar_bg)),
        error_fg: on("errorForeground", status_bg),
        git_added: on("gitDecoration.addedResourceForeground", sidebar_bg),
        git_modified: on("gitDecoration.modifiedResourceForeground", sidebar_bg),
        git_deleted: on("gitDecoration.deletedResourceForeground", sidebar_bg),
        git_renamed: on("gitDecoration.renamedResourceForeground", sidebar_bg),
        git_untracked: on("gitDecoration.untrackedResourceForeground", sidebar_bg),
        git_conflict: on("gitDecoration.conflictingResourceForeground", sidebar_bg),
        diff_ins_bg: on("diffEditor.insertedTextBackground", editor_bg),
        diff_del_bg: on("diffEditor.removedTextBackground", editor_bg),
        diff_filler: on("diffEditor.diagonalFill", editor_bg),
        diff_ins_line: on("diffEditor.insertedLineBackground", editor_bg),
        diff_del_line: on("diffEditor.removedLineBackground", editor_bg),
        diff_ins_text: raw("diffEditor.insertedTextBackground"),
        diff_del_text: raw("diffEditor.removedTextBackground"),
        mini_bg: on("minimap.background", editor_bg),
        mini_fg_opacity: raw("minimap.foregroundOpacity").3,
        mini_find: on("minimap.findMatchHighlight", editor_bg),
        mini_sel: on("minimap.selectionHighlight", editor_bg),
        mini_added: on("minimapGutter.addedBackground", editor_bg),
        mini_modified: on("minimapGutter.modifiedBackground", editor_bg),
        mini_deleted: on("minimapGutter.deletedBackground", editor_bg),
        mini_slider: raw("minimapSlider.background"),
        mini_slider_hover: raw("minimapSlider.hoverBackground"),
        mini_slider_active: raw("minimapSlider.activeBackground"),
        peek_border: on("peekView.border", editor_bg),
        peek_title_bg: on("peekViewTitle.background", editor_bg),
        peek_title_fg: on("peekViewTitleLabel.foreground", on("peekViewTitle.background", editor_bg)),
        peek_desc_fg: on("peekViewTitleDescription.foreground", on("peekViewTitle.background", editor_bg)),
    };
    (pal, missing.into_inner())
}

pub fn build_pal(t: &Map<String, Value>) -> Pal {
    build_pal_checked(t).0
}

/// The embedded "Visual Studio Dark - C++" theme (cpptools, MIT).
pub fn embedded_theme() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str(&jsonc_to_json(include_str!("../assets/cpptools_dark_vs_new.json"))).unwrap_or_default()
    })
}

/// The live palette, read as `P.field`. Each install leaks one small `Pal` on
/// purpose: references handed out earlier stay valid without a lock.
pub struct LivePal;
pub static P: LivePal = LivePal;
static PAL: AtomicPtr<Pal> = AtomicPtr::new(std::ptr::null_mut());

impl Deref for LivePal {
    type Target = Pal;
    fn deref(&self) -> &Pal {
        let p = PAL.load(Ordering::Acquire);
        if p.is_null() {
            static D: OnceLock<Pal> = OnceLock::new();
            return D.get_or_init(|| build_pal(embedded_theme()["colors"].as_object().unwrap_or(&Map::new())));
        }
        // SAFETY: `p` came from Box::into_raw in `install` and is never freed.
        unsafe { &*p }
    }
}

/// Token colours as a syntect theme, plus the styles of the semantic classes.
pub struct Syntax {
    pub theme: Theme,
    pub sem: Vec<SStyle>,
    /// Bumped on every build; highlight caches built with another value are stale.
    pub gen: u64,
}

static GEN: AtomicU64 = AtomicU64::new(1);
static SYN: RwLock<Option<Arc<Syntax>>> = RwLock::new(None);

pub fn syntax() -> Arc<Syntax> {
    if let Some(s) = SYN.read().ok().and_then(|g| g.clone()) {
        return s;
    }
    let s = Arc::new(build_syntax(embedded_theme(), &[]));
    match SYN.write() {
        Ok(mut g) => g.get_or_insert(s).clone(),
        Err(_) => s,
    }
}

/// Swap the live palette and token theme (a settings reload).
pub fn install(pal: Pal, syn: Syntax) {
    PAL.store(Box::into_raw(Box::new(pal)), Ordering::Release);
    if let Ok(mut g) = SYN.write() {
        *g = Some(Arc::new(syn));
    }
}

/// Font style words of a VS Code rule.
fn font_style(fs: &str) -> FontStyle {
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
}

fn scolor(s: &str) -> Option<SColor> {
    let c = Rgba::parse(s)?;
    Some(SColor { r: c.0, g: c.1, b: c.2, a: 0xff })
}

/// A syntect theme from a VS Code theme (`colors`, `tokenColors`,
/// `semanticTokenColors`) and extra rules appended after the theme's own
/// (token colour customizations). Later rules win, as in VS Code.
pub fn build_syntax(theme: &Value, extra: &[Value]) -> Syntax {
    let colors = theme["colors"].as_object().cloned().unwrap_or_default();
    let fg = lookup(&colors, "editor.foreground").map(|c| c.hex()).unwrap_or_default();
    let bg = lookup(&colors, "editor.background").map(|c| c.hex()).unwrap_or_default();
    let mut t = Theme {
        name: theme["name"].as_str().map(String::from),
        settings: ThemeSettings { foreground: scolor(&fg), background: scolor(&bg), ..Default::default() },
        ..Default::default()
    };
    let rules = theme["tokenColors"].as_array().into_iter().flatten().chain(extra);
    for rule in rules {
        let scope = match &rule["scope"] {
            Value::String(s) => s.clone(),
            Value::Array(a) => a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            _ => continue,
        };
        let Ok(sel) = ScopeSelectors::from_str(&scope) else { continue };
        let s = &rule["settings"];
        t.scopes.push(ThemeItem {
            scope: sel,
            style: StyleModifier {
                foreground: s["foreground"].as_str().and_then(scolor),
                background: None,
                font_style: s["fontStyle"].as_str().map(font_style),
            },
        });
    }
    // VS Code lets the later of two equally specific rules win; syntect keeps the
    // first, so hand it the rules in reverse.
    t.scopes.reverse();
    let sem = {
        let h = Highlighter::new(&t);
        SEMS.iter().map(|s| sem_style(&h, &theme["semanticTokenColors"], *s)).collect()
    };
    Syntax { theme: t, sem, gen: GEN.fetch_add(1, Ordering::Relaxed) + 1 }
}

/// A semantic class: the theme's `semanticTokenColors` entry if it has one,
/// else its TextMate scope through the token rules (VS Code's fallback).
fn sem_style(h: &Highlighter, sem_colors: &Value, s: Sem) -> SStyle {
    let mut st = match Scope::new(s.scope()) {
        Ok(sc) => h.style_for_stack(&[sc]),
        Err(_) => h.get_default(),
    };
    match &sem_colors[s.token()] {
        Value::String(c) => st.foreground = scolor(c).unwrap_or(st.foreground),
        Value::Object(o) => {
            if let Some(c) = o.get("foreground").and_then(Value::as_str).and_then(scolor) {
                st.foreground = c;
            }
            for (k, f) in [("bold", FontStyle::BOLD), ("italic", FontStyle::ITALIC), ("underline", FontStyle::UNDERLINE)] {
                if let Some(on) = o.get(k).and_then(Value::as_bool) {
                    st.font_style.set(f, on);
                }
            }
        }
        _ => {}
    }
    st
}

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
    /// Folder holding changes (right-hand decoration).
    pub folder_dot: &'static str,
    /// Quick-diff gutter: added/modified bar, deleted marker.
    pub gutter_bar: &'static str,
    pub gutter_del: &'static str,
    pub guide: &'static str,
    /// Overview ruler lanes: git (left), find (right), cursor line.
    pub lane_left: &'static str,
    pub lane_right: &'static str,
    pub lane_cursor: &'static str,
    /// Popup border: top-left, top-right, bottom-left, bottom-right, horizontal, vertical.
    pub border: [&'static str; 6],
}

/// Codepoints taken from VS Code's codicon registry (`codiconsLibrary`); the
/// folders are Nerd Font `nf-custom-folder(_open)`, matching the Seti file icons.
const CODICONS: Icons = Icons {
    files: "\u{eaf0}",
    search: "\u{ea6d}",
    scm: "\u{ea68}",
    history: "\u{ea82}",
    chev_right: "\u{eab6}",
    chev_down: "\u{eab4}",
    folder: "\u{e5ff}",
    folder_open: "\u{e5fe}",
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
    folder_dot: "●",
    gutter_bar: "▎",
    gutter_del: "▸",
    guide: "▏",
    lane_left: "▌",
    lane_right: "▐",
    lane_cursor: "─",
    border: ["╭", "╮", "╰", "╯", "─", "│"],
};

/// `herdr-code.icons: "ascii"` or HERDR_CODE_ICONS=0: ASCII and narrow Unicode
/// only (no East Asian ambiguous widths).
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
    folder_dot: "*",
    gutter_bar: "|",
    gutter_del: "_",
    guide: "|",
    lane_left: "-",
    lane_right: "=",
    lane_cursor: "-",
    border: ["+", "+", "+", "+", "-", "|"],
};

static ASCII: AtomicBool = AtomicBool::new(false);

/// `herdr-code.icons`; HERDR_CODE_ICONS=0 always wins.
pub fn set_ascii(on: bool) {
    ASCII.store(on, Ordering::Relaxed);
}

pub fn ascii() -> bool {
    static ENV: OnceLock<bool> = OnceLock::new();
    *ENV.get_or_init(|| std::env::var("HERDR_CODE_ICONS").as_deref() == Ok("0")) || ASCII.load(Ordering::Relaxed)
}

pub fn icons() -> &'static Icons {
    if ascii() {
        &PLAIN
    } else {
        &CODICONS
    }
}

/// VS Code colour for a git status letter.
pub fn git_color(c: char) -> Color {
    match c {
        'M' | 'T' => P.git_modified,
        'R' => P.git_renamed,
        'A' | 'C' => P.git_added,
        'U' | '?' => P.git_untracked,
        'D' => P.git_deleted,
        '!' => P.git_conflict,
        _ => P.sidebar_fg,
    }
}

/// Drop `//` and `/* */` comments (outside strings) so VS Code's JSONC parses.
/// Newlines inside block comments are kept so error line numbers stay right.
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
                if b[i] == b'\n' {
                    out.push(b'\n');
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use syntect::parsing::{Scope, ScopeStack};

    #[test]
    fn jsonc_strips_comments_and_trailing_commas() {
        let j = jsonc_to_json("{\"a\": \"x//y\", // c\n \"b\": [1, 2, /* z\n */ ],\n}");
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["a"], "x//y");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        assert_eq!(j.matches('\n').count(), 3, "block comment newlines kept for error lines");
    }

    #[test]
    fn embedded_theme_maps_vscode_scopes() {
        let syn = build_syntax(embedded_theme(), &[]);
        assert!(syn.theme.scopes.len() > 50, "{}", syn.theme.scopes.len());
        let h = Highlighter::new(&syn.theme);
        let fg = |scope: &str| {
            let st = ScopeStack::from_vec(vec![Scope::new("source.c").unwrap(), Scope::new(scope).unwrap()]);
            let c = h.style_for_stack(st.as_slice()).foreground;
            (c.r, c.g, c.b)
        };
        assert_eq!(fg("comment.line.double-slash"), (0x57, 0xA6, 0x4A));
        assert_eq!(fg("string.quoted.double"), (0xCE, 0x91, 0x78));
        assert_eq!(fg("entity.name.function"), (0xDC, 0xDC, 0xAA));
        assert_eq!(fg("keyword.control"), (0xD8, 0xA0, 0xDF));
        assert_eq!(fg("keyword.control.directive.include.c"), (0x9A, 0x9A, 0x9A));
        let sem = |s: Sem| {
            let c = syn.sem[s as usize].foreground;
            (c.r, c.g, c.b)
        };
        assert_eq!(sem(Sem::Macro), (0xBE, 0xB7, 0xFF));
        assert_eq!(sem(Sem::Parameter), (0x9A, 0x9A, 0x9A));
        assert_eq!(sem(Sem::Variable), (0x9C, 0xDC, 0xFE));
        assert_eq!(sem(Sem::Property), (0xDA, 0xDA, 0xDA));
        assert_eq!(sem(Sem::Type), (0x4E, 0xC9, 0xB0));
    }

    #[test]
    fn blend_and_parse_match_vscode() {
        assert_eq!(Rgba(255, 255, 255, 0x80).over(Color::Rgb(0, 0, 0)), Color::Rgb(128, 128, 128));
        assert_eq!(Rgba(0x12, 0x34, 0x56, 0).over(Color::Rgb(0xAB, 0xCD, 0xEF)), Color::Rgb(0xAB, 0xCD, 0xEF));
        assert_eq!(Rgba::parse("#0000"), Some(Rgba(0, 0, 0, 0)));
        assert_eq!(Rgba::parse("#ccc3"), Some(Rgba(0xCC, 0xCC, 0xCC, 0x33)));
        assert_eq!(Rgba::parse("rgba(255,255,64,0.07)"), Some(Rgba(255, 255, 64, 18)));
        assert_eq!(Rgba::parse("rgb(1, 2, 3)"), Some(Rgba(1, 2, 3, 255)));
        assert_eq!(Rgba::parse("#12345"), None);
    }

    #[test]
    fn every_palette_key_resolves_and_defaults_follow_the_registry() {
        let (pal, missing) = build_pal_checked(&Map::new());
        assert!(missing.is_empty(), "no default for {missing:?}");
        // registry: tab.activeBackground -> editor.background; inactive fg = activeForeground @ 50%.
        assert_eq!(pal.tab_active_bg, Color::Rgb(0x1E, 0x1E, 0x1E));
        assert_eq!(pal.tab_inactive_fg, Rgba(255, 255, 255, 128).over(Color::Rgb(0x2D, 0x2D, 0x2D)));
        // A theme value for a referenced key flows into the keys that default to it.
        let mut t = Map::new();
        t.insert("editor.background".into(), "#102030".into());
        assert_eq!(build_pal(&t).tab_active_bg, Color::Rgb(0x10, 0x20, 0x30));
        // The embedded theme: activityBarBadge from the theme, badge from the registry.
        let p = build_pal(embedded_theme()["colors"].as_object().unwrap());
        assert_eq!(p.activity_badge_bg, Color::Rgb(0x00, 0x7A, 0xCC));
        assert_eq!(p.badge_bg, Color::Rgb(0x4D, 0x4D, 0x4D));
        assert_eq!(p.line_nr, Color::Rgb(0x2B, 0x91, 0xAF));
        assert_eq!(p.section_bg, p.sidebar_bg, "#0000 is transparent");
    }
}
