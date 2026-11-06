# herdr-code

herdr plugin, written in Rust. Run `herdr-code [PATH]` like VS Code's `code PATH`: a VS
Code-like editor for that folder, file or repo **in the same terminal**, like vim; quitting
returns to the shell. `herdr-code --tab` opens it as a `Code: <name>` tab of the herdr workspace
instead. It has an activity
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

```sh
herdr-code                    # this folder's git repo (or, outside one, the folder)
herdr-code drivers/net/dsa    # that folder as the workspace root; git from the repo above it
herdr-code ~/proj             # any folder, git repo or not
herdr-code src/main.rs:120    # the file at line 120 (also :LINE:COL), its repo as the root
herdr-code --tab src          # the same, as a Code tab in this herdr workspace
```

- `herdr-code [--tab] [PATH[:LINE[:COL]]]`; `herdr-code -h` prints the usage.
- **In place** (the default) the panel takes over the terminal it was started in, like vim.
  Close (title bar) or Ctrl+Q asks Save All / Don't Save / Cancel for unsaved files, then puts
  the terminal back (main screen, mouse, keyboard flags, bracketed paste, cursor) and exits 0,
  so the shell prompt returns. Nothing in herdr moves: no tab, no focus, no other client. It
  also works outside herdr, as a plain terminal app.
- A relative PATH resolves against the current folder. A missing PATH exits with status 1. As
  with `code -g`, `:LINE` and `:LINE:COL` are split off, except that a path which exists as
  typed (a name containing `:`) wins. A name starting with `-` needs `./`.
- A folder becomes the workspace root: the Explorer, Search and Quick Open cover it, and its
  name heads the Explorer. Inside a git repo, Source Control, the Explorer's decorations and the
  quick diff use that repo, as VS Code does for a subfolder; Source Control lists the whole
  repo's changes. A folder outside any repo works too: Source Control then says "The folder
  currently open doesn't have a git repository." and nothing runs git.
- A file opens, pinned, at LINE:COL, with its git worktree as the root, or its folder outside a
  repo.
- Settings, the layout and hot-exit backups are the same in every mode: in a plugin pane herdr
  gives `$HERDR_PLUGIN_CONFIG_DIR` / `$HERDR_PLUGIN_STATE_DIR`; elsewhere herdr-code uses the
  paths herdr would give (`~/.config/herdr/plugins/config/jonas.herdr-code`,
  `~/.local/state/herdr/plugins/jonas.herdr-code`, under `$XDG_CONFIG_HOME` / `$XDG_STATE_HOME`
  when set). Backups are kept per herdr session (`HERDR_SOCKET_PATH`), or as `local` outside
  herdr.

### As a tab: `--tab` and the plugin action

- `herdr-code --tab [PATH]` from a herdr pane, or the plugin action `jonas.herdr-code.open`
  (no path: the invoking pane's `foreground_cwd`). Without a terminal on stdin and stdout,
  `herdr-code` notes it and opens a tab instead.
- If the workspace already has a Code tab for that root (our "Code" pane with the root as its
  cwd), it is reused; otherwise a new tab opens (`herdr plugin pane open --placement tab
  --no-focus`) and is renamed `Code: <name>`. A short lock around find-or-create makes a double
  launch open one tab. A new tab gets a file in `HERDR_CODE_OPEN`. A running one gets it as
  one bracketed paste, `herdr pane send-text`, carrying a random token that the tab panel writes
  at start to `$HERDR_PLUGIN_STATE_DIR/req-<key>.token` (0600; key = session socket and pane
  id). The paste wakes the panel at once. One that does not start with `herdr-code:<token>:`
  is an ordinary paste. The panel opens only a regular file under its root. A Code tab
  started by an older herdr-code takes no requests: close it and run again.
- It never focuses anything: herdr 0.9.1 can only focus a tab for every client attached to the
  session (`herdr tab focus` moves them all, e.g. a mosh client elsewhere), so the active tab
  stays as it was. It prints `Code tab ready: click 'Code: <name>' in the tab bar`, and
  `opened <file>:<line>` for a file.
- Quitting ends the panel process; herdr then closes the tab. It never signals or closes
  anything else. herdr closing the tab (HUP, then TERM) still flushes unsaved edits to the
  hot-exit backups.

### More

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
| minimap | click: centre that line; drag the slider; the wheel scrolls the editor |
| a gutter marker (click), Alt+F5 / Shift+Alt+F5, F7 / Shift+F7 | quick-diff peek on that / the next / the previous change |
| in the peek | Revert (the hunk back to the index version, one undo step), ↑ ↓ (wraps), × or Esc closes |
| F12, Ctrl+click | Go to Definition: one result opens at its line (flashed); several follow `editor.gotoLocation.multipleDefinitions` |
| Alt+F12 | Peek Definition, even for one result |
| Shift+F12 | Find All References, in the peek ("N references") |
| in the peek | Up/Down, PgUp/PgDn, Enter or click opens, wheel scrolls the list or the preview, Esc or a click outside closes |
| Alt+← / Alt+→ | Go Back / Go Forward (also VS Code Linux's Ctrl+Alt+- / Ctrl+Shift+- where they reach, and F1) |
| Ctrl+hover | underlines the identifier under the mouse (`editorLink.activeForeground`) |

The Explorer header shows the root's name and, under it, its parent folder (`~` for
$HOME, cut from the left as `…/work/fetched` when narrow).

The editor gutter shows VS Code's quick diff against the git index (`:0:path`): green `▎`
added, blue `▎` modified, red `▸` lines deleted below. Staging a hunk clears its marker. It
recomputes about 200 ms after an edit. The editor scrollbar's overview ruler marks git
changes (left half), find matches (right half) and the cursor line.

The minimap (VS Code's `editor.minimap`) sits between the code and the scrollbar: half-block
cells, two lines tall, about 8 characters wide, coloured by the dominant token colour, with
the git changes on its left edge, find matches and the selection behind the text, and the
viewport slider while the mouse is over it (herdr forwards plain mouse motion to the pane, so
VS Code's `showSlider: "mouseover"` works). In a narrow editor it shrinks, then hides.

Clicking a quick-diff marker opens VS Code's dirty-diff peek below the change: the index
lines on red, the buffer lines on green with word highlights. Stage is disabled until the git
backend lands; Revert edits the buffer only (nothing is written or staged).

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
| `editor.rulers` | `[90]` | Vertical rulers after N display columns (tabs and wide characters counted), in `editorRuler.foreground`; `[]` for none; `[{ "column": 80, "color": "#rrggbb" }, 100]` gives one its own colour. Drawn behind the text, in the editor and both sides of the diff. F1 "View: Toggle Rulers" for the session |
| `editor.minimap.enabled` | `true` | The minimap (F1 "View: Toggle Minimap" for the session) |
| `editor.minimap.maxColumn` | `120` | Characters the minimap covers (about 8 per cell, 8 to 16 cells, at most 15% of the editor, shrunk to leave minEditorWidth) |
| `editor.minimap.renderCharacters` | `false` | `true`: braille dots (4 lines × 2 dot columns per cell) instead of half blocks |
| `editor.minimap.showSlider` | `"mouseover"` | or `"always"` |
| `editor.minimap.side` | `"right"` | or `"left"` |
| `herdr-code.minimap.minEditorWidth` | `40` | The minimap shrinks (to 4 cells) so the code keeps this many columns, then hides |
| `editor.minimap.size` | `"proportional"` | `"fill"`: the whole file over the height; `"fit"`: proportional while it fits |
| `editor.gotoLocation.multipleDefinitions` | `"peek"` | Several definitions: `"peek"` shows them, `"gotoAndPeek"` opens the first and peeks, `"goto"` opens the first |

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

## Go to Definition

There is no index: each lookup scans the open buffer (unsaved edits, locals, parameters, C
labels), then runs one `rg` over the language's files with definition patterns (C/C++ with
kernel style, Rust, Python, Go, shell, JS/TS; other files: a whole-word match). Results rank
locals first, then definitions before declarations (a declaration is dropped when a
definition exists), then the same file, directory, `foo.c`/`foo.h` pair, shallower paths. Under
a subfolder root (`herdr-code drivers/net`), a symbol not defined there is looked up in the whole
repo, so `include/` headers resolve; files outside the root open by their full path. The
status bar shows "Searching definitions…" until it ends; Esc or a new request cancels it. A
cancelled `rg` is never signalled: its pipe closes and it is reaped; Search and lookups share
one cap of two running `rg`. Every jump (definitions, peek items, Back/Forward) goes through a
50-entry history; a new jump clears Forward. The peek's colours are VS Code's `peekView*` keys.

## Known limits

- Go to Definition is pattern-based: C++ methods defined inside a class body, Go `const (…)`
  groups and macros that build names are not found; a C function head split over lines is read
  from the file (first 1 MB) to tell a definition from a prototype. Up to 200 definitions and
  1000 references are kept.
- Ctrl+click needs the terminal to report Ctrl with the mouse (herdr 0.9.1 forwards it, after
  checking the cell is not a link). Mouse buttons 8/9 (back/forward) never reach the panel:
  herdr drops them. In legacy terminals Ctrl+Alt+- and Ctrl+Shift+- arrive as Ctrl(+Alt)+7.
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
- Hot-exit backups (`$HERDR_PLUGIN_STATE_DIR/backup`) are one file per herdr session, panel
  (pane id or `local`, plus pid) and file, so two panels editing one file keep two. Opening a
  file offers the newest backup of it that no running panel owns, from any session or panel,
  noting when there are more ("2 backups found"); Restore or Discard acts on that one only.
  Older formats are found by content and never renamed. A panel counts as running while its
  pid is alive as `herdr-code`; records from before this carry no pid, so a still-running older
  panel's backup can be offered too.
