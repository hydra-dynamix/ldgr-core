#!/usr/bin/env sh
set -eu

REPO="${LDGR_REPO:-hydra-dynamix/ldgr-core}"
PACKAGE="ldgr-core"
BINARY="ldgr"
AGENTCTL_BINARY="agentctl"
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
    cargo install --git "https://github.com/$REPO" --tag "v$VERSION" --locked --force "$PACKAGE"
  else
    cargo install --git "https://github.com/$REPO" --locked --force "$PACKAGE"
  fi
}

require uname
require tar
require curl

PLATFORM="$(platform_tag)"
BINARY_FILE="$BINARY"
case "$PLATFORM" in
  windows-*) BINARY_FILE="$BINARY.exe" ;;
esac
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
SRC="$TMP_DIR/$PACKAGE-$VERSION/$PLATFORM/$BINARY_FILE"
AGENTCTL_FILE="$AGENTCTL_BINARY"
case "$PLATFORM" in
  windows-*) AGENTCTL_FILE="$AGENTCTL_BINARY.exe" ;;
esac
AGENTCTL_SRC="$TMP_DIR/$PACKAGE-$VERSION/$PLATFORM/$AGENTCTL_FILE"
[ -f "$SRC" ] || fail "archive did not contain expected binary: $PACKAGE-$VERSION/$PLATFORM/$BINARY_FILE"
[ -f "$AGENTCTL_SRC" ] || fail "archive did not contain paired launcher: $PACKAGE-$VERSION/$PLATFORM/$AGENTCTL_FILE"
mkdir -p "$INSTALL_DIR"
if [ -f "$INSTALL_DIR/$AGENTCTL_FILE" ]; then
  cp "$INSTALL_DIR/$AGENTCTL_FILE" "$INSTALL_DIR/$AGENTCTL_FILE.previous"
fi
if [ -f "$INSTALL_DIR/$BINARY_FILE" ]; then
  cp "$INSTALL_DIR/$BINARY_FILE" "$INSTALL_DIR/$BINARY_FILE.previous"
fi
cp "$AGENTCTL_SRC" "$INSTALL_DIR/$AGENTCTL_FILE"
cp "$SRC" "$INSTALL_DIR/$BINARY_FILE"
chmod +x "$INSTALL_DIR/$BINARY_FILE" "$INSTALL_DIR/$AGENTCTL_FILE"
log "Installed paired $BINARY_FILE and $AGENTCTL_FILE to $INSTALL_DIR"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) log "Add $INSTALL_DIR to PATH if needed." ;;
esac
"$INSTALL_DIR/$BINARY_FILE" --version
"$INSTALL_DIR/$AGENTCTL_FILE" --version
agentctl_version="$("$INSTALL_DIR/$AGENTCTL_FILE" --version | awk '{print $2}')"
"$INSTALL_DIR/$BINARY_FILE" compatibility --agentctl-version "$agentctl_version" --json
