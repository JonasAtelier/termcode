# Changelog

What changed in each termcode release, newest first.

Versions follow [Semantic Versioning](https://semver.org): before 1.0, a new MINOR
version (0.2.0) adds features or changes settings or keys, and a new PATCH version
(0.1.1) only fixes bugs.

## [Unreleased]

### Added
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
  each page's main action. Without the tool, an Install button.

### Changed
- The Settings window looks as VS Code's: a search box over every setting (Ctrl+F), each
  category with its count, each setting with a line on what it does, real checkboxes,
  sliders, dropdowns and text boxes, a bar beside settings set in your file, a scrollbar.

### Fixed
- A commit message indented with tabs (a JSON example, say) no longer breaks its card:
  tabs show as spaces to the next tab stop. No control character in text from outside
  (commit messages, titles, file names) reaches the terminal any more; pictures (the
  avatar, the contribution graph) keep theirs.

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
