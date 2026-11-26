#!/bin/sh
# Adds .deb packages to the apt repository in DIR (termcode's Pages tree, branch gh-pages),
# rebuilds its indexes and signs them:
#   scripts/apt-repo.sh DIR target/deb/debian/termcode_*.deb
# The signing key is the one in $GNUPGHOME, or APT_SIGNING_KEY (an armored private key,
# as the release workflow has it), imported into a throwaway keyring. The public half is
# written beside the indexes as termcode.gpg (binary) and termcode.asc (armored), so
# users can fetch it from the same place:
#   deb [signed-by=/usr/share/keyrings/termcode.gpg] https://jonasatelier.github.io/termcode stable main
set -eu
dir=$(cd "$1" && pwd)
shift
if [ -n "${APT_SIGNING_KEY:-}" ]; then
    GNUPGHOME=$(mktemp -d)
    export GNUPGHOME
    trap 'rm -rf "$GNUPGHOME"' EXIT
    printf '%s\n' "$APT_SIGNING_KEY" | gpg --batch --quiet --import
fi

pool="$dir/pool/main/t/termcode"
mkdir -p "$pool"
for deb in "$@"; do
    cp "$deb" "$pool/"
done

cd "$dir"
for arch in amd64 arm64; do
    mkdir -p "dists/stable/main/binary-$arch"
    apt-ftparchive --arch "$arch" packages pool > "dists/stable/main/binary-$arch/Packages"
    gzip -9nkf "dists/stable/main/binary-$arch/Packages"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=termcode \
    -o APT::FTPArchive::Release::Label=termcode \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    -o APT::FTPArchive::Release::Description="termcode, a VS Code-style editor in the terminal" \
    release dists/stable > Release.new
mv Release.new dists/stable/Release
gpg --batch --yes --clearsign -o dists/stable/InRelease dists/stable/Release
gpg --batch --yes --armor --detach-sign -o dists/stable/Release.gpg dists/stable/Release
gpg --export > termcode.gpg
gpg --armor --export > termcode.asc
echo "apt repository in $dir: $(ls "$pool" | wc -l) package(s), signed by $(gpg --list-keys --with-colons | awk -F: '/^fpr/{print $10; exit}')"
