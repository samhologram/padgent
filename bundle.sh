#!/bin/sh
# Builds AgentPad.app (release) and installs it in ~/Applications.
set -e
cd "$(dirname "$0")"
cargo build --release

APP=target/AgentPad.app
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/agentpad "$APP/Contents/MacOS/AgentPad"

# Icon: drawn by the app itself, then packed with the macOS icon tools.
ICONSET=target/AppIcon.iconset
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  ./target/release/agentpad --icon "$ICONSET/icon_${s}x${s}.png" "$s"
  ./target/release/agentpad --icon "$ICONSET/icon_${s}x${s}@2x.png" "$((s * 2))"
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/AppIcon.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>AgentPad</string>
  <key>CFBundleDisplayName</key><string>AgentPad</string>
  <key>CFBundleIdentifier</key><string>com.agentpad.app</string>
  <key>CFBundleExecutable</key><string>AgentPad</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
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

mkdir -p "$HOME/Applications"
ditto "$APP" "$HOME/Applications/AgentPad.app"
echo "Installed $HOME/Applications/AgentPad.app"
