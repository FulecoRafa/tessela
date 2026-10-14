#!/bin/sh
# Builds the two Source files tessela.spec needs, from a git revision:
#   tessela-<version>.tar.xz          git archive of the tree
#   tessela-<version>-vendor.tar.xz   every crate from Cargo.lock (cargo vendor),
#                                     so rpmbuild never touches the network
#
#   packaging/rpm/make-sources.sh [outdir]    (default: ~/rpmbuild/SOURCES)
#   REF=v0.1.0 packaging/rpm/make-sources.sh  (default: HEAD)
#
# Needs git, cargo and xz (in this repo: `nix develop -c packaging/rpm/make-sources.sh`).
# The crates go to cargo-vendor/, not vendor/: vendor/ already holds the
# patched copies the workspace's [patch.crates-io] points at.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(git -C "$here" rev-parse --show-toplevel)
ref=${REF:-HEAD}
version=$(git -C "$root" show "$ref:Cargo.toml" | sed -n 's/^version = "\(.*\)"$/\1/p' | head -n1)
name=tessela-$version
out=${1:-$HOME/rpmbuild/SOURCES}
mkdir -p "$out"
out=$(cd "$out" && pwd)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

git -C "$root" archive --format=tar --prefix="$name/" "$ref" > "$tmp/$name.tar"
xz -T3 -c "$tmp/$name.tar" > "$out/$name.tar.xz"

tar -xf "$tmp/$name.tar" -C "$tmp"
(cd "$tmp/$name" && cargo vendor --quiet --locked --versioned-dirs cargo-vendor > /dev/null)
tar -C "$tmp/$name" --sort=name --owner=0 --group=0 --numeric-owner \
    -cf - cargo-vendor | xz -T3 > "$out/$name-vendor.tar.xz"

echo "$out/$name.tar.xz"
echo "$out/$name-vendor.tar.xz"
