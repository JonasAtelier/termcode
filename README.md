# herdr-code

herdr plugin, written in Rust. Run `herdr-code` in a herdr pane to open a second terminal window
on the **same herdr session**. The new window shows a `Code: <repo>` workspace with a VS Code-like
editor for the pane's repo or worktree: activity bar, a resizable Explorer / Search /
Source Control / History sidebar, editor groups with preview tabs, a side-by-side diff, and
the "Visual Studio Dark - C++" colours.

## Install

```sh
cargo install --path . --root ~/.local -j3     # -> ~/.local/bin/herdr-code
herdr plugin link "$PWD"
```

The manifest runs `herdr-code` through PATH, so the herdr server's PATH must include `~/.local/bin`.

## Use

- `herdr-code` from a shell in a herdr pane, or the plugin action `jonas.herdr-code.open`. The
  action uses the invoking pane's `foreground_cwd`.
- Outside a git repo it exits with status 1. If a window is already open for this session and
  worktree, it prints "already open" and does nothing. It cannot raise the existing window.
- Terminal: `ptyxis --new-window -- …` by default. To use another terminal, set
  `HERDR_CODE_TERMINAL` to an argv prefix, for example `HERDR_CODE_TERMINAL="kitty --"`.
- Closing the panel (the title bar's Close button, or Ctrl+Q) detaches only that window's
  client. Unsaved files prompt Save All / Don't Save / Cancel first. The wrapper then closes the
  Code workspace. The session and the main window are not touched.
- Icons are VS Code codicons from a Nerd Font (`nf-cod-*`). Set `HERDR_CODE_ICONS=0` for plain
  ASCII. The sidebar width and visibility are kept in `$HERDR_PLUGIN_STATE_DIR/layout.json`.
- `prefix+g` is herdr's default `goto`, so no keybinding is shipped. To add one, bind
  `plugin_action` `jonas.herdr-code.open` to a free key.

## Panel keys

In ptyxis, Ctrl+Shift chords, Ctrl+Enter, Shift+Enter, Ctrl+Tab and Ctrl+1/2/3 arrive as their
plain forms. VTE has no kitty keyboard support and herdr does not enable modifyOtherKeys for it.
The fallbacks below always work.

| Key | Action |
|---|---|
| Alt+1 / F1, Ctrl+E | Explorer |
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

## Known limits

- Copy tries `wl-copy`, then `xclip`, then OSC 52. Paste uses bracketed paste (the terminal's
  own paste), or `wl-paste`/`xclip` for Ctrl+V, else the panel's last copy.
- Files that were truncated (over 1 MB or 20k lines), are not UTF-8, or are binary open
  read-only and can never be saved. If a file changes on disk, a clean buffer reloads; a dirty
  one keeps your edits, and the first Ctrl+S refuses so a second one is needed to overwrite.
- Find is literal (Match Case and Whole Word toggles). Search in files can also use regex.
  The diff editor is read-only and not syntax-highlighted.
- Window placement: the new client starts on the server's active workspace. The launcher sets
  this with `plugin pane open --focus`, which does not move attached clients, right before the
  client starts. Switching workspaces in the main window at that same moment can still win the
  race.
- Whether ptyxis closes the window when the command exits depends on the profile's exit action.
- Lines over 4000 bytes are not highlighted. The token colours are VS Code's
  `cpptools_dark_vs_new.json` (MIT, Microsoft), embedded under `assets/` and mapped onto
  syntect's grammars, so some scopes differ from VS Code's TextMate grammars.
