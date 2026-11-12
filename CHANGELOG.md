# Changelog

What changed in each termcode release, newest first.

Versions follow [Semantic Versioning](https://semver.org): before 1.0, a new MINOR
version (0.2.0) adds features or changes settings or keys, and a new PATCH version
(0.1.1) only fixes bugs.

## [Unreleased]

### Added
- A terminal under the editors: Ctrl+` or the button beside Split Editor shows and hides
  it, its top edge resizes it, and the shell keeps running while it is hidden.
- An Outline right of the editors: the file's symbols, nested, each with VS Code's icon
  for its kind (function, struct, class, enum, …), the one under the cursor lit; a click
  goes there. The button beside the terminal's shows and hides it.

### Fixed
- GRAPH and TIMELINE follow git run outside termcode (in its terminal or anywhere
  else): a commit, checkout, branch, tag, fetch, push or stash shows within seconds.
- The breadcrumbs (and the Outline) find the function around the cursor when its
  signature puts each parameter on its own line.
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
