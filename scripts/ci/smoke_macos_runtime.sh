#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="$REPO_ROOT/dist/usagi"
MAGPIE_FIXTURE="$REPO_ROOT/scripts/ci/fixtures/magpie_http.py"
if [[ ! -x "$BINARY" ]]; then
	printf 'missing executable runtime: %s\n' "$BINARY" >&2
	exit 1
fi

TMP_ROOT="$(mktemp -d)"
STUB_DIR="$TMP_ROOT/bin"
OUTSIDE_DIR="$TMP_ROOT/outside"
FIXTURE_DIR="$TMP_ROOT/magpie"
SMOKE_PYTHON="$(command -v python3)"
SMOKE_PATH="$STUB_DIR:/usr/bin:/bin:/usr/sbin:/sbin"
RUNTIME_PID=""
BLOCKER_PID=""
ACTIVE_DESCRIPTOR=""
export USAGI_MAGPIE_FIXTURE_DIR="$FIXTURE_DIR"
export USAGI_MAGPIE_FIXTURE_SCRIPT="$MAGPIE_FIXTURE"
export USAGI_MAGPIE_PYTHON="$SMOKE_PYTHON"
export USAGI_MAGPIE_OFFLINE_FILE="$TMP_ROOT/magpie-offline"

runtime_running() {
	local state
	state="$(ps -p "$1" -o stat= 2>/dev/null | tr -d '[:space:]' || true)"
	[[ -n "$state" && "$state" != Z* ]]
}

cleanup() {
	local status=$?
	if [[ -n "$RUNTIME_PID" ]]; then
		if runtime_running "$RUNTIME_PID" && [[ -s "$ACTIVE_DESCRIPTOR" ]]; then
			"$SMOKE_PYTHON" - "$ACTIVE_DESCRIPTOR" <<'PY' >/dev/null 2>&1 || true
import json
import sys
import urllib.request

try:
    with open(sys.argv[1], encoding="utf-8") as descriptor_file:
        base_url = json.load(descriptor_file)["base_url"]
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    opener.open(urllib.request.Request(base_url + "/api/service/stop", method="POST"), timeout=1).close()
except Exception:
    pass
PY
			for _ in $(seq 1 100); do
				if ! runtime_running "$RUNTIME_PID"; then break; fi
				sleep 0.1
			done
			if runtime_running "$RUNTIME_PID"; then kill "$RUNTIME_PID" 2>/dev/null || true; fi
		fi
		if runtime_running "$RUNTIME_PID"; then kill "$RUNTIME_PID" 2>/dev/null || true; fi
		for _ in $(seq 1 100); do
			if ! runtime_running "$RUNTIME_PID"; then break; fi
			sleep 0.1
		done
		if runtime_running "$RUNTIME_PID"; then kill -9 "$RUNTIME_PID" 2>/dev/null || true; fi
		wait "$RUNTIME_PID" 2>/dev/null || true
		RUNTIME_PID=""
	fi
	if [[ -s "$FIXTURE_DIR/magpie.pid" ]]; then
		local fixture_pid
		fixture_pid="$(cat "$FIXTURE_DIR/magpie.pid")"
		fixture_running() {
			local command
			command="$(ps -p "$fixture_pid" -o command= 2>/dev/null || true)"
			[[ "$command" == *"$MAGPIE_FIXTURE"* ]]
		}
		if fixture_running; then kill "$fixture_pid" 2>/dev/null || true; fi
		for _ in $(seq 1 100); do
			if ! fixture_running; then break; fi
			sleep 0.1
		done
		if fixture_running; then kill -9 "$fixture_pid" 2>/dev/null || true; fi
		for _ in $(seq 1 100); do
			if ! fixture_running; then break; fi
			sleep 0.1
		done
		if fixture_running; then
			printf 'simulated Magpie process %s did not exit before temp cleanup\n' "$fixture_pid" >&2
			status=1
		fi
	fi
	if [[ -n "$BLOCKER_PID" ]] && kill -0 "$BLOCKER_PID" 2>/dev/null; then
		kill "$BLOCKER_PID" 2>/dev/null || true
		wait "$BLOCKER_PID" 2>/dev/null || true
	fi
	rm -rf "$TMP_ROOT"
	exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$STUB_DIR" "$OUTSIDE_DIR" "$FIXTURE_DIR"
cat > "$STUB_DIR/magpie" <<'SH'
#!/bin/sh
case "$1" in
	healthcheck)
		[ ! -e "$USAGI_MAGPIE_OFFLINE_FILE" ]
		;;
	web)
		shift
		exec "$USAGI_MAGPIE_PYTHON" "$USAGI_MAGPIE_FIXTURE_SCRIPT" "$@"
		;;
	*)
		exit 2
		;;
esac
SH
chmod +x "$STUB_DIR/magpie"

if ! "$SMOKE_PYTHON" - <<'PY'
import urllib.error
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
for port in range(37891, 37923):
    request = urllib.request.Request(f"http://127.0.0.1:{port}/api/health")
    try:
        response = opener.open(request, timeout=0.2)
    except (urllib.error.URLError, TimeoutError):
        continue
    with response:
        marker = response.headers.get_all("X-Usagi-App", [])
        if 200 <= response.status < 300 and marker == ["Usagi"]:
            raise SystemExit(f"active Usagi instance already uses port {port}; smoke aborted")
PY
then
	exit 1
fi

start_runtime() {
	local smoke_home="$1"
	local log_file="$2"
	ACTIVE_DESCRIPTOR="$smoke_home/Library/Application Support/Usagi/runtime.json"
	mkdir -p "$smoke_home"
	(cd "$OUTSIDE_DIR" && exec env HOME="$smoke_home" PATH="$SMOKE_PATH" USAGI_DISABLE_BROWSER=1 "$BINARY") >"$log_file" 2>&1 &
	RUNTIME_PID=$!
}

wait_for_descriptor() {
	local descriptor="$1"
	local log_file="$2"
	for _ in $(seq 1 200); do
		if [[ -s "$descriptor" ]]; then return 0; fi
		if ! runtime_running "$RUNTIME_PID"; then
			if wait "$RUNTIME_PID"; then
				RUNTIME_PID=""
				printf 'runtime exited before writing its descriptor (possibly found another live Usagi)\n' >&2
			else
				local status=$?
				RUNTIME_PID=""
				printf 'runtime exited with status %s before writing its descriptor\n' "$status" >&2
			fi
			cat "$log_file" >&2
			return 1
		fi
		sleep 0.1
	done
	printf 'timed out waiting for runtime descriptor: %s\n' "$descriptor" >&2
	cat "$log_file" >&2
	return 1
}

read_descriptor() {
	"$SMOKE_PYTHON" - "$1" "$2" <<'PY'
import json
import sys
from urllib.parse import urlparse

with open(sys.argv[1], encoding="utf-8") as descriptor_file:
    descriptor = json.load(descriptor_file)
url = urlparse(descriptor["base_url"])
port = int(descriptor["port"])
if descriptor.get("schema_version") != 1 or url.hostname != "127.0.0.1":
    raise SystemExit("invalid runtime descriptor")
if descriptor.get("pid") != int(sys.argv[2]):
    raise SystemExit("runtime descriptor PID does not match launched runtime")
if descriptor["base_url"] != f"http://127.0.0.1:{port}" or url.port != port:
    raise SystemExit("descriptor base_url does not exactly match its port")
if not 37891 <= port <= 37922:
    raise SystemExit(f"descriptor port {port} is outside 37891...37922")
print(port, descriptor["base_url"], sep="\t")
PY
}

stop_and_wait() {
	local log_file="$1"
	local exit_status
	for _ in $(seq 1 100); do
		if ! runtime_running "$RUNTIME_PID"; then break; fi
		sleep 0.1
	done
	if runtime_running "$RUNTIME_PID"; then
		printf 'runtime PID %s did not exit within 10 seconds after service stop\n' "$RUNTIME_PID" >&2
		cat "$log_file" >&2
		return 1
	fi
	if wait "$RUNTIME_PID"; then exit_status=0; else exit_status=$?; fi
	RUNTIME_PID=""
	if [[ "$exit_status" -ne 0 ]]; then
		printf 'runtime exited with status %s\n' "$exit_status" >&2
		cat "$log_file" >&2
		return 1
	fi
}

SMOKE_HOME="$TMP_ROOT/home-production"
SMOKE_LOG="$TMP_ROOT/production.log"
DATABASE_DIR="$SMOKE_HOME/Library/Application Support/Usagi"
DESCRIPTOR="$DATABASE_DIR/runtime.json"
mkdir -p "$DATABASE_DIR"
"$SMOKE_PYTHON" - "$REPO_ROOT/internal/database/schema.sql" "$DATABASE_DIR/usagi.sqlite" <<'PY'
import pathlib
import sqlite3
import sys

connection = sqlite3.connect(sys.argv[2])
connection.executescript(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
version = connection.execute("""INSERT INTO price_versions
    (source, fetched_at_ns, effective_at_ns, provider_catalogs_json, maker_catalogs_json)
    VALUES ('https://models.dev/api.json', 0, 0, '{"openai":["openai"]}', '[]')""").lastrowid
connection.execute("""INSERT INTO prices
    (version_id, catalog_id, model_key, canonical_model_id, input, output, cache_read, cache_write, cache_write_1h)
    VALUES (?, 'openai', 'gpt-6-luna', 'openai/gpt-6-luna', 1.0, 2.0, 0.5, 1.5, 2.5)""", (version,))
connection.commit()
assert connection.execute("SELECT value FROM app_meta WHERE key='initial_sync_complete'").fetchone()[0] == "0"
connection.close()
PY
export USAGI_MAGPIE_BLOCK_DAILY=1
start_runtime "$SMOKE_HOME" "$SMOKE_LOG"
wait_for_descriptor "$DESCRIPTOR" "$SMOKE_LOG"
DESCRIPTOR_DATA="$(read_descriptor "$DESCRIPTOR" "$RUNTIME_PID")"
IFS=$'\t' read -r SMOKE_PORT SMOKE_BASE_URL <<< "$DESCRIPTOR_DATA"

"$SMOKE_PYTHON" - "$SMOKE_BASE_URL" "$FIXTURE_DIR" "$DATABASE_DIR/usagi.sqlite" <<'PY'
import json
import pathlib
import sqlite3
import sys
import time
import urllib.error
import urllib.request

base_url, fixture_dir, db_path = sys.argv[1:]
fixture_dir = pathlib.Path(fixture_dir)
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def request(path, method="GET"):
    req = urllib.request.Request(base_url + path, method=method)
    try:
        return opener.open(req, timeout=3)
    except urllib.error.HTTPError as error:
        return error

with request("/api/health") as response:
    assert response.status == 204 and response.headers.get_all("X-Usagi-App", []) == ["Usagi"]
with request("/") as response:
    page = response.read().decode("utf-8").lower()
    assert response.status == 200 and "<!doctype html" in page, "root response is not embedded index.html"
deadline = time.monotonic() + 15
while not (fixture_dir / "sync_waiting").exists():
    if time.monotonic() >= deadline:
        raise SystemExit("simulated Magpie did not reach the blocked first daily sync")
    time.sleep(0.05)
with request("/api/usage/summary?range=today") as response:
    body = response.read().decode("utf-8")
    assert response.status == 503 and "DATA_LOADING" in body, f"first sync response was {response.status} {body}"
(fixture_dir / "release_sync").touch()

deadline = time.monotonic() + 15
summary = None
while time.monotonic() < deadline:
    with request("/api/usage/summary?range=today") as response:
        body = response.read()
        if response.status == 200:
            summary = json.loads(body)
            break
    time.sleep(0.05)
if summary is None:
    raise SystemExit("complete SQLite summary did not become readable after simulated Magpie sync")
if summary["usage"]["input"] != 10 or summary["usage"]["output"] != 5 or summary["data_revision"] <= 0:
    raise SystemExit(f"unexpected synchronized summary: {summary!r}")
with request("/api/usage/sessions?range=today") as response:
    sessions = json.load(response)
    assert response.status == 200 and len(sessions["items"]) == 1, f"session list was {sessions!r}"
    assert sessions["items"][0]["thread"]["title"] == "Temporary runtime smoke session"

connection = sqlite3.connect(db_path)
schema_version = connection.execute("SELECT value FROM app_meta WHERE key='schema_version'").fetchone()[0]
initial_sync = connection.execute("SELECT value FROM app_meta WHERE key='initial_sync_complete'").fetchone()[0]
revision = int(connection.execute("SELECT value FROM app_meta WHERE key='data_revision'").fetchone()[0])
link_count = connection.execute("SELECT COUNT(*) FROM session_links").fetchone()[0]
request_row = connection.execute("SELECT source, provider, model, session FROM requests").fetchone()
price_row = connection.execute("SELECT canonical_model_id, input, output, cache_read, cache_write, cache_write_1h FROM prices").fetchone()
connection.close()
assert schema_version == "1" and initial_sync == "1", f"schema / initial sync = {schema_version}/{initial_sync}"
assert link_count == 0, f"session_links contains {link_count} rows"
assert revision == summary["data_revision"], f"database/API revision differs: {revision}/{summary['data_revision']}"
assert request_row == ("magpie-http", "openai", "gpt-6-luna", "smoke-session"), f"stored HTTP row was {request_row!r}"
assert price_row == ("openai/gpt-6-luna", 1.0, 2.0, 0.5, 1.5, 2.5), f"stored price row was {price_row!r}"
print(f"first sync and SQLite reads passed: schema={schema_version}, revision={revision}")
with request("/api/service/stop", "POST") as response:
    result = json.load(response)
    assert response.status == 200 and result == {"state": "stopped"}, f"stop response was {result!r}"
PY
stop_and_wait "$SMOKE_LOG"
if [[ -e "$DESCRIPTOR" ]]; then
	printf 'runtime descriptor was not removed after service stop\n' >&2
	cat "$SMOKE_LOG" >&2
	exit 1
fi

touch "$USAGI_MAGPIE_OFFLINE_FILE"
unset USAGI_MAGPIE_BLOCK_DAILY
OFFLINE_LOG="$TMP_ROOT/offline.log"
start_runtime "$SMOKE_HOME" "$OFFLINE_LOG"
wait_for_descriptor "$DESCRIPTOR" "$OFFLINE_LOG"
OFFLINE_DATA="$(read_descriptor "$DESCRIPTOR" "$RUNTIME_PID")"
IFS=$'\t' read -r OFFLINE_PORT OFFLINE_BASE_URL <<< "$OFFLINE_DATA"
"$SMOKE_PYTHON" - "$OFFLINE_BASE_URL" <<'PY'
import json
import sys
import time
import urllib.error
import urllib.request

base_url = sys.argv[1]
opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def request(path, method="GET"):
    req = urllib.request.Request(base_url + path, method=method)
    try:
        return opener.open(req, timeout=3)
    except urllib.error.HTTPError as error:
        return error

deadline = time.monotonic() + 8
while True:
    with request("/api/status") as response:
        assert response.status == 200, f"status endpoint returned {response.status}"
        state = json.load(response)["runtime_state"]
    if state == "unavailable":
        break
    if time.monotonic() >= deadline:
        raise SystemExit(f"runtime did not become unavailable; last state was {state!r}")
    time.sleep(0.05)
with request("/api/usage/summary?range=today") as response:
    summary = json.load(response)
    assert response.status == 200, f"offline summary status was {response.status}: {summary!r}"
with request("/api/usage/sessions?range=today") as response:
    sessions = json.load(response)
    assert response.status == 200, f"offline session status was {response.status}: {sessions!r}"
assert summary["usage"]["input"] == 10 and summary["usage"]["output"] == 5, f"offline summary = {summary!r}"
assert sessions["summary"]["data_revision"] == summary["data_revision"]
assert len(sessions["items"]) == 1 and sessions["items"][0]["thread"]["title"] == "Temporary runtime smoke session"
print(f"complete offline SQLite reads passed: revision={summary['data_revision']}")
with request("/api/service/stop", "POST") as response:
    result = json.load(response)
    assert response.status == 200 and result == {"state": "stopped"}, f"stop response was {result!r}"
PY
stop_and_wait "$OFFLINE_LOG"
if [[ -e "$DESCRIPTOR" ]]; then
	printf 'offline runtime descriptor was not removed after service stop\n' >&2
	cat "$OFFLINE_LOG" >&2
	exit 1
fi
printf 'runtime smoke passed: first sync, DATA_LOADING, complete offline reads, schema 1, and empty session_links\n'

cat > "$TMP_ROOT/occupy.py" <<'PY'
import socket

listener = socket.socket()
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
listener.bind(("127.0.0.1", 37891))
listener.listen()
print("READY", flush=True)
while True:
    connection, _ = listener.accept()
    with connection:
        while connection.recv(4096):
            break
        connection.sendall(b"HTTP/1.1 200 OK\r\nX-Usagi-App: Other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
PY
"$SMOKE_PYTHON" "$TMP_ROOT/occupy.py" >"$TMP_ROOT/occupier.log" 2>&1 &
BLOCKER_PID=$!
for _ in $(seq 1 50); do
	if grep -q '^READY$' "$TMP_ROOT/occupier.log"; then break; fi
	if ! kill -0 "$BLOCKER_PID" 2>/dev/null; then
		cat "$TMP_ROOT/occupier.log" >&2
		printf 'cannot reserve 127.0.0.1:37891 for dynamic port smoke\n' >&2
		exit 1
	fi
	sleep 0.1
done
if ! grep -q '^READY$' "$TMP_ROOT/occupier.log"; then
	cat "$TMP_ROOT/occupier.log" >&2
	printf 'timed out reserving 127.0.0.1:37891\n' >&2
	exit 1
fi

FALLBACK_HOME="$TMP_ROOT/home-fallback"
FALLBACK_LOG="$TMP_ROOT/fallback.log"
FALLBACK_DESCRIPTOR="$FALLBACK_HOME/Library/Application Support/Usagi/runtime.json"
start_runtime "$FALLBACK_HOME" "$FALLBACK_LOG"
wait_for_descriptor "$FALLBACK_DESCRIPTOR" "$FALLBACK_LOG"
FALLBACK_DATA="$(read_descriptor "$FALLBACK_DESCRIPTOR" "$RUNTIME_PID")"
IFS=$'\t' read -r FALLBACK_PORT FALLBACK_BASE_URL <<< "$FALLBACK_DATA"
if [[ "$FALLBACK_PORT" -lt 37892 || "$FALLBACK_PORT" -gt 37922 ]]; then
	printf 'fallback port %s is outside 37892...37922\n' "$FALLBACK_PORT" >&2
	exit 1
fi
"$SMOKE_PYTHON" - "$FALLBACK_BASE_URL" <<'PY'
import json
import sys
import urllib.error
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

def request(path, method="GET"):
    req = urllib.request.Request(sys.argv[1] + path, method=method)
    try:
        return opener.open(req, timeout=3)
    except urllib.error.HTTPError as error:
        return error

with request("/api/health") as response:
    assert response.status == 204 and response.headers.get_all("X-Usagi-App", []) == ["Usagi"]
with request("/api/service/stop", "POST") as response:
    result = json.load(response)
    assert response.status == 200 and result == {"state": "stopped"}, f"stop response was {result!r}"
PY
stop_and_wait "$FALLBACK_LOG"
if [[ -e "$FALLBACK_DESCRIPTOR" ]]; then
	printf 'fallback runtime descriptor was not removed after service stop\n' >&2
	cat "$FALLBACK_LOG" >&2
	exit 1
fi
printf 'dynamic port smoke passed: 37891 occupied, Usagi selected %s\n' "$FALLBACK_PORT"

