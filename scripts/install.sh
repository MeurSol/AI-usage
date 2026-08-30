#!/usr/bin/env bash
# Build AI-usage, install it to /Applications (so it shows in Finder/Launchpad),
# and register a LaunchAgent so it starts at login. Re-run to update.
set -euo pipefail

BUNDLE_ID="com.machine.ai-usage"
APP_NAME="AI-usage"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PLIST="$HOME/Library/LaunchAgents/$BUNDLE_ID.plist"
uid="$(id -u)"

# Prefer /Applications (where users look); fall back to ~/Applications if it
# isn't writable (no sudo).
if [ -w /Applications ]; then
    APP_DIR="/Applications/$APP_NAME.app"
else
    APP_DIR="$HOME/Applications/$APP_NAME.app"
    mkdir -p "$HOME/Applications"
fi

echo "==> Building release binary"
( cd "$ROOT" && cargo build --release )

echo "==> Assembling $APP_DIR"
rm -rf "$APP_DIR"
mkdir -p "$APP_DIR/Contents/MacOS" "$APP_DIR/Contents/Resources"
cp "$ROOT/target/release/ai-usage" "$APP_DIR/Contents/MacOS/ai-usage"
cp "$ROOT/packaging/Info.plist" "$APP_DIR/Contents/Info.plist"
cp "$ROOT/packaging/AppIcon.icns" "$APP_DIR/Contents/Resources/AppIcon.icns"
BIN="$APP_DIR/Contents/MacOS/ai-usage"

echo "==> Code-signing"
IDENTITY="AI-usage Local"
if security find-identity -p codesigning | grep -q "$IDENTITY"; then
    # Stable self-signed identity with a fixed --identifier, so the app keeps
    # one designated requirement across rebuilds. Keychain access does not
    # depend on it: credentials are read through /usr/bin/security, which sits
    # in the item's apple-tool: partition (see src/keychain.rs).
    codesign --force --options runtime \
        --identifier "$BUNDLE_ID" \
        --sign "$IDENTITY" "$APP_DIR"
    echo "    signed with '$IDENTITY'"
else
    # No stable identity: ad-hoc sign. Run scripts/setup-signing.sh once for
    # a stable one.
    codesign --force --identifier "$BUNDLE_ID" --sign - "$APP_DIR"
    echo "    ad-hoc signed (run scripts/setup-signing.sh for a stable identity)"
fi
codesign --verify --deep --strict "$APP_DIR" && echo "    signature verified"

echo "==> Registering with LaunchServices (Finder/Spotlight/Launchpad)"
LSREGISTER=/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister
"$LSREGISTER" -f "$APP_DIR" 2>/dev/null || true
# Refresh the icon cache so the new icon shows immediately.
touch "$APP_DIR"

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

echo "Done. '$APP_NAME' is installed at $APP_DIR, running, and starts at login."
echo "Find it in Finder → Applications (or Launchpad / Spotlight)."
echo "Logs: /tmp/$BUNDLE_ID.log"
