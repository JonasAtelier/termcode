# Repository Guidelines

## Project Structure & Module Organization

`termcode` is a Linux terminal editor and Git panel, built with Rust 2021 and Ratatui. It runs standalone or as a herdr workspace tab.

- `src/main.rs`: CLI parsing, workspace resolution, and herdr launching.
- `src/panel.rs`, `src/ui.rs`, `src/editor.rs`: application state, rendering, and editing.
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
- `scripts/deb.sh`: build the `.deb` (no build-machine paths in the binary).
- `cargo install --path . --root ~/.local -j2`: install locally; then `herdr plugin link "$PWD"` registers the plugin. Keep `~/.local/bin` on `PATH`.

Check `free -m` before builds; adjust concurrency to available memory. Git and ripgrep (`rg`) support repository operations and search.

## Coding Style & Naming Conventions

Use four-space indentation, `snake_case` functions/modules, `PascalCase` types, and `SCREAMING_SNAKE_CASE` constants. Match surrounding formatting; avoid unrelated reformatting. Rustfmt and Clippy are available through `cargo fmt` and `cargo clippy`; no repository-specific configuration is present. Keep parsers free of I/O and retain upstream asset licenses.

## Testing Guidelines

Use Rust's built-in `#[test]` functions inside `#[cfg(test)] mod tests`, with descriptive `snake_case` names. Add behavioral regression tests for fixes; use temporary repositories for Git mutations. No numeric coverage threshold is configured. Ignored performance tests require explicit selection and their documented environment variables. Manually verify terminal interaction changes, including input, rendering, and terminal restoration on exit.

## Commit & Pull Request Guidelines

Follow the history's `termcode: <short imperative summary>` convention. Explain changed behavior in concise body bullets and include actual test results. Keep changes focused. PRs should describe the problem, link relevant issues, list verification, and attach terminal screenshots for visible UI changes. Update `README.md` when commands, settings, or keybindings change.
