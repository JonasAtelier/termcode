# herdr-code

herdr plugin, written in Rust. Run `herdr-code [PATH]` like VS Code's `code PATH`: a VS
Code-like editor for that folder, file or repo **in the same terminal**, like vim; quitting
returns to the shell. `herdr-code --tab` opens it as a `Code: <name>` tab of the herdr workspace
instead. It has an activity
bar, a resizable Explorer (with Timeline) / Search /
Source Control (Changes over Graph) sidebar, editor groups with preview tabs, a side-by-side diff, and
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
- Blocks: the activity bar, each sidebar section and each editor group are rounded cards with a
  no blank gutter beside the sidebar and a 1-cell gutter between editor groups, while
  stacked sections share one border row (`├─┤`)
  (`herdr-code.blocks.style`); the borders are `editorGroup.border`, the focused block's a
  little lighter. Each icon keeps a blank cell after it, room for a
  non-Mono Nerd Font glyph. Activity bar items are 3 rows tall (2 in compact), the icon
  centred: the active one has `activityBar.activeBorder` down its left edge over a subtle
  `activityBar.activeBackground`, a hovered one turns from `inactiveForeground` to `foreground`.
  the focused block's a little lighter. Icon sizes (`herdr-code.icons.size`):
  `herdr-code --icon-demo` shows the three side by side in your terminal and font; any key quits.
- Sessions, per launch folder: reopening `herdr-code` from the same folder (or on the same
  PATH) brings back the last session there, as VS Code does: the editor groups and tabs (pinned or preview, the active ones), each file's
  cursor, selection and scroll, the sidebar view, width and visibility, the Explorer's open
  folders and scroll, the Search query and toggles (not its results), the Source Control
  layout, and diff tabs (still-pending working-tree/index changes, commits still there). Missing
  files are skipped and positions clamped; tabs reopen lazily, their files read when first
  shown. Unsaved text comes back from the hot-exit backups (the Restore prompt). It is saved
  about a second after a change, on quit, when herdr closes the pane, and after a crash, to
  `$HERDR_PLUGIN_STATE_DIR/sessions/<hash of the launch folder>.json` (in place and as a tab
  alike). A no-argument launch from `arch/sh` keeps its own session though its root is the
  repo; a reused `--tab` keeps the session of the folder it was opened from.
  `herdr-code FILE:L` opens that file on top of it.
- Visuals: the built-in theme uses layered grey surfaces, muted selections and diff line
  backgrounds, with stronger word highlights. The card border stays continuous and keeps
  its own colour. A separate blue underline marks the active tab's top edge without an
  extra row (a leading marker without cards). This needs coloured-underline transport;
  herdr 0.9.1 loses that colour and displays the line in the border's colour instead.
  Dirty tabs keep a separate marker beside Close even on hover. Empty editors offer clickable Open File,
  Search and Source Control shortcuts. Graph nodes are hollow except HEAD, which is solid.
  Tool icons stay monochrome; Seti file colours and custom theme colours are preserved.
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
| `h` on an Explorer file | open it; the TIMELINE pane under the tree shows its history |
| Alt+4 / F4 (Ctrl+Shift+F where it reaches) | Search in files |
| click the active activity icon | hide or show the sidebar; drag the divider to resize |
| Ctrl+P, Alt+P | Quick Open |
| click / double-click | open as preview (italic tab) / keep open. Ctrl+K Enter also pins |
| drag an Explorer file to an editor edge | open it in a new group (max 3); centre or tab bar = that group |
| Ctrl+F, Ctrl+H | find / replace in the current file. Enter or F3 next, Shift+F3 previous, Esc closes |
| Ctrl+S, Ctrl+W, Ctrl+Q | save (atomic), close tab, quit (all prompt for unsaved changes) |
| Ctrl+Z, Ctrl+Y | undo, redo |
| gutter fold button, Ctrl+Shift+[ / ] | fold / unfold the current block |
| Ctrl+K Ctrl+0 / Ctrl+K Ctrl+J | fold all / unfold all (also available from F1) |
| Ctrl+C / X / V, Ctrl+A | clipboard (whole line without a selection), select all |
| Tab / Shift+Tab, Enter | indent / outdent, new line with auto-indent |
| Ctrl+PgDn / PgUp, Ctrl+1/2/3 | next / previous tab, focus group (Ctrl+digit only with kitty keys) |
| Add commit (Source Control) | the commit dialog, see [Source Control](#source-control) |
| F7 / Shift+F7 in a diff | next / previous change |
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
| rest the pointer on a function name for 0.5 s | preview its documentation; move away or press Esc to close; wheel inside the popup scrolls long comments |

Sticky scroll: while the Explorer is scrolled, one row under the root header shows the full
path of the folder the first visible row is in, breadcrumb style (deep in `arch/sh/boards`:
`▾ arch › sh › boards`), each name coloured as in the tree and clickable (it scrolls that
folder back into view); too narrow, it is cut from the left (`… › sh › boards`). It follows
every folder end, and shows nothing while the path's top-level folder is still on screen.
With `workbench.tree.stickyScrollMaxItemCount` above 1 the folders are nested rows instead,
outermost first (`arch`, `sh`, `boards`), as VS Code draws them. Search pins the file over
its hits, Source Control the section over its files; the wheel over them scrolls the list.
A thin line (`sideBarStickyScroll.border`, here the cards' border colour) separates them
from the list along the last pinned row's bottom edge, without a separator-only row
(or in its configured listItemGap). Its underline colour has the same transport limitation
noted above. Never over 40% of the list; the background is `sideBarStickyScroll.background`.

Function documentation hover uses the existing bounded F12 lookup, including the repo
outside a subfolder workspace. It shows adjacent C/kernel-doc/JSDoc blocks, Rust/Go
line comments and Python docstrings, using open buffers' unsaved comments. Like VS Code's
hover, it shows a syntax-coloured function signature above the documentation, with a
separator, formatted parameters/returns and basic Markdown (emphasis, inline code,
fenced examples and link labels). It uses `editorHoverWidget.background`, `.foreground`
and `.border`; it prefers the space above the function and falls below when needed. If no
comment is found, the popup shows only the signature, without an empty documentation section.
No language server is started. It recognises call-shaped names (`name(`); lookup is
heuristic, so overloads, generated definitions and complex generic calls may not resolve.
Comments are limited to 80 lines; a popup can be scrolled without scrolling the editor.

The folding gutter uses the same chevrons as Explorer, with two clickable columns and a
hover highlight. Code folding follows indentation (VS Code's fallback strategy), including nested functions
and blocks. A folded header shows an ellipsis; original line numbers and file contents are
preserved. Go to Definition and find reveal hidden results. Editing reveals folded blocks;
fold state is local to each editor view and resets after text changes or restarting.

The editor gutter shows VS Code's quick diff against the git index (`:0:path`): green `▎`
added, blue `▎` modified, red `▸` lines deleted below. Staging a hunk clears its marker. It
recomputes about 200 ms after an edit. The editor scrollbar's overview ruler marks git
changes (left half), find matches (right half) and the cursor line.

The minimap (VS Code's `editor.minimap`) sits between the code and the scrollbar and shows
the code's silhouette, like VS Code's default `renderCharacters`: braille cells of 2 × 4 dots,
one dot row per slot of the file's height and one dot per run of columns (so `maxColumn` fits), lit where that run
has code, so indentation and line lengths show. Each cell takes the most frequent token
colour of its dots, muted towards the background (`minimap.foregroundOpacity` scales it). The
git changes are on its left edge, find matches and the selection behind the dots, and the
viewport slider is a translucent overlay, always visible by default (herdr forwards plain mouse
motion to the pane, so explicit `showSlider: "mouseover"` also works). In a narrow editor it
shrinks, then hides. `renderCharacters: false` draws half blocks instead, one line per half.

Clicking a quick-diff marker opens VS Code's dirty-diff peek below the change: the index
lines on red, the buffer lines on green with word highlights. Stage stages that one hunk (an
unsaved buffer asks to save first: Save & Stage / Cancel); Revert edits the buffer only.

## Source Control

One view, VS Code's: the CHANGES pane over the GRAPH pane. Click a header to collapse it;
drag the GRAPH header, or the line above it (lit in `sash.hoverBorder` on hover), to move the
split (kept in layout.json).

- CHANGES: an **Add commit** button, then Merge Changes (conflicts), Staged Changes and
  Changes (untracked files as `U`). Row actions open, stage `+`, unstage `−`, discard `↶`
  (always asks). Merge Changes' `+` stages every conflict (marks it resolved); Changes' "Stage
  All" leaves conflicts alone.
- GRAPH: the history of HEAD in `--topo-order`, lanes in the `scmGraph.foreground1-5`
  colours, ref badges (branch, remote, tag, detached HEAD). It loads 100 commits; the last row
  "View more" loads the next 100 (scrolling never loads). Click a commit to list its files
  inline (status letters in `gitDecoration.*` colours), click a file for a read-only
  side-by-side diff: first parent left, commit right, rows aligned, F7 / Shift+F7 between
  changes. Keys with the graph focused (Tab from CHANGES): arrows, Enter, Left/Right
  collapse/expand, `i` the commit card, `r` reload.
- Side-by-side diffs (commit and working-tree) scroll as the editor does, both sides together:
  one scrollbar at the right edge (drag the slider, click the track to page) whose overview
  ruler marks removed rows in its left half and inserted ones in its right
  (`diffEditorOverview.removedForeground` / `.insertedForeground`; unset, VS Code's text
  backgrounds at twice the alpha) and the change F7 last reached; the editor's minimap on the
  modified side, its git lane showing the changes. Wheel: vertical; Shift+wheel or the
  horizontal wheel: sideways, no further than the longest visible line. Keys: Up/Down,
  PgUp/PgDn, Ctrl+Home/End (top, bottom), Home/End (line starts, line ends); F7 / Shift+F7
  reveal the next / previous change centred.
- Commit card: rest the pointer on a commit row (graph or Timeline) for 0.5 s, press `i`, or
  click the ⓘ at the selected row's end. Author, relative and local date, the full message,
  files changed with insertions and deletions, refs, the short hash, [Copy hash]
  [Open changes] (opens its first file's diff).
- Commit dialog (git-gui style): Unstaged Changes over Staged Changes (Will Commit) with
  [Stage →] [Stage Changed] [← Unstage] (double-click or Enter on a row too), the selected
  file's diff on the right, New Commit / Amend Last Commit (amend fills in the last message),
  the message with a subject count (50) and a ruler at column 72, and [Rescan] (F5)
  [Sign Off] (Ctrl+S) [Commit] (Alt+Enter, Ctrl+Enter where it reaches) [Commit & Push]
  [Cancel]. An empty message is refused; with nothing staged it offers to stage everything
  and commit. Esc or Cancel keeps the draft, also across restarts
  (`$HERDR_PLUGIN_STATE_DIR/drafts/`). The commit runs in the background.
- Explorer TIMELINE: the active file's commits (renames followed), 100 at a time; click one
  for that commit's diff of the file.
- Status bar: the branch (`*` unstaged, `+` staged changes; click: checkout a local, remote
  or tag ref, Create new branch…, Create new branch from…), then `↓behind ↑ahead` (click:
  VS Code's "This action will pull and push commits from and to '<upstream>'", OK by
  default, then pull and push) or **Publish Branch** (`push -u` to `origin`, else the only remote), and the cursor line's blame
  (rest on it or click it for the commit card).
- Git commands: F1 "Git: …" or the "…" on the Source Control title: Pull, Push, Fetch, Fetch
  (Prune), Sync, Publish Branch, Checkout to…, Create Branch (From)…, Delete Branch…, Merge
  Branch…, Rebase Branch…, Stash, Stash (Include Untracked), Stash Staged, Apply / Pop Latest
  Stash, Apply / Pop / Drop Stash…, Undo Last Commit, Commit (Amend), Abort Merge, Abort
  Rebase, Continue Rebase, Show Git Output, Cancel Running Operation. Rebase, the aborts,
  Drop Stash, deleting an unmerged branch and undoing a merge commit ask first, with Cancel
  as the default.
- Git work runs in the background with a spinner in the status bar; clicking the spinner
  cancels it (its pipes close, git ends at its next write; nothing is signalled). ssh runs
  with `BatchMode` and short timeouts, HTTP gives up below 1 KB/s for 30 s, and git runs in
  its own session without a controlling terminal, so ssh, hooks and credential helpers can
  never prompt over the panel. Your own `GIT_SSH_COMMAND` / `core.sshCommand` is used as it
  is, without those timeouts: add `-o BatchMode=yes -o ConnectTimeout=15
  -o ServerAliveInterval=15 -o ServerAliveCountMax=2` to it, or a dead connection stalls
  until ssh gives up (cancel it from the status bar). Push sends an explicit
  `refs/heads/<branch>:<upstream>`, so a `+` in `remote.*.push` never forces. A failure shows
  a notification with [Show Git Output].
- Staging a hunk compares the index with the working file as git would stage it
  (`core.autocrlf`, `eol`/`text`, `ident` and clean filters such as LFS applied).
- Git Output (Show Git Output): every git command the panel ran, with its duration, exit
  code and stderr; it follows new commands while open.
- Merge conflicts: Merge Changes lists them. In the editor each block is coloured with
  `merge.*` and its `<<<<<<<` line carries Accept Current Change | Accept Incoming Change |
  Accept Both Changes | Compare Changes (short labels when narrow). Accepting edits the buffer
  as one undoable change; save, then stage the file (`+`) to mark it resolved.

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
| `herdr-code.blocks.style` | `"cards"` (compact: `"lines"`) | `"cards"`: rounded blocks, no blank gutter beside the sidebar, a gutter between editor groups, one shared border between stacked ones; `"lines"`: one shared line between blocks, joined (`├┤┬┴`); `"none"`: flat |
| `herdr-code.fileIcons` | `{}` | `extensions`, `fileNames`, `folderNames`: maps to `{ "glyph": "…", "color": "#rrggbb" }` (either half); `folder`, `folderOpen` |
| `indentRainbow.enabled` | `true` | oderwat.indent-rainbow, same defaults |
| `indentRainbow.colors` | the extension's four `rgba(…,0.07)` | Blended over the editor background |
| `indentRainbow.errorColor`, `indentRainbow.tabmixColor` | `rgba(128,32,32,0.6)`, `rgba(128,32,96,0.6)` | Indent not a multiple of the tab size; tabs and spaces mixed (`""` turns tab-mix off) |
| `indentRainbow.indicatorStyle` | `"classic"` | `"light"`: a thin line per level instead of a background |
| `indentRainbow.excludedLanguages`, `.includedLanguages`, `.ignoreErrorLanguages` | `["plaintext"]`, `[]`, `["markdown"]` | VS Code language ids |
| `indentRainbow.ignoreLinePatterns` | `["/[ \t]* [*]/g", "/[ \t]+[/]{2}/g"]` | JS regex literals; a matching line never gets the error colour |
| `editor.guides.indentation` | `true` | Indent guides (`editorIndentGuide.background` / `.activeBackground`) |
| `editor.tabSize` | `4` | Tab stops, the indent unit, rainbow and guides |
| `workbench.tree.enableStickyScroll` | `true` | Sticky scroll in the Explorer, Search and Source Control lists |
| `herdr-code.restoreSession` | `true` | Reopen the last session's groups, tabs and sidebar for this launch folder |
| `workbench.editor.restoreViewState` | `true` | Restore each file's cursor, selection and scroll: in the restored tabs, and when a closed file is opened again |
| `workbench.tree.stickyScrollMaxItemCount` | `1` | 1: one breadcrumb row of the folder you are in; more (up to 10): nested rows, outermost first, as VS Code (its default is 7) |
| `editor.rulers` | `[90]` | Vertical rulers after N display columns (tabs and wide characters counted), in `editorRuler.foreground`; `[]` for none; `[{ "column": 80, "color": "#rrggbb" }, 100]` gives one its own colour. Drawn behind the text, in the editor and both sides of the diff. F1 "View: Toggle Rulers" for the session |
| `editor.minimap.enabled` | `true` | The minimap (F1 "View: Toggle Minimap" for the session) |
| `editor.minimap.maxColumn` | `120` | Characters the minimap covers (about 8 per cell, 8 to 16 cells, at most 15% of the editor, shrunk to leave minEditorWidth) |
| `editor.minimap.renderCharacters` | `true` | Braille dots (4 lines × 2 dot columns per cell); `false`: half blocks, a line per half |
| `editor.minimap.showSlider` | `"always"` | or `"mouseover"` |
| `editor.minimap.side` | `"right"` | or `"left"` |
| `herdr-code.minimap.minEditorWidth` | `40` | The minimap shrinks (to 4 cells) so the code keeps this many columns, then hides |
| `editor.minimap.size` | `"fill"` | The whole file over the height (at most a line per row); `"proportional"`: a line per dot row, a long file scrolls; `"fit"`: proportional while it fits, else fill |
| `editor.gotoLocation.multipleDefinitions` | `"peek"` | Several definitions: `"peek"` shows them, `"gotoAndPeek"` opens the first and peeks, `"goto"` opens the first |
| `git.blame.statusBarItem.enabled` | `true` | The cursor line's author and age in the status bar |
| `git.blame.editorDecoration.enabled` | `false` | The same, with the commit subject, as ghost text after the cursor line |
| `git.confirmSync` | `true` | Sync asks first (OK is the default, as in VS Code); "OK, Don't Show Again" sets it to `false` |

`herdr-code.spacing` (comfortable / compact defaults):

| Key | Comfortable | Compact | Meaning |
|---|---|---|---|
| `sidebarWidth` | 36 | 30 | Sidebar content columns (a card adds its 2-cell frame) until you drag the divider (a dragged width is kept in layout.json) |
| `sidebarPadding` | 2 | 0 | Left/right padding of sidebar rows and inputs |
| `sectionGap` | 1 | 0 | Blank rows under the sidebar title (not above a card) and between SCM sections |
| `editorPadding` | 0 | 0 | Extra cells before the right-aligned line numbers in editors, diffs and peeks; the number field reserves at least two digits. The Git marker follows the number, then the fold arrow and its clickable trailing blank where folding is available |
| `tabPadding` | 2 | 1 | Spaces each side of a tab label |
| `statusBarGap` | 3 | 3 | Spaces between status bar items |
| `statusBarPadding` | 2 | 1 | Spaces at the status bar's left and right edges |
| `activityBarWidth` | 5 | 3 | Activity bar content columns (a card adds its 2-cell frame), each item 3 rows (compact 2) with the icon centred |
| `listItemGap` | 0 | 0 | 0 or 1 blank row between sidebar list items |
| `popupPadding` | `[3, 1]` | `[0, 0]` | Horizontal and vertical padding in Quick Open, the command palette and dialogs |
| `iconGap` | 2 | 1 | Blank cells after an icon (tree, tabs, SCM rows, title bar, buttons, find widget, status bar); at least 1, since a non-Mono Nerd Font icon can overflow its cell |
| `treeIndent` | 2 | 2 | Cells per tree depth level |
| `titleBarPadding` | 2 | 1 | Left/right padding in the title bar |
| `buttonPadding` | 2 | 1 | Spaces inside buttons (Close, Save, Discard, …) |
| `findWidgetPadding` | 1 | 0 | Padding inside the find/replace widget's frame |

Explorer rows halve the sidebar edge padding (including the leading blank) and both
arrow/icon gaps, rounded to whole cells with at least one blank cell. Tree depth
indentation is unchanged. Other sidebar views keep their configured spacing.

Editor tabs halve `tabPadding` and `iconGap`, rounding up to whole cells. The
reserved unsaved-change marker counts toward the gap before the close button.

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
  The diff editor is read-only and its text is not syntax-highlighted (its minimap is).
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
