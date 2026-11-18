---
name: termcode-api-check
description: Verify termcode's API end to end — starts a termcode in a private tmux session on a scratch repo, calls every method (hello, commands, editor.open, state.get paths, buffer.edit/save, terminal.send, events.wait, the screen) and reports PASS/FAIL. Use after changing src/api.rs, src/rpc.rs or anything they call, before committing API work, or when asked "does the API work?".
---

# termcode API check

```bash
cargo build --release -j2                      # check free -m first
scripts/api-check.py target/release/termcode   # or no argument: the termcode on PATH
```

- Exit 0 and `N/N passed`: the API works as skills/termcode/SKILL.md describes.
- A `FAIL` line shows the check and the error or reply that broke it. Fix the code, not
  the check, unless the API was changed on purpose; then change the check, SKILL.md
  (the agents' guide) and doc/api.md together.
- It starts two termcodes: one with `TERMCODE_API_ALLOW=edit,run` for the methods,
  one as the user's would be for the safety checks, answering its prompts with tmux keys
  (Enter: Deny, Left+Enter: Allow once, Esc).
- It cleans up after itself (its tmux session `tc-api-check-<pid>`, the scratch repo).
  It never touches the user's own termcodes: it talks only to the one it started (`--to PID`).
- Needs tmux and git; the diagnostics check runs only where shellcheck is installed.
- New method: add its check to `scripts/api-check.py`, its row to SKILL.md's table, and
  a unit test in `src/rpc.rs`.

Report the PASS/FAIL table verbatim; never say the API works unless this ran here and
passed.
