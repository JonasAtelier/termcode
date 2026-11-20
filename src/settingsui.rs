//! Categorized settings dialog, with reversible previews and explicit persistence.

use crate::ui::kit::Look;
use crate::panel::{App, Doc, Drag};
use crate::parse::Chord;
use crate::settings::{self, Settings};
use crate::theme::{icons, Rgba, P};
use crate::ui::{fill, popup_block};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use serde_json::{Map, Value};
use unicode_width::UnicodeWidthStr;

const CATEGORIES: &[&str] =
    &["Appearance", "Layout", "Editor", "Minimap", "Indent Rainbow", "Files & Session", "Source Control", "Keys", "API"];

#[derive(Clone, Copy)]
enum Kind {
    Check(bool),
    Number(u16, u16, u16),
    Choice(&'static [&'static str]),
    Text(&'static str),
    Json(&'static str),
}

struct Field {
    category: usize,
    label: &'static str,
    // A slash separates a member of the spacing object from its top-level key.
    key: &'static str,
    kind: Kind,
}

use Kind::*;
const FIELDS: &[Field] = &[
    Field { category: 0, label: "Color theme", key: "workbench.colorTheme", kind: Text(settings::EMBEDDED_ID) },
    Field { category: 0, label: "Follow VS Code theme", key: "termcode.followVSCode", kind: Check(false) },
    Field { category: 0, label: "Density", key: "workbench.density", kind: Choice(&["comfortable", "compact"]) },
    Field {
        category: 0,
        label: "Block borders",
        key: "termcode.blocks.style",
        kind: Choice(&["cards", "lines", "none"]),
    },
    Field { category: 0, label: "Icons", key: "termcode.icons", kind: Choice(&["auto", "nerd", "unicode", "ascii"]) },
    Field { category: 0, label: "Colors", key: "termcode.colors", kind: Choice(&["auto", "truecolor", "256", "16"]) },
    Field { category: 0, label: "Workbench colors (JSON)", key: "workbench.colorCustomizations", kind: Json("{}") },
    Field { category: 0, label: "Token colors (JSON)", key: "editor.tokenColorCustomizations", kind: Json("{}") },
    Field { category: 0, label: "File icons (JSON)", key: "termcode.fileIcons", kind: Json("{}") },
    Field { category: 1, label: "Sidebar width", key: "termcode.spacing/sidebarWidth", kind: Number(12, 200, 36) },
    Field { category: 1, label: "Sidebar padding", key: "termcode.spacing/sidebarPadding", kind: Number(0, 4, 2) },
    Field { category: 1, label: "Section gap", key: "termcode.spacing/sectionGap", kind: Number(0, 3, 1) },
    Field { category: 1, label: "Editor padding", key: "termcode.spacing/editorPadding", kind: Number(0, 8, 0) },
    Field { category: 1, label: "Tab padding", key: "termcode.spacing/tabPadding", kind: Number(0, 4, 2) },
    Field { category: 1, label: "Status item gap", key: "termcode.spacing/statusBarGap", kind: Number(1, 8, 3) },
    Field {
        category: 1,
        label: "Activity bar width",
        key: "termcode.spacing/activityBarWidth",
        kind: Number(2, 9, 5),
    },
    Field { category: 1, label: "List item gap", key: "termcode.spacing/listItemGap", kind: Number(0, 1, 0) },
    Field {
        category: 1,
        label: "Popup padding [x,y] (JSON)",
        key: "termcode.spacing/popupPadding",
        kind: Json("[3,1]"),
    },
    Field { category: 1, label: "Icon gap", key: "termcode.spacing/iconGap", kind: Number(1, 4, 2) },
    Field { category: 1, label: "Tree indent", key: "termcode.spacing/treeIndent", kind: Number(0, 8, 2) },
    Field { category: 1, label: "Title padding", key: "termcode.spacing/titleBarPadding", kind: Number(0, 6, 2) },
    Field { category: 1, label: "Button padding", key: "termcode.spacing/buttonPadding", kind: Number(0, 4, 2) },
    Field {
        category: 1,
        label: "Find widget padding",
        key: "termcode.spacing/findWidgetPadding",
        kind: Number(0, 3, 1),
    },
    Field {
        category: 1,
        label: "Status bar padding",
        key: "termcode.spacing/statusBarPadding",
        kind: Number(0, 6, 2),
    },
    Field { category: 2, label: "Tab size", key: "editor.tabSize", kind: Number(1, 16, 4) },
    Field {
        category: 2,
        label: "Render whitespace",
        key: "editor.renderWhitespace",
        kind: Choice(&["boundary", "none", "selection", "trailing", "all"]),
    },
    Field {
        category: 2,
        label: "Word occurrences",
        key: "editor.occurrencesHighlight",
        kind: Choice(&["singleFile", "off"]),
    },
    Field { category: 2, label: "Matching brackets", key: "editor.matchBrackets", kind: Choice(&["always", "never"]) },
    Field { category: 2, label: "Bracket pair colours", key: "editor.bracketPairColorization.enabled", kind: Check(true) },
    Field { category: 2, label: "Word wrap", key: "editor.wordWrap", kind: Choice(&["off", "on"]) },
    Field { category: 2, label: "Breadcrumbs", key: "breadcrumbs.enabled", kind: Check(true) },
    Field { category: 2, label: "Error checking (shell, C/C++)", key: "termcode.errorChecking", kind: Check(true) },
    Field {
        category: 2,
        label: "Auto-close brackets",
        key: "editor.autoClosingBrackets",
        kind: Choice(&["always", "never"]),
    },
    Field {
        category: 2,
        label: "Auto-close quotes",
        key: "editor.autoClosingQuotes",
        kind: Choice(&["always", "never"]),
    },
    Field { category: 2, label: "Indent guides", key: "editor.guides.indentation", kind: Check(true) },
    Field { category: 2, label: "Column rulers (JSON)", key: "editor.rulers", kind: Json("[90]") },
    Field {
        category: 2,
        label: "Multiple definitions",
        key: "editor.gotoLocation.multipleDefinitions",
        kind: Choice(&["peek", "gotoAndPeek", "goto"]),
    },
    Field { category: 3, label: "Enable minimap", key: "editor.minimap.enabled", kind: Check(true) },
    Field { category: 3, label: "Maximum column", key: "editor.minimap.maxColumn", kind: Number(20, 400, 120) },
    Field { category: 3, label: "Render characters", key: "editor.minimap.renderCharacters", kind: Check(true) },
    Field {
        category: 3,
        label: "Show slider",
        key: "editor.minimap.showSlider",
        kind: Choice(&["always", "mouseover"]),
    },
    Field { category: 3, label: "Minimap side", key: "editor.minimap.side", kind: Choice(&["right", "left"]) },
    Field { category: 3, label: "Minimap scale", key: "editor.minimap.scale", kind: Number(1, 3, 3) },
    Field {
        category: 3,
        label: "Minimap size",
        key: "editor.minimap.size",
        kind: Choice(&["proportional", "fill", "fit"]),
    },
    Field {
        category: 3,
        label: "Minimum editor width",
        key: "termcode.minimap.minEditorWidth",
        kind: Number(10, 400, 40),
    },
    Field { category: 4, label: "Enable indent rainbow", key: "indentRainbow.enabled", kind: Check(true) },
    Field {
        category: 4,
        label: "Indicator style",
        key: "indentRainbow.indicatorStyle",
        kind: Choice(&["classic", "light"]),
    },
    Field {
        category: 4,
        label: "Rainbow colors (JSON)",
        key: "indentRainbow.colors",
        kind: Json(
            r#"["rgba(255,255,64,0.07)","rgba(127,255,127,0.07)","rgba(255,127,255,0.07)","rgba(79,236,236,0.07)"]"#,
        ),
    },
    Field {
        category: 4,
        label: "Indent error color",
        key: "indentRainbow.errorColor",
        kind: Text("rgba(128,32,32,0.6)"),
    },
    Field {
        category: 4,
        label: "Mixed-tab color",
        key: "indentRainbow.tabmixColor",
        kind: Text("rgba(128,32,96,0.6)"),
    },
    Field {
        category: 4,
        label: "Excluded languages (JSON)",
        key: "indentRainbow.excludedLanguages",
        kind: Json(r#"["plaintext"]"#),
    },
    Field { category: 4, label: "Included languages (JSON)", key: "indentRainbow.includedLanguages", kind: Json("[]") },
    Field {
        category: 4,
        label: "Ignore error languages (JSON)",
        key: "indentRainbow.ignoreErrorLanguages",
        kind: Json(r#"["markdown"]"#),
    },
    Field {
        category: 4,
        label: "Ignore line patterns (JSON)",
        key: "indentRainbow.ignoreLinePatterns",
        kind: Json(r#"["/[ \\t]* [*]/g","/[ \\t]+[/]{2}/g"]"#),
    },
    Field { category: 5, label: "Trim trailing whitespace", key: "files.trimTrailingWhitespace", kind: Check(true) },
    Field { category: 5, label: "Insert final newline", key: "files.insertFinalNewline", kind: Check(true) },
    Field { category: 5, label: "Restore session", key: "termcode.restoreSession", kind: Check(true) },
    Field {
        category: 5,
        label: "Restore file view state",
        key: "workbench.editor.restoreViewState",
        kind: Check(true),
    },
    Field { category: 5, label: "Enable sticky scroll", key: "workbench.tree.enableStickyScroll", kind: Check(true) },
    Field { category: 5, label: "Reveal the open file", key: "explorer.autoReveal", kind: Check(true) },
    Field {
        category: 5,
        label: "Sticky scroll rows",
        key: "workbench.tree.stickyScrollMaxItemCount",
        kind: Number(1, 10, 1),
    },
    Field { category: 6, label: "Status bar blame", key: "git.blame.statusBarItem.enabled", kind: Check(true) },
    Field { category: 6, label: "Inline blame", key: "git.blame.editorDecoration.enabled", kind: Check(false) },
    Field { category: 6, label: "Confirm sync", key: "git.confirmSync", kind: Check(true) },
    Field { category: 7, label: "Prefix", key: "termcode.prefix", kind: Text(crate::settings::DEFAULT_PREFIX) },
    Field { category: 7, label: "Quick keys", key: "termcode.prefixKeys", kind: Json(crate::settings::DEFAULT_PREFIX_KEYS) },
    Field { category: 7, label: "Keybindings (JSON)", key: "termcode.keybindings", kind: Json("{}") },
    Field { category: 8, label: "Programs may", key: "termcode.api", kind: Choice(&["ask", "read-only", "off"]) },
];

/// What a setting does, under its name (and what the search box also finds).
fn describe(key: &str) -> &'static str {
    match key {
        "workbench.colorTheme" => "The color theme: a VS Code theme's name, or a path to its JSON file.",
        "termcode.followVSCode" => "Use the theme VS Code is set to, read from its settings.",
        "workbench.density" => "Comfortable spacing, or compact for small terminals.",
        "termcode.blocks.style" => "How panels are framed: cards, lines between them, or nothing.",
        "termcode.icons" => "Icon glyphs: Nerd Font, plain Unicode or ASCII (auto picks by the fonts found).",
        "termcode.colors" => "Colour depth sent to the terminal (auto reads it from the environment).",
        "workbench.colorCustomizations" => "Workbench colour overrides, as VS Code's setting of that name.",
        "editor.tokenColorCustomizations" => "Syntax colour overrides, as VS Code's setting of that name.",
        "termcode.fileIcons" => "File and folder icons by name or extension, over the built-in ones.",
        "termcode.spacing/sidebarWidth" => "The sidebar's width, in columns.",
        "termcode.spacing/sidebarPadding" => "Blank columns inside the sidebar's edges.",
        "termcode.spacing/sectionGap" => "Blank rows between the sidebar's sections.",
        "termcode.spacing/editorPadding" => "Blank columns left of the editor's text.",
        "termcode.spacing/tabPadding" => "Blank columns on each side of a tab's name.",
        "termcode.spacing/statusBarGap" => "Columns between the status bar's items.",
        "termcode.spacing/activityBarWidth" => "The activity bar's width, in columns.",
        "termcode.spacing/listItemGap" => "A blank row between list items (1) or none (0).",
        "termcode.spacing/popupPadding" => "Columns and rows inside popups, as [x, y].",
        "termcode.spacing/iconGap" => "Columns between an icon and its text.",
        "termcode.spacing/treeIndent" => "Columns each Explorer level is indented by.",
        "termcode.spacing/titleBarPadding" => "Blank columns at the title bar's ends.",
        "termcode.spacing/buttonPadding" => "Blank columns inside a button.",
        "termcode.spacing/findWidgetPadding" => "Blank columns inside the Find widget.",
        "termcode.spacing/statusBarPadding" => "Blank columns at the status bar's ends.",
        "editor.tabSize" => "Columns a tab takes, and an indent when the file does not say.",
        "editor.renderWhitespace" => "Show spaces and tabs: none, between words (boundary), selected, or all.",
        "editor.occurrencesHighlight" => "Light up the other places of the word under the cursor.",
        "editor.matchBrackets" => "Light up the bracket that matches the one at the cursor.",
        "editor.bracketPairColorization.enabled" => "Colour nested brackets by depth.",
        "editor.wordWrap" => "Wrap long lines at the editor's edge.",
        "breadcrumbs.enabled" => "The file's path and the symbols around the cursor, over the text.",
        "termcode.errorChecking" => "Underline problems shellcheck and the C/C++ compiler find.",
        "termcode.keybindings" => "Your own keys for F1 commands: \"ctrl+alt+p\": \"Git: Pull\".",
        "termcode.prefix" => "The key before a quick key, anywhere, the terminal too: then one key runs a command.",
        "termcode.prefixKeys" => "The quick keys after the prefix: click a key (or Enter, then the arrows) to give it a command.",
        "editor.autoClosingBrackets" => "Type the closing bracket with the opening one.",
        "editor.autoClosingQuotes" => "Type the closing quote with the opening one.",
        "editor.guides.indentation" => "Thin lines at each indentation level.",
        "editor.rulers" => "Vertical lines at these columns, as [80, 120].",
        "editor.gotoLocation.multipleDefinitions" => "Go to Definition with more than one: peek them, or go to the first.",
        "editor.minimap.enabled" => "The code overview at the editor's side.",
        "editor.minimap.maxColumn" => "The widest a line is drawn in the minimap.",
        "editor.minimap.renderCharacters" => "Draw the text's shapes, or plain blocks.",
        "editor.minimap.showSlider" => "When the visible part shows on the minimap.",
        "editor.minimap.side" => "Which side of the editor the minimap is on.",
        "editor.minimap.size" => "How the minimap fits the file: proportional, fill or fit.",
        "editor.minimap.scale" => "How large the minimap draws code in Ghostty and kitty: 1, 2 or 3 (a line 2, 4 or 6 pixels tall).",
        "termcode.minimap.minEditorWidth" => "Hide the minimap when the editor is narrower than this.",
        "indentRainbow.enabled" => "Colour each indentation level.",
        "indentRainbow.indicatorStyle" => "Colour whole indents (classic) or a light line (light).",
        "indentRainbow.colors" => "The colours, one per level, as a JSON list.",
        "indentRainbow.errorColor" => "The colour of an indent that is not a multiple of the tab size.",
        "indentRainbow.tabmixColor" => "The colour of an indent mixing tabs and spaces.",
        "indentRainbow.excludedLanguages" => "Languages without the rainbow.",
        "indentRainbow.includedLanguages" => "Only these languages (empty: all).",
        "indentRainbow.ignoreErrorLanguages" => "Languages whose odd indents are not errors.",
        "indentRainbow.ignoreLinePatterns" => "Lines matching these patterns are left alone.",
        "files.trimTrailingWhitespace" => "Remove spaces at the ends of lines when saving.",
        "files.insertFinalNewline" => "End the file with a newline when saving.",
        "termcode.restoreSession" => "Reopen the tabs and layout a folder had last time.",
        "termcode.api" => "What other programs (scripts, AI agents) may do through `termcode api`: ask you before they change files or run anything, only read, or nothing.",
        "workbench.editor.restoreViewState" => "Reopened files come back where you were in them.",
        "workbench.tree.enableStickyScroll" => "Keep the folders of the top row in view while scrolling.",
        "explorer.autoReveal" => "Show the open file in the Explorer: its folders opened, it selected.",
        "workbench.tree.stickyScrollMaxItemCount" => "The most folder rows kept in view.",
        "git.blame.statusBarItem.enabled" => "Who changed the cursor's line, and when, in the status bar.",
        "git.blame.editorDecoration.enabled" => "Who changed the cursor's line, at its end.",
        "git.confirmSync" => "Ask before pulling and pushing with Sync.",
        _ => "",
    }
}

/// Rows a setting takes in the list: its name, what it does, its control, a gap.
const FIELD_H: u16 = 4;

/// The quick keys' setting: edited on a keyboard (`draw_keyboard`), not as JSON text.
const QUICK: &str = "termcode.prefixKeys";

/// The keyboard's rows of keys, each indented as a keyboard staggers them.
const BOARD: [(u16, &str); 4] = [(0, "`1234567890-="), (2, "qwertyuiop[]\\"), (3, "asdfghjkl;'"), (4, "zxcvbnm,./")];

/// The quick keys' keyboard with the keys: the key chosen, and its command list when open.
struct Board {
    sel: Chord,
    pick: Option<Pick>,
}

/// The commands a key can run: the text typed to find one, the one chosen.
struct Pick {
    query: String,
    at: usize,
}

/// What a key can be given: none (a free key), then every F1 command matching `q`.
fn pick_items(q: &str) -> Vec<String> {
    std::iter::once(String::new()).chain(crate::panel::filter_commands_recent(q, &[], &[])).collect()
}

/// A key as `termcode.prefixKeys` names it: "t", "\\", "left", "ctrl+x".
fn chord_name((code, mods): Chord) -> String {
    let mut s = String::new();
    for (m, name) in [(KeyModifiers::CONTROL, "ctrl+"), (KeyModifiers::ALT, "alt+"), (KeyModifiers::SHIFT, "shift+")] {
        if mods.contains(m) {
            s.push_str(name);
        }
    }
    match code {
        KeyCode::Char(' ') => s.push_str("space"),
        KeyCode::Char(c) => s.push(c),
        KeyCode::F(n) => s.push_str(&format!("f{n}")),
        other => s.push_str(&format!("{other:?}").to_lowercase()),
    }
    s
}

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    /// The search box at the top.
    Search,
    Categories,
    Fields,
    Apply,
    Save,
}

#[derive(Default)]
struct Hits {
    area: Rect,
    title: Rect,
    close: Rect,
    categories: Vec<(Rect, usize)>,
    rows: Vec<(Rect, Rect, usize)>,
    buttons: Vec<(Rect, usize)>,
    options: Vec<(Rect, usize)>,
    content: Rect,
    search: Rect,
    /// The quick keys' keyboard: its keys, the command list's rows and frame.
    keys: Vec<(Rect, Chord)>,
    picks: Vec<(Rect, String)>,
    pick: Rect,
}

struct Edit {
    row: usize,
    text: String,
    cursor: usize,
    all: bool,
}

pub struct Dialog {
    original: Map<String, Value>,
    draft: Map<String, Value>,
    disk: Option<String>,
    baseline: Settings,
    toggles: [bool; 5],
    applied: bool,
    category: usize,
    selected: usize,
    focus: Focus,
    scroll: usize,
    dropdown: Option<usize>,
    edit: Option<Edit>,
    position: Option<Position>,
    drag: Option<(Position, Position)>,
    slider: Option<(Rect, usize)>,
    confirm: bool,
    confirm_selected: usize,
    quit_after: bool,
    note: String,
    hits: Hits,
    graphics: crate::settingsgraphics::Graphics,
    palette: Option<(Map<String, Value>, Result<crate::theme::Pal, String>)>,
    /// The search box's text: the list shows the settings it finds in every category.
    query: String,
    /// The quick keys' keyboard while it has the keys (the arrows choose a key).
    board: Option<Board>,
}

fn toggles(app: &App) -> [bool; 5] {
    [app.rainbow_on, app.guides_on, app.minimap_on, app.rulers_on, app.ruler_on]
}

pub fn open(app: &mut App) {
    if app.settings_dlg.is_some() {
        return;
    }
    let disk = app.settings_file.as_deref().map(settings::read_text).transpose();
    let result = disk.and_then(|s| {
        let disk = s.flatten();
        Ok((settings::parse_jsonc(disk.as_deref().unwrap_or(""))?, disk))
    });
    match result {
        Err(e) => app.msg = format!("Cannot open Settings: {e}"),
        Ok((original, disk)) => {
            crate::hover::dismiss(app);
            app.drag = Drag::None;
            app.drop = None;
            app.nav.peek = None;
            app.settings_dlg = Some(Dialog {
                draft: original.clone(),
                original,
                disk,
                baseline: app.settings.clone(),
                toggles: toggles(app),
                applied: false,
                category: 0,
                selected: 0,
                focus: Focus::Categories,
                scroll: 0,
                dropdown: None,
                edit: None,
                position: None,
                drag: None,
                slider: None,
                confirm: false,
                confirm_selected: 0,
                quit_after: false,
                note: "Apply previews; Save keeps changes. Drag the title to see the editor.".into(),
                hits: Hits::default(),
                graphics: crate::settingsgraphics::Graphics::new(),
                palette: None,
                query: String::new(),
                board: None,
            });
        }
    }
}

/// A search's lower-cased text `q` is in the setting's name, key or description.
fn found(f: &Field, q: &str) -> bool {
    [f.label, &f.key.replace('/', "."), describe(f.key), CATEGORIES[f.category]].iter().any(|t| t.to_lowercase().contains(q))
}

impl Dialog {
    /// The settings listed: the category's, or (a search) every one the text is found in.
    fn rows(&self) -> Vec<usize> {
        let q = self.query.trim().to_lowercase();
        FIELDS
            .iter()
            .enumerate()
            .filter(|(_, f)| if q.is_empty() { f.category == self.category } else { found(f, &q) })
            .map(|(i, _)| i)
            .collect()
    }

    /// The setting was set (it is in the settings file), not left at its default.
    fn is_set(&self, row: usize) -> bool {
        let key = FIELDS[row].key;
        match key.split_once('/') {
            Some((k, member)) => self.draft.get(k).is_some_and(|v| v.get(member).is_some()),
            None => self.draft.contains_key(key),
        }
    }

    fn value_in(&self, map: &Map<String, Value>, row: usize) -> Value {
        let field = &FIELDS[row];
        let explicit = if let Some((key, member)) = field.key.split_once('/') {
            map.get(key).and_then(|v| v.get(member))
        } else {
            map.get(field.key)
        };
        if let Some(value) = explicit {
            return value.clone();
        }
        let compact = map.get("workbench.density").and_then(Value::as_str) == Some("compact");
        if compact {
            let spacing = match field.key {
                "termcode.spacing/sidebarWidth" => Some(30),
                "termcode.spacing/sidebarPadding"
                | "termcode.spacing/sectionGap"
                | "termcode.spacing/findWidgetPadding" => Some(0),
                "termcode.spacing/tabPadding"
                | "termcode.spacing/iconGap"
                | "termcode.spacing/titleBarPadding"
                | "termcode.spacing/buttonPadding"
                | "termcode.spacing/statusBarPadding" => Some(1),
                "termcode.spacing/activityBarWidth" => Some(3),
                _ => None,
            };
            if let Some(n) = spacing {
                return Value::from(n);
            }
            if field.key == "termcode.spacing/popupPadding" {
                return serde_json::json!([0, 0]);
            }
            if field.key == "termcode.blocks.style" {
                return Value::from("lines");
            }
        }
        default_of(field.kind)
    }

    fn value(&self, row: usize) -> Value {
        self.value_in(&self.draft, row)
    }

    fn set(&mut self, row: usize, value: Value) {
        let field = &FIELDS[row];
        if let Some((key, member)) = field.key.split_once('/') {
            let v = self.draft.entry(key).or_insert_with(|| Value::Object(Map::new()));
            if !v.is_object() {
                *v = Value::Object(Map::new());
            }
            v.as_object_mut().unwrap().insert(member.into(), value);
            // Returning an inherited setting to its original value should leave no override.
            if self.value(row) == self.value_in(&self.original, row) {
                let original = self.original.get(key).and_then(|v| v.get(member));
                if original.is_none() {
                    self.draft[key].as_object_mut().unwrap().remove(member);
                }
                if self.draft[key].as_object().is_some_and(Map::is_empty) && !self.original.contains_key(key) {
                    self.draft.remove(key);
                }
            }
        } else if value == self.value_in(&self.original, row) && !self.original.contains_key(field.key) {
            self.draft.remove(field.key);
        } else {
            self.draft.insert(field.key.into(), value);
        }
        self.note = "Changes are pending. Apply to preview, or Save to keep them.".into();
    }

    fn dirty(&self) -> bool {
        self.draft != self.original || self.edit.as_ref().is_some_and(|e| e.text != self.display(e.row))
    }

    fn display(&self, row: usize) -> String {
        let v = self.value(row);
        if matches!(FIELDS[row].kind, Text(_) | Choice(_)) {
            v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())
        } else {
            v.to_string()
        }
    }

    fn commit_edit(&mut self) -> bool {
        let Some(e) = self.edit.as_ref() else {
            return true;
        };
        let row = e.row;
        let parsed = match FIELDS[row].kind {
            Json(default) => serde_json::from_str::<Value>(&crate::theme::jsonc_to_json(&e.text))
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    if (default.starts_with('{') && !v.is_object()) || (default.starts_with('[') && !v.is_array()) {
                        Err("expected the same JSON object or array type as the default".into())
                    } else {
                        Ok(v)
                    }
                }),
            _ => Ok(Value::from(e.text.clone())),
        };
        let checked = parsed.and_then(|v| {
            validate(FIELDS[row].key, &v)?;
            Ok(v)
        });
        match checked {
            Ok(v) => {
                self.edit = None;
                self.set(row, v);
                true
            }
            Err(e) => {
                self.note = format!("{}: {e}", FIELDS[row].label);
                false
            }
        }
    }

    fn change(&mut self, row: usize, delta: i32) {
        let value = self.value(row);
        match FIELDS[row].kind {
            Check(default) => self.set(row, Value::Bool(!value.as_bool().unwrap_or(default))),
            Number(lo, hi, default) => self.set(
                row,
                Value::from((value.as_i64().unwrap_or(default as i64) + delta as i64).clamp(lo as i64, hi as i64)),
            ),
            Choice(options) => {
                let current = options.iter().position(|s| Some(*s) == value.as_str()).unwrap_or(0);
                let next = (current as i32 + delta).rem_euclid(options.len() as i32) as usize;
                self.set(row, Value::from(options[next]));
            }
            _ => {}
        }
    }

    fn activate(&mut self, row: usize) {
        match FIELDS[row].kind {
            Check(_) => self.change(row, 1),
            Choice(options) => {
                self.dropdown = Some(options.iter().position(|s| Some(*s) == self.value(row).as_str()).unwrap_or(0))
            }
            Json(_) if FIELDS[row].key == QUICK => {
                self.board = Some(Board { sel: (KeyCode::Char('t'), KeyModifiers::NONE), pick: None })
            }
            Text(_) | Json(_) => {
                let text = self.display(row);
                self.edit = Some(Edit { row, cursor: text.len(), text, all: false });
            }
            Number(..) => {}
        }
    }

    fn apply(&mut self, app: &mut App, save: bool) -> bool {
        if !self.commit_edit() {
            return false;
        }
        let resolved = match settings::resolve(&self.draft) {
            Ok(s) => s,
            Err(e) => {
                self.note = format!("Not applied: {e}");
                return false;
            }
        };
        if save {
            let Some(path) = app.settings_file.as_deref() else {
                self.note = "No settings file in this session".into();
                return false;
            };
            if app.docs.values().any(|d| matches!(d, Doc::Text(b) if b.path.as_deref() == Some(path) && b.dirty())) {
                self.note = "settings.json has unsaved editor changes; save or discard them first".into();
                return false;
            }
            let changes = self
                .draft
                .iter()
                .filter(|(k, v)| self.original.get(*k) != Some(*v))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            match settings::write_changes(path, self.disk.as_deref(), &changes) {
                Ok(text) => self.disk = Some(text),
                Err(e) => {
                    self.note = format!("Not saved: {e}");
                    return false;
                }
            }
        }
        app.apply_settings(resolved);
        self.applied = true;
        self.note = "Preview applied. Save to keep it; Discard restores the previous settings.".into();
        if save {
            self.original = self.draft.clone();
            self.baseline = app.settings.clone();
            self.toggles = toggles(app);
            self.applied = false;
            self.note = "Settings saved and applied.".into();
        }
        true
    }

    fn restore(&self, app: &mut App) {
        if self.applied {
            app.apply_settings(self.baseline.clone());
            [app.rainbow_on, app.guides_on, app.minimap_on, app.rulers_on, app.ruler_on] = self.toggles;
        }
    }

    fn close(&mut self, app: &mut App) -> bool {
        if self.dirty() {
            self.confirm = true;
            self.confirm_selected = 0;
            self.drag = None;
            self.slider = None;
            true
        } else {
            self.restore(app);
            false
        }
    }

    fn category(&mut self, category: usize) {
        self.category = category;
        self.board = None;
        self.query.clear();
        self.selected = 0;
        self.scroll = 0;
        self.dropdown = None;
        self.focus = Focus::Categories;
    }

    fn tab(&mut self, back: bool) {
        let count = self.rows().len();
        // Search, the categories, each setting, Apply, Save; round again.
        let current = match self.focus {
            Focus::Search => 0,
            Focus::Categories => 1,
            Focus::Fields => self.selected + 2,
            Focus::Apply => count + 2,
            Focus::Save => count + 3,
        };
        let next = (current as i32 + if back { -1 } else { 1 }).rem_euclid((count + 4) as i32) as usize;
        self.focus = match next {
            0 => Focus::Search,
            1 => Focus::Categories,
            n if n < count + 2 => {
                self.selected = n - 2;
                Focus::Fields
            }
            n if n == count + 2 => Focus::Apply,
            _ => Focus::Save,
        };
    }

    /// A key in the search box: text, Backspace; ↓ or Enter go to what it found; Esc
    /// empties it (then closes, as Esc does).
    fn search_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => self.query.push(c),
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Down | KeyCode::Enter if !self.rows().is_empty() => self.focus = Focus::Fields,
            KeyCode::Esc if !self.query.is_empty() => self.query.clear(),
            _ => return false,
        }
        (self.selected, self.scroll) = (0, 0);
        true
    }

    fn key(&mut self, app: &mut App, k: KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if self.confirm {
            match k.code {
                KeyCode::Esc => {
                    self.confirm = false;
                    self.quit_after = false;
                }
                KeyCode::Left | KeyCode::BackTab => self.confirm_selected = (self.confirm_selected + 2) % 3,
                KeyCode::Right | KeyCode::Tab => self.confirm_selected = (self.confirm_selected + 1) % 3,
                KeyCode::Enter => return self.confirm_action(app, self.confirm_selected),
                _ => {}
            }
            return true;
        }
        if ctrl && k.code == KeyCode::Char('q') {
            self.quit_after = true;
            return self.close(app);
        }
        if ctrl && k.code == KeyCode::Char('s') {
            self.apply(app, true);
            return true;
        }
        if ctrl && k.code == KeyCode::Char('v') {
            if let Some(text) = crate::panel::clipboard_get() {
                self.insert(&text);
            }
            return true;
        }
        if ctrl && k.code == KeyCode::Char('a') {
            if let Some(e) = self.edit.as_mut() {
                e.all = true;
            }
            return true;
        }
        if self.board.is_some() {
            self.board_key(k);
            return true;
        }
        if self.edit.is_some() {
            match k.code {
                KeyCode::Esc => {
                    self.edit = None;
                }
                KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab => {
                    if self.commit_edit() && k.code != KeyCode::Enter {
                        self.tab(k.code == KeyCode::BackTab);
                    }
                }
                _ => self.edit_key(k),
            }
            return true;
        }
        // Ctrl+F or / : the search box.
        if ctrl && k.code == KeyCode::Char('f') || self.focus != Focus::Search && k.code == KeyCode::Char('/') {
            self.focus = Focus::Search;
            return true;
        }
        if self.focus == Focus::Search && self.search_key(k) {
            return true;
        }
        let rows = self.rows();
        if rows.is_empty() {
            match k.code {
                KeyCode::Esc => return self.close(app),
                KeyCode::Tab | KeyCode::BackTab => self.tab(k.code == KeyCode::BackTab),
                _ => {}
            }
            return true;
        }
        let row = rows[self.selected.min(rows.len() - 1)];
        if let Some(option) = self.dropdown {
            if let Choice(options) = FIELDS[row].kind {
                match k.code {
                    KeyCode::Esc => self.dropdown = None,
                    KeyCode::Up => self.dropdown = Some((option + options.len() - 1) % options.len()),
                    KeyCode::Down => self.dropdown = Some((option + 1) % options.len()),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        self.set(row, Value::from(options[option]));
                        self.dropdown = None;
                    }
                    _ => {}
                }
            }
            return true;
        }
        match k.code {
            KeyCode::Esc => return self.close(app),
            KeyCode::Tab | KeyCode::BackTab => {
                self.tab(k.code == KeyCode::BackTab || k.modifiers.contains(KeyModifiers::SHIFT))
            }
            KeyCode::Up if self.focus == Focus::Categories => {
                self.category((self.category + CATEGORIES.len() - 1) % CATEGORIES.len())
            }
            KeyCode::Down if self.focus == Focus::Categories => self.category((self.category + 1) % CATEGORIES.len()),
            KeyCode::Right | KeyCode::Enter if self.focus == Focus::Categories => self.focus = Focus::Fields,
            KeyCode::Up if self.focus == Focus::Fields => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down if self.focus == Focus::Fields => self.selected = (self.selected + 1).min(rows.len() - 1),
            KeyCode::PageUp if self.focus == Focus::Fields => self.selected = self.selected.saturating_sub(5),
            KeyCode::PageDown if self.focus == Focus::Fields => self.selected = (self.selected + 5).min(rows.len() - 1),
            KeyCode::Left if self.focus == Focus::Fields => self.change(row, -1),
            KeyCode::Right if self.focus == Focus::Fields => self.change(row, 1),
            KeyCode::Enter | KeyCode::Char(' ') => match self.focus {
                Focus::Fields => self.activate(row),
                Focus::Apply => {
                    self.apply(app, false);
                }
                Focus::Save => {
                    self.apply(app, true);
                }
                _ => {}
            },
            _ => {}
        }
        true
    }

    fn confirm_action(&mut self, app: &mut App, button: usize) -> bool {
        match button {
            1 => {
                self.restore(app);
                false
            }
            2 if self.apply(app, true) => false,
            _ => {
                self.confirm = false;
                self.quit_after = false;
                true
            }
        }
    }

    /// The quick keys as the draft has them: the defaults with the user's over them.
    fn quick(&self) -> Vec<(Chord, String)> {
        settings::prefix_keys(self.draft.get(QUICK)).unwrap_or_default()
    }

    /// `chord` runs `cmd` ("": none). Only what differs from the defaults is written.
    fn bind(&mut self, chord: Chord, cmd: &str) {
        let row = FIELDS.iter().position(|f| f.key == QUICK).expect("the quick keys' setting");
        let mut user = self.draft.get(QUICK).and_then(Value::as_object).cloned().unwrap_or_default();
        user.retain(|k, _| crate::parse::parse_chord(k).ok() != Some(chord));
        let default = settings::prefix_keys(None).unwrap_or_default().into_iter().find(|(c, _)| *c == chord).map(|(_, n)| n);
        if default.as_deref().unwrap_or("") != cmd {
            user.insert(chord_name(chord), Value::from(cmd));
        }
        self.set(row, if user.is_empty() { default_of(FIELDS[row].kind) } else { Value::Object(user) });
    }

    /// The chosen key's command list, at its command.
    fn open_pick(&mut self) {
        let Some(sel) = self.board.as_ref().map(|b| b.sel) else { return };
        let cur = self.quick().into_iter().find(|(c, _)| *c == sel).map(|(_, n)| n).unwrap_or_default();
        let at = pick_items("").iter().position(|i| *i == cur).unwrap_or(0);
        if let Some(b) = self.board.as_mut() {
            b.pick = Some(Pick { query: String::new(), at });
        }
    }

    /// The key beside the chosen one as drawn: along its row (`dx`), or the nearest in
    /// the row above or below (`dy`).
    fn step(&mut self, dx: i32, dy: i32) {
        let Some(b) = self.board.as_mut() else { return };
        let Some(cur) = self.hits.keys.iter().find(|k| k.1 == b.sel).map(|k| k.0) else { return };
        let mid = |r: Rect| r.x as i32 + r.width as i32 / 2;
        let next = self
            .hits
            .keys
            .iter()
            .filter(|(r, _)| if dy == 0 { r.y == cur.y && (mid(*r) - mid(cur)) * dx > 0 } else { (r.y as i32 - cur.y as i32) * dy > 0 })
            .min_by_key(|(r, _)| ((r.y as i32 - cur.y as i32).abs(), (mid(*r) - mid(cur)).abs()));
        if let Some(&(_, c)) = next {
            b.sel = c;
        }
    }

    /// A key while the keyboard shows: the arrows choose a key, a typed one goes to its
    /// key, Enter opens its command list; in the list, typing finds a command, ↑↓ choose,
    /// Enter gives it to the key. Esc closes the list, then the keyboard.
    fn board_key(&mut self, k: KeyEvent) {
        let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some(b) = self.board.as_mut() else { return };
        if let Some(p) = b.pick.as_mut() {
            let last = pick_items(&p.query).len().saturating_sub(1);
            match k.code {
                KeyCode::Esc => b.pick = None,
                KeyCode::Up => p.at = p.at.saturating_sub(1),
                KeyCode::Down => p.at = (p.at + 1).min(last),
                KeyCode::PageUp => p.at = p.at.saturating_sub(8),
                KeyCode::PageDown => p.at = (p.at + 8).min(last),
                KeyCode::Enter => {
                    let (sel, cmd) = (b.sel, pick_items(&p.query).swap_remove(p.at.min(last)));
                    b.pick = None;
                    self.bind(sel, &cmd);
                }
                KeyCode::Backspace | KeyCode::Char(_) => {
                    match k.code {
                        KeyCode::Char(c) if plain => p.query.push(c),
                        KeyCode::Backspace => {
                            p.query.pop();
                        }
                        _ => return,
                    }
                    // Typing goes to the best match, past "none".
                    p.at = usize::from(!p.query.is_empty() && pick_items(&p.query).len() > 1);
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Esc => self.board = None,
            KeyCode::Tab | KeyCode::BackTab => {
                self.board = None;
                self.tab(k.code == KeyCode::BackTab);
            }
            KeyCode::Enter => self.open_pick(),
            KeyCode::Left => self.step(-1, 0),
            KeyCode::Right => self.step(1, 0),
            KeyCode::Up => self.step(0, -1),
            KeyCode::Down => self.step(0, 1),
            KeyCode::Char(c) if plain => {
                let c = (KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::NONE);
                if self.hits.keys.iter().any(|k| k.1 == c) {
                    b.sel = c;
                }
            }
            _ => {}
        }
    }

    fn edit_key(&mut self, k: KeyEvent) {
        let Some(e) = self.edit.as_mut() else {
            return;
        };
        let prev = |s: &str, i| s[..i].char_indices().next_back().map_or(0, |(j, _)| j);
        match k.code {
            KeyCode::Home => {
                e.cursor = 0;
                e.all = false;
            }
            KeyCode::End => {
                e.cursor = e.text.len();
                e.all = false;
            }
            KeyCode::Left => {
                e.cursor = prev(&e.text, e.cursor);
                e.all = false;
            }
            KeyCode::Right => {
                e.cursor += e.text[e.cursor..].chars().next().map_or(0, char::len_utf8);
                e.all = false;
            }
            KeyCode::Backspace | KeyCode::Delete if e.all => {
                e.text.clear();
                e.cursor = 0;
                e.all = false;
            }
            KeyCode::Backspace if e.cursor > 0 => {
                let i = prev(&e.text, e.cursor);
                e.text.replace_range(i..e.cursor, "");
                e.cursor = i;
            }
            KeyCode::Delete if e.cursor < e.text.len() => {
                let end = e.cursor + e.text[e.cursor..].chars().next().unwrap().len_utf8();
                e.text.replace_range(e.cursor..end, "");
            }
            KeyCode::Char(c) if !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.insert(&c.to_string())
            }
            _ => {}
        }
    }

    fn insert(&mut self, text: &str) {
        if self.confirm {
            return;
        }
        let Some(e) = self.edit.as_mut() else {
            return;
        };
        if e.text.len().saturating_add(text.len()) > crate::editor::MAX_BYTES as usize {
            self.note = "Setting exceeds 1 MB".into();
            return;
        }
        if e.all {
            e.text.clear();
            e.cursor = 0;
            e.all = false;
        }
        let text = text.replace(['\r', '\n'], " ");
        e.text.insert_str(e.cursor, &text);
        e.cursor += text.len();
    }

    fn slider_at(&mut self, r: Rect, row: usize, x: u16) {
        if let Number(lo, hi, _) = FIELDS[row].kind {
            let width = r.width.saturating_sub(1).max(1) as u32;
            let at = x.saturating_sub(r.x).min(r.width.saturating_sub(1)) as u32;
            self.set(row, Value::from(lo as u32 + (at * (hi - lo) as u32 + width / 2) / width));
        }
    }

    fn mouse(&mut self, app: &mut App, m: MouseEvent) -> bool {
        let p = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Up(MouseButton::Left) => {
                self.drag = None;
                self.slider = None;
            }
            MouseEventKind::Drag(MouseButton::Left) if !self.confirm => {
                if let Some((start, origin)) = self.drag {
                    self.position = Some(Position::new(
                        (origin.x as i32 + p.x as i32 - start.x as i32).max(0) as u16,
                        (origin.y as i32 + p.y as i32 - start.y as i32).max(0) as u16,
                    ));
                } else if let Some((r, row)) = self.slider {
                    self.slider_at(r, row, p.x);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.confirm {
                    if let Some((_, action)) = self.hits.buttons.iter().find(|(r, _)| r.contains(p)) {
                        return self.confirm_action(app, *action);
                    }
                    return true;
                }
                if self.hits.close.contains(p) {
                    return self.close(app);
                }
                if self.hits.title.contains(p) {
                    self.drag = Some((p, self.hits.area.as_position()));
                    self.dropdown = None;
                    return true;
                }
                if let Some((_, option)) = self.hits.options.iter().find(|(r, _)| r.contains(p)).copied() {
                    let row = self.rows()[self.selected];
                    if let Choice(options) = FIELDS[row].kind {
                        self.set(row, Value::from(options[option]));
                    }
                    self.dropdown = None;
                    return true;
                }
                if self.dropdown.take().is_some() {
                    return true;
                }
                if let Some((_, button)) = self.hits.buttons.iter().find(|(r, _)| r.contains(p)).copied() {
                    self.focus = if button == 0 { Focus::Apply } else { Focus::Save };
                    self.apply(app, button == 1);
                    return true;
                }
                if !self.commit_edit() {
                    return true;
                }
                // The quick keys: a command clicked in a key's list goes to the key; a key
                // clicked opens its list; a click anywhere else leaves the keyboard.
                if let Some(b) = self.board.as_mut().filter(|b| b.pick.is_some()) {
                    if let Some((_, cmd)) = self.hits.picks.iter().find(|(r, _)| r.contains(p)).cloned() {
                        let sel = b.sel;
                        b.pick = None;
                        self.bind(sel, &cmd);
                        return true;
                    }
                    if self.hits.pick.contains(p) {
                        return true;
                    }
                }
                self.board = None;
                if let Some(&(_, c)) = self.hits.keys.iter().find(|(r, _)| r.contains(p)) {
                    self.board = Some(Board { sel: c, pick: None });
                    self.focus = Focus::Fields;
                    self.selected = self.rows().iter().position(|&i| FIELDS[i].key == QUICK).unwrap_or(self.selected);
                    self.open_pick();
                    return true;
                }
                if self.hits.search.contains(p) {
                    self.board = None;
                    self.focus = Focus::Search;
                    return true;
                }
                if let Some((_, category)) = self.hits.categories.iter().find(|(r, _)| r.contains(p)).copied() {
                    self.category(category);
                }
                if let Some((_, control, row)) = self.hits.rows.iter().find(|(r, _, _)| r.contains(p)).copied() {
                    self.selected = self.rows().iter().position(|i| *i == row).unwrap_or(0);
                    self.focus = Focus::Fields;
                    if matches!(FIELDS[row].kind, Number(..)) && control.contains(p) {
                        self.slider = Some((control, row));
                        self.slider_at(control, row, p.x);
                    } else if !matches!(FIELDS[row].kind, Number(..)) {
                        self.activate(row);
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if self.hits.pick.contains(p) && self.board.as_ref().is_some_and(|b| b.pick.is_some()) =>
            {
                if let Some(pk) = self.board.as_mut().and_then(|b| b.pick.as_mut()) {
                    let last = pick_items(&pk.query).len().saturating_sub(1);
                    pk.at = if m.kind == MouseEventKind::ScrollDown { (pk.at + 3).min(last) } else { pk.at.saturating_sub(3) };
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if !self.confirm && self.hits.content.contains(p) && self.commit_edit() =>
            {
                let rows = self.rows();
                if rows.is_empty() {
                    return true;
                }
                let step = 1;
                self.board = None;
                if m.kind == MouseEventKind::ScrollDown {
                    self.selected = (self.selected + step).min(rows.len() - 1);
                } else {
                    self.selected = self.selected.saturating_sub(step);
                }
                self.focus = Focus::Fields;
                self.dropdown = None;
            }
            _ => {}
        }
        true
    }
}

fn validate(key: &str, v: &Value) -> Result<(), String> {
    if key.starts_with("indentRainbow.") {
        if let Some(a) = v.as_array() {
            for value in a {
                let s = value.as_str().ok_or("expected an array of strings")?;
                if key.ends_with("colors") && crate::theme::Rgba::parse(s).is_none() {
                    return Err(format!("invalid color: {s}"));
                }
                if key.ends_with("ignoreLinePatterns") {
                    crate::decor::js_regex(s).map_err(|e| e.to_string())?;
                }
            }
        } else if key.ends_with("Color")
            && !v.as_str().is_some_and(|s| s.is_empty() || crate::theme::Rgba::parse(s).is_some())
        {
            return Err("invalid color".into());
        }
    }
    if key == "termcode.spacing/popupPadding" {
        let a = v.as_array().ok_or("expected [horizontal, vertical]")?;
        if a.len() != 2 || !a[0].as_u64().is_some_and(|n| n <= 8) || !a[1].as_u64().is_some_and(|n| n <= 4) {
            return Err("padding must be [0..8, 0..4]".into());
        }
    }
    if key == "editor.rulers" {
        for r in v.as_array().ok_or("expected an array of columns")? {
            let col = r.as_u64().or_else(|| r.get("column").and_then(Value::as_u64));
            if !col.is_some_and(|n| n > 0) {
                return Err("each ruler needs a positive column".into());
            }
            if let Some(c) = r.get("color") {
                if !c.as_str().is_some_and(|s| crate::theme::Rgba::parse(s).is_some()) {
                    return Err("invalid ruler color".into());
                }
            }
        }
    }
    Ok(())
}

pub fn key(app: &mut App, k: KeyEvent) {
    let Some(mut d) = app.settings_dlg.take() else {
        return;
    };
    let keep = d.key(app, k);
    finish(app, d, keep);
}

pub fn mouse(app: &mut App, m: MouseEvent) -> bool {
    let Some(mut d) = app.settings_dlg.take() else {
        return false;
    };
    // Where the pointer is, for what lights under it (the kit's buttons, the keyboard).
    if m.kind == MouseEventKind::Moved {
        app.hover = Some(Position::new(m.column, m.row));
    }
    let keep = d.mouse(app, m);
    finish(app, d, keep);
    true
}

pub fn paste(app: &mut App, text: &str) {
    if let Some(d) = app.settings_dlg.as_mut() {
        d.insert(text);
    }
}

fn finish(app: &mut App, d: Dialog, keep: bool) {
    if keep {
        app.settings_dlg = Some(d);
    } else {
        app.check_settings();
        if d.quit_after {
            app.request_quit();
        }
    }
}

fn text(f: &mut Frame, r: Rect, s: &str, style: Style) {
    if r.width > 0 && r.height > 0 {
        f.render_widget(Paragraph::new(crate::ui::fit(s, r.width as usize)).style(style), r);
    }
}

pub fn draw(app: &mut App, f: &mut Frame) {
    let hover = app.hover;
    let Some(d) = app.settings_dlg.as_mut() else {
        return;
    };
    d.hits = Hits::default();
    d.graphics.refresh_size();
    let a = f.area();
    let w = a.width.saturating_sub(if a.width >= 50 { 6 } else { 0 }).clamp(1, 108).min(a.width);
    let h = a.height.saturating_sub(if a.height >= 16 { 4 } else { 0 }).clamp(1, 32).min(a.height);
    let preferred = d.position.unwrap_or(Position::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2));
    let r = Rect::new(
        preferred.x.clamp(a.x, a.right().saturating_sub(w)),
        preferred.y.clamp(a.y, a.bottom().saturating_sub(h)),
        w,
        h,
    );
    if d.position.is_some() {
        d.position = Some(r.as_position());
    }
    d.hits.area = r;
    f.render_widget(Clear, r);
    let block = popup_block(P.widget_bg, P.widget_fg, (0, 0));
    f.render_widget(block, r);
    if w < 28 || h < 10 {
        text(f, r, "Settings: enlarge terminal", Style::new().fg(P.widget_fg).bg(P.widget_bg));
        return;
    }
    let st = Style::new().fg(P.widget_fg).bg(P.widget_bg);
    let dim = st.fg(P.dim);
    let active = Style::new().fg(P.button_fg).bg(P.button_bg);
    let accent = P.focus_border;
    // The title (dragging it moves the window), a dot while there are unsaved changes.
    d.hits.title = Rect::new(r.x + 1, r.y, w - 5, 1);
    text(f, Rect::new(r.x + 2, r.y, 10, 1), " Settings ", st.add_modifier(Modifier::BOLD));
    if d.dirty() {
        text(f, Rect::new(r.x + 12, r.y, 2, 1), "● ", st.fg(accent));
    }
    d.hits.close = Rect::new(r.right() - 4, r.y, 3, 1);
    text(f, d.hits.close, &format!(" {} ", icons().close), st);
    // The search box, across the top.
    let rows = d.rows();
    let sr = Rect::new(r.x + 2, r.y + 1, w - 4, 1);
    d.hits.search = sr;
    let searching = d.focus == Focus::Search;
    f.render_widget(Clear, sr);
    let count = if d.query.is_empty() { String::new() } else { format!("{} found ", rows.len()) };
    let caret = crate::ui::kit::search_input(f, sr, &d.query, "Search settings  (Ctrl+F)", searching, &count, P.dim);
    if searching {
        f.set_cursor_position((caret, sr.y));
    }
    let nav_w = if w >= 64 { 22 } else { 16 };
    let top = r.y + 3;
    let footer_y = r.bottom() - 4;
    let divider_x = r.x + nav_w;
    let border = Style::new().fg(P.widget_border).bg(P.widget_bg);
    text(f, Rect::new(r.x + 1, top - 1, w - 2, 1), &icons().border[4].repeat((w - 2) as usize), border);
    text(f, Rect::new(r.x + 1, footer_y, w - 2, 1), &icons().border[4].repeat((w - 2) as usize), border);
    for y in top..footer_y {
        text(f, Rect::new(divider_x, y, 1, 1), icons().border[5], border);
    }
    for (x, y, glyph) in [(divider_x, top - 1, "┬"), (divider_x, footer_y, "┴"), (r.x, top - 1, "├"), (r.right() - 1, top - 1, "┤"), (r.x, footer_y, "├"), (r.right() - 1, footer_y, "┤")] {
        text(f, Rect::new(x, y, 1, 1), if crate::theme::ascii() { "+" } else { glyph }, border);
    }
    // The categories, each with how many settings it has (a search: how many it found).
    let q = d.query.trim().to_lowercase();
    let nav_count = (footer_y.saturating_sub(top + 1)).max(1) as usize;
    let nav_off = d.category.saturating_sub(nav_count - 1);
    for (slot, (i, label)) in CATEGORIES.iter().enumerate().skip(nav_off).take(nav_count).enumerate() {
        let y = top + 1 + slot as u16;
        if y >= footer_y {
            break;
        }
        let n = FIELDS.iter().filter(|f| f.category == i && (q.is_empty() || found(f, &q))).count();
        let row = Rect::new(r.x + 1, y, nav_w - 1, 1);
        let current = i == d.category && q.is_empty();
        let style = if current && d.focus == Focus::Categories {
            active
        } else if current {
            st.bg(P.list_inactive_bg).add_modifier(Modifier::BOLD)
        } else if n == 0 {
            dim
        } else {
            st
        };
        fill(f, row, style.bg.unwrap_or(P.widget_bg));
        text(f, row, &format!(" {label}"), style);
        let count = format!("{n} ");
        text(f, Rect::new(row.right() - count.width() as u16, y, count.width() as u16, 1), &count, style.fg(if current && d.focus == Focus::Categories { P.button_fg } else { P.dim }));
        d.hits.categories.push((row, i));
    }
    // The settings: a heading (the category, or the search), then each one as VS Code lists
    // them: its name (a bar left of it when set in the file), what it does, its control.
    let mut content = Rect::new(divider_x + 3, top + 1, w.saturating_sub(nav_w + 6), footer_y.saturating_sub(top + 1));
    let graphics = d.graphics.enabled() && !d.confirm && d.dropdown.is_none();
    // The palettes beside Appearance's settings (a search finding only those too).
    let appearance = !rows.is_empty() && rows.iter().all(|&row| FIELDS[row].category == 0);
    let preview = (appearance && graphics && content.width >= 65 && content.height >= 17).then(|| {
        let area = Rect::new(content.right() - 26, content.y, 26, content.height);
        content.width -= 29;
        area
    });
    let heading = if q.is_empty() { CATEGORIES[d.category].to_string() } else { format!("Search: {}", d.query.trim()) };
    text(f, Rect::new(content.x, content.y, content.width, 1), &heading, st.add_modifier(Modifier::BOLD).fg(P.sidebar_title));
    content = Rect::new(content.x, content.y + 2, content.width, content.height.saturating_sub(2));
    d.hits.content = content;
    let mut keyboard = None;
    if rows.is_empty() {
        text(f, Rect::new(content.x, content.y, content.width, 1), "No settings found.", dim);
    } else {
        d.selected = d.selected.min(rows.len() - 1);
        // Each setting its rows (the keyboard more); the list starts where the chosen one shows whole.
        let quick_h = 3 + board_h(&d.quick());
        let field_h = |row: usize| if FIELDS[row].key == QUICK { quick_h } else { FIELD_H };
        d.scroll = d.scroll.min(d.selected);
        // A key's command list opens under the keyboard: the keyboard goes to the top.
        if d.board.as_ref().is_some_and(|b| b.pick.is_some()) {
            d.scroll = d.selected;
        }
        while d.scroll < d.selected && rows[d.scroll..=d.selected].iter().map(|&r| field_h(r)).sum::<u16>() > content.height + 1 {
            d.scroll += 1;
        }
        let mut y = content.y;
        let mut shown = 0;
        for &row in rows.iter().skip(d.scroll) {
            let fh = field_h(row);
            if shown > 0 && y + fh - 1 > content.bottom() {
                break;
            }
            shown += 1;
            let field = &FIELDS[row];
            let selected = rows[d.selected] == row && d.focus == Focus::Fields;
            let block = Rect::new(content.x - 1, y, content.width + 1, fh - 1);
            if selected {
                fill(f, block, P.list_inactive_bg);
            }
            let bg = if selected { P.list_inactive_bg } else { P.widget_bg };
            let row_st = st.bg(bg);
            if d.is_set(row) {
                for yy in y..(y + fh - 1).min(content.bottom()) {
                    text(f, Rect::new(content.x - 2, yy, 1, 1), "▎", st.fg(accent));
                }
            }
            // "Category: Name" in a search, as VS Code shows where a setting lives.
            let name = if q.is_empty() { field.label.to_string() } else { format!("{}: {}", CATEGORIES[field.category], field.label) };
            text(f, Rect::new(content.x, y, content.width, 1), &name, row_st.add_modifier(Modifier::BOLD).fg(if selected { accent } else { P.widget_fg }));
            text(f, Rect::new(content.x, y + 1, content.width, 1), describe(field.key), row_st.fg(P.dim));
            let control = Rect::new(content.x, y + 2, content.width, 1);
            let value = d.value(row);
            let input = Style::new().fg(P.input_fg).bg(P.input_bg);
            match field.kind {
                Check(default) => {
                    let on = value.as_bool().unwrap_or(default);
                    if graphics && d.graphics.pixel_controls() {
                        // A real switch, in pixels, where the terminal draws pictures.
                        d.graphics.switch(f, Rect::new(control.x, control.y, 4, 1), on, selected, [bg, if on { P.button_bg } else { P.widget_border }, P.button_fg]);
                        text(f, Rect::new(control.x + 5, control.y, control.width.saturating_sub(5), 1), if on { "Enabled" } else { "Disabled" }, row_st);
                    } else {
                        crate::ui::kit::check(f, control, (control.x, control.y), on, if on { "Enabled" } else { "Disabled" });
                    }
                    d.hits.rows.push((Rect::new(content.x, y, content.width, FIELD_H - 1), Rect::new(control.x, control.y, control.width.min(14), 1), row));
                }
                Number(lo, hi, default) => {
                    let n = value.as_u64().unwrap_or(default as u64).clamp(lo as u64, hi as u64);
                    let track_w = control.width.saturating_sub(8).clamp(4, 30);
                    let track = Rect::new(control.x, control.y, track_w, 1);
                    let pos = ((n - lo as u64) * track_w.saturating_sub(1) as u64 / (hi - lo).max(1) as u64) as u16;
                    let (fill_c, rest) = if crate::theme::ascii() { ("=", "-") } else { ("━", "─") };
                    text(f, Rect::new(track.x, track.y, pos, 1), &fill_c.repeat(pos as usize), row_st.fg(accent));
                    text(f, Rect::new(track.x + pos, track.y, 1, 1), if crate::theme::ascii() { "O" } else { "●" }, row_st.fg(if selected { accent } else { P.widget_fg }));
                    text(f, Rect::new(track.x + pos + 1, track.y, track_w - pos - 1, 1), &rest.repeat((track_w - pos - 1) as usize), row_st.fg(P.widget_border));
                    let num = format!(" {n} ");
                    text(f, Rect::new(track.right() + 2, control.y, num.width() as u16, 1), &num, input);
                    d.hits.rows.push((Rect::new(content.x, y, content.width, FIELD_H - 1), track, row));
                }
                Choice(options) => {
                    let wide = options.iter().map(|o| o.width()).max().unwrap_or(4).max(d.display(row).width()) + 5;
                    let r2 = Rect::new(control.x, control.y, (wide as u16).min(control.width), 1);
                    let shown = format!(" {:<w$} {} ", crate::ui::fit(&d.display(row), wide - 5), icons().chev_down, w = wide - 5);
                    text(f, r2, &shown, input);
                    d.hits.rows.push((Rect::new(content.x, y, content.width, FIELD_H - 1), r2, row));
                }
                Json(_) if field.key == QUICK => {
                    let at = Rect::new(control.x, control.y, control.width, content.bottom().saturating_sub(control.y));
                    let frame = draw_keyboard(d, f, at, hover);
                    keyboard = Some(frame);
                    d.hits.rows.push((Rect::new(content.x, y, content.width, fh - 1), frame, row));
                }
                Text(_) | Json(_) => {
                    let r2 = Rect::new(control.x, control.y, control.width.min(56), 1);
                    if let Some(e) = d.edit.as_ref().filter(|e| e.row == row) {
                        let room = r2.width.saturating_sub(2) as usize;
                        let before = &e.text[..e.cursor];
                        let tail: String = before.chars().rev().take(room.saturating_sub(1)).collect::<Vec<_>>().into_iter().rev().collect();
                        fill(f, r2, P.input_bg);
                        let style = if e.all { active } else { input };
                        text(f, Rect::new(r2.x + 1, r2.y, r2.width - 1, 1), &format!("{tail}{}", &e.text[e.cursor..]), style);
                        f.set_cursor_position((r2.x + 1 + tail.width() as u16, r2.y));
                        text(f, Rect::new(r2.x - 1, r2.y, 1, 1), "▏", st.fg(accent));
                    } else {
                        fill(f, r2, P.input_bg);
                        text(f, Rect::new(r2.x + 1, r2.y, r2.width - 1, 1), &d.display(row), input);
                    }
                    d.hits.rows.push((Rect::new(content.x, y, content.width, FIELD_H - 1), r2, row));
                }
            }
            y += fh;
        }
        // A scrollbar when they do not all fit.
        if rows.len() > shown && content.height > 0 {
            crate::ui::kit::slider(f, Rect::new(r.right() - 2, content.y, 1, content.height), rows.len(), shown, d.scroll);
        }
    }
    if let Some(frame) = keyboard {
        draw_pick(d, f, frame, r);
    }
    if let Some(area) = preview {
        if !d.palette.as_ref().is_some_and(|(draft, _)| draft == &d.draft) {
            let palette = settings::resolve(&d.draft).map(|s| {
                crate::theme::build_pal(s.theme.json["colors"].as_object().unwrap_or(&Map::new()))
            });
            d.palette = Some((d.draft.clone(), palette));
        }
        let saved = crate::theme::build_pal(d.baseline.theme.json["colors"].as_object().unwrap_or(&Map::new()));
        for (i, label) in ["Draft palette", "Saved palette"].iter().enumerate() {
            let y = area.y + i as u16 * 9;
            text(f, Rect::new(area.x, y, area.width, 1), label, dim);
            let card = Rect::new(area.x, y + 1, area.width, 7);
            let palette = if i == 1 { Ok(&saved) } else { d.palette.as_ref().unwrap().1.as_ref() };
            match palette {
                Ok(p) => d.graphics.palette(f, card, i == 1, p),
                Err(e) => {
                    f.render_widget(Paragraph::new(format!("Preview unavailable: {e}")).wrap(ratatui::widgets::Wrap { trim: true }).style(st), card);
                }
            }
        }
    }
    // The footer: the setting's key and what last happened; the keys; Apply and Save.
    let key = rows.get(d.selected).map(|&row| FIELDS[row].key.replace('/', ".")).unwrap_or_default();
    let line1 = if d.note.is_empty() { key } else { format!("{key}  ·  {}", d.note) };
    text(f, Rect::new(r.x + 2, footer_y + 1, w.saturating_sub(4), 1), &line1, dim);
    let keys = if d.board.is_some() { "Arrows choose a key · Enter changes it · Esc back" } else { "Tab moves · ↑↓ choose · ←→ change · Enter edits · Esc closes" };
    text(f, Rect::new(r.x + 2, footer_y + 2, w.saturating_sub(26), 1), keys, dim);
    let selected_row = rows.get(d.selected).copied().unwrap_or(0);
    for (index, label, focus) in [(0, "Apply", Focus::Apply), (1, "Save", Focus::Save)] {
        let button = Rect::new(r.right() - 21 + index as u16 * 10, footer_y + 2, 9, 1);
        crate::ui::kit::button(f, button, label, if index == 1 { Look::Primary } else { Look::Secondary }, P.widget_bg, d.focus == focus);
        d.hits.buttons.push((button, index));
    }
    if let Some(option) = d.dropdown {
        if let Choice(options) = FIELDS[selected_row].kind {
            if let Some((_, control, _)) = d.hits.rows.iter().find(|(_, _, row)| *row == selected_row).copied() {
                let height = (options.len() as u16 + 2).min(h - 4);
                let popup =
                    Rect::new(control.x, (control.y + 1).min(r.bottom() - height - 1), control.width.min(30), height);
                f.render_widget(Clear, popup);
                f.render_widget(
                    popup_block(P.widget_bg, P.widget_fg, (0, 0)),
                    popup,
                );
                let visible = height.saturating_sub(2) as usize;
                let off = option.saturating_sub(visible.saturating_sub(1));
                for (i, label) in options.iter().enumerate().skip(off).take(visible) {
                    let row = Rect::new(popup.x + 1, popup.y + 1 + (i - off) as u16, popup.width.saturating_sub(2), 1);
                    text(f, row, &format!(" {label}"), if i == option { active } else { st });
                    d.hits.options.push((row, i));
                }
            }
        }
    }
    if d.confirm {
        let cw = w.saturating_sub(4).min(66);
        let ch = 7;
        let pop = Rect::new(r.x + (w - cw) / 2, r.y + (h - ch) / 2, cw, ch);
        f.render_widget(Clear, pop);
        f.render_widget(
            popup_block(P.widget_bg, P.widget_fg, (0, 0)).title(" Unsaved settings "),
            pop,
        );
        text(f, Rect::new(pop.x + 2, pop.y + 2, cw - 4, 1), "Save changes? Discard also rolls back the preview.", st);
        d.hits.buttons.clear();
        let bw = ((cw - 6) / 3).min(9);
        let bx = pop.right() - 2 - (3 * bw + 2);
        for (index, label) in ["Cancel", "Discard", "Save"].iter().enumerate() {
            let button = Rect::new(bx + index as u16 * (bw + 1), pop.y + 4, bw, 1);
            crate::ui::kit::button(f, button, label, if index == d.confirm_selected { Look::Primary } else { Look::Secondary }, P.widget_bg, false);
            d.hits.buttons.push((button, index));
        }
    }
}

/// Where a panel `w`×`h` opens for the key at `key`: under it, or over it when the dialog
/// `dlg` has no room below; inside the dialog's frame.
fn below(key: Rect, w: u16, h: u16, dlg: Rect) -> Rect {
    let w = w.min(dlg.width.saturating_sub(2));
    let x = key.x.min(dlg.right().saturating_sub(w + 1)).max(dlg.x + 1);
    let y = if key.bottom() + h < dlg.bottom() { key.bottom() } else { key.y.saturating_sub(h).max(dlg.y + 1) };
    Rect::new(x, y, w, h.min(dlg.height.saturating_sub(2)))
}

/// A key on the drawn keyboard (a key settings.json names that is not, F5 or Ctrl+X,
/// gets a row of its own under it).
fn on_board((code, mods): Chord) -> bool {
    mods.is_empty()
        && match code {
            KeyCode::Char(c) => c == ' ' || BOARD.iter().any(|(_, row)| row.contains(c)),
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down => true,
            _ => false,
        }
}

/// The keyboard's rows: its frame, five rows of keys a blank row apart, and a row for keys
/// off the board when settings.json names some.
fn board_h(quick: &[(Chord, String)]) -> u16 {
    11 + if quick.iter().all(|(c, _)| on_board(*c)) { 0 } else { 2 }
}

/// The quick keys as a keyboard in a frame at `at`: a key with a command in the button
/// colour, a free one plain, the one the arrows chose white; what the key pointed at (or
/// chosen) runs on the frame's bottom line. Its frame.
fn draw_keyboard(d: &mut Dialog, f: &mut Frame, at: Rect, hover: Option<Position>) -> Rect {
    let quick = d.quick();
    let defaults = settings::prefix_keys(None).unwrap_or_default();
    let cmd_of = |keys: &[(Chord, String)], c: Chord| keys.iter().find(|(k, _)| *k == c).map(|(_, n)| n.clone());
    let sel = d.board.as_ref().map(|b| b.sel);
    // Keys 3 cells wide, a blank between them where it fits.
    let step = 3 + u16::from(at.width >= 60);
    let (x0, y0) = (at.x + 2, at.y + 1);
    let none = KeyModifiers::NONE;
    let mut keys: Vec<(Rect, Chord, String)> = Vec::new();
    for (i, (indent, row)) in BOARD.iter().enumerate() {
        for (j, ch) in row.chars().enumerate() {
            keys.push((Rect::new(x0 + indent + j as u16 * step, y0 + 2 * i as u16, 3, 1), (KeyCode::Char(ch), none), ch.to_uppercase().collect()));
        }
    }
    let space = Rect::new(x0 + 4 + 2 * step, y0 + 8, 6 * step - 1, 1);
    keys.push((space, (KeyCode::Char(' '), none), "Space".into()));
    for (i, (code, label)) in [(KeyCode::Left, "←"), (KeyCode::Up, "↑"), (KeyCode::Down, "↓"), (KeyCode::Right, "→")].into_iter().enumerate() {
        keys.push((Rect::new(space.right() + 2 + i as u16 * step, space.y, 3, 1), (code, none), label.into()));
    }
    let mut x = x0;
    for (c, _) in quick.iter().filter(|(c, _)| !on_board(*c)) {
        let label = crate::parse::chord_label(*c);
        let w = label.width() as u16 + 2;
        keys.push((Rect::new(x, y0 + 10, w, 1), *c, label));
        x += w + 1;
    }
    let right = keys.iter().map(|k| k.0.right()).max().unwrap_or(x0);
    let frame = Rect::new(at.x, at.y, (right + 2 - at.x).min(at.width), board_h(&quick)).intersection(Rect::new(at.x, at.y, at.width, at.height.max(1)));
    // The keyboard's case: the editor's colour, a free key lighter, so each kind stands apart.
    let case = P.editor_bg;
    let lit = crate::ui::kit::lit_fg();
    let Color::Rgb(lr, lg, lb) = lit else { return frame };
    let free = Rgba(lr, lg, lb, 255).alpha(0.16).over(case);
    let free_hot = Rgba(lr, lg, lb, 255).alpha(0.3).over(case);
    let st = Style::new().fg(P.widget_fg).bg(case);
    let swatch = if crate::theme::ascii() { "#" } else { "■" };
    let prefix = match d.draft.get("termcode.prefix").and_then(Value::as_str).unwrap_or(settings::DEFAULT_PREFIX) {
        "" => " The prefix is off ".to_string(),
        p => format!(" {}, then ", crate::parse::parse_chord(p).map_or_else(|_| p.to_string(), crate::parse::chord_label)),
    };
    let legend = Line::from(vec![
        Span::styled(swatch, st.fg(P.button_bg)),
        Span::styled(" runs a command  ", st.fg(P.dim)),
        Span::styled(swatch, st.fg(free)),
        Span::styled(" free ", st.fg(P.dim)),
    ])
    .right_aligned();
    let pointed = hover.and_then(|p| keys.iter().find(|k| k.0.contains(p)).map(|k| k.1)).or(sel);
    let info = match pointed {
        Some(c) => {
            let what = cmd_of(&quick, c);
            let was = cmd_of(&defaults, c);
            let mut spans = vec![
                Span::styled(format!(" {}  ", crate::parse::chord_label(c)), st.fg(P.focus_border).bold()),
                Span::styled(what.clone().unwrap_or_else(|| "No command".into()), st),
            ];
            if what != was {
                spans.push(Span::styled(format!("  (default: {})", was.as_deref().unwrap_or("none")), st.fg(P.dim)));
            }
            spans.push(Span::styled(" ", st));
            Line::from(spans)
        }
        None => Line::styled(" Point at a key to see its command; click it to change it ", st.fg(P.dim)),
    };
    let block = popup_block(case, P.widget_fg, (0, 0))
        .title(Span::styled(prefix, st.fg(P.sidebar_title).bold()))
        .title(legend)
        .title_bottom(info);
    f.render_widget(block, frame);
    let inner = Rect::new(frame.x + 1, frame.y + 1, frame.width.saturating_sub(2), frame.height.saturating_sub(2));
    for (r, c, label) in keys {
        if r.right() > inner.right() || r.bottom() > inner.bottom() {
            continue;
        }
        let bound = cmd_of(&quick, c).is_some();
        // Noted as a button: lit under the pointer, pressed while held.
        let kit = crate::ui::kit::look_at(if bound { Look::Primary } else { Look::Secondary }, r);
        let style = if sel == Some(c) {
            Style::new().fg(P.editor_bg).bg(lit).bold()
        } else if bound {
            kit
        } else {
            Style::new().fg(P.widget_fg).bg(if crate::ui::kit::hot(r) { free_hot } else { free })
        };
        let bg = style.bg.unwrap_or(free);
        fill(f, r, bg);
        let t = crate::ui::fit(&label, r.width.saturating_sub(2) as usize);
        text(f, Rect::new(r.x + r.width.saturating_sub(t.width() as u16) / 2, r.y, t.width() as u16, 1), &t, style);
        crate::ui::kit::round_ends(f, r, bg, case);
        d.hits.keys.push((r, c));
    }
    frame
}

/// The chosen key's command list, under the keyboard `frame`: a box to find a command,
/// "none", then the commands; the key's own marked.
fn draw_pick(d: &mut Dialog, f: &mut Frame, frame: Rect, dlg: Rect) {
    let Some((sel, query, at)) = d.board.as_ref().and_then(|b| b.pick.as_ref().map(|p| (b.sel, p.query.clone(), p.at))) else { return };
    let Some(kr) = d.hits.keys.iter().find(|k| k.1 == sel).map(|k| k.0) else { return };
    let st = Style::new().fg(P.widget_fg).bg(P.widget_bg);
    let items = pick_items(&query);
    let at = at.min(items.len().saturating_sub(1));
    if let Some(p) = d.board.as_mut().and_then(|b| b.pick.as_mut()) {
        p.at = at;
    }
    // Under the keyboard, over what follows it (the footer too); over it only when there is no room.
    let room = dlg.bottom().saturating_sub(frame.bottom() + 1);
    let w = 52.min(dlg.width.saturating_sub(2));
    let x = kr.x.saturating_sub(2).min(dlg.right().saturating_sub(w + 1)).max(dlg.x + 1);
    let panel = if room >= 6 { Rect::new(x, frame.bottom(), w, room.min(14)) } else { below(Rect::new(x, frame.y, kr.width, frame.height), w, 14, dlg) };
    f.render_widget(Clear, panel);
    let title = Span::styled(format!(" {} runs ", crate::parse::chord_label(sel)), st.fg(P.sidebar_title).bold());
    let block = popup_block(P.widget_bg, P.widget_fg, (1, 0)).title(title);
    let inner = block.inner(panel);
    f.render_widget(block, panel);
    d.hits.pick = panel;
    if inner.height < 3 {
        return;
    }
    let input = Rect::new(inner.x, inner.y, inner.width, 1);
    let caret = crate::ui::kit::search_input(f, input, &query, "Type to find a command", true, "", P.dim);
    f.set_cursor_position((caret, input.y));
    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height - 2);
    let cur = d.quick().into_iter().find(|(c, _)| *c == sel).map(|(_, n)| n).unwrap_or_default();
    let off = at.saturating_sub(list.height as usize - 1);
    let active = Style::new().fg(P.button_fg).bg(P.button_bg);
    for (i, item) in items.iter().enumerate().skip(off).take(list.height as usize) {
        let row = Rect::new(list.x, list.y + (i - off) as u16, list.width, 1);
        let style = if i == at { active } else { st };
        let mark = if *item == cur { "● " } else { "  " };
        fill(f, row, style.bg.unwrap_or(P.widget_bg));
        text(f, row, &format!("{mark}{}", if item.is_empty() { "None (a free key)" } else { item }), style);
        d.hits.picks.push((row, item.clone()));
    }
    if items.len() > list.height as usize {
        crate::ui::kit::slider(f, Rect::new(panel.right() - 1, list.y, 1, list.height), items.len(), list.height as usize, off);
    }
}

/// A field's default (comfortable density).
fn default_of(kind: Kind) -> Value {
    match kind {
        Check(b) => Value::from(b),
        Number(_, _, n) => Value::from(n),
        Choice(options) => Value::from(options[0]),
        Text(s) => Value::from(s),
        Json(s) => serde_json::from_str(s).unwrap(),
    }
}

/// What is sensible for a setting beyond its range: the values that work best, and the
/// ones that may break the layout or a feature.
fn advice(key: &str) -> &'static str {
    match key {
        "workbench.colorTheme" => "an installed VS Code theme's name; an unknown one falls back to the default",
        "workbench.density" => "compact for terminals under about 100 columns",
        "termcode.icons" => "ascii or unicode if boxes show instead of icons (no Nerd Font)",
        "termcode.colors" => "keep auto; 256 or 16 only when colours look wrong in an old terminal",
        "termcode.spacing/sidebarWidth" => "recommended 28-48 · under 20 cuts file names, over 80 crowds the editor",
        "termcode.spacing/sidebarPadding" => "recommended 1-2",
        "termcode.spacing/sectionGap" => "recommended 0-1",
        "termcode.spacing/editorPadding" => "recommended 0-2 · more takes columns from the text",
        "termcode.spacing/tabPadding" => "recommended 1-2 · 0 runs tab names together",
        "termcode.spacing/statusBarGap" => "recommended 2-4",
        "termcode.spacing/activityBarWidth" => "recommended 4-6 · under 4 its icons may be cut",
        "termcode.spacing/listItemGap" => "0 or 1 · 1 halves the rows a list shows",
        "termcode.spacing/popupPadding" => "recommended [2-4, 0-1] · more shrinks what a popup shows",
        "termcode.spacing/iconGap" => "recommended 1-2",
        "termcode.spacing/treeIndent" => "recommended 2-4 · 0 flattens the tree",
        "termcode.spacing/titleBarPadding" => "recommended 1-3",
        "termcode.spacing/buttonPadding" => "recommended 1-2 · 0 makes buttons hard to hit",
        "termcode.spacing/findWidgetPadding" => "recommended 0-1",
        "termcode.spacing/statusBarPadding" => "recommended 1-3",
        "editor.tabSize" => "recommended 2, 4 or 8 · new indents use it too",
        "editor.rulers" => "columns, as [80, 120]; [] for none · past the window's width they do not show",
        "editor.minimap.maxColumn" => "recommended 80-160 · in pixels the minimap's width limits it (up to 24 cells)",
        "editor.minimap.scale" => "recommended 2-3 · 1 is barely legible; dots elsewhere ignore it",
        "editor.minimap.size" => "proportional keeps the slider easy to grab; fill squeezes a long file into dots",
        "termcode.minimap.minEditorWidth" => "recommended 30-60 · higher hides the minimap in split editors",
        "indentRainbow.colors" => "rgba with alpha 0.05-0.15 keeps the text readable",
        "workbench.tree.stickyScrollMaxItemCount" => "recommended 1-3 · each sticky row is a row less of the list",
        "termcode.restoreSession" => "off: every start is a clean window",
        "termcode.api" => "ask is safe: you approve each program once; read-only or off if no program should drive termcode",
        "git.confirmSync" => "off: Sync pulls and pushes without asking",
        "termcode.prefix" => "a key your shell rarely needs; \"\" turns it off · not ctrl+b (herdr's) or a key termcode uses",
        "termcode.prefixKeys" => "\"\" removes a default; any F1 command name works",
        "termcode.keybindings" => "a key termcode uses gets yours instead; \"\" turns a key off",
        _ => "",
    }
}

/// The settings.json a new user starts with (and `termcode --default-config` prints):
/// this window's options in its categories, each commented out at its default, so
/// uncommenting a line is all it takes to change one.
pub fn template() -> String {
    template_with(None)
}

/// The settings file: its categories, and for each option what it does, the values it
/// takes, what works best (and what may break), then the option, commented out at its
/// default, or as `user` has it (Refresh Settings File). `user`'s options the window does
/// not know close the file.
fn template_with(user: Option<&serde_json::Map<String, Value>>) -> String {
    let mut out = String::from(
        "// termcode settings, by category. For each option: what it does, the values it takes\n\
         // (and which work best), then the option itself. A line starting with // is off and\n\
         // shows the default: remove the // and change the value; saving applies it. VS Code's\n\
         // settings.json format: comments and trailing commas are fine. A broken file is\n\
         // reported in the status bar and the last good settings stay.\n\
         // F1 Preferences: Refresh Settings File rewrites this layout, keeping your values.\n{\n",
    );
    // An object over lines, nested by 4 spaces as the file is; anything else on one.
    let json = |v: &Value| match v {
        Value::Object(m) if !m.is_empty() => serde_json::to_string_pretty(v)
            .unwrap()
            .lines()
            .map(|l| {
                let n = l.len() - l.trim_start().len();
                format!("{}{}", " ".repeat(n * 2), l.trim_start())
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => serde_json::to_string(v).unwrap(),
    };
    // An option: its lines, commented out (off) or not, the value's lines indented with it.
    let option = |out: &mut String, indent: &str, key: &str, value: &Value, on: bool| {
        let mark = if on { "" } else { "// " };
        let text = format!("\"{key}\": {},", json(value));
        for l in text.lines() {
            out.push_str(&format!("    {mark}{indent}{l}\n"));
        }
    };
    let range = |f: &Field| -> String {
        let r = match f.kind {
            Check(_) => "true or false".to_string(),
            Number(lo, hi, _) => format!("{lo} to {hi}"),
            Choice(o) => o.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", "),
            Text(_) => "text".to_string(),
            Json(_) => "JSON".to_string(),
        };
        match advice(f.key) {
            "" => r,
            a => format!("{r} · {a}"),
        }
    };
    let mut known: Vec<&str> = Vec::new();
    for (c, name) in CATEGORIES.iter().enumerate() {
        out.push_str(&format!("\n    // ---- {name} ----\n"));
        let mut group: Option<(&str, bool)> = None;
        for f in FIELDS.iter().filter(|f| f.category == c) {
            let (key, member) = f.key.split_once('/').map_or((f.key, None), |(k, m)| (k, Some(m)));
            known.push(key);
            if let Some((_, on)) = group.filter(|(g, _)| member.is_none() || *g != key) {
                out.push_str(if on { "    },\n" } else { "    // },\n" });
                group = None;
            }
            match member {
                Some(m) => {
                    let set = user.and_then(|u| u.get(key)).and_then(Value::as_object);
                    if group.is_none() {
                        out.push_str(&format!("\n    // {}: {}\n", "Spacing", "the layout's gaps, in cells (the compact density starts from smaller ones)"));
                        out.push_str(&format!("    {}\"{key}\": {{\n", if set.is_some() { "" } else { "// " }));
                        group = Some((key, set.is_some()));
                    }
                    let mine = set.and_then(|s| s.get(m));
                    out.push_str(&format!("    //     {} · {}\n", describe(f.key), range(f)));
                    option(&mut out, "    ", m, mine.unwrap_or(&default_of(f.kind)), mine.is_some());
                }
                None => {
                    out.push_str(&format!("\n    // {}\n    //   {}\n", describe(f.key), range(f)));
                    let mine = user.and_then(|u| u.get(key));
                    option(&mut out, "", key, mine.unwrap_or(&default_of(f.kind)), mine.is_some());
                }
            }
        }
        if let Some((_, on)) = group {
            out.push_str(if on { "    },\n" } else { "    // },\n" });
        }
    }
    // The user's options this window does not have, kept as they are.
    let other: Vec<(&String, &Value)> = user.map(|u| u.iter().filter(|(k, _)| !known.contains(&k.as_str())).collect()).unwrap_or_default();
    if !other.is_empty() {
        out.push_str("\n    // ---- Yours, not in the Settings window ----\n");
        for (k, v) in other {
            option(&mut out, "", k, v, true);
        }
    }
    out.push_str("}\n");
    out
}

/// Settings.json rewritten in the template's layout with the user's values (the old file
/// kept beside it as settings.json.bak, its comments in it): where it went, or why not.
pub fn refresh(path: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let user = if text.trim().is_empty() { serde_json::Map::new() } else { crate::settings::parse_jsonc(&text).map_err(|e| format!("fix it first: {e}"))? };
    let backup = path.with_extension("json.bak");
    if !text.is_empty() {
        std::fs::write(&backup, &text).map_err(|e| format!("{}: {e}", backup.display()))?;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, template_with(Some(&user))).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_file_lists_every_option_at_its_default() {
        let t = template();
        assert!(crate::settings::parse_jsonc(&t).unwrap().is_empty(), "all commented out: the defaults");
        // Uncomment every setting line: each key of the window, at the window's default.
        let on: Vec<String> = t.lines().map(|l| {
            let s = l.trim_start();
            if s.starts_with("// \"") || s.starts_with("// }") || s.starts_with("//     \"") { l.replacen("// ", "", 1) } else { l.to_string() }
        }).collect();
        let m = crate::settings::parse_jsonc(&on.join("\n")).unwrap();
        for f in FIELDS {
            let got = match f.key.split_once('/') { Some((k, s)) => &m[k][s], None => &m[f.key] };
            assert_eq!(got, &default_of(f.kind), "{}", f.key);
        }
        assert_eq!(m.len() + 14, FIELDS.len(), "nothing else: the 15 spacing members are one key");
        assert!(crate::settings::resolve(&m).is_ok(), "uncommented, it loads");
        for c in CATEGORIES {
            assert!(t.contains(&format!("---- {c} ----")), "{c}");
        }
    }

    #[test]
    fn a_refreshed_settings_file_keeps_the_users_values_and_explains_every_option() {
        let dir = std::env::temp_dir().join(format!("termcode-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("settings.json");
        std::fs::write(&p, "// mine\n{ \"editor.tabSize\": 2, \"termcode.spacing\": { \"sidebarWidth\": 40 }, \"some.other\": [1] }\n").unwrap();
        let backup = refresh(&p).unwrap();
        assert!(std::fs::read_to_string(&backup).unwrap().contains("// mine"), "the old file kept");
        let t = std::fs::read_to_string(&p).unwrap();
        let m = crate::settings::parse_jsonc(&t).unwrap();
        assert_eq!((m["editor.tabSize"].clone(), m["termcode.spacing"]["sidebarWidth"].clone(), m["some.other"].clone()), (2.into(), 40.into(), serde_json::json!([1])));
        assert_eq!(m.len(), 3, "only the user's values are on: {m:?}");
        assert!(t.contains("1 to 16 · recommended 2, 4 or 8"), "a range and its advice");
        assert!(t.contains("// ---- Keys ----") && t.contains("\"t\": \"View: Toggle Terminal\""));
        assert!(crate::settings::resolve(&m).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    use std::fs;

    fn screen(app: &mut App, t: &mut Terminal<TestBackend>) -> String {
        t.draw(|f| crate::ui::draw(app, f)).unwrap();
        let b = t.backend().buffer();
        (0..b.area.height)
            .map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ctrl+F, then `q` typed into the search box.
    fn search(app: &mut App, q: &str) {
        app.on_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in q.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    fn click(app: &mut App, t: &mut Terminal<TestBackend>, label: &str) {
        let text = screen(app, t);
        let d = app.settings_dlg.as_ref().unwrap();
        let labels: &[&str] = if d.confirm { &["Cancel", "Discard", "Save"] } else { &["Apply", "Save"] };
        let button = labels
            .iter()
            .position(|s| *s == label)
            .and_then(|i| d.hits.buttons.iter().find(|(_, n)| *n == i))
            .map(|(r, _)| (r.x, r.y));
        let (x, y) = button.unwrap_or_else(|| {
            let (x, y) = text
                .lines()
                .enumerate()
                .find_map(|(y, line)| line.find(label).map(|x| (line[..x].chars().count() as u16, y as u16)))
                .unwrap_or_else(|| panic!("missing {label:?}\n{text}"));
            (x, y)
        });
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn setup(tag: &str) -> (App, Terminal<TestBackend>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("termcode-settingsui-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "// keep this comment\n{\n  \"editor.tabSize\": 4, // keep this too\n  \"custom.unrelated\": {\"value\": 9},\n}\n").unwrap();
        let mut app = App::new(dir, None);
        app.settings_file = Some(path.clone());
        app.check_settings();
        open(&mut app);
        (app, Terminal::new(TestBackend::new(110, 36)).unwrap(), path)
    }

    #[test]
    fn switch_fallback_stays_legible_and_keeps_mouse_keyboard_and_apply_behavior() {
        let (mut app, mut t, path) = setup("graphical-switch");
        click(&mut app, &mut t, "Minimap");
        screen(&mut app, &mut t);
        let (_, control, _) = app.settings_dlg.as_ref().unwrap().hits.rows[0];
        let symbols: String = (control.x..control.x + 3)
            .map(|x| t.backend().buffer()[(x, control.y)].symbol()).collect();
        assert_eq!(symbols, format!(" {} ", icons().check), "a checkbox, ticked (no low-resolution image blocks)");
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: control.x + 2, row: control.y, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.settings_dlg.as_ref().unwrap().draft["editor.minimap.enabled"], false);
        assert!(app.settings.minimap.enabled, "a switch edits only the draft");
        assert!(screen(&mut app, &mut t).contains("    Disabled"));
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(app.settings_dlg.as_ref().unwrap().value(app.settings_dlg.as_ref().unwrap().rows()[0]), true);
        press(&mut app, KeyCode::Char(' '));
        click(&mut app, &mut t, "Apply");
        assert!(!app.settings.minimap.enabled);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn palette_cards_preview_draft_colors_without_applying_or_saving() {
        let (mut app, mut t, path) = setup("palette-cards");
        let before = fs::read(&path).unwrap();
        let original = app.settings.theme.clone();
        search(&mut app, "workbench colors");
        click(&mut app, &mut t, "Workbench colors");
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.paste_text(r##"{"editor.background":"#123456"}"##);
        press(&mut app, KeyCode::Enter);
        let shown = screen(&mut app, &mut t);
        assert!(shown.contains("Draft palette") && shown.contains("Saved palette"), "{shown}");
        let b = t.backend().buffer();
        let (x, y) = shown.lines().enumerate().find_map(|(y, line)| {
            line.find("Draft palette").map(|x| (line[..x].chars().count() as u16, y as u16))
        }).unwrap();
        assert!((y + 1..y + 7).any(|yy| (x..x + 24).any(|xx| {
            let cell = &b[(xx, yy)];
            cell.bg == Color::Rgb(0x12, 0x34, 0x56) || cell.fg == Color::Rgb(0x12, 0x34, 0x56)
        })), "draft palette must use the edited background");
        assert_eq!(app.settings.theme, original);
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn apply_only_previews_and_discard_restores_the_previous_settings() {
        let (mut app, mut t, path) = setup("preview");
        let before = fs::read(&path).unwrap();
        click(&mut app, &mut t, "Editor");
        click(&mut app, &mut t, "Tab size");
        press(&mut app, KeyCode::Right);
        assert_eq!(app.settings.tab_size, 4, "editing must not apply");
        click(&mut app, &mut t, "Apply");
        assert_eq!(app.settings.tab_size, 5, "Apply must update the running editor");
        assert_eq!(fs::read(&path).unwrap(), before, "Apply must not persist");
        app.refresh();
        assert_eq!(app.settings.tab_size, 5, "focus refresh must not replace the preview");
        press(&mut app, KeyCode::Esc);
        assert!(screen(&mut app, &mut t).contains("Unsaved settings"));
        click(&mut app, &mut t, "Cancel");
        assert!(app.settings_dlg.is_some());
        press(&mut app, KeyCode::Esc);
        click(&mut app, &mut t, "Discard");
        assert!(app.settings_dlg.is_none());
        assert_eq!(app.settings.tab_size, 4, "discard must roll back previews");
        assert_eq!(fs::read(&path).unwrap(), before);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_applies_and_persists_without_losing_comments_or_other_keys() {
        let (mut app, mut t, path) = setup("save");
        click(&mut app, &mut t, "Editor");
        click(&mut app, &mut t, "Tab size");
        press(&mut app, KeyCode::Right);
        click(&mut app, &mut t, "Save");
        assert_eq!(app.settings.tab_size, 5);
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("// keep this comment") && text.contains("// keep this too"));
        let values = crate::settings::parse_jsonc(&text).unwrap();
        assert_eq!(values["editor.tabSize"], 5);
        assert_eq!(values["custom.unrelated"], serde_json::json!({"value": 9}));
        press(&mut app, KeyCode::Esc);
        assert!(app.settings_dlg.is_none(), "saved settings close without a warning");
        assert_eq!(crate::settings::load(&path).unwrap().tab_size, 5);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn leaving_unsaved_settings_defaults_to_cancel_and_never_saves_on_enter() {
        let (mut app, mut t, path) = setup("cancel-default");
        let before = fs::read(&path).unwrap();
        click(&mut app, &mut t, "Editor");
        press(&mut app, KeyCode::Right);
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Esc);
        screen(&mut app, &mut t);
        press(&mut app, KeyCode::Enter);
        assert!(app.settings_dlg.is_some(), "Enter on the warning must keep editing by default");
        assert!(!app.settings_dlg.as_ref().unwrap().confirm);
        assert_eq!(fs::read(&path).unwrap(), before, "Enter must not save implicitly");
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn checkbox_dropdown_and_drag_use_the_drawn_positions() {
        let (mut app, mut t, path) = setup("controls");
        click(&mut app, &mut t, "Editor");
        click(&mut app, &mut t, "Render whitespace");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        click(&mut app, &mut t, "Apply");
        assert_eq!(app.settings.render_whitespace, crate::editor::Whitespace::None);
        click(&mut app, &mut t, "Minimap");
        click(&mut app, &mut t, "Enable minimap");
        click(&mut app, &mut t, "Apply");
        assert!(!app.settings.minimap.enabled);
        screen(&mut app, &mut t);
        let from = app.settings_dlg.as_ref().unwrap().hits.area;
        let mouse = |app: &mut App, kind, x, y| {
            app.on_mouse(MouseEvent { kind, column: x, row: y, modifiers: KeyModifiers::NONE });
        };
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from.x + 12, from.y);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 1, 1);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 1, 1);
        screen(&mut app, &mut t);
        let after = app.settings_dlg.as_ref().unwrap().hits.area;
        assert_eq!(after.as_position(), Position::new(0, 1));
        let close = app.settings_dlg.as_ref().unwrap().hits.close;
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), close.x, close.y);
        click(&mut app, &mut t, "Discard");
        assert!(app.settings.minimap.enabled);
        assert_eq!(app.settings.render_whitespace, crate::editor::Whitespace::Boundary);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_refuses_external_edits_and_invalid_json_without_losing_the_draft() {
        let (mut app, mut t, path) = setup("conflict");
        click(&mut app, &mut t, "Editor");
        click(&mut app, &mut t, "Tab size");
        press(&mut app, KeyCode::Right);
        fs::write(&path, "{\"editor.tabSize\": 8}\n").unwrap();
        click(&mut app, &mut t, "Save");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"editor.tabSize\": 8}\n");
        assert_eq!(app.settings.tab_size, 4);
        assert!(app.settings_dlg.as_ref().unwrap().note.contains("changed on disk"));
        press(&mut app, KeyCode::Esc);
        click(&mut app, &mut t, "Discard");
        assert_eq!(app.settings.tab_size, 8, "closing should load the external settings");
        open(&mut app);
        search(&mut app, "workbench colors");
        click(&mut app, &mut t, "Workbench colors");
        app.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.paste_text("{invalid JSON}");
        click(&mut app, &mut t, "Apply");
        assert!(app.settings_dlg.as_ref().unwrap().edit.is_some());
        assert_eq!(app.settings.tab_size, 8);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn tiny_terminals_and_resize_keep_the_dialog_within_the_frame() {
        let (mut app, mut t, path) = setup("resize");
        for (w, h) in [(110, 36), (50, 18), (30, 10), (15, 4), (1, 1), (100, 30)] {
            t.backend_mut().resize(w, h);
            t.resize(Rect::new(0, 0, w, h)).unwrap();
            screen(&mut app, &mut t);
            let r = app.settings_dlg.as_ref().unwrap().hits.area;
            assert!(r.right() <= w && r.bottom() <= h, "{w}x{h}: {r:?}");
        }
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn sliders_drag_to_both_limits_and_save_nested_spacing() {
        let (mut app, mut t, path) = setup("slider");
        click(&mut app, &mut t, "Layout");
        screen(&mut app, &mut t);
        let (_, track, _) = app.settings_dlg.as_ref().unwrap().hits.rows[0];
        let mouse = |app: &mut App, kind, x| {
            app.on_mouse(MouseEvent { kind, column: x, row: track.y, modifiers: KeyModifiers::NONE });
        };
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), track.x);
        assert_eq!(app.settings_dlg.as_ref().unwrap().draft["termcode.spacing"]["sidebarWidth"], 12);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), track.right() + 10);
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), track.right() + 10);
        assert_eq!(app.settings_dlg.as_ref().unwrap().draft["termcode.spacing"]["sidebarWidth"], 200);
        assert_eq!(app.settings.spacing.sidebar_width, 36);
        click(&mut app, &mut t, "Save");
        assert_eq!(settings::load(&path).unwrap().spacing.sidebar_width, 200);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_then_preview_then_discard_returns_to_the_last_save() {
        let (mut app, mut t, path) = setup("save-preview");
        click(&mut app, &mut t, "Editor");
        click(&mut app, &mut t, "Tab size");
        press(&mut app, KeyCode::Right);
        click(&mut app, &mut t, "Save");
        click(&mut app, &mut t, "Tab size");
        press(&mut app, KeyCode::Right);
        click(&mut app, &mut t, "Apply");
        assert_eq!(app.settings.tab_size, 6);
        press(&mut app, KeyCode::Esc);
        click(&mut app, &mut t, "Discard");
        assert_eq!(app.settings.tab_size, 5);
        assert_eq!(settings::load(&path).unwrap().tab_size, 5);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn quick_keys_show_as_a_keyboard_and_a_key_takes_a_command_from_its_list() {
        let (mut app, mut t, path) = setup("keyboard");
        search(&mut app, "Quick keys");
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Ctrl+T, then") && s.contains("Space") && s.contains("runs a command"), "the keyboard, on the page: {s}");
        let key = |app: &App, ch: char| {
            let d = app.settings_dlg.as_ref().unwrap();
            d.hits.keys.iter().find(|k| k.1 == (KeyCode::Char(ch), KeyModifiers::NONE)).unwrap().0
        };
        let at = |app: &mut App, kind, r: Rect| app.on_mouse(MouseEvent { kind, column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE });
        let draft = |app: &App| app.settings_dlg.as_ref().unwrap().draft.get(QUICK).cloned();
        // Pointing at a key: a card with what it runs.
        let (q, tk) = (key(&app, 'q'), key(&app, 't'));
        at(&mut app, MouseEventKind::Moved, q);
        assert!(screen(&mut app, &mut t).contains(" Q  No command "));
        at(&mut app, MouseEventKind::Moved, tk);
        assert!(screen(&mut app, &mut t).contains("T  View: Toggle Terminal"));
        // Clicking one: its list; typing finds a command, Enter gives it to the key.
        at(&mut app, MouseEventKind::Down(MouseButton::Left), q);
        for c in "toggle word wrap".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let s = screen(&mut app, &mut t);
        assert!(s.contains("Q runs") && s.contains("View: Toggle Word Wrap"), "{s}");
        press(&mut app, KeyCode::Enter);
        assert_eq!(draft(&app), Some(serde_json::json!({"q": "View: Toggle Word Wrap"})));
        // A default key set to none is written as ""; given its default again, it is not written.
        at(&mut app, MouseEventKind::Down(MouseButton::Left), tk);
        for _ in 0..40 {
            press(&mut app, KeyCode::PageUp);
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(draft(&app), Some(serde_json::json!({"q": "View: Toggle Word Wrap", "t": ""})));
        screen(&mut app, &mut t);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Enter);
        for c in "toggle terminal".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(draft(&app), Some(serde_json::json!({"q": "View: Toggle Word Wrap"})));
        // Esc: back to the settings; Save keeps it.
        press(&mut app, KeyCode::Esc);
        assert!(app.settings_dlg.as_ref().unwrap().board.is_none());
        assert!(app.settings_dlg.is_some(), "Esc left the keyboard, not the dialog");
        click(&mut app, &mut t, "Save");
        let c = (KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(app.settings.prefix_keys.contains(&(c, "View: Toggle Word Wrap".into())));
        let saved = settings::parse_jsonc(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved.get(QUICK), Some(&serde_json::json!({"q": "View: Toggle Word Wrap"})));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
