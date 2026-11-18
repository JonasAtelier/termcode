# termcode API: design

> Phase 1 is built (src/api.rs the socket and waits, src/rpc.rs the methods; the guide
> for agents is skills/termcode/SKILL.md, the end-to-end check `scripts/api-check.py`).
> The rest is the plan.

## The idea

**The API is termcode.** Keys, F1, quick keys and the mouse already all end in one place,
commands acting on one state. The API exposes the same things, so any process is a peer
of the keyboard: a shell script, CI, an agent, a tool of your own, another termcode.
Nothing is "for agents"; agents are one client among many. And nothing is "for this
machine": the API is lines of JSON over any byte stream, so a termcode on the other end
of SSH, a network or a serial cable is a client too.

```
 clients   scripts · CLI · agents (MCP) · extensions · tests · other termcodes
              │  JSON-RPC 2.0, one message per line
 link       Unix socket · stdio · SSH · TCP+TLS · serial (UART, USB CDC) · MCP adapter
              │
 core       command   state   events   edit   ui (contribute)   provider
              │
 termcode   the App and its event loop  (or the same App with no screen: headless)
```

Rule from now on: **no feature without its command**, so the API grows with every
feature by itself.

## 1. `command`: everything termcode can do

- `command.list`: every command with a stable id, its title, its arguments' schema and
  its key. Generated from the registry (`COMMANDS`, `GIT_COMMANDS`), never by hand.
- `command.run {id, args}`: `view.toggleMinimap`, `git.commit {message}`,
  `editor.open {path, line, col}`, `terminal.send {id, text}`…
- Ids are stable (`view.toggleMinimap`); titles may change. Settings, `keybindings` and
  `prefixKeys` accept ids too.

## 2. `state`: one tree, read by path

```
/workspace          root, folders, branch
/editors            groups → tabs → path, dirty, cursor, selections, scroll, look
/buffers/<path>     text (unsaved included), version, language, symbols
/diagnostics        per file
/git                status, branch, ahead/behind, log (paged)
/terminals/<n>      screen, scrollback (a range), cwd, running program
/ui                 focus, panes and where they are, theme
/ui/screen          the screen as text, ANSI or PNG: what the user sees
/settings           the values in effect and where each came from
```

`state.get {path, range?}`. `/ui/screen` lets a client see exactly what the user sees:
for agents, screenshots in docs, visual tests, bug reports.

## 3. `events`: watch, and wait

- `events.watch {topics}` streams notifications on the same connection:
  `buffer.changed`, `file.saved`, `file.opened`, `cursor`, `diagnostics`, `git`,
  `terminal.output`, `focus`, `command.ran`.
- `events.wait {topic, match, timeout}` answers once: a terminal printed a line matching
  a regex, diagnostics settled, a Git job ended, a file was saved. Scripts stay free of
  sleeps and races (as herdr's `wait`).

## 4. `edit`: versioned, undoable, reviewable

`buffer.edit {path, version, changes: [{range, text}], label, mode}`

- `version`: the buffer's version the client read; stale → an error with the current one,
  and the client rebases. Unsaved buffers and unopened files both work.
- One edit = one undo step, labelled by the client (`Undo: rename foo (claude)`).
- `mode`:
  - `apply`: straight in.
  - `propose`: opens the diff look; the user takes or drops each hunk; the reply says
    what was taken.
  - `ghost`: shown as grey text at the cursor, Tab takes it: completions from anything.
- `buffer.save`, `buffer.format`; Git writes go through `command.run`.

## 5. `ui`: clients add to termcode (extensions without a plugin runtime)

The client runs anywhere, in any language; termcode draws. What a client adds lives as
long as its connection.

- **Commands**: in F1, bindable to keys and quick keys; running one calls the client.
- **Views**: a declarative tree (list, tree, table, text, markdown, progress, form,
  button) shown as a side bar page, an editor tab or a panel tab; clicks and keys come
  back as events. A CI dashboard, a test explorer, a serial plotter, a ticket list.
- **Decorations**: underlines, gutter marks, text after a line, line backgrounds:
  coverage, test results, review comments, an agent's notes.
- **Status bar items, notifications, prompts** (buttons; the reply is the button).
- `termcode.extensions` (setting): commands termcode starts with itself and stops with
  itself, as herdr plugins.

## 6. `provider`: language features from outside

A client registers for languages: hover, definition, references, completion, symbols,
diagnostics, formatting. termcode asks providers first, its own `rg` search after.
**An LSP bridge is then just a client** (`termcode-lsp clangd`): LSP needs no core
change, and any indexer (ctags, a company's) plugs in the same way.

## 7. Record and replay

Everything the user does is already a command (`Cmd::Keys` replays keys). F1
**Developer: Record Actions** writes them as API calls, one per line; `termcode api run
FILE` plays them back. Macros, reproducible bug reports (attached by Report Issue), UI
tests, demo recordings.

## 8. Headless

`termcode --headless [--api-stdio] [FOLDER]`: the same App without a screen, the API on
stdin/stdout. CI and scripts open, edit, check (shellcheck, clang), run Git, render a
screenshot, quit. termcode's own tests can use it.

## 9. Many termcodes

`termcode api ls` lists running termcodes (pid, folder). A client picks one with
`--to FOLDER|PID`; by default `TERMCODE_SOCKET` (in termcode's terminal), else the one
open on the current folder. Later: **follow**, one termcode showing another's editor
(pairing over SSH with a forwarded socket).

## 10. Links: termcode to termcode, over anything

The protocol needs only a two-way byte stream, so the link is a choice, not a design:

| Link | How | Who checks who |
|---|---|---|
| Unix socket | the default, this machine | file permissions (same user) |
| stdio | `termcode api --stdio`: the socket on stdin/stdout, so any pipe is a link | the pipe's owner |
| SSH | `termcode connect ssh://host[/folder]` runs `termcode api --stdio` there | SSH keys |
| TCP | `termcode listen tcp://0.0.0.0:7419`, `termcode connect tcp://host:7419` | TLS, paired once (below) |
| Serial | `termcode listen serial:///dev/ttyUSB0?baud=115200`, `connect` the same | pairing; the cable is the boundary |

What a link gives, the same over each:

- **Control**: run the other side's commands, type into its terminals, open its files.
- **Mirror**: watch the other side's screen live (`/ui/screen` as changes only, as tmux
  sends a pane), or follow its editor, cursor and tabs in your own layout.
- **Remote workspace**: your termcode, its files: the other side's Explorer, Git,
  terminals and checks, drawn here. The remote development termcode doesn't have today,
  with nothing installed but termcode on both sides.
- **Pairing**: two people in one buffer, each edit versioned (section 4); a conflict is
  rebased, and later merged by a CRDT if live co-editing needs it.

For slow and noisy links:

- **Framing**: over serial each line carries a sequence number and CRC32; a bad or lost
  line is sent again. Sockets, SSH and TCP don't need it.
- **Budget**: `hello` states the link's speed; the other side then sends changes only,
  compressed (zstd), drops screen frames before events, events before nothing. 115200
  baud is about 11 KB/s: enough for control, text and a mirrored text screen; pictures
  stay local.
- **Reconnect**: a dropped link resumes from the last sequence number; the session
  (grants, watches) survives.

Pairing (TCP, serial): `termcode listen` shows a 6-digit code; `connect` asks for it;
both remember the other's key (as SSH's known_hosts) and say so if it changes.
Listening is off unless started, and a remote client starts with `read` only.

## Protocol

- JSON-RPC 2.0, one message per line: `socat`, `nc -U`, any language.
- `hello {client, version, wants}` first; the reply says what was granted and the API
  version.
- Methods by area: `command.*`, `state.*`, `events.*`, `buffer.*`, `ui.*`, `provider.*`.
- CLI: `termcode api METHOD [JSON]`; `termcode api schema` prints every method's schema
  (generated); `termcode api --skill` prints [SKILL.md](../skills/termcode/SKILL.md).
- MCP: `termcode mcp` serves MCP on stdio, its tools mapped onto the API (open, read,
  edit, propose, run a command, terminal, screen), so an agent connects with one line of
  config. An adapter, nothing of its own.

## Safety

- Local by default: the socket 0600 in a 0700 folder. Other links only when `listen` or
  `connect` is run, each authenticated (section 10); remote clients start read-only.
- Grants by kind (built, see above): `read` given; `edit` and `run` asked per client;
  later `ui` and `provider` too. Answers last while termcode runs.
- Every edit undoable; F1 **Developer: Show API Log** lists what each client did.
- `termcode.api.enabled: false` closes the socket (then `termcode PATH` in its terminal
  opens a second termcode, as before).

## Phases

| | What | Version |
|---|---|---|
| 1 ✅ | JSON-RPC on the socket, `hello`, `command.list/run` (ids from titles), `editor.open`, `state.get` (workspace, editors, buffers, diagnostics, git, terminals, screen), `buffer.edit` (`apply`, versioned, one undo step), `buffer.save`, `terminal.send`, `events.wait` (terminal output), the `termcode api` CLI, SKILL.md | X |
| 1b | grants ✅, serial ports ✅ (`serial.*`: monitor, read, wait, attach, send, set, close, notes), `buffer.edit` `propose`, `events.watch` ✅ (a stream) | Y |
| 2 | `events.wait` ✅, `/ui/screen` ✅, `ui.*`: notifications ✅, prompts ✅, decorations ✅; commands, status bar items | Y |
| 3 | `provider.*` (and the LSP bridge), views, record and replay, headless, MCP | X each |
| 4 | Links: `--stdio` and SSH first (they need no new security), then mirror, remote workspace, TCP and serial with pairing | X |

## Decided for phase 1

- Only the same user's processes connect (folder 0700, socket 0600, the peer's uid
  checked with SO_PEERCRED; the folder must be the user's own).
- A client is the process behind the connection past shells and `termcode api` (an
  agent, a script), by pid and start time. Reading is free; `Edit` (open, change, save)
  and `Run` (commands, terminals, the screen) are asked: Allow while it runs, Allow
  once, Deny until restart. Enter and Esc never allow (Deny is selected). termcode's own
  terminal is the user's and is not asked. `termcode.api`: `ask`, `read-only`, `off`;
  `TERMCODE_API_ALLOW=edit,run` in termcode's environment allows for automation.
- Every allowed change or run shows on the status bar (`API: <client> — <method>`).
- Limits: 16 connections, 64 waiting requests, 64 KB a request, a wait 1 h, a pattern
  1000 characters with bounded backtracking, output matched only when it grew.
- Command ids are made from titles (`View: Toggle Minimap` → `view.toggleMinimap`).
- `editor.open` and `terminal.send` are methods of their own (they take arguments).

## Open questions

1. Grants: ask once per client (above), or trust everything started in termcode's own terminal?
2. `buffer.edit`'s default mode: `apply` or `propose`?
3. Headless and MCP earlier than phase 3?
4. Stable ids for all ~140 commands now (they become public names)?
5. Links: which first after SSH, TCP or serial? And is mirroring a screen enough at
   first, or the full remote workspace?
