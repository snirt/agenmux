#!/usr/bin/env bash
# Assemble, ad-hoc sign, and install the AgentsMon.app notification helper.
# UNUserNotificationCenter requires a signed, registered app bundle; ad-hoc
# signing is enough for local builds, but the bundle must live in a real
# Applications folder (temporary directories cannot register on macOS 26).
#
# usage: install-app.sh [dest-dir]   (default: ~/Applications)
set -euo pipefail
DIR="$(cd "$(dirname "$0")/.." && pwd)"

[ "$(uname -s)" = Darwin ] || { echo "agents-mon: install-app is macOS-only" >&2; exit 1; }

dest="${1:-$HOME/Applications}"

bin="${AGENTS_MON_NOTIFIER_BIN:-$DIR/target/release/agents-mon-notifier}"
if [ ! -x "$bin" ]; then
  cargo build --release --manifest-path "$DIR/Cargo.toml"
fi
[ -x "$bin" ] || { echo "agents-mon: notifier binary not found: $bin" >&2; exit 1; }

version="$(bash "$DIR/scripts/version.sh")"
app="$dest/AgentsMon.app"
mkdir -p "$app/Contents/MacOS"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>io.github.snirt.agents-mon</string>
	<key>CFBundleName</key>
	<string>AgentsMon</string>
	<key>CFBundleDisplayName</key>
	<string>AgentsMon</string>
	<key>CFBundleExecutable</key>
	<string>agents-mon-notifier</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$version</string>
	<key>CFBundleVersion</key>
	<string>$version</string>
	<key>LSUIElement</key>
	<true/>
</dict>
</plist>
PLIST
cp -f "$bin" "$app/Contents/MacOS/agents-mon-notifier"
codesign --force --sign - "$app"

echo "installed $app"
# first run registers the bundle with LaunchServices and triggers the
# notification permission prompt
"$app/Contents/MacOS/agents-mon-notifier" "AgentsMon" \
  "Notifications are set up. Allow AgentsMon if macOS asks." || true
echo "If no prompt appeared, enable it under System Settings → Notifications → AgentsMon."
