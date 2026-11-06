# herdr-code

herdr plugin, written in Rust. Run `herdr-code` in a herdr pane to open a `Code: <repo>` **tab in
that pane's workspace**, with a VS Code-like editor for the pane's repo or worktree: activity
bar, a resizable Explorer / Search /
Source Control / History sidebar, editor groups with preview tabs, a side-by-side diff, and
the "Visual Studio Dark - C++" colours (or any VS Code theme, see [Settings](#settings)).

## Install

```sh
cargo install --path . --root ~/.local -j3     # -> ~/.local/bin/herdr-code
herdr plugin link "$PWD"
```

The manifest runs `herdr-code` through PATH, so the herdr server's PATH must include `~/.local/bin`.

## Use

- `herdr-code` from a shell in a herdr pane, or the plugin action `jonas.herdr-code.open`. The
  action uses the invoking pane's `foreground_cwd`.
- Outside a git repo it exits with status 1. If the workspace already has a Code tab for this
  worktree (our "Code" pane with the worktree root as its cwd), it is reused; otherwise a new
  tab opens (`herdr plugin pane open --placement tab`, without `--focus`) and is renamed
  `Code: <repo>`. A short lock around find-or-create makes a double launch open one tab.
- It never focuses anything: herdr 0.9.1 can only focus a tab for every client attached to the
  session (`herdr tab focus` moves them all, e.g. a mosh client elsewhere), so the active tab
  stays as it was. It prints `Code tab ready: click 'Code: <repo>' in the tab bar`.
- Close (title bar) or Ctrl+Q asks Save All / Don't Save / Cancel for unsaved files, then ends
  the panel process; herdr then closes the tab. It never signals or closes anything else.
  herdr closing the tab (HUP, then TERM) still flushes unsaved edits to the hot-exit backups.
- Icons: UI icons are VS Code codicons (`nf-cod-*`); file icons are VS Code's default Seti
  theme mapped to `nf-seti-*`, coloured like VS Code (`cargo run --example gen_seti`). Both need a
  Nerd Font. `"herdr-code.icons": "ascii"` or `HERDR_CODE_ICONS=0` (always wins) gives plain
  ASCII. The sidebar width and visibility are kept in `$HERDR_PLUGIN_STATE_DIR/layout.json`.
- `prefix+g` is herdr's default `goto`, so no keybinding is shipped. To add one, bind
  `plugin_action` `jonas.herdr-code.open` to a free key.

## Panel keys

In VTE terminals (ptyxis, gnome-terminal), Ctrl+Shift chords, Ctrl+Enter, Shift+Enter, Ctrl+Tab
and Ctrl+1/2/3 arrive as their plain forms. VTE has no kitty keyboard support and herdr does not
enable modifyOtherKeys for it. The fallbacks below always work.

| Key | Action |
|---|---|
| F1 (Ctrl+Shift+P where it reaches) | Command palette: Open Settings (JSON), Toggle Indent Rainbow, Toggle Minimap Ruler, … |
| Alt+1, Ctrl+E | Explorer |
| Alt+2 / F2, Ctrl+G | Source Control |
| Alt+3 | History (`h` on an Explorer file: that file's history) |
| Alt+4 / F4 (Ctrl+Shift+F where it reaches) | Search in files |
| click the active activity icon | hide or show the sidebar; drag the divider to resize |
| Ctrl+P, Alt+P | Quick Open |
| click / double-click | open as preview (italic tab) / keep open. Ctrl+K Enter also pins |
| drag an Explorer file to an editor edge | open it in a new group (max 3); centre or tab bar = that group |
| Ctrl+F, Ctrl+H | find / replace in the current file. Enter or F3 next, Shift+F3 previous, Esc closes |
| Ctrl+S, Ctrl+W, Ctrl+Q | save (atomic), close tab, quit (all prompt for unsaved changes) |
| Ctrl+Z, Ctrl+Y | undo, redo |
| Ctrl+C / X / V, Ctrl+A | clipboard (whole line without a selection), select all |
| Tab / Shift+Tab, Enter | indent / outdent, new line with auto-indent |
| Ctrl+PgDn / PgUp, Ctrl+1/2/3 | next / previous tab, focus group (Ctrl+digit only with kitty keys) |
| Alt+Enter (Ctrl+Enter) | commit the staged changes with the message |
| `r` in a list | refresh. Git status also refreshes on view switch and on pane focus |
| scrollbars | click the track to page, drag the slider; the wheel scrolls the list or editor under it |

The Explorer header shows the worktree's name and, under it, its parent folder (`~` for
$HOME, cut from the left as `…/work/fetched` when narrow).

The editor gutter shows VS Code's quick diff against the git index (`:0:path`): green `▎`
added, blue `▎` modified, red `▸` lines deleted below. Staging a hunk clears its marker. It
recomputes about 200 ms after an edit. The editor scrollbar's overview ruler marks git
changes (left half), find matches (right half) and the cursor line.

## Settings

`$HERDR_PLUGIN_CONFIG_DIR/settings.json` (normally
`~/.config/herdr/plugins/config/jonas.herdr-code/settings.json`), VS Code's format: comments
and trailing commas are fine. F1 "Preferences: Open Settings (JSON)" opens it, creating it from
a commented template. A save in the panel applies it at once; an edit from elsewhere applies on
the next focus or view switch. A missing file means the defaults. A broken file never crashes
the panel: the status bar shows the error and the last good settings stay.

| Key | Default | Meaning |
|---|---|---|
| `workbench.colorTheme` | embedded "Visual Studio Dark - C++" | A theme id or label from VS Code's built-in extensions or `~/.vscode/extensions` (`contributes.themes`, `include` chains resolved), or an absolute path to a theme JSON |
| `workbench.colorCustomizations` | `{}` | Colour keys over the theme, also per theme as `"[Theme Name]": {…}` |
| `editor.tokenColorCustomizations` | `{}` | `comments`, `strings`, `keywords`, `numbers`, `types`, `functions`, `variables`, `textMateRules`, also per theme |
| `herdr-code.followVSCode` | `false` | Take the three keys above from `~/.config/Code/User/settings.json` instead |
| `workbench.density` | `"comfortable"` | `"compact"` is the original tight layout (no padding, no popup frames) |
| `herdr-code.spacing` | see below | Each key overrides the density's value |
| `herdr-code.icons` | `"nerd"` | `"ascii"`: no Nerd Font glyphs |
| `herdr-code.fileIcons` | `{}` | `extensions`, `fileNames`, `folderNames`: maps to `{ "glyph": "…", "color": "#rrggbb" }` (either half); `folder`, `folderOpen` |
| `indentRainbow.enabled` | `true` | oderwat.indent-rainbow, same defaults |
| `indentRainbow.colors` | the extension's four `rgba(…,0.07)` | Blended over the editor background |
| `indentRainbow.errorColor`, `indentRainbow.tabmixColor` | `rgba(128,32,32,0.6)`, `rgba(128,32,96,0.6)` | Indent not a multiple of the tab size; tabs and spaces mixed (`""` turns tab-mix off) |
| `indentRainbow.indicatorStyle` | `"classic"` | `"light"`: a thin line per level instead of a background |
| `indentRainbow.excludedLanguages`, `.includedLanguages`, `.ignoreErrorLanguages` | `["plaintext"]`, `[]`, `["markdown"]` | VS Code language ids |
| `indentRainbow.ignoreLinePatterns` | `["/[ \t]* [*]/g", "/[ \t]+[/]{2}/g"]` | JS regex literals; a matching line never gets the error colour |
| `editor.guides.indentation` | `true` | Indent guides (`editorIndentGuide.background` / `.activeBackground`) |
| `editor.tabSize` | `4` | Tab stops, the indent unit, rainbow and guides |

`herdr-code.spacing` (comfortable / compact defaults):

| Key | Comfortable | Compact | Meaning |
|---|---|---|---|
| `sidebarWidth` | 36 | 30 | Sidebar columns until you drag the divider (a dragged width is kept in layout.json) |
| `sidebarPadding` | 2 | 0 | Left/right padding of sidebar rows and inputs |
| `sectionGap` | 1 | 0 | Blank rows under the sidebar title and between SCM sections |
| `editorPadding` | 2 | 1 | Gutter gaps: `number␠␠marker␠␠code` in comfortable (compact: `number marker␠code`); 2 or more also leaves the blank row under the tab bar |
| `tabPadding` | 2 | 1 | Spaces each side of a tab label |
| `statusBarGap` | 3 | 3 | Spaces between status bar items |
| `statusBarPadding` | 2 | 1 | Spaces at the status bar's left and right edges |
| `activityBarWidth` | 5 | 3 | Activity bar columns, icons centred, a blank row between icons |
| `listItemGap` | 0 | 0 | 0 or 1 blank row between sidebar list items |
| `popupPadding` | `[3, 1]` | `[0, 0]` | Horizontal and vertical padding in Quick Open, the command palette and dialogs |
| `iconGap` | 2 | 1 | Blank cells after an icon (tree, tabs, SCM rows, title bar, buttons, find widget, status bar); at least 1, since a non-Mono Nerd Font icon can overflow its cell |
| `treeIndent` | 2 | 2 | Cells per tree depth level |
| `titleBarPadding` | 2 | 1 | Left/right padding in the title bar |
| `buttonPadding` | 2 | 1 | Spaces inside buttons (Close, Save, Discard, …) |
| `findWidgetPadding` | 1 | 0 | Padding inside the find/replace widget's frame |

Every icon is laid out as one cell and followed by at least one blank cell; scrollbars keep a
blank column between them and the text (comfortable).

Unset colour keys fall back to VS Code's dark colour-registry defaults
(`assets/vscode_dark_defaults.json`), as in VS Code. Example:

```jsonc
{
    "workbench.colorTheme": "Default Dark Modern",
    "workbench.colorCustomizations": {
        "statusBar.background": "#3B3B3B",
        "[Default Dark Modern]": { "editorIndentGuide.background": "#333333" },
    },
    "editor.tokenColorCustomizations": {
        "comments": "#6A9955",
        "textMateRules": [{ "scope": "keyword.control", "settings": { "fontStyle": "bold" } }],
    },
    "workbench.density": "comfortable",
    "herdr-code.fileIcons": {
        "fileNames": { "Kconfig": { "glyph": "\ue615", "color": "#6D8086" } },
        "folderNames": { "drivers": { "color": "#E2C08D" } },
    },
    "indentRainbow.indicatorStyle": "light",
    "editor.tabSize": 8,
}
```

The command palette's View toggles (indent rainbow, guides, minimap ruler) last for the
session; the next settings load resets them to the file.

## Known limits

- Copy tries `wl-copy`, then `xclip`, then OSC 52. Paste uses bracketed paste (the terminal's
  own paste), or `wl-paste`/`xclip` for Ctrl+V, else the panel's last copy.
- Files that were truncated (over 1 MB or 20k lines), are not UTF-8, or are binary open
  read-only and can never be saved. If a file changes on disk, a clean buffer reloads; a dirty
  one keeps your edits, and the first Ctrl+S refuses so a second one is needed to overwrite.
- Find is literal (Match Case and Whole Word toggles). Search in files can also use regex.
  The diff editor is read-only and not syntax-highlighted.
- Lines over 4000 bytes are not highlighted. The default token colours are VS Code's
  `cpptools_dark_vs_new.json` (MIT, Microsoft), embedded under `assets/`. `assets/seti.json`
  and `assets/vscode_dark_defaults.json` are data from VS Code (MIT, Microsoft) and Seti UI
  (MIT, Jesse Weed); see `assets/vscode_LICENSE.txt`. syntect's Sublime
  grammars name some scopes differently from VS Code's TextMate grammars; a small alias table
  renames the ones themes care about (`#include` and `#define` as `keyword.control.directive`,
  calls as `entity.name.function`, struct names as `entity.name.type`, …).
- C/C++ (and, loosely, Rust) get a per-line stand-in for cpptools' semantic colouring: a name
  before `(` is a function (a macro if ALL_CAPS), ALL_CAPS names are macros, a name after
  `struct`/`union`/`enum`/`class` or ending in `_t` is a type, a name after `.`/`->` is a
  property, the current function's parameter names are parameters, other names are local
  variables. It does not know globals, typedefs without `_t`, or enum constants outside their
  `enum`. A theme's `tokenColors` given as a `.tmTheme` path is ignored.
- Colour keys a theme leaves unset fall back to VS Code's *dark* registry defaults, so a
  light theme that does not set every workbench colour gets dark sidebars or tabs there.
- Hot-exit backups written before backups carried the session (`scope`) are moved once under
  the current session's key when the panel starts in their worktree. If that name is already
  taken the old file is left in `$HERDR_PLUGIN_STATE_DIR/backup` untouched; nothing is deleted.
