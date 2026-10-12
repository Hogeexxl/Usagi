#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BINARY="$REPO_ROOT/dist/usagi"
if [[ ! -x "$BINARY" ]]; then
	printf 'missing executable runtime: %s\n' "$BINARY" >&2
	exit 1
fi

TMP_ROOT="$(mktemp -d)"
STUB_DIR="$TMP_ROOT/bin"
OUTSIDE_DIR="$TMP_ROOT/outside"
SMOKE_PATH="$STUB_DIR:/usr/bin:/bin:/usr/sbin:/sbin"
RUNTIME_PID=""
BLOCKER_PID=""

cleanup() {
	local status=$?
	if [[ -n "$RUNTIME_PID" ]]; then
		if runtime_running "$RUNTIME_PID"; then
			kill "$RUNTIME_PID" 2>/dev/null || true
		fi
		wait "$RUNTIME_PID" 2>/dev/null || true
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

mkdir -p "$STUB_DIR" "$OUTSIDE_DIR"
cat > "$STUB_DIR/magpie" <<'SH'
#!/bin/sh
exit 1
SH
chmod +x "$STUB_DIR/magpie"

if ! python3 - <<'PY'
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

write_occupier() {
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
        connection.sendall(
            b"HTTP/1.1 200 OK\r\n"
            b"X-Usagi-App: Other\r\n"
            b"Content-Length: 0\r\n"
            b"Connection: close\r\n\r\n"
        )
PY
}

start_runtime() {
	local smoke_home="$1"
	local log_file="$2"
	mkdir -p "$smoke_home"
	(
		cd "$OUTSIDE_DIR"
		exec env HOME="$smoke_home" PATH="$SMOKE_PATH" USAGI_DISABLE_BROWSER=1 "$BINARY"
	) >"$log_file" 2>&1 &
	RUNTIME_PID=$!
}

runtime_running() {
	local state
	state="$(ps -p "$1" -o stat= 2>/dev/null || true)"
	state="${state//[[:space:]]/}"
	[[ -n "$state" && "$state" != Z* ]]
}

wait_for_descriptor() {
	local descriptor="$1"
	local log_file="$2"
	for _ in $(seq 1 200); do
		if [[ -s "$descriptor" ]]; then
			return 0
		fi
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
	python3 - "$1" "$2" <<'PY'
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
		if ! runtime_running "$RUNTIME_PID"; then
			break
		fi
		sleep 0.1
	done
	if runtime_running "$RUNTIME_PID"; then
		printf 'runtime PID %s did not exit within 10 seconds after service stop\n' "$RUNTIME_PID" >&2
		cat "$log_file" >&2
		return 1
	fi
	if wait "$RUNTIME_PID"; then
		exit_status=0
	else
		exit_status=$?
	fi
	RUNTIME_PID=""
	if [[ "$exit_status" -ne 0 ]]; then
		printf 'runtime exited with status %s\n' "$exit_status" >&2
		cat "$log_file" >&2
		return 1
	fi
}

SMOKE_HOME="$TMP_ROOT/home-production"
SMOKE_LOG="$TMP_ROOT/production.log"
DESCRIPTOR="$SMOKE_HOME/Library/Application Support/Usagi/runtime.json"
start_runtime "$SMOKE_HOME" "$SMOKE_LOG"
wait_for_descriptor "$DESCRIPTOR" "$SMOKE_LOG"
DESCRIPTOR_DATA="$(read_descriptor "$DESCRIPTOR" "$RUNTIME_PID")"
IFS=$'\t' read -r SMOKE_PORT SMOKE_BASE_URL <<< "$DESCRIPTOR_DATA"

python3 - "$SMOKE_BASE_URL" <<'PY'
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

with request("/api/health") as response:
    assert response.status == 204, f"health status was {response.status}"
    assert response.headers.get_all("X-Usagi-App", []) == ["Usagi"], "health marker mismatch"

with request("/") as response:
    page = response.read().decode("utf-8").lower()
    assert response.status == 200, f"static status was {response.status}"
    assert "<!doctype html" in page, "root response is not embedded index.html"

deadline = time.monotonic() + 8
while True:
    with request("/api/status") as response:
        assert response.status == 200, f"status endpoint returned {response.status}"
        state = json.load(response)["runtime_state"]
    if state == "unavailable":
        break
    if time.monotonic() >= deadline:
        raise SystemExit(f"runtime did not become unavailable; last state was {state!r}")
    time.sleep(0.1)

with request("/api/service/stop", "POST") as response:
    result = json.load(response)
    assert response.status == 200, f"stop status was {response.status}"
    assert result == {"state": "stopped"}, f"stop response was {result!r}"
PY
stop_and_wait "$SMOKE_LOG"
if [[ -e "$DESCRIPTOR" ]]; then
	printf 'runtime descriptor was not removed after service stop\n' >&2
	cat "$SMOKE_LOG" >&2
	exit 1
fi
printf 'production runtime smoke passed on port %s\n' "$SMOKE_PORT"

write_occupier
python3 "$TMP_ROOT/occupy.py" >"$TMP_ROOT/occupier.log" 2>&1 &
BLOCKER_PID=$!
for _ in $(seq 1 50); do
	if grep -q '^READY$' "$TMP_ROOT/occupier.log"; then
		break
	fi
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
python3 - "$FALLBACK_BASE_URL" <<'PY'
import sys
import urllib.error
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
request = urllib.request.Request(sys.argv[1] + "/api/health")
try:
    response = opener.open(request, timeout=3)
except urllib.error.HTTPError as error:
    response = error
with response:
    assert response.status == 204, f"fallback health status was {response.status}"
    assert response.headers.get_all("X-Usagi-App", []) == ["Usagi"], "fallback health marker mismatch"
PY
python3 - "$FALLBACK_BASE_URL" <<'PY'
import json
import sys
import urllib.error
import urllib.request

opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
request = urllib.request.Request(sys.argv[1] + "/api/service/stop", method="POST")
try:
    response = opener.open(request, timeout=3)
except urllib.error.HTTPError as error:
    response = error
with response:
    result = json.load(response)
    assert response.status == 200, f"fallback stop status was {response.status}"
    assert result == {"state": "stopped"}, f"fallback stop response was {result!r}"
PY
stop_and_wait "$FALLBACK_LOG"
if [[ -e "$FALLBACK_DESCRIPTOR" ]]; then
	printf 'fallback runtime descriptor was not removed after service stop\n' >&2
	cat "$FALLBACK_LOG" >&2
	exit 1
fi
printf 'dynamic port smoke passed: 37891 occupied, Usagi selected %s\n' "$FALLBACK_PORT"
