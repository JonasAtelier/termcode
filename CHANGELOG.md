# Changelog

What changed in each termcode release, newest first.

Versions follow [Semantic Versioning](https://semver.org): before 1.0, a new MINOR
version (0.2.0) adds features or changes settings or keys, and a new PATCH version
(0.1.1) only fixes bugs.

## [Unreleased]

### Added
- The line between editor groups drags: their widths (or heights) follow, in the code
  view and in a view's own (Serial's channels side by side). With cards, a pane's edge
  takes either border there (the side bar's, the editors', a panel's).
- In Ghostty and kitty the Source Control graph is drawn in pixels, as VS Code draws
  it: smooth curved branch lines, a ring for each commit (HEAD's filled), the lanes going
  on past an opened commit's files, and the subjects lined up right of it. Elsewhere the
  graph stays in box-drawing characters.
- Resting on a picture shows it in a card: an image file in the Explorer (beside its
  tooltip), or a picture's path in the editor (`docs/shot.png`, `~/pics/a.jpg`: beside the
  file, then in the workspace), with its size in pixels. Real pixels in Ghostty and kitty,
  half-blocks elsewhere.
- In Ghostty and kitty the activity bar's icons, and the GitHub view's mark (where the
  account has no picture), are drawn in pixels, bigger than the font draws them. Other
  icon sets, and terminals without pictures, keep the font's icons.
- In Ghostty and kitty the editor's and the diff's scrollbars are drawn in pixels: a
  rounded slider placed to the pixel, the overview ruler's marks (changes, problems,
  matches, the cursor) as thin bars over it. Quick Open's slider too.
- In Ghostty and kitty popups (Find, Commit, Settings, hover cards) get a rounded frame
  in pixels: a thin line at their outer edge, the corners clear.
- In Ghostty and kitty the Settings and Commit dialogs are drawn with more pixels: fields,
  dropdowns, the search box, the chosen category, setting, file and the message box have
  rounded corners; number settings are sliders with a round knob; New Commit / Amend are
  round radio buttons; separators meet a popup's frame.
- Extensions can show pictures: a `{"picture": <base64 PNG>, "rows": 8}` row (a plot, a
  waveform, a diagram), in real pixels in Ghostty and kitty, half-blocks elsewhere.
- In Ghostty and kitty the UI kit draws in pixels where text cannot: buttons get rounded
  ends (with any font), scrollbar sliders are rounded and move by the pixel, checkboxes
  are rounded boxes with a real tick. A popup over them keeps them under it.
- Colours written in a file (`#0e639c`, `#fff8`, `rgb(…)`, `rgba(…)`) show on their own
  colour, their text black or white to read on it (`#123`, an issue's number, does not).
- In Ghostty and kitty, badges (Source Control's count, the activity bar's) are rounded
  in pixels, and bars are smooth rounded bars: an extension's install progress, and the
  ELF and map reports' bars (a memory region's green, amber past 75%, red past 90%).

### Fixed
- Settings in a small window: the quick keys' keyboard no longer draws past the window;
  categories that do not fit get a slider and go by the wheel, a long name is cut before
  its count; the keyboard's legend gives way to its prefix.
- A diff no longer scrolls one column sideways (its first characters gone) when a
  selection ends on a side's right edge, as a double-clicked word there.
- A quick key's command list stays whole over Settings' footer (drawn last, as tall as
  its commands), and Apply and Save under it take no clicks.
- A view's right panel (Serial's settings, an extension's) resizes by its left edge, as
  the Outline does; its edge lights under the pointer.
- The editor's caret (or a field's behind it) no longer blinks through the Settings and
  Commit dialogs; only a field in the dialog being typed in shows one.

## [1.0.0] - 2026-11-20

### Added
- Settings › Keys shows the quick keys as a keyboard: a key with a command is lit, pointing at
  one shows what it runs on its frame, clicking it opens the commands under it (type to find one,
  or None to free the key). Only what differs from the defaults is saved.
- The title bar shows termcode's version: `main.rs — termcode 1.0.0`.
- A long tab bar scrolls: the wheel over it, or its ‹ › arrows when tabs are hidden on
  that side; it follows the active tab (Ctrl+PgDn, a file opened) and leaves no blank end.
- Extensions: a program adds its own view, as Serial and GitHub are views. termcode
  draws the frame (an activity bar icon, side bar sections split by draggable lines, a
  tab, the right and bottom panels); the extension fills them with rows and answers
  clicks and keys over JSON-RPC on stdio, with the whole API. `termcode ext install`
  (a folder or a git URL; shows the command and permissions, asks), `list`, `remove`,
  `new`. Guides: doc/extensions.md, `termcode ext --skill`.
- The + view, right above Settings: install an extension from a git URL or a folder
  (shown at once, no reload), turn any view off or on, remove an extension.
- The + view, after VS Code's Extensions view: a search box (a URL or a folder installs),
  INSTALLED, BUILT-IN and RECOMMENDED sections, an item each (icon, name, version,
  description, state), and a page per extension (Disable, Uninstall, Restart, its
  commands, API methods, log, README). Installed extensions' icons sit right above +,
  in termcode's icon set (`icon` may give one per set); their side bars' sections look
  and fold like Source Control's.
- Installing and uninstalling an extension show their progress on the status bar and in
  the + view: a bar and a percentage, from git's clone, the build's own (cargo's units
  built) and the files removed. termcode no longer waits up to 5 s to notice a step done.
- The Serial view moved out of termcode: it is the xcomm extension now (+ view, SUGGESTED;
  or `termcode ext install https://github.com/JonasAtelier/termcode#extensions/xcomm`),
  serving the API's `serial.*` and `serial.output` as before. termcode no longer links xcomm.
- Extensions can serve API methods (`api` in their manifest, each method's tier asked as
  termcode's own are) and publish events; their tabs can scroll themselves (a terminal's
  scrollback), select and copy text, and show a card on hover. A git URL may name a
  folder of the repository (`URL#folder`).
- Extensions can have several tabs, their own F1 commands, ask the user for a line or a
  pick (`ui.input`, `ui.pick`, also in the API), hear pastes, their tab's size and closing,
  and build at install. The Serial view runs out of tree too: `extensions/xcomm/`.
- Serial, the Programmer and GitHub are built-in drivers of the same view subsystem as
  extensions (one table of ops each, as Linux DSA's in-tree drivers); each can be turned off.
- Mistakes underlined as you type, no language server: C/C++ through the compiler after a
  short pause (not only on save), Rust's syntax in-process. Saving a Rust file runs
  `cargo check` in the background for type and borrow errors: asked once per project (it
  runs build scripts), never in an untrusted repository, skipped under 1.5 GB of free
  memory, two jobs at most.
- A Serial view, on xcomm: the serial ports with their adapter, USB ID, by-id link and
  who holds each (another xcomm, in which herdr pane); the channels open here with their
  line settings, state and bytes in and out; per channel the baud (‹ ›), data bits,
  parity, stop bits and flow control (each click the next), DTR, RTS, a
  break, capture to a file and close. A channel's output streams in an editor tab (split
  it like code) and the keys typed there go to the device (all but F1, Ctrl+Q and the
  tab keys); Shift+PgUp/PgDn and the wheel scroll back; a drag selects (a double click,
  a word) and Ctrl+C copies it. Ctrl+T W closes it (a new quick
  key, View: Close Editor, for any tab). Ports are locked and opened as xcomm does, and opened again after an
  unplug, by the USB port. Needs Rust 1.93 to build. A port another xcomm holds can be
  shared through its control socket: read only (watch and capture, nothing disturbed)
  or read & write (typing goes through that xcomm), asked in a small window first. Dots
  show each port's and channel's state: green reads and writes, yellow is on hold or
  busy (it still reads), red is disconnected. A note beside each tty (`n` or ✎; kept per
  adapter, by its USB serial number); rest the pointer on a port, or press `i`, for its
  card: adapter, USB ID and serial, by-id and by-path links, holder, line settings, modem
  lines, traffic. Agents: `serial.ports` and `serial.note` in the API, and a port opened
  here has xcomm's control socket, so `xcomm ctl ttyUSB0 run … --until …` (and xcomm's
  agent skill) drives it as an xcomm session, its sends shown as dim `[ctl]` lines;
  per channel, Agents: read & write, read only or off.
- An API: `termcode api METHOD [JSON]` (JSON-RPC 2.0 on termcode's private socket) runs
  any command, opens files, reads open and unsaved text, problems, Git, terminals and the
  screen, edits (versioned, one undo step) and saves, types into a terminal and waits for
  its output. `termcode api --skill` prints the guide for AI agents. Only your user's
  programs reach it; before one changes files or runs anything termcode asks you, and
  `termcode.api` makes it read-only or turns it off. Serial ports too: `serial.monitor`,
  `read`, `wait`, `attach` (asked in termcode when another xcomm holds it), `send` (only
  attached; shown in the tab as `[api]`), `set` and `close`. `events.watch` keeps a
  connection for events: edits, saves, files opened and closed, the active tab, the
  cursor, problems, Git, terminal and serial output (`termcode api events.watch` prints
  them, a JSON line each). `ui.notify` (the status bar), `ui.prompt` (a question with
  buttons, the one clicked the answer), `ui.decorate` / `ui.clear` (marks on lines,
  shown as problems with a note). `termcode mcp`: the API as an MCP server for agents.
  `termcode --headless [--api-stdio] FOLDER`: termcode with no terminal, driven through
  the API alone (on stdin and stdout with `--api-stdio`); it asks no one, so it does
  only what TERMCODE_API_ALLOW lets it.
- GitHub sign-in by QR code: next to Sign in with GitHub, Sign in with a QR code shows
  GitHub's device page as a QR code for your phone, with gh's one-time code to enter
  there (Q in the view). gh keeps the token either way.

### Changed
- Serial view: a ports pane and a channels pane, the line between dragged to resize (as
  GitHub's, CHANNELS on it with a dot per channel in its state's colour, or past five
  a count per state), each scrolling; blocks and buttons light up under the pointer, a port's
  card shows while the pointer rests on it. Monitor on a port watches it, read only,
  no questions; Attach on a channel turns typing on (asked only when someone else holds
  the port, or said when it is gone). Its settings (baud, format,
  modem lines, break, record, agents) are a panel beside its tab: the Outline's button.
- Split Editor Down also by dragging a tab to an editor's top or bottom edge, or the new
  button beside Split Editor; each space can have four groups.
- Code, Serial, GitHub and the Programmer each have their own editor area: a view shows
  only its own tabs and groups (Serial its streams, no terminal panel), and going back
  to the Explorer brings the code back as it was. A tab opens in its own area.
- Split Editor Down (Ctrl+T -) as well as Right (Ctrl+T \\): groups in columns, up to
  four (2 × 2); Ctrl+T and the arrows move between them.
- The side-by-side diff's minimap is the editor's: pixels in Ghostty and kitty, and each
  changed line tinted (green inserted or changed, red removed).
- The change, problem and find marks beside the scrollbar sit under its slider exactly
  when their lines are on screen (they drifted down the file).
- termcode's own folders (unsaved-file backups, commit drafts, sessions, settings) are
  made readable by you alone, whatever made them before.
- The .deb's binary is built with link-time optimisation: about a fifth smaller.
- ratatui 0.30, vt100 0.16; the Settings window's pictures no longer need ratatui-image.

### Fixed
- Editor splits: as many as the editor area holds (each group 30 columns by 7 rows at
  least), no longer 4 groups in all; a 3 + 1 layout's right side splits down. Moving a
  column's only tab away, closing it beside another view's groups, or restoring a session
  no longer moves the group under it into another column. Split buttons and drop edges
  show only where a split fits.
- termcode no longer records a crash when its terminal goes away under it (its window
  closed, its tmux session killed), which made the next start report an error.

## [0.5.0] - 2026-11-18

### Added
- A prefix key, Ctrl+T (`termcode.prefix`), then one quick key runs a command, wherever
  the keys are, the terminal too: t terminal, c / n / p / x new, next, previous, close
  terminal (the last one only hidden), f go to file, s search, e Explorer, g Source Control, b side bar, o Outline,
  m minimap, z word wrap, v preview, h hexdump, [ ] previous / next editor, `,` Settings,
  arrows the pane that way (View: Navigate Left/Right/Up/Down).
  The keys show while it waits; Ctrl+T twice sends Ctrl+T; `termcode.prefixKeys` changes
  them (any F1 command).
- settings.json explains every option: what it does, the values it takes, which work
  best and which may break. F1 Preferences: Refresh Settings File rewrites an older file
  in this layout, keeping your values (the old one as settings.json.bak).

### Changed
- termcode is open source, under the MIT license.
- The minimap keeps a fixed scale (`editor.minimap.size` now defaults to
  `"proportional"`), so its slider stays easy to grab in a long file. It shows a stretch
  of the file that stays put while you scroll inside it and moves only when the view
  reaches its top or bottom edge; a click jumps there and centres the stretch on it.
  The slider dragged to (or past) its edge, or a click held near one, keeps the editor
  scrolling while held, faster the farther past the edge. `"fill"` brings back the
  whole file.
- In Ghostty and kitty the minimap is drawn in real pixels at VS Code's scale
  (`editor.minimap.scale`, 1-3, default 3: a line 6 px tall, a character 3 px wide;
  the minimap widens to up to 24 cells for it),
  not braille dots: each character is its glyph in your monospace font, shrunk as VS
  Code shrinks it, in its token's colour; it is sent only when the code
  or its stretch changes, so scrolling stays light. Other terminals keep the dots.

## [0.4.0] - 2026-11-17

### Added
- A log file (`.log`, `app.log.1`, `syslog`…) turns into its **log** look: any size (a
  million lines in a fraction of a second, only the rows in view read), errors and
  warnings coloured with counts, the time since the line before, a filter (`/`, text and
  levels), F8 or `n` to the next problem, the end followed as the file grows.
- Picking a repository in the GitHub view shows it in the editor area as github.com does.
  Its main page lists the files and folders with each one's last commit, then insights
  (pull requests and issues by state, languages, the year's commits as a heat map,
  commits by weekday and hour, lines added and removed a week, top contributors) and the
  README. On the right: a button back to the main page and the files as a tree. A file
  clicked shows its code in the middle (read-only, with its last commit) and its outline
  under the tree. The branch at the top switches the branch.
- An ELF file (a `.so`, an executable, a `.o`, a `.ko`) opens as what `readelf` says about
  it: a summary first (type and machine, SONAME, the libraries it needs, the symbol
  versions it defines and needs, its build ID, the compiler), then readelf's parts, each a
  heading for the Outline. Buttons on its banner switch between readelf and hexdump (a
  binary file's: hexdump and text).
- Symlinks in the Explorer carry an arrow (red when what they point to is missing); a link
  to a folder opens as one. Resting the pointer on a link shows where it points, as the
  link has it.
- A Programmer view (the chip icon above GitHub) does for STM32 chips what
  STM32CubeProgrammer does, through its `STM32_Programmer_CLI`: Device (connect and disconnect, read the
  target, reset it), Memory (a grid of 8-, 16- or 32-bit values with ASCII, saved as a
  `.bin`), Program (`.bin`, `.hex`, `.elf`, `.srec`, `.s19`, picked folder by folder;
  verify, full erase, reset after), Erase (the chip or sectors, asked first), Option Bytes
  (read only) and a Serial Console (the target's UART output through the ST-LINK's
  virtual COM port; lines sent back). Each feature's page shows in the editor area; the
  tool's output and progress, and the target's, in the panel's new Programmer and Serial
  tabs. Pages are cards: the target at the top with Connect or Disconnect, a running
  job's progress with its steps, fields typed in place (a bad value said in red), F5 for
  each page's main action. Without the tool, an Install button. A switch at the top picks
  the tool: STM32Cube, Arduino (`arduino-cli`: the boards it sees, a sketch compiled and
  uploaded for the chosen one) or ESP-IDF (`idf.py`: a project built, flashed, its flash
  erased, its target set). An upload borrows the Serial Console's port and gives it back.

- `termcode PATH` typed in termcode's own terminal opens PATH in that termcode: a file of
  its folder in place, another folder by switching to it (the shells keep running; each
  folder keeps its session). `--new` still starts a second one. Each termcode listens on
  a private socket for this (`$XDG_RUNTIME_DIR/termcode/`, only you can reach it).

- The Explorer follows the active editor, as VS Code's `explorer.autoReveal`: a click in
  another editor group, another tab or a file opened shows its file there (its folders
  opened, it selected and in view). A folder you close stays closed until the editor
  changes; the setting turns it off.

- Pictures and PDFs open as themselves. A PNG, JPEG, GIF, WebP, BMP or ICO shows fitted
  to its tab, zoomed with + and - (0 fits, 1 is actual size, the arrows pan); a PDF
  reads as a document (poppler's pdftoppm): its pages scrolled with a scrollbar, two
  side by side while whole pages fit, its text selected with the mouse and copied with
  Ctrl+C, searched with Ctrl+F, its table of contents in the Outline; also as its text
  (pdftotext) with a heading for each page. The bar's buttons switch to the hexdump.

- `/usr/share/termcode/setup.sh` sets up what termcode's features use (clang, readelf,
  poppler, gh, a Nerd Font, Ghostty, serial access, arduino-cli, ESP-IDF; herdr's Code
  tab and pictures if you have herdr), asking before each part; `--dry-run` shows the
  commands only.

- A patch (`.patch`, `.diff`: `git format-patch`, `git diff` output) turns into a diff
  in its tab, as Source Control shows changes: **diff** and **source** on the
  breadcrumbs' row (or Ctrl+Shift+V). Each file side by side under a bar with its name
  (new, deleted, renamed), each hunk's `@@` line, the files' own line numbers, the
  commit message above; F7 goes from change to change.

- Where a program's bytes go, as a build analyzer shows it. An ELF file has **size**
  beside readelf and hexdump: text, data and bss, its sections, its largest symbols
  (with the Flash and RAM bars of the `.map` beside it). A linker map (`.map`) turns
  into its **usage**: each memory region's use as a bar, its sections (and where they
  load from), what each object file and library puts in each region, the largest pieces.

### Changed
- Arrows in the Search results show each match as they land on it (a preview tab; the
  keys stay in the list). Enter and the mouse work as before.
- A Markdown file (a README) turns into its preview and back in its own tab: **preview**
  and **source** on the breadcrumbs' row, or Ctrl+Shift+V (Markdown: Toggle Preview).
  Each keeps its place; the preview follows unsaved edits. Ctrl+K V still opens it
  beside the source.
- The Markdown preview looks as GitHub shows a README: pictures beside it drawn (real
  pixels in Ghostty and kitty), the HTML READMEs use (`<p align="center">`, `<img>`,
  `<details>`, `<br>`, `<kbd>`…), alerts (`> [!NOTE]`…), tables in a grid (aligned,
  wrapped, striped), ordered lists counted, setext headings, bare URLs as links,
  `:emoji:` and entities. Nothing is fetched: a picture from the web shows its name.
- The Settings window looks as VS Code's: a search box over every setting (Ctrl+F), each
  category with its count, each setting with a line on what it does, real checkboxes,
  sliders, dropdowns and text boxes, a bar beside settings set in your file, a scrollbar.

### Fixed
- The terminal panel's top edge drags down as well as up (the drag no longer goes to the
  shell once the pointer is over it).
- A commit message indented with tabs (a JSON example, say) no longer breaks its card:
  tabs show as spaces to the next tab stop. No control character in text from outside
  (commit messages, titles, file names) reaches the terminal any more; pictures (the
  avatar, the contribution graph) keep theirs.
- Closing the window termcode runs in no longer makes the next start report "termcode hit
  an error last time".
- A repository whose submodule's own config runs a program (`core.fsmonitor`, a filter…)
  now asks for trust too: git read it for status and diff in the repository.
- GitHub's Sign in runs gh in a new terminal, never in one already in use (it may be in
  vim, on another host over ssh, or hold half a command).
- A symlink moved to the Trash restores as the link, not over what it pointed to.

## [0.3.0] - 2026-11-16

### Added
- A binary file (or any `.bin`) opens as a read-only hexdump, as `hexdump -C` prints it:
  offsets, bytes in hex, and the printable ones as text.
- Resting the pointer on a file in the Explorer shows its MD5 on the line under it, with
  a Copy button. It is worked out by termcode itself and stops when the pointer leaves.
- F1 View: Toggle Hexdump: a binary file as text, or any file as a hexdump.
- A paste in the Explorer counts the files it has copied; Esc stops it, and the item it
  was on goes rather than stays half copied.
- The terminals go on across a reload (a new termcode installed): the shells, what runs
  in them and what they showed. A reload no longer waits for a program in them.
- A screenshot of the GitHub view in the README.

### Changed
- A hexdump has no line numbers: its offsets number it.
- The release workflow uses actions/checkout and actions/upload-artifact v7 (Node 24).

### Fixed
- A click on the MD5 line, off its Copy button, goes to the Explorer row under it.
- Files a reload killed before its exec left (its session, a terminal's screen) are
  removed at the next start.
- Terminal tab names are looked up once a second, not on every frame.
- The breadcrumbs show the symbols around the cursor from a file's first frame (they
  could show only its path until the next redraw).

## [0.2.0] - 2026-11-16

### Added
- A terminal under the editors: Ctrl+` or the button beside Split Editor shows and hides
  it, its top edge resizes it, and the shell keeps running while it is hidden. A drag
  selects and copies its text (a double-click, a word); a program that uses the mouse
  (vim, htop) gets the clicks. Quitting while something runs in it asks first, and a
  new termcode waits for it before reloading.
- An Outline right of the editors: the file's symbols, nested, each with VS Code's icon
  for its kind (function, struct, class, enum, …), the one under the cursor lit; a click
  goes there. The button beside the terminal's shows and hides it. Its rows fold, and
  F1 View: Focus Outline walks it by keys.
- A GitHub view (its item above Settings), in any folder, through the GitHub CLI
  (`gh`): sign in from it, then two panes. Above: your account (avatar in a circle), its
  contribution graph by year or month in green blocks, all your repositories, review
  requests, your pull requests, assigned issues and notifications. Below: the repository
  picked above, its description and status, its stats (commit graph, languages,
  contributors), Actions runs, pull requests and issues. Open, edit a title, check out,
  re-run, cancel or close, each confirmed first.
- The Explorer moves a file or folder dragged onto a folder (asked first), and copies,
  cuts and pastes them with Ctrl+C, Ctrl+X and Ctrl+V; a copy whose name is taken
  becomes `name (1)`, `name (2)` …. Open editors follow what moves. Delete moves an
  item to the Trash and Shift+Delete deletes it for good, each asked first. Ctrl+Z
  there takes back a move, rename, paste or delete; Ctrl+Y does it again.
- The Search box and Find's fields: Ctrl+A or a double-click selects the whole text;
  Ctrl+C copies it, Ctrl+X cuts it, and typing or a paste replaces it.
- The commit dialog's message: a drag selects (a double-click, a word), and Ctrl+C,
  Ctrl+X and Ctrl+V copy, cut and paste. Its diff and a commit's card select the same way,
  and Ctrl+C copies.
- More than one terminal: Ctrl+Shift+` or the panel's `+` opens another shell, each with a
  tab on the border; Ctrl+PgUp / Ctrl+PgDn or a click switch, `×` closes one. Ctrl+F at the
  shell's prompt finds text in its scrollback.
- The Explorer selects several items (Ctrl+click, Shift+click, Shift+↑↓, Ctrl+A) and moves,
  copies and deletes them together; Ctrl+Z takes such a step back at once. Ctrl (or Alt)
  as a drag ends copies instead of moving, and resting on a closed folder opens it.

### Fixed
- Delete in the Explorer on another disk than your home folder: the item goes to that
  disk's own Trash, as file managers do, instead of only offering to delete it for good.
- Pasting a big folder in the Explorer no longer freezes the window: it copies in the
  background.
- Two windows on one folder reloading together (a new termcode installed) each come back
  with their own tabs, and the Explorer's Ctrl+Z history survives the reload.
- Markdown previews are rendered once per change, not on every frame.
- GRAPH and TIMELINE follow git run outside termcode (in its terminal or anywhere
  else): a commit, checkout, branch, tag, fetch, push or stash shows within seconds.
- The breadcrumbs (and the Outline) find the function around the cursor when its
  signature puts each parameter on its own line.
- A commit's hover card shows its whole message, long lines wrapped, and scrolls with
  the wheel (it showed 12 lines of it); its buttons stay in view.
- A query longer than its box (Search, Find, Quick Open) no longer runs under the caret
  or Find's toggles: its end shows, then the caret on a blank cell.
- The minimap's change marks light only the dot rows of the changed lines, not the
  whole cell (4 lines, or many more in a long file).

## [0.1.0] - 2026-11-12

The first public release.

### Editor
- Tabs, preview tabs and up to three editor groups; drag a file to an edge to split.
- Syntax colours from VS Code themes, a minimap, indent guides and rainbow, rulers,
  word wrap, folding, multiple cursors, find and replace, bracket colours.
- Go to Definition, Peek, Find All References and a REFERENCES pane, searched with
  ripgrep (no language server).
- Error checking: shellcheck for shell scripts, the project's compiler (or clang) for
  C/C++, with underlines and an explanation on hover.
- Markdown preview; unsaved work survives a crash; sessions reopen where you left off.
- Files up to 16 MB or 500,000 lines open whole and editable.

### Source Control
- Changes with stage, unstage and discard, a commit dialog, and a commit graph coloured
  by branch with a card for each commit.
- Pull, push, sync, branches, merges, rebases and stashes from F1.
- A repository whose own git settings run programs keeps git off until you trust it.

### Look and setup
- Works in the terminal it is started in, or as a Code tab in a herdr workspace.
- Icons from a Nerd Font when one is installed, standard Unicode symbols otherwise.
- 24-bit colour where the terminal has it, else the nearest of 256 or 16.
- A Settings window (Ctrl+,) and a settings file listing every option at its default.
- Your own keys for any F1 command (`termcode.keybindings`).
- F1 Help: Report Issue copies what a bug report needs.

### Packages
- Static amd64 and arm64 `.deb` packages for any Debian or Ubuntu release, with a man
  page (`man termcode`) and bash, zsh and fish completions.
