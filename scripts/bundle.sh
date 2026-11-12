#!/bin/bash
# Build fluteterm.app (menu-bar app, no Dock icon) into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release
APP=dist/fluteterm.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/fluteterm "$APP/Contents/MacOS/fluteterm"

# Icon: rendered by fluteterm itself, packed with sips + iconutil.
ICONSET=$(mktemp -d)/fluteterm.iconset
mkdir -p "$ICONSET"
target/release/fluteterm gen-icon "$ICONSET/icon_512x512@2x.png" 1024
for s in 16 32 128 256 512; do
  sips -z $s $s "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$ICONSET/icon_512x512@2x.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/fluteterm.icns"

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>fluteterm</string>
  <key>CFBundleDisplayName</key><string>fluteterm</string>
  <key>CFBundleIdentifier</key><string>dev.fluteterm.app</string>
  <key>CFBundleVersion</key><string>${VERSION}</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>CFBundleExecutable</key><string>fluteterm</string>
  <key>CFBundleIconFile</key><string>fluteterm</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSUIElement</key><true/>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSUserNotificationAlertStyle</key><string>banner</string>
</dict>
</plist>
PLIST
codesign --force --deep -s - "$APP" >/dev/null 2>&1 || true
echo "built $APP"
