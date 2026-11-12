#!/bin/bash
# Build dist/fluteterm-<version>.dmg: drag-to-Applications disk image.
# Uses only built-in tools (hdiutil). Run scripts/bundle.sh first or let this do it.
set -euo pipefail
cd "$(dirname "$0")/.."

./scripts/bundle.sh

VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
ARCH=$(uname -m)
DMG="dist/fluteterm-${VERSION}-${ARCH}.dmg"
STAGE=$(mktemp -d)/fluteterm

mkdir -p "$STAGE"
cp -R dist/fluteterm.app "$STAGE/"
ln -s /Applications "$STAGE/Applications"
cat > "$STAGE/README.txt" <<'EOF'
fluteterm — drop-down terminal for macOS

1. Drag fluteterm.app into Applications.
2. Open it (first time: right-click → Open, because the app is not notarized).
3. Press F12 (fn+F12 on laptop keyboards) to drop it down.

Optional, from any terminal:
  /Applications/fluteterm.app/Contents/MacOS/fluteterm install claude   # Claude Code integration
  /Applications/fluteterm.app/Contents/MacOS/fluteterm install login    # start at login
  ln -sf /Applications/fluteterm.app/Contents/MacOS/fluteterm ~/.local/bin/fluteterm   # CLI
EOF

rm -f "$DMG"
hdiutil create -volname "fluteterm ${VERSION}" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov "$DMG" >/dev/null
rm -rf "$(dirname "$STAGE")"

# Ad-hoc sign the image too (real distribution needs a Developer ID + notarization).
codesign --force -s - "$DMG" >/dev/null 2>&1 || true
echo "built $DMG ($(du -h "$DMG" | cut -f1))"
