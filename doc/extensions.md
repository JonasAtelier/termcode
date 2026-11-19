# Writing a termcode extension

An extension adds a view to termcode, as GitHub and Serial are views. It works the way
a Linux DSA switch driver plugs into the DSA subsystem: **termcode is the subsystem,
your extension is the driver.** termcode owns the frame: the activity bar item, the
panes, their layout, splits, scrolling, hover, the theme, keys and permissions. Your
extension fills the slots it wants with rows and answers what the user does there.

An extension is a program in any language. termcode starts it and talks JSON-RPC 2.0
on its stdin and stdout, one message per line.

## The slots

```
┌──┬──────────────┬──────────────────────────────┬──────────────┐
│◆ │ LEFT         │ MAIN                         │ RIGHT        │
│  │  section A   │ a tab in the editor area     │ the Outline's│
│  │ ──────────── │ (your view's own space)      │ place        │
│  │  section B   ├──────────────────────────────┤              │
│  │              │ BOTTOM (the terminal's place)│              │
└──┴──────────────┴──────────────────────────────┴──────────────┘
```

| Slot | Where | Notes |
|---|---|---|
| `left` | the side bar | in sections (`section`), one under another; the user drags the lines between them |
| `main` | tabs in the editor area | `tab` names one (none: `"main"`, opened when your view is shown); they split like any tab |
| `right` | the panel right of the editors | shown while it has rows |
| `bottom` | the panel under the editors | resized like the terminal |

Clicking your activity bar icon shows your view: only your panes, as GitHub shows only
its own. Fill only the slots you need.

## Quick start

```bash
termcode ext new hello                 # hello/: a manifest and main.py filling every slot
termcode ext install hello --link      # installed as a link: edit, reload, see
```

Reload termcode's window (F1, **Developer: Reload Window**) and click **◆** in the
activity bar. Or, in termcode, click **+** right above Settings, type the folder (or a
git URL) and press Install: it shows at once. `examples/extension/` in termcode's
source is the same example.

The **+** view lists every view: the built-in ones (the Programmer, GitHub) and the
installed extensions, each with **Turn off** / **Turn on**, and **Remove** for an
extension. Installed extensions live in `~/.config/termcode/extensions/<id>`. A git URL
may name a folder of the repository: `https://github.com/you/repo#extensions/thing`.

## The manifest: `termcode-extension.json`

```json
{
  "id": "hello",
  "name": "Hello",
  "version": "0.1.0",
  "description": "What it does, one line.",
  "icon": "◆",
  "run": ["python3", "main.py"],
  "permissions": []
}
```

| Field | |
|---|---|
| `id` | 1 to 40 of `a-z`, `0-9`, `-`; its folder's name once installed |
| `name` | the view's title |
| `icon` | one character for the activity bar, or one per icon set: `{"nerd": "\uf1e6", "unicode": "⇄", "ascii": "S"}` (each falls back to the plainer one) |
| `run` | the command: a program on PATH, or `./path` inside the extension's folder, then its arguments |
| `permissions` | what it may do beyond reading: `"edit"` (change files, show notices, ask the user), `"run"` (commands, terminals). Granted at install |
| `commands` | `[{"id", "title"}]`: in F1 as "Name: Title", there before it starts; running one starts it and sends `view.command` |
| `api` | `{"namespace": {"method": "read" \| "edit" \| "run"}}`: API methods it serves (below) |
| `build` | a command run in its folder at install (`["make"]`, a `cargo install` into `./bin`), shown before the user agrees. Its progress shows on the status bar when it prints `NN%`, or cargo's `Building … n/m` (termcode asks cargo for it: don't pass `--quiet`) |

It runs in its own folder; `TERMCODE_WORKSPACE` is the workspace (also
`state.get` `/workspace`).

## The contract

Your extension calls termcode (requests, with an `id`; each gets a reply):

| Method | Params | Does |
|---|---|---|
| `view.set` | `slot`, `section`? (left), `tab`? (main), `title`?, `rows` | replaces that pane's rows |
| `view.append` | the same | adds rows at the end (a log; it follows the end until scrolled up) |
| `view.clear` | `slot`, `section`? / `tab`? | removes the pane (left without `section`: every section; a tab: closed) |
| `view.open` | `tab`? | shows your view and that tab, in front (needs `edit`) |
| `ui.input` | `text`, `value`? | asks the user for a line (`{"text"}`, null if closed; needs `edit`) |
| `ui.pick` | `text`, `items`: labels or `{label, detail}` | asks the user to pick one (`{"index", "label"}`; needs `edit`) |
| every API method | | `state.get`, `editor.open`, `buffer.edit`, `ui.notify`, `ui.prompt`, `events.watch`, … ([api.md](api.md)) |

termcode tells your extension (notifications, no `id`; don't reply):

| Method | Params | When |
|---|---|---|
| `view.shown` | `visible` | your view was shown or left |
| `view.action` | `slot`, `section`, `tab`, `action` | a click on a row or cell that has an `action` |
| `view.key` | `slot`, `tab`, `key`, `ctrl`, `alt`, `shift` | a key while your side bar or tab has the focus (`key`: `a`, `Enter`, `Up`, `F5`, …) |
| `view.paste` | `tab`, `text` | a paste into your tab |
| `view.tab` | `tab` | your tab now in front (what your right panel is about) |
| `view.resize` | `tab`, `width`, `height` | your tab's size, first and when it changes (a terminal-like view sizes its screen by it) |
| `view.closed` | `tab` | the user closed your tab (you decide what that ends; `view.clear` forgets it) |
| `view.wheel` | `tab`, `lines` | the wheel over a tab you scroll yourself (`view.set` with `scroll: "self"`) |
| `view.command` | `id` | one of your F1 commands ran |
| `event` | `topic`, … | what you asked for with `events.watch` |

Replies and notifications share your stdin: while you wait for a reply, keep what else
arrives for later (the example's `call()` does).

Your extension starts when its view is first shown, and should exit when its stdin
closes (termcode ended or reloaded). If it ends, its side bar shows why, with a
**Restart** button.

## Rows

A row is one of:

```json
"plain text"
{"header": "Devices", "open": true, "extra": "3", "action": "fold:devices"}
{"text": "one cell", "fg": "error", "bold": true}
["several ", {"text": "cells", "fg": "accent"}, {"text": "Go", "button": true, "action": "go"}]
{"cells": ["a list item"], "action": "open:3"}
```

A cell: `text`, `fg`, `bg`, `bold`, `dim`, `italic`, `button` (drawn as a button), `action`
(what `view.action` sends back on a click), `hint` (a card shown while the pointer is
on it). A `header` row is drawn as the side bar's section headers are (a chevron, the
title, a dim `extra`); the side bar's own sections fold by their chevron. An extension
with a `README.md` in its folder shows it on its page in the + view. A row with `action` clicks as a whole, like a list item; a row's `hint` is its
card. Rows and cells light up under the pointer when they click. In a tab, dragging
selects text and Ctrl+C copies it (termcode's, not sent to you).

Colors are theme names, so your view follows the user's theme: `fg`, `bg`, `dim`,
`accent`, `error`, `warning`, `ok`, `info`, `selection`, `input`; or `#rrggbb`. Control characters are
shown as `�`: escapes never reach the terminal. A pane keeps the last 10,000 rows.

## Permissions and safety

- Reading (state, files, diagnostics) and your own panes need nothing.
- `edit` and `run` are shown when the user installs your extension and granted then.
  Anything not granted is asked in termcode the first time, as for any API client.
- An extension runs as the user, with their files and network. Ask for what you need
  and nothing more; say in `description` what you do.

## Debugging

- What your program prints on stderr is in `~/.local/state/termcode/extensions/<id>.log`.
- Stdout is the protocol: print nothing else there, and flush after each message.
- `termcode ext list` shows what is installed, and why a broken one doesn't load.

## A full example: Serial, out of tree

termcode's Serial view is an extension: `extensions/xcomm/` in termcode's source, in Rust
on the xcomm crate. Ports and channels in the side bar (a card on hover), a tab per
channel (its screen sent as rows, at most 30 times a second, `scroll: "self"` for its
scrollback), keys and pastes to the device, line settings on the right, notes through
`ui.input`, F1 commands with `ui.pick`; it serves the API's `serial.*` for agents and
publishes `serial.output`. Its manifest builds it at install (`cargo install --path .
--root .` into `./bin`). Install it from the + view (SUGGESTED).

## Serving API methods, publishing events

An extension can serve API methods to other clients (agents, scripts): its manifest
names them, each with what calling it may do, asked of the caller as termcode's own are:

```json
"api": {"serial": {"ports": "read", "send": "run", "note": "edit"}}
```

A call to `serial.ports` then comes to the extension as `api.call {method, params,
caller}` with an `id` (a string): answer it with that `id` (`result`, or `error`), when
ready (a wait answers later). `events.emit {topic, …}` publishes an event in a namespace
it serves (`serial.output`) to `events.watch` clients. termcode's own namespaces
(`state`, `buffer`, `ui`, …) are never an extension's.

## Built-in views are drivers too

As in DSA, in-tree and out-of-tree drivers plug into the same subsystem. The Programmer
(`src/programmer.rs`), GitHub (`src/github/`) and the **+** view (`src/ext/manager.rs`)
are Rust drivers built into termcode: each fills one
`ext::Ops` table (`src/ext/mod.rs`, like `dsa_switch_ops`) and is listed in
`ext::DRIVERS`. termcode's frame has no special case for any of them. A built-in view
can draw anything (a terminal screen, pictures); an extension draws rows, from any
language, without rebuilding termcode. Extensions themselves are served by one more
table of ops, `src/ext/process.rs`.

## Sharing it

Put it in a git repository with the manifest at its root. Others install it with:

```bash
termcode ext install https://github.com/you/termcode-hello
```

They see its name, command and permissions, and confirm. `termcode ext remove hello`
takes it away.
