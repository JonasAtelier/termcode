//! Categorized settings dialog, with reversible previews and explicit persistence.

use crate::panel::{App, Doc, Drag};
use crate::settings::{self, Settings};
use crate::theme::{icons, P};
use crate::ui::popup_block;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::prelude::*;
use ratatui::widgets::{Clear, Paragraph};
use serde_json::{Map, Value};

const CATEGORIES: &[&str] =
    &["Appearance", "Layout", "Editor", "Minimap", "Indent Rainbow", "Files & Session", "Source Control"];

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
    Field {
        category: 3,
        label: "Minimap size",
        key: "editor.minimap.size",
        kind: Choice(&["fill", "proportional", "fit"]),
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
    Field {
        category: 5,
        label: "Sticky scroll rows",
        key: "workbench.tree.stickyScrollMaxItemCount",
        kind: Number(1, 10, 1),
    },
    Field { category: 6, label: "Status bar blame", key: "git.blame.statusBarItem.enabled", kind: Check(true) },
    Field { category: 6, label: "Inline blame", key: "git.blame.editorDecoration.enabled", kind: Check(false) },
    Field { category: 6, label: "Confirm sync", key: "git.confirmSync", kind: Check(true) },
];

#[derive(Clone, Copy, PartialEq)]
enum Focus {
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
            });
        }
    }
}

impl Dialog {
    fn rows(&self) -> Vec<usize> {
        FIELDS.iter().enumerate().filter(|(_, f)| f.category == self.category).map(|(i, _)| i).collect()
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
        self.selected = 0;
        self.scroll = 0;
        self.dropdown = None;
        self.focus = Focus::Categories;
    }

    fn tab(&mut self, back: bool) {
        let count = self.rows().len();
        let current = match self.focus {
            Focus::Categories => 0,
            Focus::Fields => self.selected + 1,
            Focus::Apply => count + 1,
            Focus::Save => count + 2,
        };
        let next = (current as i32 + if back { -1 } else { 1 }).rem_euclid((count + 3) as i32) as usize;
        self.focus = if next == 0 {
            Focus::Categories
        } else if next <= count {
            self.selected = next - 1;
            Focus::Fields
        } else if next == count + 1 {
            Focus::Apply
        } else {
            Focus::Save
        };
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
        let rows = self.rows();
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
                if !self.confirm && self.hits.content.contains(p) && self.commit_edit() =>
            {
                let rows = self.rows();
                let step = (self.hits.content.height as usize / 3).max(1);
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
    let active = Style::new().fg(P.button_fg).bg(P.button_bg);
    d.hits.title = Rect::new(r.x + 1, r.y, w - 2, 1);
    text(
        f,
        Rect::new(r.x + 1, r.y, w - 6, 1),
        if d.dirty() { " Settings *  (drag to move) " } else { " Settings  (drag to move) " },
        st.add_modifier(Modifier::BOLD),
    );
    d.hits.close = Rect::new(r.right() - 4, r.y, 3, 1);
    text(f, d.hits.close, &format!(" {} ", icons().close), st);
    let nav_w = if w >= 64 { 20 } else { 15 };
    let footer_y = r.bottom() - 5;
    let divider_x = r.x + nav_w;
    let mut content = Rect::new(divider_x + 2, r.y + 2, w.saturating_sub(nav_w + 4), h - 8);
    let graphics = d.graphics.enabled() && !d.confirm && d.dropdown.is_none();
    let preview = (d.category == 0 && graphics && content.width >= 65 && content.height >= 17)
        .then(|| {
            let area = Rect::new(content.right() - 26, content.y, 26, content.height);
            content.width -= 29;
            area
        });
    d.hits.content = content;
    let rows = d.rows();
    d.selected = d.selected.min(rows.len() - 1);
    let capacity = (content.height as usize / 3).max(1);
    if d.selected < d.scroll {
        d.scroll = d.selected;
    }
    if d.selected >= d.scroll + capacity {
        d.scroll = d.selected + 1 - capacity;
    }
    let nav_count = ((h - 7) / 2).max(1) as usize;
    let nav_off = d.category.saturating_sub(nav_count - 1);
    for (slot, (i, label)) in CATEGORIES.iter().enumerate().skip(nav_off).take(nav_count).enumerate() {
        let y = r.y + 2 + slot as u16 * 2;
        if y >= footer_y {
            break;
        }
        let row = Rect::new(r.x + 1, y, nav_w - 1, 1);
        let style = if i == d.category { active } else { st };
        text(f, row, &format!(" {label}"), style);
        d.hits.categories.push((row, i));
    }
    let border = Style::new().fg(P.widget_border).bg(P.widget_bg);
    for y in r.y + 1..footer_y {
        text(f, Rect::new(divider_x, y, 1, 1), icons().border[5], border);
    }
    text(f, Rect::new(r.x + 1, footer_y, w - 2, 1), &icons().border[4].repeat((w - 2) as usize), border);
    for (x, glyph) in [(divider_x, "┴"), (r.x, "├"), (r.right() - 1, "┤")] {
        text(f, Rect::new(x, footer_y, 1, 1), if crate::theme::ascii() { "+" } else { glyph }, border);
    }
    for (slot, &row) in rows.iter().skip(d.scroll).take(capacity).enumerate() {
        let y = content.y + slot as u16 * 3;
        let field = &FIELDS[row];
        let selected = d.focus == Focus::Fields && rows[d.selected] == row;
        let label_style = if selected { st.add_modifier(Modifier::BOLD).fg(P.focus_border) } else { st };
        text(f, Rect::new(content.x, y, content.width, 1), field.label, label_style);
        let control = Rect::new(content.x, y + 1, content.width,
            if matches!(field.kind, Check(_)) && graphics && d.graphics.pixel_controls() && content.width >= 15 { 2 } else { 1 });
        let value = d.value(row);
        let style = if selected { Style::new().fg(P.input_fg).bg(P.input_bg) } else { st };
        match field.kind {
            Check(default) => {
                let on = value.as_bool().unwrap_or(default);
                if control.height == 2 {
                    let switch = Rect::new(control.x, control.y, 6, 2);
                    let bg = if selected { P.input_bg } else { P.widget_bg };
                    d.graphics.switch(f, switch, on, selected,
                        [bg, if on { P.button_bg } else { P.widget_border }, P.button_fg]);
                    text(f, Rect::new(control.x + 7, control.y, control.width - 7, 1),
                        if on { "Enabled" } else { "Disabled" }, style);
                } else {
                    text(f, control, if on { "[x] Enabled" } else { "[ ] Disabled" }, style);
                }
            }
            Number(lo, hi, default) => {
                let n = value.as_u64().unwrap_or(default as u64).clamp(lo as u64, hi as u64);
                let track_w = control.width.saturating_sub(8).clamp(1, 28);
                let track = Rect::new(control.x + 1, control.y, track_w, 1);
                let pos = ((n - lo as u64) * track_w.saturating_sub(1) as u64 / (hi - lo).max(1) as u64) as usize;
                let bar: String = (0..track_w as usize)
                    .map(|i| {
                        if i == pos {
                            'O'
                        } else if i < pos {
                            '='
                        } else {
                            '-'
                        }
                    })
                    .collect();
                text(f, control, &format!("[{bar}] {n}"), style);
                d.hits.rows.push((Rect::new(content.x, y, content.width, 2), track, row));
                continue;
            }
            Choice(_) => text(f, control, &format!("[ {}  v ]", d.display(row)), style),
            Text(_) | Json(_) => {
                if let Some(e) = d.edit.as_ref().filter(|e| e.row == row) {
                    let room = control.width.saturating_sub(2) as usize;
                    let before = &e.text[..e.cursor];
                    let tail: String = before
                        .chars()
                        .rev()
                        .take(room.saturating_sub(1))
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    let shown = format!("{tail}|{}", &e.text[e.cursor..]);
                    text(f, control, &shown, if e.all { active } else { style });
                } else {
                    text(f, control, &format!("[ {} ]", d.display(row)), style);
                }
            }
        }
        d.hits.rows.push((Rect::new(content.x, y, content.width, control.height + 1), control, row));
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
            text(f, Rect::new(area.x, y, area.width, 1), label, st.fg(P.dim));
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
    let selected_row = rows[d.selected];
    let key = FIELDS[selected_row].key.replace('/', ".");
    text(f, Rect::new(content.x, footer_y - 1, content.width, 1), &key, st.fg(P.dim));
    text(f, Rect::new(r.x + 2, r.bottom() - 4, w - 4, 1), &d.note, st);
    text(
        f,
        Rect::new(r.x + 2, r.bottom() - 3, w - 4, 1),
        "Tab: focus  Arrows: adjust  Enter: edit  Esc: close",
        st.fg(P.dim),
    );
    for (index, label, focus) in [(0, "Apply", Focus::Apply), (1, "Save", Focus::Save)] {
        let button = Rect::new(r.right() - 21 + index as u16 * 10, r.bottom() - 2, 9, 1);
        let style = if d.focus == focus { active.add_modifier(Modifier::BOLD) } else { active };
        f.render_widget(Paragraph::new(label).alignment(Alignment::Center).style(style), button);
        crate::ui::round_ends(f, button, P.button_bg, P.widget_bg);
        d.hits.buttons.push((button, index));
    }
    text(
        f,
        Rect::new(r.x + 2, r.bottom() - 2, w.saturating_sub(25), 1),
        &format!("{}-{} / {}", d.scroll + 1, (d.scroll + capacity).min(rows.len()), rows.len()),
        st.fg(P.dim),
    );
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
            let style = if index == d.confirm_selected { active.add_modifier(Modifier::BOLD) } else { active };
            f.render_widget(Paragraph::new(crate::ui::fit(label, bw as usize)).alignment(Alignment::Center).style(style), button);
            if bw as usize >= label.len() + 2 {
                crate::ui::round_ends(f, button, P.button_bg, P.widget_bg);
            }
            d.hits.buttons.push((button, index));
        }
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

/// The settings.json a new user starts with (and `termcode --default-config` prints):
/// this window's options in its categories, each commented out at its default, so
/// uncommenting a line is all it takes to change one.
pub fn template() -> String {
    let mut out = String::from(
        "// termcode settings: the options of the Settings window (Ctrl+,), each at its default.\n\
         // Uncomment a line and change its value; saving applies it. This is VS Code's\n\
         // settings.json format: comments and trailing commas are fine. A broken file is\n\
         // reported in the status bar and the last good settings stay. More: README.md, Settings.\n{\n",
    );
    let line = |out: &mut String, indent: &str, key: &str, value: &Value, hint: &str| {
        let entry = format!("\"{key}\": {},", serde_json::to_string(value).unwrap());
        out.push_str(&format!("    // {indent}{entry:<width$} // {hint}\n", width = 52 - indent.len()));
    };
    for (c, name) in CATEGORIES.iter().enumerate() {
        out.push_str(&format!("\n    // ---- {name} ----\n"));
        let mut group: Option<&str> = None;
        for f in FIELDS.iter().filter(|f| f.category == c) {
            let hint = match f.kind {
                Choice(o) => format!("{}: {}", f.label, o.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ")),
                Number(lo, hi, _) => format!("{} ({lo} to {hi})", f.label),
                _ => f.label.trim_end_matches(" (JSON)").to_string(),
            };
            let (key, member) = f.key.split_once('/').map_or((f.key, None), |(k, m)| (k, Some(m)));
            if group.is_some() && group != member.map(|_| key) {
                out.push_str("    // },\n");
                group = None;
            }
            match member {
                Some(m) => {
                    if group.is_none() {
                        out.push_str(&format!("    // \"{key}\": {{   // the compact density starts from smaller values\n"));
                        group = Some(key);
                    }
                    line(&mut out, "    ", m, &default_of(f.kind), &hint);
                }
                None => line(&mut out, "", key, &default_of(f.kind), &hint),
            }
        }
        if group.is_some() {
            out.push_str("    // },\n");
        }
    }
    out.push_str("}\n");
    out
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
        let symbols: String = (control.x..control.x + 4)
            .map(|x| t.backend().buffer()[(x, control.y)].symbol()).collect();
        assert_eq!(symbols, "[x] ", "small switches must not become low-resolution image blocks");
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: control.x + 2, row: control.y, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.settings_dlg.as_ref().unwrap().draft["editor.minimap.enabled"], false);
        assert!(app.settings.minimap.enabled, "a switch edits only the draft");
        assert!(screen(&mut app, &mut t).contains("[ ] Disabled"));
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
}
