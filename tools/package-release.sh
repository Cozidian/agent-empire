#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root after a native release build.
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) echo "Unsupported architecture: $arch" >&2; exit 1 ;; esac
binary=${CARGO_TARGET_DIR:-target}/release/empire
test "$("$binary" --version)" = "empire $version"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -m 755 "$binary" "$stage/empire"
strip "$stage/empire"
install -m 644 LICENSE README.md "$stage/"
python3 tools/third-party-notices.py --check
install -m 644 THIRD-PARTY-NOTICES.txt "$stage/"
install -m 644 "$(rustc --print sysroot)/share/doc/rust/COPYRIGHT-library.html" "$stage/RUST-THIRD-PARTY-NOTICES.html"
printf '%s\n' "$version" > "$stage/VERSION"
mkdir -p dist
tar -czf "dist/empire-linux-$arch.tar.gz" -C "$stage" empire LICENSE README.md VERSION THIRD-PARTY-NOTICES.txt RUST-THIRD-PARTY-NOTICES.html
install -m 755 tools/install.sh dist/install.sh
