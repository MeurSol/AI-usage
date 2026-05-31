#!/usr/bin/env bash
# Stop AI-usage, remove its LaunchAgent and the installed .app.
set -euo pipefail

BUNDLE_ID="com.machine.ai-usage"
PLIST="$HOME/Library/LaunchAgents/$BUNDLE_ID.plist"
uid="$(id -u)"

launchctl bootout "gui/$uid/$BUNDLE_ID" 2>/dev/null || true
pkill -f "AI-usage.app/Contents/MacOS/ai-usage" 2>/dev/null || true
rm -f "$PLIST"
# Remove from both possible install locations.
rm -rf "/Applications/AI-usage.app" "$HOME/Applications/AI-usage.app"
echo "Uninstalled AI-usage."
