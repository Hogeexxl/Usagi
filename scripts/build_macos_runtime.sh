#!/usr/bin/env bash
set -euo pipefail

APP_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$APP_ROOT"

npm --prefix "$APP_ROOT/frontend" run build

rm -rf "$APP_ROOT/internal/webui/dist"
mkdir -p "$APP_ROOT/internal/webui/dist"
cp -R "$APP_ROOT/frontend/dist/." "$APP_ROOT/internal/webui/dist/"

mkdir -p "$APP_ROOT/dist"
go build -tags production -o "$APP_ROOT/dist/usagi" ./cmd/usagi
