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
  CANDIDATE_URLS+=(
    "https://github.com/${REPO}/releases/latest/download/farhand-${TARGET}.tar.gz"
    "https://github.com/${REPO}/releases/latest/download/farhand-v1.0.0-${TARGET}.tar.gz"
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

DOWNLOADED=false
for URL in "${CANDIDATE_URLS[@]}"; do
  echo "Attempting download from: ${URL}"
  if curl -fsSL "$URL" -o "${TMP_DIR}/farhand.tar.gz" 2>/dev/null; then
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
  # Fallback: check if local cargo workspace is present
  # Detect a farhand source checkout by its layout, not by a package name: the
  # published crate names (farhand-cli / farhand-agent) are not the directory
  # names, and a name-based check rots the next time either one changes.
  if [ -f "Cargo.toml" ] && [ -f "crates/fh/Cargo.toml" ] && [ -f "crates/fhd/Cargo.toml" ]; then
    echo "Release tarball not found online. Building from local source via cargo..."
    cargo build --release -p farhand-cli -p farhand-agent
    cp -f target/release/fh "${INSTALL_DIR}/fh"
    cp -f target/release/fhd "${INSTALL_DIR}/fhd"
  else
    echo "Error: unable to download pre-built release binary and no local Rust workspace detected." >&2
    exit 1
  fi
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
