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

if [ "$VERSION" = "latest" ]; then
  RELEASE_URL="https://github.com/${REPO}/releases/latest/download/farhand-${TARGET}.tar.gz"
else
  # Ensure v prefix
  case "$VERSION" in
    v*) TAG="$VERSION" ;;
    *) TAG="v$VERSION" ;;
  esac
  RELEASE_URL="https://github.com/${REPO}/releases/download/${TAG}/farhand-${TAG}-${TARGET}.tar.gz"
fi

echo "Attempting download from: ${RELEASE_URL}"

# If curl succeeds, install pre-built binaries; otherwise fallback to cargo build if local source is present
if curl -fsSL "$RELEASE_URL" -o "${TMP_DIR}/farhand.tar.gz" 2>/dev/null; then
  echo "Extracting binary package..."
  tar -xzf "${TMP_DIR}/farhand.tar.gz" -C "${TMP_DIR}"
  cp -f "${TMP_DIR}/fh" "${INSTALL_DIR}/fh"
  cp -f "${TMP_DIR}/fhd" "${INSTALL_DIR}/fhd"
else
  # Fallback: check if local cargo workspace is present
  if [ -f "Cargo.toml" ] && grep -q 'name = "fh"' crates/fh/Cargo.toml 2>/dev/null; then
    echo "Release tarball not found online. Building from local source via cargo..."
    cargo build --release -p fh -p fhd
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
