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
cd "$APP_ROOT"

npm --prefix "$APP_ROOT/frontend" run build

rm -rf "$APP_ROOT/internal/webui/dist"
mkdir -p "$APP_ROOT/internal/webui/dist"
cp -R "$APP_ROOT/frontend/dist/." "$APP_ROOT/internal/webui/dist/"

mkdir -p "$APP_ROOT/dist"
rm -f "$APP_ROOT/dist/usagi" "$APP_ROOT/dist/usagi.build.json" "$APP_ROOT/dist/usagi.build.json.tmp"
go build \
	-tags production \
	-trimpath \
	-ldflags "-X github.com/Hogeexxl/Usagi/internal/api.AppVersion=$VERSION" \
	-o "$APP_ROOT/dist/usagi" \
	./cmd/usagi

BUILD_MANIFEST="$APP_ROOT/dist/usagi.build.json"
MANIFEST_TEMP="$BUILD_MANIFEST.tmp"
BINARY_SHA256="$(shasum -a 256 "$APP_ROOT/dist/usagi" | awk '{print $1}')"
printf '{"version":"%s","goos":"darwin","goarch":"arm64","sha256":"%s"}\n' \
	"$VERSION" "$BINARY_SHA256" >"$MANIFEST_TEMP"
mv "$MANIFEST_TEMP" "$BUILD_MANIFEST"
