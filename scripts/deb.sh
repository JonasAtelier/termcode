#!/bin/sh
# Builds the Debian package: target/deb/debian/termcode_<version>-<revision>_<arch>.deb.
# Arguments go to `cargo deb` (cargo install cargo-deb, once), for example:
#   scripts/deb.sh                                      # for this machine
#   scripts/deb.sh --target aarch64-unknown-linux-gnu   # cross-build (needs that toolchain)
#   scripts/deb.sh --deb-revision 2 -o dist/            # another revision, another folder
set -eu
cd "$(dirname "$0")/.."
# No build-machine paths in the binary (they name the user): $HOME and this checkout
# become ~ and . in the paths panics and dependencies carry.
export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ --remap-path-prefix=$PWD=."
# Its own build folder: those flags would otherwise rebuild `cargo install`'s every time.
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-target/deb}"

# Debian's machine-readable copyright file: termcode's LICENSE, then every bundled
# license, each text indented one space with "." for blank lines.
text() { sed -e 's/^$/./' -e 's/^/ /' "$1"; }
mkdir -p target
{
    printf 'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n'
    printf 'Upstream-Name: termcode\nUpstream-Contact: JonasAtelier\n'
    printf 'Source: https://github.com/JonasAtelier/termcode\n\n'
    printf 'Files: *\nCopyright: 2026 JonasAtelier\nLicense: LicenseRef-termcode\n'
    text LICENSE
    printf '\nFiles: assets/vscode_dark_defaults.json assets/seti.json\n'
    printf 'Copyright: 2015-present Microsoft Corporation; 2014 Jesse Weed\nLicense: MIT\n'
    text assets/vscode_LICENSE.txt
    printf '\nFiles: assets/cpptools_dark_vs_new.json\nCopyright: Microsoft Corporation\nLicense: MIT\n'
    text assets/cpptools_LICENSE.txt
} > target/copyright

exec cargo deb "$@"
