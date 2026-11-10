#!/usr/bin/env bash
set -euo pipefail

repo=Cozidian/agent-empire
version=latest
bin_dir=${EMPIRE_INSTALL_DIR:-"$HOME/.local/bin"}
from_dir=
die() { printf 'empire: %s\n' "$*" >&2; exit 1; }
usage() {
    cat <<'EOF'
Install a prebuilt Empire binary (Linux x86-64 or ARM64; glibc 2.35+ and ALSA).
Usage: bash install.sh [--version vX.Y.Z] [--bin-dir DIR] [--from-dir DIR]
  --version   Download a particular release instead of the latest release.
  --bin-dir   Install here (default: EMPIRE_INSTALL_DIR or ~/.local/bin).
  --from-dir  Use an already downloaded archive and SHA256SUMS; no network.
EOF
}
while (($#)); do
    case "$1" in
        --version|--bin-dir|--from-dir)
            (($# >= 2)) && [[ -n "$2" ]] || die "$1 needs a value"
            case "$1" in --version) version=$2 ;; --bin-dir) bin_dir=$2 ;; --from-dir) from_dir=$2 ;; esac
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "Unknown option: $1" ;;
    esac
done
[[ "$version" == latest || "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || die 'Use a release version such as v0.1.0'
[[ $(uname -s) == Linux ]] || die 'Only Linux is supported'
arch=$(uname -m)
case "$arch" in x86_64|aarch64) ;; *) die "Unsupported architecture: $arch" ;; esac
for cmd in tar sha256sum mktemp install; do command -v "$cmd" >/dev/null || die "Missing command: $cmd"; done
archive="empire-linux-$arch.tar.gz"
tmp=$(mktemp -d)
staged=
cleanup() { rm -rf "$tmp"; [[ -z "$staged" ]] || rm -f "$staged"; }
trap cleanup EXIT
if [[ -n "$from_dir" ]]; then
    cp -- "$from_dir/$archive" "$from_dir/SHA256SUMS" "$tmp/"
else
    command -v curl >/dev/null || die 'Install curl first, or use --from-dir'
    base="https://github.com/$repo/releases"
    if [[ "$version" == latest ]]; then base+='/latest/download'; else base+="/download/$version"; fi
    for name in "$archive" SHA256SUMS; do
        curl -q --proto '=https' --tlsv1.2 --fail --location --silent --show-error "$base/$name" -o "$tmp/$name" || die 'Download failed. For a private repo, use gh release download and --from-dir.'
    done
fi
# Check only our archive; never trust filenames or paths from the downloaded manifest.
digest=$(awk -v name="$archive" '$2 == name {print $1}' "$tmp/SHA256SUMS")
[[ "$digest" =~ ^[0-9a-fA-F]{64}$ ]] || die "Missing or ambiguous checksum for $archive"
(cd "$tmp" && printf '%s  %s\n' "$digest" "$archive" | sha256sum --check --status) || die 'Checksum verification failed'
# Extract only regular file content to fixed paths; archive paths cannot escape the temp directory.
tar -xOzf "$tmp/$archive" empire > "$tmp/empire"
tar -xOzf "$tmp/$archive" VERSION > "$tmp/VERSION"
release_version=$(cat "$tmp/VERSION")
[[ "$release_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die 'Invalid archive version'
[[ "$version" == latest || "$version" == "v$release_version" ]] || die 'Archive version does not match the requested release'
chmod 755 "$tmp/empire"
installed_version=$("$tmp/empire" --version) || die 'Cannot run this binary. Check glibc 2.35+ and the ALSA runtime library.'
[[ "$installed_version" == "empire $release_version" ]] || die 'Binary version does not match the archive'
mkdir -p -- "$bin_dir"
[[ ! -d "$bin_dir/empire" ]] || die "$bin_dir/empire is a directory"
# Keep attribution beside the installed command. The original v0.1.0 archives predate these files.
tar -tzf "$tmp/$archive" > "$tmp/members"
for notice in LICENSE THIRD-PARTY-NOTICES.txt RUST-THIRD-PARTY-NOTICES.html; do
    if grep -Fxq "$notice" "$tmp/members"; then
        tar -xOzf "$tmp/$archive" "$notice" > "$tmp/$notice"
        staged=$(mktemp "$bin_dir/.empire-install.XXXXXX")
        install -m 644 "$tmp/$notice" "$staged"
        mv -fT -- "$staged" "$bin_dir/empire.$notice"
        staged=
    elif [[ "$release_version" != 0.1.0 ]]; then
        die "Archive is missing $notice"
    fi
done
# Rename a new inode so a running daemon keeps its existing executable until the next launch.
staged=$(mktemp "$bin_dir/.empire-install.XXXXXX")
install -m 755 "$tmp/empire" "$staged"
mv -fT -- "$staged" "$bin_dir/empire"
staged=
printf 'Installed %s to %s/empire\n' "$installed_version" "$bin_dir"
case ":$PATH:" in *":$bin_dir:"*) ;; *) printf 'Add this directory to PATH: %s\n' "$bin_dir" ;; esac
printf 'Run empire to open the galaxy. Existing agents have not been interrupted.\n'
