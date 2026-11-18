//! settings.json in VS Code's format (JSONC: comments and trailing commas) from
//! ~/.config/termcode (`data_dir`). A missing file means the defaults; a bad one is
//! an error the panel shows while it keeps the last good settings.

use crate::decor::{js_regex, Rainbow};
use crate::icons::FileIcons;
use crate::theme::{self, jsonc_to_json, Rgba};
use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

/// The id the embedded cpptools theme has in VS Code.
pub const EMBEDDED_ID: &str = "Visual Studio Dark - C++";

/// `termcode.spacing`. The density gives the defaults; each key overrides one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spacing {
    /// Sidebar columns until the divider is dragged (a drag is persisted).
    pub sidebar_width: u16,
    /// Inner left/right padding of sidebar rows and inputs.
    pub sidebar_padding: u16,
    /// Blank rows between sidebar sections and headers.
    pub section_gap: u16,
    /// Extra columns before the right-aligned line numbers in editors, diffs and peeks.
    pub editor_padding: u16,
    /// Tab padding budget; the UI halves it and shares it across both edges.
    pub tab_padding: u16,
    /// Spaces between status bar items.
    pub status_gap: u16,
    /// Activity bar columns (icons centred).
    pub activity_width: u16,
    /// Blank rows between sidebar list items (0 or 1).
    pub list_gap: u16,
    /// Horizontal and vertical padding inside framed popups and dialogs.
    pub popup_padding: (u16, u16),
    /// Blank cells after an icon before its label (at least 1: a non-Mono Nerd
    /// Font icon can overflow its cell).
    pub icon_gap: u16,
    /// Cells per tree depth level.
    pub tree_indent: u16,
    /// Left/right padding in the title bar.
    pub title_padding: u16,
    /// Spaces inside buttons (Close, Restore, Save, …).
    pub button_padding: u16,
    /// Horizontal padding inside the find/replace widget.
    pub find_padding: u16,
    /// Left/right edge padding of the status bar.
    pub status_padding: u16,
}

impl Spacing {
    pub const COMFORTABLE: Spacing = Spacing {
        // Content widths: cards add their 2-cell frame (38 and 7 wide then).
        sidebar_width: 36,
        sidebar_padding: 2,
        section_gap: 1,
        // The number field already reserves room for at least two digits.
        editor_padding: 0,
        tab_padding: 2,
        status_gap: 3,
        activity_width: 5,
        list_gap: 0,
        popup_padding: (3, 1),
        icon_gap: 2,
        tree_indent: 2,
        title_padding: 2,
        button_padding: 2,
        find_padding: 1,
        status_padding: 2,
    };
    /// The original tight layout.
    pub const COMPACT: Spacing = Spacing {
        sidebar_width: 30,
        sidebar_padding: 0,
        section_gap: 0,
        editor_padding: 0,
        tab_padding: 1,
        status_gap: 3,
        activity_width: 3,
        list_gap: 0,
        popup_padding: (0, 0),
        icon_gap: 1,
        tree_indent: 2,
        title_padding: 1,
        button_padding: 1,
        find_padding: 0,
        status_padding: 1,
    };

    /// `base` with the keys `v` sets, each clamped to a usable range.
    pub fn parse(base: Spacing, v: &Value) -> Spacing {
        let n = |k: &str, lo: u16, hi: u16, d: u16| v[k].as_u64().map_or(d, |x| x.clamp(lo as u64, hi as u64) as u16);
        let pp = &v["popupPadding"];
        Spacing {
            sidebar_width: n("sidebarWidth", 12, 200, base.sidebar_width),
            sidebar_padding: n("sidebarPadding", 0, 4, base.sidebar_padding),
            section_gap: n("sectionGap", 0, 3, base.section_gap),
            editor_padding: n("editorPadding", 0, 8, base.editor_padding),
            tab_padding: n("tabPadding", 0, 4, base.tab_padding),
            status_gap: n("statusBarGap", 1, 8, base.status_gap),
            activity_width: n("activityBarWidth", 2, 9, base.activity_width),
            list_gap: n("listItemGap", 0, 1, base.list_gap),
            popup_padding: (
                pp[0].as_u64().map_or(base.popup_padding.0, |x| x.min(8) as u16),
                pp[1].as_u64().map_or(base.popup_padding.1, |x| x.min(4) as u16),
            ),
            icon_gap: n("iconGap", 1, 4, base.icon_gap),
            tree_indent: n("treeIndent", 0, 8, base.tree_indent),
            title_padding: n("titleBarPadding", 0, 6, base.title_padding),
            button_padding: n("buttonPadding", 0, 4, base.button_padding),
            find_padding: n("findWidgetPadding", 0, 3, base.find_padding),
            status_padding: n("statusBarPadding", 0, 6, base.status_padding),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    /// `workbench.density: "compact"`: the original tight layout, no popup frames.
    pub compact: bool,
    pub spacing: Spacing,
    /// `termcode.icons`: a fixed set, or None for "auto" (`theme::auto_icons`).
    pub icons: Option<crate::theme::IconSet>,
    /// `termcode.colors`: a fixed colour depth, or None for auto (`colors::detect`).
    pub colors: Option<crate::colors::Depth>,
    /// `termcode.keybindings`: key -> F1 command name ("" turns the key off).
    pub keybindings: Vec<(crate::parse::Chord, String)>,
    /// `termcode.prefix` (Ctrl+T): the key before a quick key; None when "" (off).
    pub prefix: Option<crate::parse::Chord>,
    /// `termcode.prefixKeys` over `DEFAULT_PREFIX_KEYS`: quick key -> F1 command.
    pub prefix_keys: Vec<(crate::parse::Chord, String)>,
    /// `termcode.errorChecking`: shellcheck and the C/C++ compiler underline problems.
    pub error_checking: bool,
    pub file_icons: FileIcons,
    pub rainbow: Rainbow,
    pub guides: bool,
    /// `workbench.tree.enableStickyScroll`, `workbench.tree.stickyScrollMaxItemCount`.
    pub sticky: bool,
    pub sticky_max: usize,
    pub tab_size: usize,
    /// `files.trimTrailingWhitespace` and `files.insertFinalNewline`, applied on save.
    /// `editor.renderWhitespace`.
    pub render_whitespace: crate::editor::Whitespace,
    /// `editor.occurrencesHighlight`: other uses of the word at the cursor.
    pub occurrences: bool,
    /// `editor.matchBrackets`: highlight the bracket pair at the cursor.
    pub match_brackets: bool,
    /// `editor.bracketPairColorization.enabled`: brackets coloured by nesting depth.
    pub bracket_colors: bool,
    /// `editor.wordWrap`; unset wraps Markdown only, as VS Code's `[markdown]` default does.
    pub word_wrap: Option<bool>,
    /// `breadcrumbs.enabled`: the path and symbol row above the text.
    pub breadcrumbs: bool,
    /// `editor.autoClosingBrackets`, `editor.autoClosingQuotes`.
    pub auto_brackets: bool,
    pub auto_quotes: bool,
    pub trim_trailing: bool,
    pub final_newline: bool,
    /// `editor.rulers`: display columns, each with an optional own colour.
    pub rulers: Vec<(usize, Option<Rgba>)>,
    pub follow_vscode: bool,
    pub minimap: crate::minimap::Cfg,
    /// `editor.gotoLocation.multipleDefinitions`.
    pub goto_multiple: crate::navui::GotoMode,
    pub theme: ThemeData,
    /// `git.blame.statusBarItem.enabled`: the cursor line's blame in the status bar.
    pub blame_status: bool,
    /// `termcode.blocks.style`: cards, lines or none between the blocks.
    pub blocks: crate::blocks::BlockStyle,
    /// `git.blame.editorDecoration.enabled`: the same as ghost text after the cursor line.
    pub blame_decoration: bool,
    /// `git.confirmSync`: ask before Sync pulls and pushes.
    pub confirm_sync: bool,
    /// `workbench.editor.restoreViewState`: cursors and scroll come back with a file.
    pub restore_view_state: bool,
    /// `explorer.autoReveal`: the active editor's file shown in the Explorer.
    pub auto_reveal: bool,
    /// `termcode.restoreSession`: the groups, tabs and sidebar of the last session.
    pub restore_session: bool,
    /// Parts that were skipped (a bad regex): shown, the rest still applies.
    pub warnings: Vec<String>,
}

/// A resolved colour theme with the customizations folded in.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeData {
    /// VS Code theme JSON (merged through `include`): colors, tokenColors, semanticTokenColors.
    pub json: Value,
    /// Token colour customizations, applied after the theme's rules.
    pub extra_rules: Vec<Value>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            compact: false,
            spacing: Spacing::COMFORTABLE,
            icons: None,
            error_checking: true,
            colors: None,
            keybindings: Vec::new(),
            prefix: crate::parse::parse_chord(DEFAULT_PREFIX).ok(),
            prefix_keys: prefix_keys(None).unwrap_or_default(),
            file_icons: FileIcons::default(),
            rainbow: Rainbow::default(),
            guides: true,
            sticky: true,
            // One row: the top-level folder holding the top row (VS Code's default is 7).
            sticky_max: 1,
            tab_size: 4,
            render_whitespace: crate::editor::Whitespace::Boundary,
            occurrences: true,
            match_brackets: true,
            bracket_colors: true,
            word_wrap: None,
            breadcrumbs: true,
            auto_brackets: true,
            auto_quotes: true,
            trim_trailing: true,
            final_newline: true,
            rulers: vec![(90, None)],
            follow_vscode: false,
            minimap: crate::minimap::Cfg::default(),
            goto_multiple: Default::default(),
            theme: ThemeData { json: theme::embedded_theme().clone(), extra_rules: Vec::new() },
            blame_status: true,
            blocks: crate::blocks::BlockStyle::Cards,
            blame_decoration: false,
            confirm_sync: true,
            restore_view_state: true,
            auto_reveal: true,
            restore_session: true,
            warnings: Vec::new(),
        }
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// `~/.config/termcode/settings.json` (`data_dir`).
pub fn path() -> PathBuf {
    crate::data_dir(false).join("settings.json")
}

/// VS Code's user settings, for `termcode.followVSCode`.
pub fn vscode_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("Code/User/settings.json")
}

/// Where VS Code themes can live: the built-in extensions, then the user's.
pub fn theme_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = [
        "/snap/code/current/usr/share/code/resources/app/extensions",
        "/usr/share/code/resources/app/extensions",
        "/opt/visual-studio-code/resources/app/extensions",
        "/usr/lib/code/extensions",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    v.push(home().join(".vscode/extensions"));
    v.retain(|d| d.is_dir());
    v
}

/// JSONC text to an object; an empty file is `{}` as in VS Code.
pub fn parse_jsonc(text: &str) -> Result<Map<String, Value>, String> {
    let json = jsonc_to_json(text);
    if json.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str(&json).map_err(|e| e.to_string())? {
        Value::Object(m) => Ok(m),
        _ => Err("the top level must be an object".into()),
    }
}

fn read_jsonc(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    read_text(path)?.map(|text| parse_jsonc(&text).map_err(|e| format!("{}: {e}", path.display()))).transpose()
}

/// Read a settings source without allowing an external edit to allocate unbounded memory.
pub fn read_text(path: &Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let err = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(&e)),
    };
    let mut text = String::new();
    f.take(crate::editor::MAX_BYTES + 1).read_to_string(&mut text).map_err(|e| err(&e))?;
    if text.len() as u64 > crate::editor::MAX_BYTES {
        return Err(err(&"larger than 1 MB"));
    }
    Ok(Some(text))
}

/// The key before a quick key: rarely a shell's, and none of termcode's or herdr's own.
pub const DEFAULT_PREFIX: &str = "ctrl+t";

/// The quick keys after the prefix: mnemonic, tmux's where it has one (c n p).
pub const DEFAULT_PREFIX_KEYS: &str = r#"{
    "t": "View: Toggle Terminal",
    "c": "Terminal: Create New Terminal",
    "n": "Terminal: Focus Next Terminal",
    "p": "Terminal: Focus Previous Terminal",
    "x": "Terminal: Close Terminal",
    "f": "Go to File...",
    "s": "View: Show Search",
    "e": "View: Show Explorer",
    "g": "View: Show Source Control",
    "b": "View: Toggle Primary Side Bar Visibility",
    "o": "View: Toggle Outline",
    "m": "View: Toggle Minimap",
    "z": "View: Toggle Word Wrap",
    "v": "Markdown: Toggle Preview",
    "h": "View: Toggle Hexdump",
    "[": "View: Open Previous Editor",
    "]": "View: Open Next Editor",
    ",": "Preferences: Open Settings",
    "left": "View: Navigate Left",
    "right": "View: Navigate Right",
    "up": "View: Navigate Up",
    "down": "View: Navigate Down"
}"#;

/// `termcode.prefixKeys`: the defaults, each key of the user's over them ("" removes it).
/// A key or command that does not exist is an error, as for keybindings.
fn prefix_keys(v: Option<&Value>) -> Result<Vec<(crate::parse::Chord, String)>, String> {
    let defaults: Value = serde_json::from_str(DEFAULT_PREFIX_KEYS).expect("valid");
    let mut out: Vec<(crate::parse::Chord, String)> = Vec::new();
    let user = match v {
        Some(v) => Some(v.as_object().ok_or("termcode.prefixKeys: an object of \"key\": \"command\"")?),
        None => None,
    };
    for map in [defaults.as_object(), user].into_iter().flatten() {
        for (key, cmd) in map {
            let chord = crate::parse::parse_chord(key).map_err(|e| format!("termcode.prefixKeys: {e}"))?;
            let cmd = cmd.as_str().ok_or_else(|| format!("termcode.prefixKeys: {key:?} needs a command name"))?;
            if !cmd.is_empty() && !crate::panel::all_commands().any(|(l, _)| l == cmd) {
                return Err(format!("termcode.prefixKeys: no command {cmd:?} (F1 lists them)"));
            }
            out.retain(|(c, _)| *c != chord);
            if !cmd.is_empty() {
                out.push((chord, cmd.to_string()));
            }
        }
    }
    Ok(out)
}

/// `termcode.keybindings`: `{ "ctrl+alt+p": "Git: Pull" }`. A key or command that does
/// not exist is an error, so a typo shows instead of a key that silently does nothing.
fn keybindings(v: Option<&Value>) -> Result<Vec<(crate::parse::Chord, String)>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let map = v.as_object().ok_or("termcode.keybindings: an object of \"key\": \"command\"")?;
    map.iter().map(|(key, cmd)| {
        let chord = crate::parse::parse_chord(key).map_err(|e| format!("termcode.keybindings: {e}"))?;
        let cmd = cmd.as_str().ok_or_else(|| format!("termcode.keybindings: {key:?} needs a command name"))?;
        if !cmd.is_empty() && !crate::panel::all_commands().any(|(l, _)| l == cmd) {
            return Err(format!("termcode.keybindings: no command {cmd:?} (F1 lists them)"));
        }
        Ok((chord, cmd.to_string()))
    }).collect()
}

/// Read and resolve the settings file (and VS Code's, when followed).
pub fn load(path: &Path) -> Result<Settings, String> {
    let user = read_jsonc(path)?.unwrap_or_default();
    resolve(&user)
}

/// Resolve an in-memory draft through the same path as settings.json.
pub fn resolve(user: &Map<String, Value>) -> Result<Settings, String> {
    let vscode = if user.get("termcode.followVSCode").and_then(Value::as_bool) == Some(true) {
        Some(read_jsonc(&vscode_path())?.unwrap_or_default())
    } else {
        None
    };
    from_maps(user, vscode.as_ref(), &theme_dirs())
}

pub fn from_maps(user: &Map<String, Value>, vscode: Option<&Map<String, Value>>, dirs: &[PathBuf]) -> Result<Settings, String> {
    let d = Settings::default();
    // With followVSCode, VS Code's theme keys win where it sets them.
    let tkey = |k: &str| vscode.and_then(|v| v.get(k)).or_else(|| user.get(k));
    let sel = tkey("workbench.colorTheme").and_then(Value::as_str);
    let mut json = resolve_theme(sel, dirs)?;
    let names: Vec<String> =
        [sel.unwrap_or(EMBEDDED_ID).to_string(), json["name"].as_str().unwrap_or("").to_string()].into();
    customize_colors(&mut json, tkey("workbench.colorCustomizations"), &names);
    let extra_rules = token_rules(tkey("editor.tokenColorCustomizations"), &names);
    let g = |k: &str| user.get(k);
    let (rainbow, warnings) = rainbow(user);
    let compact = g("workbench.density").and_then(Value::as_str) == Some("compact");
    let base = if compact { Spacing::COMPACT } else { Spacing::COMFORTABLE };
    Ok(Settings {
        compact,
        spacing: Spacing::parse(base, g("termcode.spacing").unwrap_or(&Value::Null)),
        error_checking: g("termcode.errorChecking").and_then(Value::as_bool).unwrap_or(true),
        colors: g("termcode.colors").and_then(Value::as_str).and_then(crate::colors::parse),
        keybindings: keybindings(g("termcode.keybindings"))?,
        prefix: match g("termcode.prefix").map(|v| v.as_str().ok_or("termcode.prefix: a key, as \"ctrl+t\"")) {
            None => crate::parse::parse_chord(DEFAULT_PREFIX).ok(),
            Some(Ok("")) => None,
            Some(Ok(k)) => Some(crate::parse::parse_chord(k).map_err(|e| format!("termcode.prefix: {e}"))?),
            Some(Err(e)) => return Err(e.into()),
        },
        prefix_keys: prefix_keys(g("termcode.prefixKeys"))?,
        icons: match g("termcode.icons").and_then(Value::as_str) {
            Some("nerd") => Some(crate::theme::IconSet::Nerd),
            Some("unicode") => Some(crate::theme::IconSet::Unicode),
            Some("ascii") => Some(crate::theme::IconSet::Ascii),
            _ => None,
        },
        file_icons: g("termcode.fileIcons").map(FileIcons::parse).unwrap_or_default(),
        rainbow,
        guides: g("editor.guides.indentation").and_then(Value::as_bool).unwrap_or(d.guides),
        sticky: g("workbench.tree.enableStickyScroll").and_then(Value::as_bool).unwrap_or(d.sticky),
        sticky_max: g("workbench.tree.stickyScrollMaxItemCount").and_then(Value::as_u64).map_or(d.sticky_max, |n| n.clamp(1, 10) as usize),
        render_whitespace: match g("editor.renderWhitespace").and_then(Value::as_str) {
            Some("none") => crate::editor::Whitespace::None,
            Some("selection") => crate::editor::Whitespace::Selection,
            Some("trailing") => crate::editor::Whitespace::Trailing,
            Some("all") => crate::editor::Whitespace::All,
            Some("boundary") => crate::editor::Whitespace::Boundary,
            _ => d.render_whitespace,
        },
        occurrences: !matches!(g("editor.occurrencesHighlight"), Some(Value::Bool(false))) && g("editor.occurrencesHighlight").and_then(Value::as_str) != Some("off"),
        match_brackets: g("editor.matchBrackets").and_then(Value::as_str) != Some("never"),
        bracket_colors: g("editor.bracketPairColorization.enabled").and_then(Value::as_bool).unwrap_or(d.bracket_colors),
        word_wrap: g("editor.wordWrap").and_then(Value::as_str).map(|w| w != "off"),
        breadcrumbs: g("breadcrumbs.enabled").and_then(Value::as_bool).unwrap_or(d.breadcrumbs),
        auto_brackets: g("editor.autoClosingBrackets").and_then(Value::as_str) != Some("never"),
        auto_quotes: g("editor.autoClosingQuotes").and_then(Value::as_str) != Some("never"),
        trim_trailing: g("files.trimTrailingWhitespace").and_then(Value::as_bool).unwrap_or(d.trim_trailing),
        final_newline: g("files.insertFinalNewline").and_then(Value::as_bool).unwrap_or(d.final_newline),
        tab_size: g("editor.tabSize").and_then(Value::as_u64).filter(|n| (1..=16).contains(n)).map_or(d.tab_size, |n| n as usize),
        rulers: g("editor.rulers").and_then(Value::as_array).map_or(d.rulers, |a| rulers(a)),
        follow_vscode: vscode.is_some(),
        minimap: crate::minimap::Cfg::parse(user),
        goto_multiple: crate::navui::goto_mode(g("editor.gotoLocation.multipleDefinitions")),
        theme: ThemeData { json, extra_rules },
        blame_status: g("git.blame.statusBarItem.enabled").and_then(Value::as_bool).unwrap_or(d.blame_status),
        blocks: crate::blocks::parse(g("termcode.blocks.style"), compact),
        blame_decoration: g("git.blame.editorDecoration.enabled").and_then(Value::as_bool).unwrap_or(d.blame_decoration),
        confirm_sync: g("git.confirmSync").and_then(Value::as_bool).unwrap_or(d.confirm_sync),
        restore_view_state: g("workbench.editor.restoreViewState").and_then(Value::as_bool).unwrap_or(d.restore_view_state),
        auto_reveal: g("explorer.autoReveal").and_then(Value::as_bool).unwrap_or(d.auto_reveal),
        restore_session: g("termcode.restoreSession").and_then(Value::as_bool).unwrap_or(d.restore_session),
        warnings,
    })
}

/// `editor.rulers`: `[90]`, or VS Code's `[{ "column": 80, "color": "#rrggbb" }, 100]`.
fn rulers(a: &[Value]) -> Vec<(usize, Option<Rgba>)> {
    a.iter()
        .filter_map(|r| match r {
            Value::Number(n) => Some((n.as_u64()? as usize, None)),
            Value::Object(o) => Some((o.get("column")?.as_u64()? as usize, o.get("color").and_then(Value::as_str).and_then(Rgba::parse))),
            _ => None,
        })
        .filter(|(c, _)| *c > 0)
        .collect()
}

/// Set a boolean key in settings.json text, keeping everything else byte for
/// byte. JSONC aware: strings and comments are skipped, so only a top-level
/// `"key": true|false` is replaced; otherwise the key goes after the object's `{`.
pub fn set_bool(text: &str, key: &str, on: bool) -> String {
    let b = text.as_bytes();
    let (mut i, mut depth, mut open) = (0, 0usize, None);
    // A string just read at the top level of the object: its text.
    let mut last: Option<&str> = None;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            b'"' => {
                let start = i + 1;
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                last = (depth == 1).then(|| &text[start..i.min(b.len())]);
                i += 1;
                continue;
            }
            b'{' | b'[' => {
                if depth == 0 && b[i] == b'{' && open.is_none() {
                    open = Some(i);
                }
                depth += 1;
                last = None;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                last = None;
            }
            b':' if depth == 1 && last == Some(key) => {
                let v = text[i + 1..].trim_start();
                let at = text.len() - v.len();
                for lit in ["true", "false"] {
                    if v.starts_with(lit) {
                        return format!("{}{on}{}", &text[..at], &text[at + lit.len()..]);
                    }
                }
                last = None;
            }
            b',' => last = None,
            _ => {}
        }
        i += 1;
    }
    match open {
        Some(o) => format!("{}\n    \"{key}\": {on},{}", &text[..=o], &text[o + 1..]),
        None if text.trim().is_empty() => format!("{{\n    \"{key}\": {on},\n}}\n"),
        None => text.to_string(),
    }
}

/// Write `key: on` into settings.json, or refuse and leave the file byte-identical.
/// The file must read as UTF-8 and parse; the edit must parse with the key set and
/// every other key unchanged. The write is the editor's atomic save (temp file,
/// fsync, rename, mode kept; a symlink's target). A missing file is created.
pub fn write_bool(path: &Path, key: &str, on: bool) -> Result<(), String> {
    let manual = || format!("add \"{key}\": {on} to settings.json manually");
    let text = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let made = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|_| {
                let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;
                std::io::Write::write_all(&mut f, set_bool("", key, on).as_bytes())?;
                f.sync_all()
            });
            return made.map_err(|e| format!("{}: {e}; {}", path.display(), manual()));
        }
        Err(e) => return Err(format!("{}: {e}; {}", path.display(), manual())),
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| format!("settings.json is not UTF-8; {}", manual()))?,
    };
    let old = parse_jsonc(&text).map_err(|e| format!("settings.json does not parse ({e}); {}", manual()))?;
    let new = set_bool(&text, key, on);
    let safe = parse_jsonc(&new).is_ok_and(|m| {
        m.get(key) == Some(&Value::Bool(on))
            && m.len() == old.len() + usize::from(!old.contains_key(key))
            && old.iter().all(|(k, v)| k == key || m.get(k) == Some(v))
    });
    if !safe {
        return Err(format!("could not edit settings.json safely; {}", manual()));
    }
    let mut b = crate::editor::Buffer::open(path);
    if let Some(why) = &b.readonly {
        return Err(format!("settings.json: {why}; {}", manual()));
    }
    let mut v = crate::editor::View::default();
    b.select_all(&mut v);
    let body = if b.final_newline { new.strip_suffix("\r\n").or_else(|| new.strip_suffix('\n')).unwrap_or(&new) } else { &new };
    b.insert(&mut v, body);
    if b.text() != new {
        return Err(format!("could not edit settings.json safely; {}", manual()));
    }
    b.save()
}

/// Update only changed top-level values, preserving JSONC comments and unrelated keys.
/// Refuse stale drafts rather than silently overwriting an external edit.
pub fn write_changes(path: &Path, expected: Option<&str>, changes: &Map<String, Value>) -> Result<String, String> {
    let current = read_text(path)?;
    if current.as_deref() != expected {
        return Err("settings.json changed on disk; close and reopen Settings before saving".into());
    }
    let old = current.as_deref().unwrap_or("");
    let mut want = parse_jsonc(old)?;
    let mut text = old.to_string();
    for (key, value) in changes {
        text = set_value(&text, key, value)?;
        want.insert(key.clone(), value.clone());
    }
    if text.len() as u64 > crate::editor::MAX_BYTES { return Err("settings.json would exceed 1 MB".into()); }
    if parse_jsonc(&text)? != want {
        return Err("could not edit settings.json safely".into());
    }
    if current.is_none() {
        if let Some(parent) = path.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
    }
    let mut b = if current.is_some() {
        let b = crate::editor::Buffer::open(path);
        if b.readonly.is_some() || b.text() != old {
            return Err("settings.json changed or cannot be edited safely".into());
        }
        b
    } else {
        let mut b = crate::editor::Buffer::from_text("", "JSON", "");
        b.readonly = None;
        b.path = Some(path.to_path_buf());
        b
    };
    let mut view = crate::editor::View::default();
    b.select_all(&mut view);
    let body = if b.final_newline { text.strip_suffix("\r\n").or_else(|| text.strip_suffix('\n')).unwrap_or(&text) } else { &text };
    b.insert(&mut view, body);
    if b.text() != text { return Err("could not edit settings.json safely".into()); }
    b.save()?;
    Ok(text)
}

fn set_value(text: &str, key: &str, value: &Value) -> Result<String, String> {
    fn trivia(b: &[u8], mut i: usize) -> usize {
        loop {
            while b.get(i).is_some_and(u8::is_ascii_whitespace) { i += 1; }
            if b.get(i..i + 2) == Some(b"//") {
                while i < b.len() && b[i] != b'\n' { i += 1; }
            } else if b.get(i..i + 2) == Some(b"/*") {
                i += 2;
                while i + 1 < b.len() && &b[i..i + 2] != b"*/" { i += 1; }
                i = (i + 2).min(b.len());
            } else { return i; }
        }
    }
    fn string_end(b: &[u8], mut i: usize) -> usize {
        i += 1;
        while i < b.len() {
            match b[i] {
                b'\\' => i += 2,
                b'"' => return i + 1,
                _ => i += 1,
            }
        }
        b.len()
    }
    // The caller validates JSONC first. Scan original bytes so comments and Unicode
    // outside the replaced value keep their exact positions and spelling.
    let parsed = parse_jsonc(text)?;
    let encoded = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let b = text.as_bytes();
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let open = trivia(b, 0);
    if b.get(open) != Some(&b'{') {
        return Ok(format!("{text}{nl}{{{nl}    {}: {encoded}{nl}}}{nl}", serde_json::to_string(key).unwrap()));
    }
    let mut i = open + 1;
    let mut found = None;
    loop {
        i = trivia(b, i);
        if b.get(i) == Some(&b'}') { break; }
        if i >= b.len() { return Err("incomplete settings object".into()); }
        let end_key = string_end(b, i);
        let name: String = serde_json::from_str(&text[i..end_key]).map_err(|e| e.to_string())?;
        i = trivia(b, end_key);
        if b.get(i) != Some(&b':') { return Err("missing settings colon".into()); }
        let start = trivia(b, i + 1);
        i = start;
        let (mut depth, mut end) = (0usize, start);
        loop {
            i = trivia(b, i);
            match b.get(i) {
                None => return Err("incomplete setting value".into()),
                Some(b',' | b'}') if depth == 0 => break,
                Some(b'"') => i = string_end(b, i),
                Some(b'{' | b'[') => { depth += 1; i += 1; }
                Some(b'}' | b']') => { depth -= 1; i += 1; }
                _ => i += 1,
            }
            end = i;
        }
        if name == key { found = Some(start..end); }
        if b.get(i) == Some(&b',') { i += 1; } else { break; }
    }
    if let Some(range) = found {
        let encoded = match (parsed.get(key).and_then(Value::as_object), value.as_object()) {
            (Some(old), Some(new)) if old.keys().all(|k| new.contains_key(k)) => {
                let mut object = text[range.clone()].to_string();
                for (k, v) in new.iter().filter(|(k, v)| old.get(*k) != Some(*v)) {
                    object = set_value(&object, k, v)?;
                }
                object
            }
            _ => encoded,
        };
        let mut out = text.to_string();
        out.replace_range(range, &encoded);
        Ok(out)
    } else {
        let comma = if parsed.is_empty() { "" } else { "," };
        Ok(format!("{}{nl}    {}: {encoded}{comma}{}", &text[..=open], serde_json::to_string(key).unwrap(), &text[open + 1..]))
    }
}

/// `indentRainbow.*`; a pattern that does not compile is skipped with a warning.
fn rainbow(user: &Map<String, Value>) -> (Rainbow, Vec<String>) {
    let mut r = Rainbow::default();
    let g = |k: &str| user.get(&format!("indentRainbow.{k}"));
    let strings = |v: &Value| v.as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect::<Vec<_>>());
    if let Some(b) = g("enabled").and_then(Value::as_bool) {
        r.enabled = b;
    }
    if let Some(c) = g("colors").and_then(strings) {
        r.colors = c.iter().filter_map(|c| Rgba::parse(c)).collect();
    }
    if let Some(c) = g("errorColor").and_then(Value::as_str) {
        // The extension falls back to a fainter red for an empty value.
        r.error = Rgba::parse(c).or_else(|| Rgba::parse("rgba(128,32,32,0.3)")).unwrap_or(r.error);
    }
    if let Some(c) = g("tabmixColor").and_then(Value::as_str) {
        r.tabmix = Rgba::parse(c); // "" turns tab-mix marking off
    }
    if let Some(s) = g("indicatorStyle").and_then(Value::as_str) {
        r.light = s == "light";
    }
    for (k, dst) in [("includedLanguages", &mut r.included), ("excludedLanguages", &mut r.excluded), ("ignoreErrorLanguages", &mut r.ignore_errors)] {
        if let Some(v) = g(k).and_then(strings) {
            *dst = v;
        }
    }
    let mut warnings = Vec::new();
    if let Some(p) = g("ignoreLinePatterns").and_then(strings) {
        r.ignore_lines = p
            .iter()
            .filter_map(|p| js_regex(p).map_err(|e| warnings.push(e)).ok())
            .collect();
    }
    (r, warnings)
}

/// `workbench.colorTheme`: empty means the embedded theme; an absolute path is a
/// theme file; otherwise a theme id (or label) contributed by a VS Code
/// extension's package.json.
pub fn resolve_theme(sel: Option<&str>, dirs: &[PathBuf]) -> Result<Value, String> {
    let Some(sel) = sel.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(theme::embedded_theme().clone());
    };
    if sel.starts_with('/') {
        return load_theme_file(Path::new(sel), 0);
    }
    // VS Code migrates the old built-in ids ("Default Dark Modern" -> "Dark Modern").
    let wanted = [Some(sel), sel.strip_prefix("Default ")];
    let hit = |s: Option<&str>| s.is_some() && wanted.contains(&s);
    for dir in dirs {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        let mut exts: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        exts.sort();
        for ext in exts {
            let Ok(text) = fs::read_to_string(ext.join("package.json")) else { continue };
            if !text.contains("\"themes\"") {
                continue;
            }
            let Ok(pkg) = serde_json::from_str::<Value>(&text) else { continue };
            // Built-in labels are "%key%" placeholders into package.nls.json.
            let nls: Value = fs::read_to_string(ext.join("package.nls.json"))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_default();
            for t in pkg["contributes"]["themes"].as_array().into_iter().flatten() {
                let label = t["label"].as_str().map(|l| match l.strip_prefix('%').and_then(|k| k.strip_suffix('%')) {
                    Some(k) => nls[k].as_str().unwrap_or(l),
                    None => l,
                });
                let id = t["id"].as_str().or(label);
                if hit(id) || hit(label) {
                    let p = t["path"].as_str().ok_or_else(|| format!("theme {sel:?} has no path"))?;
                    return load_theme_file(&ext.join(p), 0);
                }
            }
        }
    }
    if sel == EMBEDDED_ID {
        return Ok(theme::embedded_theme().clone());
    }
    Err(format!("workbench.colorTheme: no installed theme {sel:?}"))
}

/// A theme file with its `include` chain merged: the included theme first,
/// this file's colours over it and its token rules after it.
pub fn load_theme_file(path: &Path, depth: u8) -> Result<Value, String> {
    if depth > 8 {
        return Err(format!("{}: theme include chain too deep", path.display()));
    }
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut v: Value = serde_json::from_str(&jsonc_to_json(&text)).map_err(|e| format!("{}: {e}", path.display()))?;
    if !v["tokenColors"].is_array() {
        v["tokenColors"] = Value::Array(Vec::new()); // a .tmTheme path is not supported
    }
    let Some(inc) = v["include"].as_str().map(String::from) else { return Ok(v) };
    let mut base = load_theme_file(&path.parent().unwrap_or(Path::new("/")).join(inc), depth + 1)?;
    for key in ["colors", "semanticTokenColors"] {
        if let (Some(b), Some(own)) = (base[key].as_object_mut(), v[key].as_object()) {
            b.extend(own.clone());
        } else if v[key].is_object() {
            base[key] = v[key].clone();
        }
    }
    if let (Some(b), Some(own)) = (base["tokenColors"].as_array_mut(), v["tokenColors"].as_array()) {
        b.extend(own.iter().cloned());
    }
    if v["name"].is_string() {
        base["name"] = v["name"].clone();
    }
    Ok(base)
}

/// Is `k` a theme-scoped block (`"[Theme Name]"`) for one of `names`?
fn scoped_for(k: &str, names: &[String]) -> Option<bool> {
    let inner = k.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.split("][").any(|n| n == "*" || names.iter().any(|m| m == n)))
}

fn customize_colors(theme: &mut Value, cust: Option<&Value>, names: &[String]) {
    let Some(cust) = cust.and_then(Value::as_object) else { return };
    if !theme["colors"].is_object() {
        theme["colors"] = Value::Object(Map::new());
    }
    let mut apply = |m: &Map<String, Value>| {
        for (k, v) in m {
            if v.is_string() && !k.starts_with('[') {
                theme["colors"][k] = v.clone();
            }
        }
    };
    apply(cust);
    for (k, v) in cust {
        if scoped_for(k, names) == Some(true) {
            if let Some(m) = v.as_object() {
                apply(m);
            }
        }
    }
}

/// `editor.tokenColorCustomizations` as TextMate rules: VS Code's shorthand
/// groups, then `textMateRules`, then theme-scoped blocks.
fn token_rules(cust: Option<&Value>, names: &[String]) -> Vec<Value> {
    const GROUPS: &[(&str, &str)] = &[
        ("comments", "comment, punctuation.definition.comment"),
        ("strings", "string, meta.embedded.assembly"),
        ("keywords", "keyword - keyword.operator, keyword.control, storage, storage.type"),
        ("numbers", "constant.numeric"),
        ("types", "entity.name.type, entity.name.class, support.type, support.class"),
        ("functions", "entity.name.function, support.function"),
        ("variables", "variable, entity.name.variable"),
    ];
    fn one(m: &Map<String, Value>, out: &mut Vec<Value>) {
        for (g, scope) in GROUPS {
            let settings = match m.get(*g) {
                Some(Value::String(c)) => serde_json::json!({ "foreground": c }),
                Some(o @ Value::Object(_)) => o.clone(),
                _ => continue,
            };
            out.push(serde_json::json!({ "scope": scope, "settings": settings }));
        }
        out.extend(m.get("textMateRules").and_then(Value::as_array).into_iter().flatten().cloned());
    }
    let mut out = Vec::new();
    let Some(cust) = cust.and_then(Value::as_object) else { return out };
    one(cust, &mut out);
    for (k, v) in cust {
        if scoped_for(k, names) == Some(true) {
            if let Some(m) = v.as_object() {
                one(m, &mut out);
            }
        }
    }
    out
}

/// Creates the settings file from the commented template (`settingsui::template`) when it
/// is missing: a new user starts with every option listed, each at its default.
pub fn create(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut f) => f.write_all(crate::settingsui::template().as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("termcode-set-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn render_whitespace_defaults_to_boundary() {
        use crate::editor::Whitespace as W;
        let mode = |text: &str| from_maps(&parse_jsonc(text).unwrap(), None, &[]).unwrap().render_whitespace;
        assert_eq!(mode("{}"), W::Boundary);
        for (name, want) in [("none", W::None), ("boundary", W::Boundary), ("selection", W::Selection), ("trailing", W::Trailing), ("all", W::All)] {
            assert_eq!(mode(&format!(r#"{{"editor.renderWhitespace": "{name}"}}"#)), want);
        }
    }

    #[test]
    fn word_highlight_is_on_unless_turned_off() {
        let on = |text: &str| from_maps(&parse_jsonc(text).unwrap(), None, &[]).unwrap().occurrences;
        assert!(on("{}"));
        assert!(on(r#"{"editor.occurrencesHighlight": "singleFile"}"#));
        assert!(!on(r#"{"editor.occurrencesHighlight": "off"}"#));
        assert!(!on(r#"{"editor.occurrencesHighlight": false}"#));
    }

    #[test]
    fn matching_brackets_are_on_unless_set_to_never() {
        let on = |text: &str| from_maps(&parse_jsonc(text).unwrap(), None, &[]).unwrap().match_brackets;
        assert!(on("{}"));
        assert!(!on(r#"{"editor.matchBrackets": "never"}"#));
    }

    #[test]
    fn auto_closing_is_on_unless_set_to_never() {
        let on = |text: &str| {
            let s = from_maps(&parse_jsonc(text).unwrap(), None, &[]).unwrap();
            (s.auto_brackets, s.auto_quotes)
        };
        assert_eq!(on("{}"), (true, true));
        assert_eq!(on(r#"{"editor.autoClosingBrackets": "never"}"#), (false, true));
        assert_eq!(on(r#"{"editor.autoClosingQuotes": "never"}"#), (true, false));
        assert_eq!(on(r#"{"editor.autoClosingBrackets": "always"}"#), (true, true));
    }

    #[test]
    fn save_cleanup_is_on_unless_turned_off() {
        let on = |text: &str| {
            let s = from_maps(&parse_jsonc(text).unwrap(), None, &[]).unwrap();
            (s.trim_trailing, s.final_newline)
        };
        assert_eq!(on("{}"), (true, true));
        assert_eq!(on(r#"{"files.trimTrailingWhitespace": false, "files.insertFinalNewline": false}"#), (false, false));
    }

    #[test]
    fn jsonc_settings_and_defaults() {
        let m = parse_jsonc("// c\n{ \"workbench.density\": \"compact\", /* x */ \"editor.tabSize\": 2, }\n").unwrap();
        let s = from_maps(&m, None, &[]).unwrap();
        assert!(s.compact);
        assert_eq!(s.spacing, Spacing::COMPACT);
        assert_eq!(s.tab_size, 2);
        let m = parse_jsonc(r#"{"termcode.spacing": {"listItemGap": 1, "sidebarWidth": 500, "popupPadding": [5]}}"#).unwrap();
        let sp = from_maps(&m, None, &[]).unwrap().spacing;
        assert_eq!((sp.list_gap, sp.sidebar_width, sp.popup_padding), (1, 200, (5, 1)), "clamped; unset keys keep the density's");
        assert_eq!(sp.tab_padding, Spacing::COMFORTABLE.tab_padding);
        assert!(s.guides && s.rainbow.enabled && s.icons.is_none(), "icons: auto");
        assert!(parse_jsonc("").unwrap().is_empty(), "empty file = {{}}");
        assert!(parse_jsonc("// only a comment\n").unwrap().is_empty());
        let e = parse_jsonc("{\n  \"a\": 1\n  \"b\": 2\n}").unwrap_err();
        assert!(e.contains("line 3"), "{e}");
        assert!(parse_jsonc("[1]").is_err());
        let d = tmp("missing");
        let s = load(&d.join("settings.json")).unwrap();
        assert_eq!(s.theme.json, *theme::embedded_theme(), "missing file = defaults");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn theme_id_resolves_through_an_include_chain() {
        let d = tmp("theme");
        let ext = d.join("me.themes-1.0.0");
        fs::create_dir_all(ext.join("themes")).unwrap();
        fs::write(
            ext.join("package.json"),
            r#"{"contributes":{"themes":[{"id":"My Dark","label":"%myLabel%","path":"./themes/my.json"}]}}"#,
        )
        .unwrap();
        fs::write(ext.join("package.nls.json"), r#"{"myLabel": "My Dark (label)"}"#).unwrap();
        fs::write(
            ext.join("themes/base.json"),
            "{ // base\n \"colors\": {\"editor.background\": \"#111111\", \"editor.foreground\": \"#222222\"},\n \"tokenColors\": [{\"scope\": \"comment\", \"settings\": {\"foreground\": \"#00ff00\"}}] }",
        )
        .unwrap();
        fs::write(
            ext.join("themes/my.json"),
            r##"{"name": "My", "include": "./base.json", "colors": {"editor.foreground": "#333333",},
               "tokenColors": [{"scope": "string", "settings": {"foreground": "#ff0000"}}]}"##,
        )
        .unwrap();
        for sel in ["My Dark", "My Dark (label)", "Default My Dark"] {
            let t = resolve_theme(Some(sel), std::slice::from_ref(&d)).unwrap();
            assert_eq!(t["colors"]["editor.background"], "#111111", "from the include");
            assert_eq!(t["colors"]["editor.foreground"], "#333333", "own colour wins");
            let scopes: Vec<&str> = t["tokenColors"].as_array().unwrap().iter().map(|r| r["scope"].as_str().unwrap()).collect();
            assert_eq!(scopes, ["comment", "string"], "included rules first");
        }
        assert!(resolve_theme(Some("Nope"), std::slice::from_ref(&d)).unwrap_err().contains("Nope"));
        let abs = ext.join("themes/my.json");
        assert_eq!(resolve_theme(Some(abs.to_str().unwrap()), &[]).unwrap()["name"], "My");
        assert_eq!(resolve_theme(Some(EMBEDDED_ID), &[]).unwrap(), *theme::embedded_theme());
        // An include loop is an error, not a hang.
        fs::write(d.join("loop.json"), r#"{"include": "./loop.json"}"#).unwrap();
        assert!(load_theme_file(&d.join("loop.json"), 0).unwrap_err().contains("too deep"));
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn customizations_override_colours_and_tokens() {
        let m = parse_jsonc(
            r##"{
            "workbench.colorCustomizations": {
                "statusBar.background": "#123456",
                "[Some Other Theme]": { "sideBar.background": "#000001" },
                "[Visual Studio Dark - C++]": { "activityBar.background": "#654321" },
            },
            "editor.tokenColorCustomizations": {
                "comments": "#abcdef",
                "textMateRules": [{ "scope": "keyword.control", "settings": { "foreground": "#010203" } }],
            },
        }"##,
        )
        .unwrap();
        let s = from_maps(&m, None, &[]).unwrap();
        let pal = theme::build_pal(s.theme.json["colors"].as_object().unwrap());
        use ratatui::style::Color;
        assert_eq!(pal.status_bg, Color::Rgb(0x12, 0x34, 0x56));
        assert_eq!(pal.activity_bg, Color::Rgb(0x65, 0x43, 0x21), "scoped to the active theme");
        assert_eq!(pal.sidebar_bg, Color::Rgb(0x25, 0x25, 0x26), "scoped to another theme: ignored");
        let syn = theme::build_syntax(&s.theme.json, &s.theme.extra_rules);
        let h = syntect::highlighting::Highlighter::new(&syn.theme);
        let fg = |sc: &str| {
            let c = h.style_for_stack(&[syntect::parsing::Scope::new(sc).unwrap()]).foreground;
            (c.r, c.g, c.b)
        };
        assert_eq!(fg("comment.line"), (0xAB, 0xCD, 0xEF));
        assert_eq!(fg("keyword.control"), (1, 2, 3));
        assert_eq!(fg("string"), (0xCE, 0x91, 0x78), "untouched rules stay");
        // followVSCode: VS Code's theme keys win.
        let vs = parse_jsonc(r##"{"workbench.colorCustomizations": {"statusBar.background": "#0000ff"}}"##).unwrap();
        let s = from_maps(&m, Some(&vs), &[]).unwrap();
        assert_eq!(s.theme.json["colors"]["statusBar.background"], "#0000ff");
        assert!(s.follow_vscode);
    }

    #[test]
    fn rainbow_keys_and_bad_patterns() {
        let m = parse_jsonc(
            r#"{"indentRainbow.colors": ["rgba(1,2,3,0.5)"], "indentRainbow.tabmixColor": "",
                "indentRainbow.indicatorStyle": "light", "indentRainbow.excludedLanguages": ["c"]}"#,
        )
        .unwrap();
        let r = from_maps(&m, None, &[]).unwrap().rainbow;
        // editor.rulers: [90] by default, [] for none, numbers and objects with colours.
        assert_eq!(Settings::default().rulers, vec![(90, None)]);
        let none = parse_jsonc(r#"{"editor.rulers": []}"#).unwrap();
        assert!(from_maps(&none, None, &[]).unwrap().rulers.is_empty());
        let two = parse_jsonc(r##"{"editor.rulers": [{"column": 80, "color": "#ff0000"}, 100, "x"]}"##).unwrap();
        assert_eq!(from_maps(&two, None, &[]).unwrap().rulers, vec![(80, Some(Rgba(255, 0, 0, 255))), (100, None)]);
        assert_eq!(r.colors, vec![Rgba(1, 2, 3, 128)]);
        assert!(r.tabmix.is_none() && r.light && !r.applies("c") && r.applies("rust"));
        let bad = parse_jsonc(r#"{"indentRainbow.ignoreLinePatterns": ["/(/", "/x/"]}"#).unwrap();
        let s = from_maps(&bad, None, &[]).unwrap();
        assert_eq!(s.rainbow.ignore_lines.len(), 1, "the bad pattern is skipped, the good one kept");
        assert!(s.warnings[0].contains("skipped"), "{:?}", s.warnings);
    }

    #[test]
    fn settings_dialog_saves_structured_values_and_keeps_jsonc_outside_them() {
        let dir = tmp("dialog-jsonc");
        let path = dir.join("settings.json");
        let text = "// leading 中\n{\n  \"editor.tabSize\": 2,\n  \"editor.tabSize\": /* before */ 4 /* after */,\n  \"workbench.colorCustomizations\": {\"old\": [1, {\"text\": \"} // ,\"}]},\n  \"custom.key\": \"keep 中文\",\n  \"termcode.spacing\": { /* spacing */ \"sidebarWidth\": 36, /* width */ \"iconGap\": 3 /* gap */ },\n}\n";
        fs::write(&path, text).unwrap();
        let changes = serde_json::json!({
            "editor.tabSize": 6,
            "termcode.spacing": {"sidebarWidth": 42, "iconGap": 3},
            "workbench.colorCustomizations": {"editor.background": "#123456"},
            "editor.rulers": [80, {"column": 100, "color": "#555555"}],
        }).as_object().unwrap().clone();
        let saved = write_changes(&path, Some(text), &changes).unwrap();
        assert!(saved.contains("// leading 中") && saved.contains("/* before */ 6 /* after */"));
        assert!(saved.contains("\"custom.key\": \"keep 中文\""));
        assert!(saved.contains("/* spacing */") && saved.contains("/* width */") && saved.contains("/* gap */"));
        let s = load(&path).unwrap();
        assert_eq!(s.tab_size, 6);
        assert_eq!(s.rulers.len(), 2);
        assert_eq!(s.theme.json["colors"]["editor.background"], "#123456");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_dialog_creates_missing_config_and_preserves_crlf_on_insert() {
        let dir = tmp("dialog-new-crlf");
        let path = dir.join("nested/settings.json");
        let changes = serde_json::json!({"editor.tabSize": 7}).as_object().unwrap().clone();
        write_changes(&path, None, &changes).unwrap();
        assert_eq!(load(&path).unwrap().tab_size, 7);
        let text = "{\r\n  // keep\r\n  \"custom.key\": true,\r\n}\r\n";
        fs::write(&path, text).unwrap();
        let saved = write_changes(&path, Some(text), &changes).unwrap();
        assert!(saved.contains("  // keep\r\n  \"custom.key\": true,\r\n"));
        assert_eq!(load(&path).unwrap().tab_size, 7);
        assert!(!saved.replace("\r\n", "").contains('\n'), "inserted lines must match CRLF");
        fs::remove_dir_all(dir).unwrap();
    }

}
