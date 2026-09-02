#!/usr/bin/env bash
# Assemble, ad-hoc sign, and install the Agenmux.app notification helper.
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

[ "$(uname -s)" = Darwin ] || {
	echo "agenmux: install-app is macOS-only" >&2
	exit 1
}

quiet=0
if [ "${1:-}" = "--quiet" ]; then
	quiet=1
	shift
fi
dest="${1:-$HOME/Applications}"

bin="${AGENMUX_NOTIFIER_BIN:-${AGENTS_MON_NOTIFIER_BIN:-$DIR/target/release/agenmux-notifier}}"
if [ ! -x "$bin" ]; then
	cargo build --release --manifest-path "$DIR/Cargo.toml"
fi
[ -x "$bin" ] || {
	echo "agenmux: notifier binary not found: $bin" >&2
	exit 1
}

version="$(bash "$DIR/scripts/version.sh")"
app="$dest/Agenmux.app"
legacy_app="$dest/AgentsMon.app"
stage="$dest/.Agenmux.app.$$"
backup="$dest/.Agenmux.app.backup.$$"
trap 'rm -rf "$stage" "$backup"' EXIT
rm -rf "$stage" "$backup"
mkdir -p "$stage/Contents/MacOS"
cat >"$stage/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleIdentifier</key>
	<string>io.github.snirt.agenmux</string>
	<key>CFBundleName</key>
	<string>agenmux</string>
	<key>CFBundleDisplayName</key>
	<string>agenmux</string>
	<key>CFBundleExecutable</key>
	<string>agenmux-notifier</string>
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
cp -f "$bin" "$stage/Contents/MacOS/agenmux-notifier"
codesign --force --sign - "$stage"
if [ -e "$app" ]; then
    mv "$app" "$backup"
fi
if ! mv "$stage" "$app"; then
    [ ! -e "$backup" ] || mv "$backup" "$app"
    exit 1
fi
rm -rf "$backup" "$legacy_app"
trap - EXIT

echo "installed $app"
[ "$quiet" = 1 ] && exit 0
# --setup registers the bundle, asks for permission, waits for the user's
# answer, and posts a test notification when granted
if "$app/Contents/MacOS/agenmux-notifier" --setup; then
	echo "✓ notifications enabled"
else
	echo "Notifications are off. Enable: System Settings → Notifications → agenmux."
fi
