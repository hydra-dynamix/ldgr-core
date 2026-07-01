#!/usr/bin/env sh
set -eu

REPO="${LDGR_REPO:-hydra-dynamix/ldgr-core}"
PACKAGE="ldgr-core"
BINARY="ldgr"
INSTALL_DIR="${LDGR_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${LDGR_VERSION:-}"
BASE_URL="${LDGR_RELEASE_BASE_URL:-https://github.com/$REPO/releases/download}"
TMP_DIR="${TMPDIR:-/tmp}/ldgr-install.$$"

log() { printf '%s\n' "$*" >&2; }
fail() { log "error: $*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

require() {
  have "$1" || fail "required command not found: $1"
}

normalize_arch() {
  case "$1" in
    x86_64|amd64) printf 'x86_64' ;;
    aarch64|arm64) printf 'aarch64' ;;
    *) fail "unsupported CPU architecture: $1" ;;
  esac
}

platform_tag() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(normalize_arch "$(uname -m)")"
  case "$os" in
    linux) printf 'linux-%s' "$arch" ;;
    darwin) printf 'macos-%s' "$arch" ;;
    mingw*|msys*|cygwin*) printf 'windows-%s' "$arch" ;;
    *) fail "unsupported operating system: $os" ;;
  esac
}

latest_version() {
  require curl
  curl -fsSL "https://api.github.com/repos/$REPO/releases" \
    | sed -n 's/.*"tag_name":[[:space:]]*"v\([^"]*\)".*/\1/p' \
    | head -n 1
}

sha256_check() {
  checksum_file="$1"
  archive_file="$2"
  expected="$(awk '{print $1}' "$checksum_file")"
  if have sha256sum; then
    actual="$(sha256sum "$archive_file" | awk '{print $1}')"
  elif have shasum; then
    actual="$(shasum -a 256 "$archive_file" | awk '{print $1}')"
  else
    log "warning: neither sha256sum nor shasum found; skipping checksum verification"
    return 0
  fi
  [ "$expected" = "$actual" ] || fail "checksum mismatch for $archive_file"
}

install_from_source() {
  if ! have cargo; then
    fail "no prebuilt release asset for $PLATFORM and cargo is not installed; set LDGR_VERSION or install Rust/cargo"
  fi
  log "No prebuilt release asset for $PLATFORM; falling back to cargo install from $REPO."
  if [ -n "$VERSION" ]; then
    cargo install --git "https://github.com/$REPO" --tag "v$VERSION" --locked --force --package "$PACKAGE"
  else
    cargo install --git "https://github.com/$REPO" --locked --force --package "$PACKAGE"
  fi
}

require uname
require tar
require curl

PLATFORM="$(platform_tag)"
if [ -z "$VERSION" ]; then
  VERSION="$(latest_version)"
  [ -n "$VERSION" ] || fail "could not resolve latest $REPO release version"
fi

ARCHIVE="$PACKAGE-$VERSION-$PLATFORM.tar.gz"
URL="$BASE_URL/v$VERSION/$ARCHIVE"
CHECKSUM_URL="$URL.sha256"

mkdir -p "$TMP_DIR"
log "Installing $BINARY $VERSION for $PLATFORM"
log "Download: $URL"

if ! curl -fsSL "$URL" -o "$TMP_DIR/$ARCHIVE"; then
  install_from_source
  exit 0
fi
curl -fsSL "$CHECKSUM_URL" -o "$TMP_DIR/$ARCHIVE.sha256"
sha256_check "$TMP_DIR/$ARCHIVE.sha256" "$TMP_DIR/$ARCHIVE"

tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"
SRC="$TMP_DIR/$PACKAGE-$VERSION/$PLATFORM/$BINARY"
[ -f "$SRC" ] || fail "archive did not contain expected binary: $PACKAGE-$VERSION/$PLATFORM/$BINARY"
mkdir -p "$INSTALL_DIR"
cp "$SRC" "$INSTALL_DIR/$BINARY"
chmod +x "$INSTALL_DIR/$BINARY"
log "Installed $BINARY to $INSTALL_DIR/$BINARY"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) log "Add $INSTALL_DIR to PATH if needed." ;;
esac
"$INSTALL_DIR/$BINARY" --version
