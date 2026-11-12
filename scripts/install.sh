#!/bin/bash
# Install fluteterm: CLI to ~/.local/bin, app bundle to ~/Applications.
# Optional: --claude (install Claude Code hooks)  --login (start at login)
set -euo pipefail
cd "$(dirname "$0")/.."
# Upgrading from the pre-rename "drpdown": stop it and retire its files. Running Claude
# Code sessions still call the old hook path until restarted, so the old app path
# becomes a link to fluteterm (delete ~/Applications/drpdown.app any time later).
if [ -e ~/Applications/drpdown.app ] && [ ! -L ~/Applications/drpdown.app ]; then
  ~/Applications/drpdown.app/Contents/MacOS/drpdown quit 2>/dev/null || true
  for _ in $(seq 1 50); do
    pgrep -f "drpdown.app/Contents/MacOS/drpdown" >/dev/null || break
    sleep 0.1
  done
  rm -rf ~/Applications/drpdown.app
  ln -s ~/Applications/fluteterm.app ~/Applications/drpdown.app
fi
[ -L ~/.local/bin/drpdown ] && rm -f ~/.local/bin/drpdown
./scripts/bundle.sh
mkdir -p ~/.local/bin ~/Applications
# Quit a running instance first.
if [ -x ~/.local/bin/fluteterm ]; then ~/.local/bin/fluteterm quit 2>/dev/null || true; fi
for _ in $(seq 1 50); do
  pgrep -f "fluteterm.app/Contents/MacOS/fluteterm" >/dev/null || break
  sleep 0.1
done
rm -rf ~/Applications/fluteterm.app
cp -R dist/fluteterm.app ~/Applications/
# The CLI is a symlink into the bundle so hooks, CLI and app are one binary.
ln -sf ~/Applications/fluteterm.app/Contents/MacOS/fluteterm ~/.local/bin/fluteterm
echo "installed ~/Applications/fluteterm.app and ~/.local/bin/fluteterm"
# Hooks installed by an older build point at the old binary: refresh them.
if grep -q "drpdown hook claude" ~/.claude/settings.json 2>/dev/null; then
  ~/.local/bin/fluteterm install claude >/dev/null && echo "re-pointed Claude Code hooks to fluteterm"
fi
for arg in "$@"; do
  case "$arg" in
    --claude) ~/.local/bin/fluteterm install claude ;;
    --login) ~/.local/bin/fluteterm install login ;;
  esac
done
open ~/Applications/fluteterm.app
for _ in $(seq 1 50); do ~/.local/bin/fluteterm ping >/dev/null 2>&1 && break; sleep 0.1; done
echo "fluteterm is running — press F12 (fn+F12 on laptop keyboards)."
