---
name: termcode-extension
description: Design, write and install a termcode extension — a program that adds its own view to the termcode editor (activity bar icon, side bar sections, a main tab, right and bottom panels) over JSON-RPC on stdio. Use when asked to build a termcode extension or plugin, add a view or pane to termcode, or turn a tool into a termcode view.
---

# termcode extension

termcode is the subsystem, the extension a driver (as Linux DSA): termcode owns layout,
splits, scrolling, hover, theme, keys and permissions; the extension only fills slots
with rows and handles clicks and keys. Full guide: `doc/extensions.md`; the API it can
also call: `termcode api --skill`. This guide: `termcode ext --skill`.

## Make one

```bash
termcode ext new NAME              # NAME/termcode-extension.json + main.py (all slots, a call() helper)
termcode ext install NAME --link   # then the user reloads termcode (F1: Developer: Reload Window),
                                   # or installs it from the + view above Settings (shows at once)
termcode ext list                  # installed, and why a broken one doesn't load
```

Start from the generated `main.py`: keep its `send`/`call`/`messages` helpers.

## Manifest (`termcode-extension.json`, the folder's root)

`id` (a-z 0-9 -, ≤40), `name`, `version`, `description`, `icon` (one character),
`run` (`["python3", "main.py"]`; `./x` = inside the folder), `permissions` (`[]`,
`"edit"`, `"run"`), `commands` (`[{id, title}]`, in F1), `build` (run at install, e.g.
`cargo install --path . --root .`). It runs in its own folder; `TERMCODE_WORKSPACE`: the
workspace. A big example: `extensions/xcomm/` (termcode's Serial view, out of tree).

## Protocol (JSON-RPC 2.0, a message a line, stdin/stdout)

Calls (with `id`; await the reply, queue other lines meanwhile):

| Method | Params |
|---|---|
| `view.set` / `view.append` | `slot`: left, main, right, bottom; `section` (left: which; drag-resizable, stacked in order created); `tab` (main: which tab; default "main"); `title`; `rows` |
| `view.clear` | `slot`, `section`? / `tab`? (a tab: closed) |
| `view.open` | `tab`? (needs edit) shows the view and that tab |
| `ui.input` / `ui.pick` | `text`, `value`? / `items` (needs edit): a line typed, a row picked (null: closed) |
| any API method | `state.get`, `editor.open`, `buffer.edit`, `ui.notify`, `ui.prompt`, `events.watch`, `terminal.send`, … |

Notifications to it (no `id`, never reply): `view.shown {visible}`, `view.action {slot,
section, tab, action}`, `view.key {slot, tab, key, ctrl, alt, shift}`, `view.paste {tab,
text}`, `view.tab {tab}` (in front), `view.resize {tab, width, height}`, `view.closed {tab}`,
`view.command {id}`, `event {topic, …}`.

Rows: `"text"` · `{text, fg, bg, bold, dim, italic, button, action}` · `[cells…]` ·
`{"cells": [...], "action": "id"}` (whole row clicks). Colors: `fg dim accent error
warning ok info selection` or `#rrggbb`. ≤10,000 rows a pane.

## Rules

- Stdout is the protocol: one JSON object per line, flushed. Logs go to stderr
  (`~/.local/state/termcode/extensions/<id>.log`).
- Exit when stdin closes. Never busy-loop; block on stdin or your data source.
- Redraw a pane with one `view.set`; stream with `view.append`. Keep `action` strings short
  ids (`open:3`), and keep the state on your side.
- Ask for the fewest permissions: reading needs none. Anything else is asked of the user.
- Don't wait long on one call (`events.wait`): your calls are answered in order. Use
  `events.watch` for changes.
- Install from a folder with `--link` while developing; publish as a git repo with the
  manifest at its root (`termcode ext install https://…`). Never install for the user
  without them confirming.
- Check it: install with `--link`, ask the user to reload and click its icon; read its log.
