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
exec cargo deb "$@"
