#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
	printf 'usage: %s <version>\n' "$(basename "$0")" >&2
	exit 1
fi

VERSION="$1"
if [[ ! "$VERSION" =~ ^([0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?|0\.0\.0-ci)$ ]]; then
	printf 'invalid version: %s\n' "$VERSION" >&2
	exit 1
fi

APP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_BINARY="$APP_ROOT/dist/usagi"
BUILD_MANIFEST="$APP_ROOT/dist/usagi.build.json"
INFO_TEMPLATE="$APP_ROOT/packaging/macos/Info.plist"
STAGE_ROOT="$APP_ROOT/dist/macos-dmg-stage"
APP_BUNDLE="$STAGE_ROOT/Usagi.app"
DMG_PATH="$APP_ROOT/dist/Usagi-v${VERSION}-macos-arm64.dmg"

rm -f "$DMG_PATH"
for required in "$SOURCE_BINARY" "$BUILD_MANIFEST" "$INFO_TEMPLATE"; do
	if [[ ! -f "$required" ]]; then
		printf 'missing required packaging input: %s\n' "$required" >&2
		exit 1
	fi
done
if [[ ! -x "$SOURCE_BINARY" ]]; then
	printf 'runtime is not executable: %s\n' "$SOURCE_BINARY" >&2
	exit 1
fi

python3 - "$BUILD_MANIFEST" "$VERSION" "$SOURCE_BINARY" <<'PY'
import hashlib
import json
import sys

manifest_path, expected_version, binary_path = sys.argv[1:]
with open(manifest_path, encoding="utf-8") as manifest_file:
    manifest = json.load(manifest_file)
if manifest.get("version") != expected_version:
    raise SystemExit(
        f"runtime build version {manifest.get('version')!r} does not equal {expected_version!r}"
    )
if manifest.get("goos") != "darwin" or manifest.get("goarch") != "arm64":
    raise SystemExit(f"runtime build target is not darwin/arm64: {manifest!r}")
with open(binary_path, "rb") as binary_file:
    digest = hashlib.sha256(binary_file.read()).hexdigest()
if manifest.get("sha256") != digest:
    raise SystemExit("runtime binary does not match its build manifest")
PY

ARCHITECTURES="$(lipo -archs "$SOURCE_BINARY" | tr -d '[:space:]')"
if [[ "$ARCHITECTURES" != "arm64" ]]; then
	printf 'runtime architecture is %s, want arm64 only\n' "$ARCHITECTURES" >&2
	exit 1
fi

rm -rf "$STAGE_ROOT"
mkdir -p "$APP_BUNDLE/Contents/MacOS" "$APP_BUNDLE/Contents/Resources"
cp "$SOURCE_BINARY" "$APP_BUNDLE/Contents/MacOS/usagi"
chmod 0755 "$APP_BUNDLE/Contents/MacOS/usagi"
sed "s/@VERSION@/$VERSION/g" "$INFO_TEMPLATE" >"$APP_BUNDLE/Contents/Info.plist"
plutil -lint "$APP_BUNDLE/Contents/Info.plist" >/dev/null
ln -s /Applications "$STAGE_ROOT/Applications"

hdiutil create \
	-volname Usagi \
	-srcfolder "$STAGE_ROOT" \
	-ov \
	-format UDZO \
	"$DMG_PATH" >/dev/null

if [[ ! -s "$DMG_PATH" ]]; then
	printf 'DMG packaging did not produce %s\n' "$DMG_PATH" >&2
	exit 1
fi
printf 'created %s\n' "$DMG_PATH"
