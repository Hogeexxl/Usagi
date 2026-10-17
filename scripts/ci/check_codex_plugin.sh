#!/bin/sh
set -eu

ROOT="$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)"
PLUGIN="${1:-$ROOT/plugins/usagi}"
EXPECTED_VERSION="${2:-}"
PLUGIN_JSON="$PLUGIN/plugin.json"
MCP_JSON="$PLUGIN/mcp.json"
SIDEBAR_SVG="$PLUGIN/assets/sidebar.svg"
APP_HTML="$PLUGIN/app/usagi-dashboard.html"
PLUGIN_BINARY="$PLUGIN/bin/usagi-codex-plugin"

cd "$ROOT"

if [ "$#" -gt 2 ]; then
	printf 'FAIL: usage: %s [plugin-root [expected-version]]\n' "$0" >&2
	exit 1
fi

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

require_file() {
	if [ ! -f "$1" ]; then
		fail "missing plugin file: $1"
	fi
}

require_line() {
	file="$1"
	line="$2"
	if ! grep -F -x -q "$line" "$file"; then
		fail "expected line not found in $file: $line"
	fi
}

expect_no_matches() {
	label="$1"
	pattern="$2"
	shift 2
	if matches=$(grep -R -n -E \
		--include='*.go' --include='*.ts' --include='*.tsx' --include='*.js' --include='*.mjs' --include='*.jsx' \
		--include='*.py' --include='*.sh' --include='*.yml' --include='*.yaml' --include='*.json' --include='*.html' --include='*.css' \
		--exclude='*_test.go' --exclude='*.test.*' --exclude='check_codex_plugin.sh' \
		--exclude-dir=node_modules --exclude-dir=dist --exclude-dir=dist-codex \
		--exclude-dir=tests --exclude-dir=fixtures --exclude-dir=test-fixtures --exclude-dir=testdata \
		-e "$pattern" "$@" 2>&1); then
		printf 'FAIL: %s found in active source:\n%s\n' "$label" "$matches" >&2
		exit 1
	else
		status=$?
		if [ "$status" -ne 1 ]; then
			printf 'FAIL: could not scan active source for %s (grep exit %s):\n%s\n' "$label" "$status" "$matches" >&2
			exit 1
		fi
	fi
}

require_file "$PLUGIN_JSON"
require_file "$MCP_JSON"
require_file "$SIDEBAR_SVG"
if [ ! -s "$APP_HTML" ]; then
	fail "plugin Dashboard HTML is missing or empty: $APP_HTML"
fi
if [ ! -x "$PLUGIN_BINARY" ]; then
	fail "plugin binary is missing or not executable: $PLUGIN_BINARY"
fi

require_line "$MCP_JSON" '      "command": "./bin/usagi-codex-plugin",'
require_line "$MCP_JSON" '      "cwd": "${PLUGIN_ROOT}"'
require_line "$PLUGIN_JSON" '  "description": "Local AI usage dashboard for Codex Desktop.",'
require_line "$PLUGIN_JSON" '        "logo": "./assets/sidebar.svg"'

if [ -n "$EXPECTED_VERSION" ]; then
	if ! printf '%s\n' "$EXPECTED_VERSION" | grep -E -x -q '^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$'; then
		fail "invalid expected plugin version: $EXPECTED_VERSION"
	fi
	plugin_version=$(
		sed -n 's/^[[:space:]]*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PLUGIN_JSON" |
			head -n 1
	)
	if [ "$plugin_version" != "$EXPECTED_VERSION" ]; then
		fail "plugin.json version is '$plugin_version', want '$EXPECTED_VERSION'"
	fi
fi

if ! grep -F -q '@modelcontextprotocol/ext-apps' "$APP_HTML"; then
	fail 'plugin Dashboard HTML does not contain the MCP App bundle'
fi
if ! grep -F -q 'id="root"' "$APP_HTML"; then
	fail 'plugin Dashboard HTML does not contain the application root'
fi
if grep -i -F -q '<iframe' "$APP_HTML"; then
	fail 'plugin Dashboard HTML contains an iframe'
fi
if grep -i -E -q 'http://127\.0\.0\.1|http://localhost' "$APP_HTML"; then
	fail 'plugin Dashboard HTML contains a localhost URL'
fi
if grep -i -E -q \
	"<script[^>]*[[:space:]]src[[:space:]]*=[[:space:]]*['\"]?([[:alpha:]][[:alnum:].+-]*:)?//|<link[^>]*[[:space:]]href[[:space:]]*=[[:space:]]*['\"]?([[:alpha:]][[:alnum:].+-]*:)?//" \
	"$APP_HTML"; then
	fail 'plugin Dashboard HTML references an external script or stylesheet URL'
fi
if ! grep -F -q '.dashboard-shell' "$APP_HTML"; then
	fail 'plugin Dashboard HTML does not contain the shared dashboard shell'
fi

expect_no_matches 'old frameDomains localhost contract' 'frameDomains' \
	cmd internal frontend/src frontend/usagi-dashboard.html frontend/vite.codex.config.ts scripts .github/workflows/ci.yml
expect_no_matches 'old hand-written ui/initialize protocol' 'ui/initialize' \
	cmd internal frontend/src frontend/usagi-dashboard.html frontend/vite.codex.config.ts scripts .github/workflows/ci.yml
expect_no_matches 'Codex EventSource (/api/events) path' 'EventSource|/api/events' \
	frontend/src/codex frontend/src/data/mcpUsagiClient.ts frontend/src/data/mcpServiceClient.ts

printf 'PASS: Codex plugin package, HTML, metadata, and active-source residue checks for %s\n' "$PLUGIN"
