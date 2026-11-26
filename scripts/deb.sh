#!/bin/sh
# Builds the Debian packages, one per architecture:
#   target/deb/<target>/debian/termcode_<version>-<revision>_<arch>.deb
# The binaries are static (musl): no glibc version to match, so one package installs on
# any Debian or Ubuntu release (and runs on any Linux).
#   scripts/deb.sh                                    # amd64 and arm64
#   TARGETS=x86_64-unknown-linux-musl scripts/deb.sh  # amd64 only
#   scripts/deb.sh --deb-revision 2                   # arguments go to every `cargo deb`
# Needs cargo-deb (cargo install cargo-deb, once); the targets are added with rustup.
set -eu
cd "$(dirname "$0")/.."
TARGETS="${TARGETS:-x86_64-unknown-linux-musl aarch64-unknown-linux-musl}"
# No build-machine paths in the binary (they name the user): $HOME and this checkout
# become ~ and . in the paths panics and dependencies carry.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=."
# Its own build folder: those flags would otherwise rebuild `cargo install`'s every time.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/deb}"
# Stripped by the linker, so no strip tool for the other architecture is needed.
export CARGO_PROFILE_RELEASE_STRIP=true
# Rust's own linker links a static musl binary for any architecture.
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
mkdir -p target

# The README for people who install it (nothing about building it).
scripts/user-readme.sh > target/README.md

# The man page, compressed as Debian keeps them (-n: no timestamp, same bytes each build).
gzip -9nc doc/termcode.1 > target/termcode.1.gz

# Debian's machine-readable copyright file: termcode's LICENSE, then every bundled
# license, each text indented one space with "." for blank lines.
text() { sed -e 's/^$/./' -e 's/^/ /' "$1"; }
{
    printf 'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n'
    printf 'Upstream-Name: termcode\n'
    printf 'Upstream-Contact: https://github.com/JonasAtelier/termcode/issues\n'
    printf 'Comment: The Rust crates built into termcode, with their copyright and\n'
    printf ' license texts, are listed in /usr/share/doc/termcode/THIRD-PARTY-LICENSES.\n\n'
    printf 'Files: *\nCopyright: 2026 JonasAtelier\nLicense: MIT\n'
    text LICENSE
    printf '\nFiles: assets/vscode_dark_defaults.json assets/seti.json\n'
    printf 'Copyright: 2015-present Microsoft Corporation; 2014 Jesse Weed\nLicense: MIT\n'
    text assets/vscode_LICENSE.txt
    printf '\nFiles: assets/cpptools_dark_vs_new.json\nCopyright: Microsoft Corporation\nLicense: MIT\n'
    text assets/cpptools_LICENSE.txt
} > target/copyright

for target in $TARGETS; do
    rustup target list --installed | grep -qx "$target" || rustup target add "$target"
    # The Rust crates built into this target's binary and their license texts.
    cargo metadata --format-version 1 --locked --filter-platform "$target" | python3 -I scripts/third-party.py > target/THIRD-PARTY-LICENSES
    cargo deb --target "$target" --profile dist --no-strip "$@"
done
