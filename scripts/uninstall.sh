#!/usr/bin/env bash
# Stop AI-usage, remove its LaunchAgent and the installed .app.
set -euo pipefail

BUNDLE_ID="com.machine.ai-usage"
APP_DIR="$HOME/Applications/AI-usage.app"
PLIST="$HOME/Library/LaunchAgents/$BUNDLE_ID.plist"
uid="$(id -u)"

launchctl bootout "gui/$uid/$BUNDLE_ID" 2>/dev/null || true
pkill -f "$APP_DIR/Contents/MacOS/ai-usage" 2>/dev/null || true
rm -f "$PLIST"
rm -rf "$APP_DIR"
echo "Uninstalled AI-usage."
