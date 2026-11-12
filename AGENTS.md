# Repository Guidelines

## Project Structure & Module Organization

`termcode` is a Linux terminal editor and Git panel, built with Rust 2021 and Ratatui. It runs standalone or as a herdr workspace tab.

- `src/main.rs`: CLI parsing, workspace resolution, and herdr launching.
- `src/panel/`: application state (`mod.rs`: the `App`, commands, startup) and its behaviour by topic: `keys.rs`, `mouse.rs`, `tabs.rs`, `qdiff.rs`, `find.rs`, `search.rs`, `quick.rs`, `rename.rs`, `sidebar.rs`, `gitfs.rs`; its tests in `tests.rs`.
- `src/ui.rs`, `src/editor.rs`: rendering and editing.
- `src/git.rs`, `src/gitops.rs`, `src/scm.rs`, `src/gitcmds.rs`: Git backend and Source Control interactions.
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
- `scripts/apt-repo.sh DIR debs...`: add packages to the apt repository (termcode-releases, branch `gh-pages`, served by GitHub Pages) and sign it; the key is in `~/.config/termcode-release/` here and the `APT_SIGNING_KEY` secret in CI.
- `scripts/deb.sh`: build the static amd64 and arm64 `.deb`s (no build-machine paths in the binary).
- Changes worth telling users go under the next version in `CHANGELOG.md` (SemVer; before 1.0, MINOR for features or changed settings/keys, PATCH for fixes).
- Release: date that `CHANGELOG.md` section, bump the version in `Cargo.toml` and `herdr-plugin.toml`, then push a `vX.Y.Z` tag; `.github/workflows/release.yml` (tags only) lints with `-D warnings`, tests, packages, install-tests and publishes.
- `cargo install --path . --root ~/.local -j2`: install locally; then `herdr plugin link "$PWD"` registers the plugin. Keep `~/.local/bin` on `PATH`.

Check `free -m` before builds; adjust concurrency to available memory. Git and ripgrep (`rg`) support repository operations and search.

## Coding Style & Naming Conventions

Use four-space indentation, `snake_case` functions/modules, `PascalCase` types, and `SCREAMING_SNAKE_CASE` constants. Match surrounding formatting; avoid unrelated reformatting. Rustfmt and Clippy are available through `cargo fmt` and `cargo clippy`; no repository-specific configuration is present. Keep parsers free of I/O and retain upstream asset licenses.

## Testing Guidelines

Use Rust's built-in `#[test]` functions inside `#[cfg(test)] mod tests`, with descriptive `snake_case` names. Add behavioral regression tests for fixes; use temporary repositories for Git mutations. No numeric coverage threshold is configured. Ignored performance tests require explicit selection and their documented environment variables. Manually verify terminal interaction changes, including input, rendering, and terminal restoration on exit.

## Commit & Pull Request Guidelines

Follow the history's `termcode: <short imperative summary>` convention. Explain changed behavior in concise body bullets and include actual test results. Keep changes focused. PRs should describe the problem, link relevant issues, list verification, and attach terminal screenshots for visible UI changes. Update `README.md` when commands, settings, or keybindings change.
