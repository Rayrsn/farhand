#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

VERSION="$(grep '^version = ' Cargo.toml | head -n 1 | cut -d '"' -f 2)"
echo "=== Packaging Farhand Release v${VERSION} ==="

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) echo "Unsupported architecture: $ARCH" >&2 && exit 1 ;;
esac

TARGET="${TARGET:-${ARCH}-${OS}}"

echo "Building release binaries for target: ${TARGET}..."
cargo build --release -p fh -p fhd

RELEASE_DIR="${ROOT_DIR}/dist/releases/v${VERSION}"
mkdir -p "${RELEASE_DIR}"

PACKAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$PACKAGE_DIR"' EXIT

STAGE_DIR="${PACKAGE_DIR}/farhand-v${VERSION}-${TARGET}"
mkdir -p "${STAGE_DIR}/services"

cp -f "target/release/fh" "${STAGE_DIR}/fh"
cp -f "target/release/fhd" "${STAGE_DIR}/fhd"
chmod 755 "${STAGE_DIR}/fh" "${STAGE_DIR}/fhd"

if [ -f "README.md" ]; then
  cp -f "README.md" "${STAGE_DIR}/"
fi
if [ -f "specs.md" ]; then
  cp -f "specs.md" "${STAGE_DIR}/"
fi
if [ -f "dist/services/fhd.service" ]; then
  cp -f "dist/services/fhd.service" "${STAGE_DIR}/services/"
fi
if [ -f "dist/services/com.farhand.fhd.plist" ]; then
  cp -f "dist/services/com.farhand.fhd.plist" "${STAGE_DIR}/services/"
fi

ARCHIVE_NAME="farhand-v${VERSION}-${TARGET}.tar.gz"
ARCHIVE_PATH="${RELEASE_DIR}/${ARCHIVE_NAME}"

echo "Creating tarball: ${ARCHIVE_NAME}..."
tar -czf "${ARCHIVE_PATH}" -C "${PACKAGE_DIR}" "farhand-v${VERSION}-${TARGET}"

# Generate SHA-256 checksum
if command -v sha256sum >/dev/null 2>&1; then
  (cd "${RELEASE_DIR}" && sha256sum "${ARCHIVE_NAME}" > "${ARCHIVE_NAME}.sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "${RELEASE_DIR}" && shasum -a 256 "${ARCHIVE_NAME}" > "${ARCHIVE_NAME}.sha256")
fi

echo ""
echo "=== Release Artifact Created ==="
echo "Path: ${ARCHIVE_PATH}"
echo "Size: $(du -h "${ARCHIVE_PATH}" | cut -f 1)"
if [ -f "${ARCHIVE_PATH}.sha256" ]; then
  echo "Checksum: $(cat "${ARCHIVE_PATH}.sha256")"
fi
echo ""
