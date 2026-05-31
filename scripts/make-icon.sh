#!/usr/bin/env bash
# Generate packaging/AppIcon.icns from scripts/render_icon.swift.
# Run when the icon design changes; the result is committed and used by install.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ICONSET="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$ICONSET"

echo "==> Rendering icon images"
swift "$ROOT/scripts/render_icon.swift" "$ICONSET"

echo "==> Building AppIcon.icns"
iconutil -c icns "$ICONSET" -o "$ROOT/packaging/AppIcon.icns"
rm -rf "$(dirname "$ICONSET")"
echo "Wrote packaging/AppIcon.icns"
