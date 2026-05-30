#!/usr/bin/env bash
# Build AI-usage, install it as ~/Applications/AI-usage.app, and register a
# LaunchAgent so it starts at login. Re-run to update an existing install.
set -euo pipefail

BUNDLE_ID="com.machine.ai-usage"
APP_NAME="AI-usage"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_DIR="$HOME/Applications/$APP_NAME.app"
PLIST="$HOME/Library/LaunchAgents/$BUNDLE_ID.plist"
uid="$(id -u)"

echo "==> Building release binary"
( cd "$ROOT" && cargo build --release )

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS"
cp "$ROOT/target/release/ai-usage" "$APP_DIR/Contents/MacOS/ai-usage"
cp "$ROOT/packaging/Info.plist" "$APP_DIR/Contents/Info.plist"
BIN="$APP_DIR/Contents/MacOS/ai-usage"

# Resolve a proxy for the login context (GUI launch has no shell env). Prefer
# the current shell's proxy, else the macOS system proxy. The app also falls
# back to scutil at runtime, so this is belt-and-suspenders.
PROXY="${HTTPS_PROXY:-${HTTP_PROXY:-}}"
if [ -z "$PROXY" ] && scutil --proxy | grep -q 'HTTPSEnable : 1'; then
    host="$(scutil --proxy | awk '/HTTPSProxy/{print $3}')"
    port="$(scutil --proxy | awk '/HTTPSPort/{print $3}')"
    [ -n "$host" ] && [ -n "$port" ] && PROXY="http://$host:$port"
fi

echo "==> Writing LaunchAgent $PLIST (proxy=${PROXY:-none})"
mkdir -p "$HOME/Library/LaunchAgents"
{
    cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>$BUNDLE_ID</string>
    <key>ProgramArguments</key>
    <array><string>$BIN</string></array>
    <key>RunAtLoad</key><true/>
    <key>ProcessType</key><string>Interactive</string>
    <key>StandardErrorPath</key><string>/tmp/$BUNDLE_ID.log</string>
    <key>StandardOutPath</key><string>/tmp/$BUNDLE_ID.log</string>
EOF
    if [ -n "$PROXY" ]; then
        cat <<EOF
    <key>EnvironmentVariables</key>
    <dict>
        <key>HTTPS_PROXY</key><string>$PROXY</string>
        <key>HTTP_PROXY</key><string>$PROXY</string>
    </dict>
EOF
    fi
    cat <<EOF
</dict>
</plist>
EOF
} > "$PLIST"

echo "==> (Re)loading LaunchAgent"
launchctl bootout "gui/$uid/$BUNDLE_ID" 2>/dev/null || true
launchctl bootstrap "gui/$uid" "$PLIST"
launchctl kickstart -k "gui/$uid/$BUNDLE_ID"

echo "Done. '$APP_NAME' is running and will start at login."
echo "First launch may prompt for Keychain access — choose 'Always Allow'."
echo "Logs: /tmp/$BUNDLE_ID.log"
