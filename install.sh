#!/bin/bash
set -e

REPO="rainnoon/oc-claw"
APP_NAME="oc-claw"
INSTALL_DIR="/Applications"

echo "Installing ${APP_NAME}..."

# Get latest release DMG URL
DMG_URL=$(curl -sL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep "browser_download_url.*\.dmg" \
  | head -1 \
  | cut -d '"' -f 4)

if [ -z "$DMG_URL" ]; then
  echo "Error: could not find DMG download URL"
  exit 1
fi

TMPDIR=$(mktemp -d)
DMG_PATH="${TMPDIR}/${APP_NAME}.dmg"

echo "Downloading..."
curl -sL "$DMG_URL" -o "$DMG_PATH"

echo "Installing..."
MOUNT_POINT=$(hdiutil attach "$DMG_PATH" -nobrowse -quiet | tail -1 | awk '{print $3}')

# Remove old version if exists
if [ -d "${INSTALL_DIR}/${APP_NAME}.app" ]; then
  rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
fi

cp -R "${MOUNT_POINT}/${APP_NAME}.app" "${INSTALL_DIR}/"
hdiutil detach "$MOUNT_POINT" -quiet

# Remove quarantine attribute
xattr -cr "${INSTALL_DIR}/${APP_NAME}.app"

# Clean up
rm -rf "$TMPDIR"

echo "Done! Opening ${APP_NAME}..."
open "${INSTALL_DIR}/${APP_NAME}.app"
