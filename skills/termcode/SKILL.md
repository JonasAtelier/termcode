---
name: termcode
description: Drive termcode, the terminal code editor, through its API — see what the user sees, open files at a line, read unsaved text and problems, edit as one undo step, save, run any command, type into its terminal and wait for output. Use when TERMCODE_SOCKET is set (you run in termcode's terminal), when the user mentions termcode, or asks you to show, open or change something in their editor.
---

# termcode

termcode is a VS Code-style editor in the terminal. Every running termcode serves a
JSON-RPC 2.0 API on a private Unix socket (yours only). Everything it can do is a
command; everything it shows is state. You act as a peer of the keyboard.
`termcode api --skill` prints this guide; the design is in doc/api.md.

## Find it

```bash
echo "$TERMCODE_SOCKET"          # set inside termcode's own terminal: that termcode
termcode api ls                  # every running termcode: pid, folder
termcode api --to ~/proj hello   # pick one by folder (or pid)
```

Without `--to`: `$TERMCODE_SOCKET`, else the termcode open on this folder, else the only one.

## Call it

```bash
termcode api METHOD '{"json": "params"}'   # the result as JSON on stdout; errors on stderr, exit 1
```

Positions are `{"line": L, "col": C}`, both from 1, the column in characters (as the
status bar shows). Paths are relative to the workspace (absolute ones must be inside it).

| Method | Params | Result |
|---|---|---|
| `hello` | | api version, termcode version, pid, root, methods |
| `command.list` | | every command: `id`, `title` (as F1 shows it), `key` |
| `command.run` | `id` (or `title`) | runs it, as F1 would |
| `editor.open` | `path`, `line`, `col` | opens it there and focuses the editor |
| `state.get` | `path`: `/workspace`, `/editors`, `/buffers/<file>`, `/diagnostics[/<file>]`, `/git`, `/terminals`, `/ui/screen` | that state |
| `buffer.edit` | `path`, `version`?, `changes`: `[{start, end?, text}]` | new `version`, `dirty` |
| `buffer.save` | `path` | `saved`, `dirty` |
| `terminal.send` | `text`, `index`? | `index`, `mark` (output position before it) |
| `events.wait` | `topic: "terminal.output"`, `match` (regex), `timeout` (s, 30), `since`?, `index`? | `matched`, `output` |

## Recipes

```bash
# What the user sees, as text
termcode api state.get '{"path": "/ui/screen"}'

# Read a file as the editor has it (unsaved edits included), then change it
termcode api state.get '{"path": "/buffers/src/main.rs"}'          # note "version"
termcode api buffer.edit '{"path": "src/main.rs", "version": 7, "changes": [
  {"start": {"line": 3, "col": 1}, "end": {"line": 3, "col": 9}, "text": "let n = 2;"}]}'

# Problems the editor underlines (shellcheck, the C compiler)
termcode api state.get '{"path": "/diagnostics/build.sh"}'

# Run tests where the user watches, and wait for the end
m=$(termcode api terminal.send '{"text": "cargo test\n"}' | sed -n 's/.*"mark": \([0-9]*\).*/\1/p')
termcode api events.wait "{\"topic\": \"terminal.output\", \"match\": \"test result\", \"since\": $m, \"timeout\": 600}"

# Anything F1 can do
termcode api command.run '{"id": "view.toggleMinimap"}'
```

## Permission

Reading is free. Opening or changing files (`editor.open`, `buffer.edit`, `buffer.save`)
and running things (`command.run`, `terminal.send`, `events.wait`, the terminals and the
screen in `state.get`) are asked of the user the first time, in termcode: Allow (while
your process runs), Allow once, or Deny (until termcode restarts). Your call waits up
to two minutes for the answer.

- On "the user said no": stop and tell the user; don't retry or work around it.
- On "read-only" or "off": the user set `termcode.api`; only they can change it.
- Ask for what the task needs and nothing more.

## Rules

- Edit with the `version` you read; on `stale`, read again and redo the change. One
  `buffer.edit` is one undo step for the user (Ctrl+Z).
- An edit opens the file's tab behind the active one; it stays unsaved until
  `buffer.save` or the user saves. Don't save unless asked.
- Don't type into a terminal, commit or run Git writes unless the user asked for that.
- `editor.open` moves the user's cursor and focus: use it when showing is the point.
- Pass `since` (the `mark` from `terminal.send`) to `events.wait`, or earlier output may match.
