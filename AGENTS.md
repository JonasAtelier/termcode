# Repository Guidelines

## Project Structure & Module Organization

`termcode` is a Linux terminal editor and Git panel, built with Rust 2021 and Ratatui. It runs standalone or as a herdr workspace tab.

- `src/main.rs`: CLI parsing, workspace resolution, and herdr launching.
- `src/panel/`: application state (`mod.rs`: the `App`, commands, startup) and its behaviour by topic: `keys.rs`, `mouse.rs`, `tabs.rs`, `qdiff.rs`, `find.rs`, `search.rs`, `quick.rs`, `rename.rs`, `sidebar.rs`, `gitfs.rs`; its tests in `tests.rs`.
- `src/ui/` (`mod.rs`; `text.rs` the editor body, `diffs.rs`, `panels.rs` the terminal, Outline and previews), `src/editor.rs`: rendering and editing.
- `src/ext/`: the view subsystem (DSA-like). `mod.rs`: `Ops` (a driver's ops table), `DRIVERS` (in-tree: Programmer, GitHub, the + view), dispatch the frame calls; `process.rs`: extensions (programs filling slots over JSON-RPC on stdio; `api::host` serves them); `manager.rs`: the + view (install, on/off, remove); `install.rs`: `termcode ext`. A built-in view adds an `OPS` const and an entry in `DRIVERS`, never a special case in the frame. Guides: doc/extensions.md, skills/termcode-extension/SKILL.md, examples/extension/ (small), extensions/xcomm/ (the Serial view, out of tree: its own crate on the xcomm crate, built by its manifest at install; it serves the API's `serial.*`).
- `src/term.rs`: the integrated terminals; `src/github/` (`mod.rs` data and events, `draw.rs`): the GitHub view.
- `src/git.rs`, `src/gitops.rs`, `src/scm/` (its UI tests in `ui_tests.rs`), `src/commitdlg.rs`, `src/gitcmds.rs`: Git backend and Source Control interactions.
- `src/parse.rs`: pure parsers; `src/settings.rs`, `src/theme.rs`, `src/session.rs`: configuration, appearance, and persistence.
- `assets/`: embedded themes, icon data, and upstream licenses.
- `examples/gen_seti.rs`: icon-data generator; `herdr-plugin.toml`: plugin integration.

Tests live beside implementation in `src/`; `target/` contains ignored build output.

## Build, Test, and Development Commands

- `cargo check -j2`: type-check without producing the executable.
- `cargo build -j2`: build the debug binary.
- `cargo test -j2`: run the standard test suite.
- `cargo test -j2 parse::tests`: run a focused module's tests.
- `cargo run -- .`: open this repository in an interactive terminal.
- `cargo run -- --tab .`: open a herdr workspace tab.
- `README.md` is for both: blocks between `<!-- dev -->` and `<!-- /dev -->` (and table rows ending `<!--dev-->`) are for building termcode; `<!-- user` ... `user -->` blocks are for people who install it. `scripts/user-readme.sh` writes the installed copy (`--releases`: the termcode-releases front page, which the release workflow updates).
- `scripts/apt-repo.sh DIR debs...`: add packages to the apt repository (termcode-releases, branch `gh-pages`, served by GitHub Pages) and sign it; CI signs with the `APT_SIGNING_KEY` secret (keep an offline copy of the key).
- `scripts/deb.sh`: build the static amd64 and arm64 `.deb`s (no build-machine paths in the binary).
- Changes worth telling users go under the next version in `CHANGELOG.md`. Version `X.Y.Z`: X for a new feature or a huge change (a new view in the side bar, a new kind of page), Y for a change to a pane or view (the minimap, quick keys), Z for bug fixes. A release bumps only the largest kind it holds, the lower numbers back to 0 (1.4.2 with a fix and a minimap change: 1.5.0).
- CI builds with the Rust pinned in `.github/workflows/release.yml` (1.97.1, as here); bump it on purpose, after `cargo clippy --all-targets -- -D warnings` passes with the new one.
- Release: date that `CHANGELOG.md` section, bump the version in `Cargo.toml` and `herdr-plugin.toml`, then push a `vX.Y.Z` tag; `.github/workflows/release.yml` (tags only) lints with `-D warnings`, tests, packages, install-tests and publishes.
- `cargo install --path . --root ~/.local -j2`: install locally; then `herdr plugin link "$PWD"` registers the plugin. Keep `~/.local/bin` on `PATH`.

Check `free -m` before builds; adjust concurrency to available memory. Git and ripgrep (`rg`) support repository operations and search.

## Coding Style & Naming Conventions

Use four-space indentation, `snake_case` functions/modules, `PascalCase` types, and `SCREAMING_SNAKE_CASE` constants. Match surrounding formatting; avoid unrelated reformatting. Rustfmt and Clippy are available through `cargo fmt` and `cargo clippy`; no repository-specific configuration is present. Keep parsers free of I/O and retain upstream asset licenses.

## Testing Guidelines

Use Rust's built-in `#[test]` functions inside `#[cfg(test)] mod tests`, with descriptive `snake_case` names. Add behavioral regression tests for fixes; use temporary repositories for Git mutations. No numeric coverage threshold is configured. Ignored performance tests require explicit selection and their documented environment variables. Manually verify terminal interaction changes, including input, rendering, and terminal restoration on exit.

## Commit & Pull Request Guidelines

Follow the history's `termcode: <short imperative summary>` convention. Explain changed behavior in concise body bullets and include actual test results. Keep changes focused. PRs should describe the problem, link relevant issues, list verification, and attach terminal screenshots for visible UI changes. Update `README.md` when commands, settings, or keybindings change.
