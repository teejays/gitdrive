#!/bin/sh
set -e

REPO="teejays/gitdrive"
INSTALL_DIR="/usr/local/bin"
BINARY_NAME="gitdrive"

# Detect platform
OS=$(uname -s)
ARCH=$(uname -m)

case "${OS}" in
  Darwin)
    case "${ARCH}" in
      arm64)  TARGET="aarch64-apple-darwin" ;;
      x86_64) TARGET="x86_64-apple-darwin" ;;
      *)      echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
    esac
    ;;
  Linux)
    case "${ARCH}" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *)      echo "Unsupported architecture: ${ARCH}"; exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: ${OS}"
    exit 1
    ;;
esac

ASSET="gitdrive-${TARGET}.tar.gz"

# Get latest release tag
echo "Detecting latest release..."
TAG=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | grep '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')

if [ -z "${TAG}" ]; then
  echo "Error: could not determine latest release."
  exit 1
fi

URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"

echo "Downloading gitdrive ${TAG} for ${OS} ${ARCH}..."
TMPDIR=$(mktemp -d)
curl -fsSL "${URL}" -o "${TMPDIR}/${ASSET}"
tar xzf "${TMPDIR}/${ASSET}" -C "${TMPDIR}"

echo "Installing to ${INSTALL_DIR}/${BINARY_NAME}..."
if [ -w "${INSTALL_DIR}" ]; then
  mv "${TMPDIR}/gitdrive-${TARGET}" "${INSTALL_DIR}/${BINARY_NAME}"
else
  sudo mv "${TMPDIR}/gitdrive-${TARGET}" "${INSTALL_DIR}/${BINARY_NAME}"
fi
chmod +x "${INSTALL_DIR}/${BINARY_NAME}"

rm -rf "${TMPDIR}"

echo "Done! Run 'gitdrive init' to get started."
