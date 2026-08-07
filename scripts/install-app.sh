#!/usr/bin/env bash
# Assemble, ad-hoc sign, and install the AgentsMon.app notification helper.
# UNUserNotificationCenter requires a signed, registered app bundle; ad-hoc
# signing is enough for local builds, but the bundle must live in a real
# Applications folder (temporary directories cannot register on macOS 26).
#
# usage: install-app.sh [--quiet] [dest-dir]   (default: ~/Applications)
#
# --quiet installs/refreshes the app without requesting permission — used by
# the automatic install; macOS then asks with the first real notification.
set -euo pipefail
DIR="$(cd "$(dirname "$0")/.." && pwd)"

[ "$(uname -s)" = Darwin ] || { echo "agents-mon: install-app is macOS-only" >&2; exit 1; }

quiet=0
if [ "${1:-}" = "--quiet" ]; then
  quiet=1
  shift
fi
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
[ "$quiet" = 1 ] && exit 0
# --setup registers the bundle, asks for permission, waits for the user's
# answer, and posts a test notification when granted
if "$app/Contents/MacOS/agents-mon-notifier" --setup; then
  echo "✓ notifications enabled"
else
  echo "Notifications are off. Enable: System Settings → Notifications → AgentsMon."
fi
