#!/bin/sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"
PLUGIN="$ROOT/plugins/usagi"
CACHE_ROOT="${HOME:?HOME must be set}/.codex/plugins/cache/usagi-local/usagi/0.2.6"
SOURCE_BINARY="$PLUGIN/bin/usagi-codex-plugin"
SOURCE_HTML="$PLUGIN/app/usagi-dashboard.html"
CACHE_BINARY="$CACHE_ROOT/bin/usagi-codex-plugin"
CACHE_HTML="$CACHE_ROOT/app/usagi-dashboard.html"

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

for artifact in "$SOURCE_BINARY" "$SOURCE_HTML" "$CACHE_BINARY" "$CACHE_HTML"; do
	if [ ! -f "$artifact" ]; then
		fail "missing installed-copy artifact: $artifact"
	fi
done

if [ ! -x "$SOURCE_BINARY" ]; then
	fail "source plugin binary is not executable: $SOURCE_BINARY"
fi
if [ ! -x "$CACHE_BINARY" ]; then
	fail "installed plugin binary is not executable: $CACHE_BINARY"
fi

if ! command -v shasum >/dev/null 2>&1; then
	fail 'shasum is required to compare installed plugin artifacts'
fi

source_binary_hash=$(shasum -a 256 "$SOURCE_BINARY") || fail 'could not hash source plugin binary'
cache_binary_hash=$(shasum -a 256 "$CACHE_BINARY") || fail 'could not hash installed plugin binary'
source_html_hash=$(shasum -a 256 "$SOURCE_HTML") || fail 'could not hash source Dashboard HTML'
cache_html_hash=$(shasum -a 256 "$CACHE_HTML") || fail 'could not hash installed Dashboard HTML'

source_binary_hash=${source_binary_hash%% *}
cache_binary_hash=${cache_binary_hash%% *}
source_html_hash=${source_html_hash%% *}
cache_html_hash=${cache_html_hash%% *}

if [ "$source_binary_hash" != "$cache_binary_hash" ]; then
	fail 'installed plugin binary SHA-256 does not match the source binary'
fi
if [ "$source_html_hash" != "$cache_html_hash" ]; then
	fail 'installed Dashboard HTML SHA-256 does not match the source HTML'
fi

printf 'PASS: installed plugin binary and Dashboard HTML match the source artifacts\n'
