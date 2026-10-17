#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
	printf 'usage: %s <dmg_path> <expected_app_version>\n' "$(basename "$0")" >&2
	exit 1
fi

DMG_PATH="$1"
EXPECTED_VERSION="$2"
if [[ ! -f "$DMG_PATH" ]]; then
	printf 'missing DMG: %s\n' "$DMG_PATH" >&2
	exit 1
fi
if [[ ! "$EXPECTED_VERSION" =~ ^([0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?|0\.0\.0-ci)$ ]]; then
	printf 'invalid expected app version: %s\n' "$EXPECTED_VERSION" >&2
	exit 1
fi

SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNTIME_SMOKE="$SCRIPT_ROOT/smoke_macos_runtime.sh"
MOUNT_ROOT="$(mktemp -d)"
MOUNTED=0

cleanup() {
	local status=$?
	if [[ "$MOUNTED" -eq 1 ]]; then
		hdiutil detach "$MOUNT_ROOT" -quiet -force || status=1
	fi
	rm -rf "$MOUNT_ROOT"
	exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

hdiutil attach "$DMG_PATH" -readonly -nobrowse -mountpoint "$MOUNT_ROOT" -quiet
MOUNTED=1

APP_BUNDLE=""
APP_BUNDLE_COUNT=0
while IFS= read -r app_bundle; do
	APP_BUNDLE="$app_bundle"
	APP_BUNDLE_COUNT=$((APP_BUNDLE_COUNT + 1))
done < <(find "$MOUNT_ROOT" -mindepth 1 -maxdepth 1 -type d -name 'Usagi.app' -print)
if [[ "$APP_BUNDLE_COUNT" -ne 1 ]]; then
	printf 'DMG must contain exactly one top-level Usagi.app; found %s\n' "$APP_BUNDLE_COUNT" >&2
	exit 1
fi
MOUNTED_BINARY="$APP_BUNDLE/Contents/MacOS/usagi"
if [[ ! -x "$MOUNTED_BINARY" ]]; then
	printf 'missing executable in mounted app bundle: %s\n' "$MOUNTED_BINARY" >&2
	exit 1
fi

bash "$RUNTIME_SMOKE" "$MOUNTED_BINARY" "$EXPECTED_VERSION"
