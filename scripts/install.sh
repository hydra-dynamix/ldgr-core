#!/usr/bin/env sh
set -eu

REPO="${LDGR_REPO:-hydra-dynamix/ldgr-core}"
PACKAGE="ldgr-core"
BINARY="ldgr"
AGENTCTL_BINARY="agentctl"
INSTALL_DIR="${LDGR_INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${LDGR_VERSION:-}"
TMP_DIR="${TMPDIR:-/tmp}/ldgr-install.$$"
CATALOG_SOURCE="${LDGR_CORE_UPDATE_INDEX:-https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/core-index.json}"
HELPER_SOURCE="${LDGR_CORE_CATALOG_HELPER:-https://raw.githubusercontent.com/$REPO/main/scripts/core-catalog.py}"
KEYRING_SOURCE="${LDGR_CORE_RELEASE_KEYRING:-}"
OFFLINE="${LDGR_INSTALL_OFFLINE:-0}"
PRERELEASE="${LDGR_PRERELEASE:-0}"

log() { printf '%s\n' "$*" >&2; }
fail() { log "error: $*"; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

usage() {
  cat <<'EOF'
Usage: install.sh

Install the paired LDGR Core release selected from the signed Core catalog.

Options:
  --help  Show this help.
EOF
}

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

fetch() {
  source="$1"
  destination="$2"
  case "$source" in
    file://*) cp "${source#file://}" "$destination" ;;
    https://*)
      [ "$OFFLINE" != "1" ] || fail "offline installation requires file:// sources"
      curl -fsSL --proto '=https' --proto-redir '=https' "$source" -o "$destination"
      ;;
    *) fail "installer sources must use HTTPS or file://: $source" ;;
  esac
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --help|-h) usage; exit 0 ;;
    *) fail "unknown option: $1" ;;
  esac
  shift
done

require uname
require tar
[ "$OFFLINE" = "1" ] || require curl
require python3

PLATFORM="$(platform_tag)"
BINARY_FILE="$BINARY"
case "$PLATFORM" in
  windows-*) BINARY_FILE="$BINARY.exe" ;;
esac
mkdir -p "$TMP_DIR"
fetch "$HELPER_SOURCE" "$TMP_DIR/core-catalog.py"
fetch "$CATALOG_SOURCE" "$TMP_DIR/core-index.json"
fetch "$CATALOG_SOURCE.sig" "$TMP_DIR/core-index.json.sig"
if [ -n "$KEYRING_SOURCE" ]; then
  fetch "$KEYRING_SOURCE" "$TMP_DIR/release-keyring.json"
else
  cat > "$TMP_DIR/release-keyring.json" <<'EOF'
{"keys":[{"key_id":"ldgr-release-2026-01","public_key":"3wI34tu3PrqWp6VdNrNsFfX1W5PWSeQ3vsR04B69d+I="}]}
EOF
fi

set -- python3 "$TMP_DIR/core-catalog.py" resolve \
  --catalog "$TMP_DIR/core-index.json" \
  --signature "$TMP_DIR/core-index.json.sig" \
  --keyring "$TMP_DIR/release-keyring.json" \
  --platform "$PLATFORM" \
  --output "$TMP_DIR/resolved.json"
[ -z "$VERSION" ] || set -- "$@" --version "$VERSION"
[ "$PRERELEASE" != "1" ] || set -- "$@" --prerelease
[ "$OFFLINE" != "1" ] || set -- "$@" --offline
"$@"

field() {
  python3 "$TMP_DIR/core-catalog.py" field \
    --resolved "$TMP_DIR/resolved.json" --name "$1"
}
VERSION="$(field version)"
AGENTCTL_VERSION="$(field agentctl_version)"
URL="$(field archive_url)"
SIGNATURE_URL="$(field signature_url)"
EXPECTED_SHA256="$(field sha256)"
SIGNING_KEY_ID="$(field signing_key_id)"
ARCHIVE="$PACKAGE-$VERSION-$PLATFORM.tar.gz"
log "Installing signed $BINARY $VERSION for $PLATFORM"
log "Download: $URL"
fetch "$URL" "$TMP_DIR/$ARCHIVE"
fetch "$URL.sha256" "$TMP_DIR/$ARCHIVE.sha256"
fetch "$SIGNATURE_URL" "$TMP_DIR/$ARCHIVE.sig"
python3 "$TMP_DIR/core-catalog.py" verify-archive \
  --resolved "$TMP_DIR/resolved.json" \
  --archive "$TMP_DIR/$ARCHIVE" \
  --checksum "$TMP_DIR/$ARCHIVE.sha256" \
  --signature "$TMP_DIR/$ARCHIVE.sig"

tar -xzf "$TMP_DIR/$ARCHIVE" -C "$TMP_DIR"
SRC="$TMP_DIR/$PACKAGE-$VERSION/$PLATFORM/$BINARY_FILE"
AGENTCTL_FILE="$AGENTCTL_BINARY"
case "$PLATFORM" in
  windows-*) AGENTCTL_FILE="$AGENTCTL_BINARY.exe" ;;
esac
AGENTCTL_SRC="$TMP_DIR/$PACKAGE-$VERSION/$PLATFORM/$AGENTCTL_FILE"
RELEASE_METADATA="$TMP_DIR/$PACKAGE-$VERSION/RELEASE-METADATA.json"
[ -f "$RELEASE_METADATA" ] || fail "archive did not contain RELEASE-METADATA.json"
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
chmod +x "$INSTALL_DIR/$AGENTCTL_FILE"
cp "$SRC" "$INSTALL_DIR/$BINARY_FILE"
chmod +x "$INSTALL_DIR/$BINARY_FILE"
log "Installed paired $BINARY_FILE and $AGENTCTL_FILE to $INSTALL_DIR"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) log "Add $INSTALL_DIR to PATH if needed." ;;
esac
core_version_output="$("$INSTALL_DIR/$BINARY_FILE" --version)"
[ "$core_version_output" = "ldgr $VERSION" ] ||
  fail "installed Core version mismatch: expected ldgr $VERSION; got $core_version_output"
agentctl_version_output="$("$INSTALL_DIR/$AGENTCTL_FILE" --version)"
[ "$agentctl_version_output" = "agentctl $AGENTCTL_VERSION" ] ||
  fail "installed agentctl version mismatch: expected agentctl $AGENTCTL_VERSION; got $agentctl_version_output"
"$INSTALL_DIR/$BINARY_FILE" compatibility \
  --agentctl-version "$AGENTCTL_VERSION" --json
"$INSTALL_DIR/$BINARY_FILE" __record-core-installation \
  --home "$HOME" \
  --agentctl-binary "$INSTALL_DIR/$AGENTCTL_FILE" \
  --release-metadata "$RELEASE_METADATA" \
  --archive-url "$URL" \
  --archive-sha256 "$EXPECTED_SHA256" \
  --signing-key-id "$SIGNING_KEY_ID"
log "Recorded official installation ownership under $HOME/.ldgr"
