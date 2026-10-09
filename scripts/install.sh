#!/usr/bin/env bash
set -euo pipefail

REPO="Rayrsn/farhand"
VERSION="${FARHAND_VERSION:-latest}"
INSTALL_DIR="${FARHAND_INSTALL_DIR:-}"

# Parse optional command-line flags
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    -h|--help)
      echo "Farhand Installer"
      echo ""
      echo "Usage: install.sh [options]"
      echo "Options:"
      echo "  --prefix <dir>     Directory to install binaries into (default: /usr/local/bin or ~/.local/bin)"
      echo "  --version <tag>    Version tag to install (default: latest)"
      echo "  -h, --help         Show this help message"
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      exit 1
      ;;
  esac
done

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2 && exit 1 ;;
esac

case "$OS" in
  linux) OS_NAME="unknown-linux-musl" ;;
  darwin) OS_NAME="apple-darwin" ;;
  *) echo "Unsupported operating system: $OS" >&2 && exit 1 ;;
esac

TARGET="${ARCH}-${OS_NAME}"
echo "Detected platform: ${OS}/${ARCH} (${TARGET})"

# Determine default install directory if not specified
if [ -z "$INSTALL_DIR" ]; then
  if [ "$(id -u)" -eq 0 ]; then
    INSTALL_DIR="/usr/local/bin"
  elif [ -w "/usr/local/bin" ]; then
    INSTALL_DIR="/usr/local/bin"
  else
    INSTALL_DIR="${HOME}/.local/bin"
  fi
fi

mkdir -p "${INSTALL_DIR}"

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

CANDIDATE_URLS=()
if [ "$VERSION" = "latest" ]; then
  # Only one naming is published for a floating "latest" fetch: the direct,
  # version-less asset. The versioned asset needs the tag, which is unknown
  # here — that is handled by the explicit-version branch below.
  CANDIDATE_URLS+=(
    "https://github.com/${REPO}/releases/latest/download/farhand-${TARGET}.tar.gz"
  )
else
  case "$VERSION" in
    v*) TAG="$VERSION" ;;
    *) TAG="v$VERSION" ;;
  esac
  CANDIDATE_URLS+=(
    "https://github.com/${REPO}/releases/download/${TAG}/farhand-${TAG}-${TARGET}.tar.gz"
    "https://github.com/${REPO}/releases/download/${TAG}/farhand-${TARGET}.tar.gz"
  )
fi

# Verify the download against the SHA-256 published next to the asset. Without
# this the script installs whatever the network returned, and the checksum that
# ships with every release is never checked by the one thing that downloads it.
verify_checksum() {
  local archive="$1" expected_file="$2" expected=""

  if [ ! -f "$expected_file" ]; then
    echo "Error: no published checksum alongside the asset; refusing to install unverified." >&2
    return 1
  fi

  # The file is "<sha256>  <filename>"; take the first field.
  expected="$(awk 'NR==1{print $1}' "$expected_file")"
  if [ -z "$expected" ]; then
    echo "Error: could not read a SHA-256 from the published checksum file." >&2
    return 1
  fi

  local actual
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$archive" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$archive" | awk '{print $1}')"
  else
    echo "Error: no sha256sum or shasum available; cannot verify the download." >&2
    return 1
  fi

  if [ "$actual" != "$expected" ]; then
    echo "Error: SHA-256 mismatch for the downloaded archive." >&2
    echo "  expected: ${expected}" >&2
    echo "  actual:   ${actual}" >&2
    echo "Refusing to install. The download may be corrupt or tampered with." >&2
    return 1
  fi

  echo "Checksum verified."
  return 0
}


DOWNLOADED=false
for URL in "${CANDIDATE_URLS[@]}"; do
  echo "Attempting download from: ${URL}"
  if curl -fsSL "$URL" -o "${TMP_DIR}/farhand.tar.gz" 2>/dev/null; then
    # The checksum asset is published beside the archive under the same name.
    CHECKSUM_URL="${URL}.sha256"
    echo "Fetching checksum from: ${CHECKSUM_URL}"
    if ! curl -fsSL "$CHECKSUM_URL" -o "${TMP_DIR}/farhand.tar.gz.sha256" 2>/dev/null; then
      echo "Error: could not fetch the published SHA-256 for this asset." >&2
      echo "Refusing to install an unverified binary." >&2
      exit 1
    fi
    if ! verify_checksum "${TMP_DIR}/farhand.tar.gz" "${TMP_DIR}/farhand.tar.gz.sha256"; then
      exit 1
    fi
    DOWNLOADED=true
    break
  fi
done


if [ "$DOWNLOADED" = "true" ]; then
  echo "Extracting binary package..."
  tar -xzf "${TMP_DIR}/farhand.tar.gz" -C "${TMP_DIR}"
  FH_BIN="$(find "${TMP_DIR}" -type f -name "fh" | head -n 1)"
  FHD_BIN="$(find "${TMP_DIR}" -type f -name "fhd" | head -n 1)"
  if [ -z "$FH_BIN" ] || [ -z "$FHD_BIN" ]; then
    echo "Error: archive did not contain fh and fhd binaries." >&2
    exit 1
  fi
  cp -f "$FH_BIN" "${INSTALL_DIR}/fh"
  cp -f "$FHD_BIN" "${INSTALL_DIR}/fhd"
else
  # There is deliberately no "build from whatever is in the current directory"
  # fallback. One existed, and it meant a failed download silently installed
  # the binaries of whatever unrelated farhand checkout happened to be the
  # working directory, under the name "Farhand", with no message. A build from
  # source is an explicit act: use `cargo install farhand-cli farhand-agent`.
  echo "Error: could not download or verify a pre-built release binary." >&2
  echo "" >&2
  echo "  Tried:" >&2
  for URL in "${CANDIDATE_URLS[@]}"; do
    echo "    ${URL}" >&2
  done
  echo "" >&2
  echo "  Install from source instead:" >&2
  echo "    cargo install farhand-cli      # provides fh" >&2
  echo "    cargo install farhand-agent    # provides fhd" >&2
  echo "" >&2
  echo "  Or pin a specific release:  bash <(curl -sL $0) --version <tag>" >&2
  exit 1
fi

chmod 755 "${INSTALL_DIR}/fh" "${INSTALL_DIR}/fhd"

echo ""
echo "=== Installation Successful ==="
echo "Farhand binaries installed to:"
echo "  Client: ${INSTALL_DIR}/fh"
echo "  Daemon: ${INSTALL_DIR}/fhd"
echo ""

# Check if INSTALL_DIR is in PATH
if ! echo ":$PATH:" | grep -q ":${INSTALL_DIR}:"; then
  echo "Notice: '${INSTALL_DIR}' is not in your current PATH."
  echo "Add it by adding this to your shell profile (~/.bashrc or ~/.zshrc):"
  echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
  echo ""
fi

"${INSTALL_DIR}/fh" --version 2>/dev/null || true
"${INSTALL_DIR}/fhd" --version 2>/dev/null || true
