#!/bin/sh
# Builds a fresh, ad-hoc signed Padgent.app for distribution.
set -e
cd "$(dirname "$0")"
PROJECT_ROOT=$(pwd -P)
# Encoded flags override RUSTFLAGS, so do not silently bypass path remapping.
if [ -n "${CARGO_ENCODED_RUSTFLAGS:-}" ]; then
  echo "Unset CARGO_ENCODED_RUSTFLAGS before building the distribution" >&2
  exit 1
fi
# Cargo splits RUSTFLAGS on whitespace. Fail closed for paths it cannot encode.
case "$HOME$PROJECT_ROOT" in *[[:space:]]*) echo "Build paths containing whitespace are not supported" >&2; exit 1;; esac
# rustc uses the last matching prefix; place the more specific project path last.
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$HOME=/app-home --remap-path-prefix=$PROJECT_ROOT=/source"
export RUSTFLAGS
CARGO_TARGET_DIR="$PROJECT_ROOT/target"
export CARGO_TARGET_DIR
cargo build --release

APP=target/Padgent.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/padgent "$APP/Contents/MacOS/Padgent"
cp LICENSE "$APP/Contents/Resources/LICENSE"

# Icon: drawn by the app itself, then packed with the macOS icon tools.
ICONSET=target/AppIcon.iconset
rm -rf "$ICONSET"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  ./target/release/padgent --icon "$ICONSET/icon_${s}x${s}.png" "$s"
  ./target/release/padgent --icon "$ICONSET/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

VERSION=$(sed -n '/^\[package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
case "$VERSION" in ''|*[!0-9.]*) echo "Invalid package version" >&2; exit 1;; esac
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Padgent</string>
  <key>CFBundleDisplayName</key><string>Padgent</string>
  <key>CFBundleIdentifier</key><string>com.agentpad.app</string>
  <key>CFBundleExecutable</key><string>Padgent</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSPrincipalClass</key><string>NSApplication</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$APP"

echo "Built $APP (ad-hoc signed; not notarized)"
