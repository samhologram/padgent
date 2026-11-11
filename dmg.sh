#!/bin/sh
# Build a clean Apple Silicon disk image. The app is ad-hoc signed, not notarized.
set -eu
cd "$(dirname "$0")"
if [ "$(uname -m)" != arm64 ]; then
  echo "Build the macOS arm64 disk image on Apple Silicon" >&2
  exit 1
fi
./bundle.sh
VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' target/Padgent.app/Contents/Info.plist)
case "$VERSION" in ''|*[!0-9.]*) echo "Invalid bundle version" >&2; exit 1;; esac
lipo -verify_arch arm64 target/Padgent.app/Contents/MacOS/Padgent
STAGING=$(mktemp -d "${TMPDIR:-/tmp}/padgent-dmg.XXXXXXXX")
trap 'rm -rf "$STAGING"' EXIT HUP INT TERM
ditto --noextattr --norsrc target/Padgent.app "$STAGING/Padgent.app"
ln -s /Applications "$STAGING/Applications"
OUTPUT="$(pwd -P)/target/Padgent-$VERSION-macOS-arm64.dmg"
hdiutil create -ov -fs HFS+ -format UDZO -volname Padgent -srcfolder "$STAGING" "$OUTPUT"
shasum -a 256 "$OUTPUT"
echo "Created $OUTPUT"
