# Changelog

What changed in each termcode release, newest first.

Versions follow [Semantic Versioning](https://semver.org): before 1.0, a new MINOR
version (0.2.0) adds features or changes settings or keys, and a new PATCH version
(0.1.1) only fixes bugs.

## [Unreleased]

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
