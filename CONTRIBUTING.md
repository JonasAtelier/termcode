# Contributing

Bugs and ideas: open an issue; its form asks for what helps. Security problems: see
[SECURITY.md](SECURITY.md).

A pull request:

- Passes what CI runs: `cargo clippy --all-targets --locked -- -D warnings` and
  `cargo test --locked` (the macOS checks run in CI).
- Brings a test that fails without the fix or feature.
- Does one thing, and says what problem it solves, what you checked, and shows a
  screenshot for a visible change.
- Names its commits `area: what changed` (`term:`, `pager:`, `ui:`, `fix:`…).

termcode is under the GPL-3.0-only license; a contribution is under it too.

[AGENTS.md](AGENTS.md) has the layout, the commands, the coding style and the release steps.
