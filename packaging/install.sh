#!/bin/sh
# codegraph installer for Linux and macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/crowbett/codegraph/main/packaging/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --version 0.2.0 --dir ~/bin
#
# Downloads the release archive for this OS/arch from GitHub releases, verifies it
# against the release's SHA256SUMS and installs the `codegraph` binary into
# ~/.local/bin (or --dir / $CODEGRAPH_INSTALL_DIR). It never edits PATH or shell
# profiles; it prints the line to add if the directory is not on PATH.
#
# Private repository: while crowbett/codegraph is private, anonymous
# downloads (including raw.githubusercontent.com for this script) return 404. Set
# GH_TOKEN (or GITHUB_TOKEN) to a token with read access to the repo, e.g.
#   GH_TOKEN=$(gh auth token) sh install.sh
# With a token the script downloads assets through the GitHub API.
#
# Options:
#   --version X.Y.Z   install that release instead of the latest (a leading v is fine)
#   --dir DIR         install directory (default ~/.local/bin)
#   -h, --help        show this help
#
# Environment: CODEGRAPH_VERSION, CODEGRAPH_INSTALL_DIR, GH_TOKEN / GITHUB_TOKEN,
# CODEGRAPH_REPO (owner/name, default crowbett/codegraph).

set -eu

REPO=${CODEGRAPH_REPO:-crowbett/codegraph}
VERSION=${CODEGRAPH_VERSION:-}
INSTALL_DIR=${CODEGRAPH_INSTALL_DIR:-${HOME}/.local/bin}
TOKEN=${GH_TOKEN:-${GITHUB_TOKEN:-}}

say() { printf 'codegraph-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

usage() {
    cat <<'EOF'
usage: install.sh [--version X.Y.Z] [--dir DIR]

Installs codegraph from GitHub releases into ~/.local/bin (or DIR), verifying the
archive against SHA256SUMS. Never edits PATH or shell profiles.
While the repo is private, set GH_TOKEN (e.g. GH_TOKEN=$(gh auth token)).
EOF
    exit 0
}

while [ $# -gt 0 ]; do
    case $1 in
        --version) [ $# -ge 2 ] || die "--version needs a value"; VERSION=$2; shift 2 ;;
        --version=*) VERSION=${1#*=}; shift ;;
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; INSTALL_DIR=$2; shift 2 ;;
        --dir=*) INSTALL_DIR=${1#*=}; shift ;;
        -h|--help) usage ;;
        *) die "unknown argument: $1 (see --help)" ;;
    esac
done
VERSION=${VERSION#v}

# --- platform ---------------------------------------------------------------
case $(uname -s) in
    Linux) OS=linux ;;
    Darwin) OS=macos ;;
    *) die "unsupported OS $(uname -s); on Windows use packaging/install.ps1" ;;
esac
case $(uname -m) in
    x86_64|amd64) ARCH=amd64 ;;
    aarch64|arm64) ARCH=arm64 ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac
# A shell running under Rosetta reports x86_64; prefer the native arm64 build.
if [ "$OS" = macos ] && [ "$ARCH" = amd64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    ARCH=arm64
fi

# --- tools ------------------------------------------------------------------
if command -v curl >/dev/null 2>&1; then
    DL=curl
elif command -v wget >/dev/null 2>&1; then
    DL=wget
else
    die "need curl or wget"
fi
if command -v sha256sum >/dev/null 2>&1; then
    SHA="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    SHA="shasum -a 256"
else
    die "need sha256sum or shasum to verify the download"
fi
command -v tar >/dev/null 2>&1 || die "need tar"

# fetch URL OUT [ACCEPT]: download with optional auth and Accept header.
fetch() {
    _accept=${3:-}
    if [ "$DL" = curl ]; then
        set -- curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
        [ -n "$TOKEN" ] && set -- "$@" -H "Authorization: Bearer $TOKEN"
        [ -n "$_accept" ] && set -- "$@" -H "Accept: $_accept"
    else
        set -- wget -q -O "$2" "$1"
        [ -n "$TOKEN" ] && set -- "$@" --header "Authorization: Bearer $TOKEN"
        [ -n "$_accept" ] && set -- "$@" --header "Accept: $_accept"
    fi
    "$@"
}

TMP=$(mktemp -d 2>/dev/null || mktemp -d -t codegraph)
trap 'rm -rf "$TMP"' EXIT INT TERM

API="https://api.github.com/repos/$REPO"

# --- release metadata -------------------------------------------------------
if [ -n "$TOKEN" ]; then
    # Authenticated: go through the API (works for private repos).
    if [ -n "$VERSION" ]; then
        rel_url="$API/releases/tags/v$VERSION"
    else
        rel_url="$API/releases/latest"
    fi
    fetch "$rel_url" "$TMP/release.json" "application/vnd.github+json" ||
        die "cannot read release metadata from $rel_url (token lacks access, or no such release)"
    if [ -z "$VERSION" ]; then
        VERSION=$(sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' "$TMP/release.json" | head -n1)
    fi
else
    if [ -z "$VERSION" ]; then
        # /releases/latest redirects to /releases/tag/vX.Y.Z.
        if [ "$DL" = curl ]; then
            loc=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") ||
                die "cannot reach github.com/$REPO (private repo? set GH_TOKEN)"
        else
            loc=$(wget -q -S --spider "https://github.com/$REPO/releases/latest" 2>&1 |
                sed -n 's/^ *Location: *//p' | tail -n1)
        fi
        VERSION=${loc##*/}
        VERSION=${VERSION#v}
    fi
fi
[ -n "$VERSION" ] && [ "$VERSION" != latest ] ||
    die "could not determine the latest version (private repo? set GH_TOKEN), or pass --version"

ASSET="codegraph-$VERSION-$OS-$ARCH.tar.gz"
say "installing codegraph $VERSION ($OS/$ARCH) into $INSTALL_DIR"

# asset_download NAME OUT
asset_download() {
    if [ -n "$TOKEN" ]; then
        # Find the asset's API URL in the release JSON (one field per line is not
        # guaranteed, so split on commas first).
        _id=$(tr ',' '\n' <"$TMP/release.json" | awk -v want="\"name\": *\"$1\"" '
            /"url": *"https:\/\/api\.github\.com\/repos\/.*\/releases\/assets\/[0-9]+"/ { url = $0 }
            $0 ~ want { print url; exit }' | sed 's/.*"\(https:[^"]*\)".*/\1/')
        [ -n "$_id" ] || die "release v$VERSION has no asset $1"
        fetch "$_id" "$2" "application/octet-stream"
    else
        fetch "https://github.com/$REPO/releases/download/v$VERSION/$1" "$2"
    fi
}

asset_download "$ASSET" "$TMP/$ASSET" || die "download of $ASSET failed"
asset_download SHA256SUMS "$TMP/SHA256SUMS" || die "download of SHA256SUMS failed"

# --- verify -----------------------------------------------------------------
expected=$(awk -v f="$ASSET" '$2 == f || $2 == "*" f { print $1 }' "$TMP/SHA256SUMS")
[ -n "$expected" ] || die "SHA256SUMS has no entry for $ASSET"
actual=$(cd "$TMP" && $SHA "$ASSET" | awk '{ print $1 }')
[ "$expected" = "$actual" ] || die "checksum mismatch for $ASSET (expected $expected, got $actual)"
say "sha256 ok"

# --- install ----------------------------------------------------------------
mkdir -p "$TMP/x"
tar -xzf "$TMP/$ASSET" -C "$TMP/x"
[ -f "$TMP/x/codegraph" ] || die "archive does not contain codegraph"
mkdir -p "$INSTALL_DIR"
# Write next to the target then rename, so a running codegraph is not truncated.
cp "$TMP/x/codegraph" "$INSTALL_DIR/.codegraph.new"
chmod 755 "$INSTALL_DIR/.codegraph.new"
mv -f "$INSTALL_DIR/.codegraph.new" "$INSTALL_DIR/codegraph"
say "installed $INSTALL_DIR/codegraph"
"$INSTALL_DIR/codegraph" version >&2 || true

case ":${PATH}:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say "$INSTALL_DIR is not on your PATH; add this line to your shell profile:"
        # shellcheck disable=SC2016 # $PATH is printed literally on purpose
        printf '  export PATH="%s:$PATH"\n' "$INSTALL_DIR" >&2
        ;;
esac
