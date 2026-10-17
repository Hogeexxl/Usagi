#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FRONTEND="$ROOT/frontend"
PLUGIN="$ROOT/plugins/usagi"

cd "$FRONTEND"
npm run build:codex

mkdir -p "$PLUGIN/app" "$PLUGIN/bin"
cp "$FRONTEND/dist-codex/usagi-dashboard.html" \
   "$PLUGIN/app/usagi-dashboard.html"

cd "$ROOT"
GOOS=darwin GOARCH=arm64 \
  go build -trimpath -o "$PLUGIN/bin/usagi-codex-plugin" ./cmd/usagi-codex-plugin

test -s "$PLUGIN/app/usagi-dashboard.html"
test -x "$PLUGIN/bin/usagi-codex-plugin"
