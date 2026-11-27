### Install

With Rust 1.93 or newer, on Linux or macOS, run `cargo install termcode --locked`.

On Debian or Ubuntu, run `dpkg --print-architecture`, download the matching
`amd64` or `arm64` package from this release, then install it from the download folder.

```sh
sudo apt install ./termcode_*.deb
```

On macOS (experimental), download `termcode-aarch64-apple-darwin.tar.gz` (Apple
Silicon) or `termcode-x86_64-apple-darwin.tar.gz` (Intel), unpack it and put `termcode`
on your PATH. A browser marks what it downloads: clear that once with
`xattr -d com.apple.quarantine termcode`. `SHA256SUMS` checks every file here, and
`gh attestation verify <file> --repo JonasAtelier/termcode` shows it was built by this
repository's release workflow.
