#!/usr/bin/env bash
set -euo pipefail

release_dmg="${USAGI_RELEASE_DMG:?USAGI_RELEASE_DMG is required}"
repository="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
tag="${USAGI_RELEASE_TAG:-${GITHUB_REF_NAME:?GITHUB_REF_NAME is required}}"
source_root="${USAGI_SOURCE_ROOT:-${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}}"

if [[ ! "$tag" =~ ^v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
  echo "S13 E2E only accepts a stable vX.Y.Z tag, got: $tag" >&2
  exit 1
fi
expected_version="${BASH_REMATCH[1]}"
expected_release_url="https://github.com/${repository}/releases/tag/${tag}"
latest_api="https://api.github.com/repos/${repository}/releases/latest"
low_version="0.0.9"

[[ -d "$source_root" ]] || { echo "S13 source root not found: $source_root" >&2; exit 1; }
source_root="$(cd "$source_root" && pwd)"
if [[ "$release_dmg" != /* ]]; then
  release_dmg="${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}/$release_dmg"
fi

# T-DIST-015 requires a real anonymous public request. Never let an Actions
# token make this check pass when the public endpoint is unavailable.
unset GH_TOKEN GITHUB_TOKEN || true

runtime_base="$RUNNER_TEMP/usagi-s13"
rm -rf "$runtime_base"
mkdir -p "$runtime_base"

attached_devices=()
attached_mounts=()
app_pid=""
cleanup() {
  set +e
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid" 2>/dev/null || true
    wait "$app_pid" 2>/dev/null || true
  fi
  for mount in "${attached_mounts[@]}"; do
    [[ -n "$mount" ]] || continue
    hdiutil detach "$mount" -quiet || hdiutil detach "$mount" -force -quiet || true
  done
  for device in "${attached_devices[@]}"; do
    [[ -n "$device" ]] || continue
    hdiutil detach "$device" -quiet || hdiutil detach "$device" -force -quiet || true
  done
}
trap cleanup EXIT INT TERM

validate_latest_json() {
  local json_path="$1"
  /usr/bin/python3 - "$json_path" "$tag" "$expected_release_url" <<'PY'
import json
import sys
path, expected_tag, expected_url = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    payload = json.load(handle)
assert payload.get("tag_name") == expected_tag, payload.get("tag_name")
assert payload.get("draft") is False, payload.get("draft")
assert payload.get("prerelease") is False, payload.get("prerelease")
assert payload.get("published_at"), payload.get("published_at")
assert payload.get("html_url") == expected_url, payload.get("html_url")
PY
}

latest_json="$runtime_base/latest.json"
public_latest_ok=0
for attempt in $(seq 1 30); do
  status="$(curl --silent --show-error \
    --header 'Accept: application/vnd.github+json' \
    --header 'X-GitHub-Api-Version: 2022-11-28' \
    --header 'User-Agent: Usagi-S13-Gate' \
    --output "$latest_json" --write-out '%{http_code}' \
    "$latest_api" || true)"
  if [[ "$status" == 200 ]] && validate_latest_json "$latest_json"; then
    public_latest_ok=1
    break
  fi
  echo "Public latest release not ready yet (attempt $attempt, HTTP $status)"
  sleep 2
done
if [[ "$public_latest_ok" != 1 ]]; then
  cat "$latest_json" >&2 || true
  echo 'T-DIST-015 failed: public latest Release was not anonymously observable' >&2
  exit 1
fi

echo "Anonymous public latest Release verified: $tag"

wait_for_health() {
  local expected="$1"
  local root="$2"
  local headers="$root/health.headers"
  for _ in $(seq 1 120); do
    status="$(curl --silent --show-error --output /dev/null --dump-header "$headers" --write-out '%{http_code}' http://127.0.0.1:3210/api/health || true)"
    tr -d '\r' < "$headers" > "$root/health.normalized" 2>/dev/null || true
    if [[ "$status" == 204 ]] \
      && grep -Eiq '^X-Usagi-App:[[:space:]]*Usagi$' "$root/health.normalized" \
      && grep -Eiq "^X-Usagi-Version:[[:space:]]*$expected$" "$root/health.normalized"; then
      return 0
    fi
    sleep 0.25
  done
  return 1
}

start_binary() {
  local binary="$1"
  local expected="$2"
  local root="$3"
  local home="$root/home"
  local codex_home="$root/codex-home"
  local tmpdir="$root/tmp"
  local previous_dir="$PWD"
  rm -rf "$root"
  mkdir -p "$home" "$codex_home" "$tmpdir"
  cd "$root"
  HOME="$home" CODEX_HOME="$codex_home" TMPDIR="$tmpdir" USAGI_DISABLE_BROWSER=1 PATH="/usr/bin:/bin" \
    "$binary" >"$root/stdout.log" 2>"$root/stderr.log" &
  app_pid=$!
  cd "$previous_dir"
  if ! wait_for_health "$expected" "$root"; then
    cat "$root/stderr.log" >&2 || true
    echo "Usagi $expected did not become healthy" >&2
    exit 1
  fi
}

stop_binary() {
  if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
    kill "$app_pid"
    wait "$app_pid" 2>/dev/null || true
  fi
  app_pid=""
  for _ in $(seq 1 40); do
    if ! curl --silent --fail http://127.0.0.1:3210/api/health >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.25
  done
  echo 'Usagi did not release port 3210' >&2
  exit 1
}

trigger_and_assert_update() {
  local expected_current="$1"
  local expected_available="$2"
  local out="$3"
  local ok=0
  for attempt in $(seq 1 30); do
    status="$(curl --silent --show-error \
      --request POST \
      --header 'x-usagi-request: 1' \
      --output "$out" --write-out '%{http_code}' \
      http://127.0.0.1:3210/api/update/check || true)"
    if [[ "$status" == 200 ]] && /usr/bin/python3 - "$out" "$expected_current" "$expected_version" "$expected_available" "$expected_release_url" <<'PY'
import json
import sys
path, current, latest, available, release_url = sys.argv[1:]
with open(path, encoding="utf-8") as handle:
    payload = json.load(handle)
assert payload.get("current_version") == current, payload
assert payload.get("latest_version") == latest, payload
assert payload.get("update_available") is (available == "true"), payload
assert payload.get("release_url") == release_url, payload
assert payload.get("checking") is False, payload
assert payload.get("last_checked_at_ms") is not None, payload
PY
    then
      ok=1
      break
    fi
    echo "Usagi update check not ready (current=$expected_current attempt=$attempt HTTP=$status)"
    sleep 2
done
  if [[ "$ok" != 1 ]]; then
    cat "$out" >&2 || true
    echo "T-DIST-015 failed for current version $expected_current" >&2
    exit 1
  fi
}

# Use the actual arm64 DMG produced by this stable Release workflow.
[[ -f "$release_dmg" ]] || { echo "Release DMG not found: $release_dmg" >&2; exit 1; }
attach_plist="$runtime_base/release-attach.plist"
hdiutil attach -plist -nobrowse -readonly "$release_dmg" > "$attach_plist"
/usr/bin/python3 - "$attach_plist" > "$runtime_base/release-entities" <<'PY'
import plistlib
import sys
with open(sys.argv[1], "rb") as handle:
    payload = plistlib.load(handle)
for entity in payload.get("system-entities", []):
    device = entity.get("dev-entry", "")
    mount = entity.get("mount-point", "")
    if device or mount:
        print(f"{device}\t{mount}")
PY
while IFS=$'\t' read -r device mount; do
  [[ -n "$device" ]] && attached_devices+=("$device")
  [[ -n "$mount" ]] && attached_mounts+=("$mount")
done < "$runtime_base/release-entities"
[[ "${#attached_mounts[@]}" -eq 1 ]] || { echo 'Expected one mounted Release DMG volume' >&2; exit 1; }
release_app="$(find "${attached_mounts[0]}" -maxdepth 1 -type d -name '*.app' -print -quit)"
[[ -n "$release_app" ]] || { echo 'Release DMG did not contain an app bundle' >&2; exit 1; }
release_app_copy="$runtime_base/released-Usagi.app"
ditto "$release_app" "$release_app_copy"
formal_root="$runtime_base/formal-$expected_version"
start_binary "$release_app_copy/Contents/MacOS/usagi" "$expected_version" "$formal_root"
trigger_and_assert_update "$expected_version" false "$formal_root/update.json"
stop_binary

echo "Released Usagi $expected_version correctly reports current/latest equality"

# Build an internal lower-version binary from the selected source root. This is
# runner-local only: no commit/tag/Release is created, so public history is not
# polluted by the T-DIST-015 probe.
manifest="$source_root/Cargo.toml"
[[ -f "$manifest" ]] || { echo "Cargo manifest not found: $manifest" >&2; exit 1; }
/usr/bin/python3 - "$manifest" "$low_version" <<'PY'
import re
import sys
path, version = sys.argv[1:]
text = open(path, encoding="utf-8").read()
pattern = r'(?ms)(^\[package\]\s*.*?^version\s*=\s*)"[^"]+"'
updated, count = re.subn(pattern, lambda m: m.group(1) + f'"{version}"', text, count=1)
if count != 1:
    raise SystemExit("failed to rewrite package version for internal S13 build")
open(path, "w", encoding="utf-8").write(updated)
PY
cargo build --manifest-path "$manifest" --release --features embedded-frontend
low_binary="$source_root/target/release/usagi"
file "$low_binary" | grep -Eiq 'arm64|aarch64' || { echo 'Internal low-version build is not arm64' >&2; exit 1; }
low_root="$runtime_base/internal-$low_version"
start_binary "$low_binary" "$low_version" "$low_root"
trigger_and_assert_update "$low_version" true "$low_root/update.json"
stop_binary

echo "Internal Usagi $low_version correctly detects public $expected_version as an update"
echo 'T-DIST-015 PASS'
