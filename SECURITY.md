# Security

Report a vulnerability privately, not in a public issue: on this repository, open
**Security → Report a vulnerability** (GitHub's private reporting). Say what is affected,
how to reproduce it, and the version (`termcode --version`).

Only the latest release gets fixes; a fix ships as a patch release, crediting you unless
you ask not to be.

What counts: anything that lets a program or person you did not allow act through
termcode (its API socket and `termcode api`, the git pager window, extensions, the files
it writes), or that leaks a credential. The API's rules are in [doc/api.md](doc/api.md).
