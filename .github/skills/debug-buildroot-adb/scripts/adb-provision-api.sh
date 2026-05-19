#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  adb-provision-api.sh <target-ssid> [target-password]

Environment:
  HOST_PORT=18080                         Host port forwarded to device port 80.
  LOCAL_BIN=./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap
  BASE_CONFIG=./buildroot/package/wlan0-bootstrap/config.toml
  REMOTE_BIN=/root/wlan0-bootstrap
  REMOTE_CONFIG=/root/wlan0-bootstrap-debug.toml
  STOP_AFTER_CONNECTED=1                  Kill the debug daemon after Connected.

This script deploys wlan0-bootstrap, starts it with a debug config whose
bind_addr is 0.0.0.0:80, forwards host TCP traffic through adb, then drives the
Web provisioning API from the host.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || $# -lt 1 ]]; then
  usage
  exit 0
fi

ssid="$1"
password="${2:-}"

host_port="${HOST_PORT:-18080}"
local_bin="${LOCAL_BIN:-./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap}"
base_config="${BASE_CONFIG:-./buildroot/package/wlan0-bootstrap/config.toml}"
remote_bin="${REMOTE_BIN:-/root/wlan0-bootstrap}"
remote_config="${REMOTE_CONFIG:-/root/wlan0-bootstrap-debug.toml}"
stop_after_connected="${STOP_AFTER_CONNECTED:-1}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "missing required command: $1" >&2
    exit 1
  fi
}

require_cmd adb
require_cmd curl
require_cmd python3

if [[ ! -x "$local_bin" ]]; then
  echo "missing executable: $local_bin" >&2
  echo "build it first with cross build --target=armv7-unknown-linux-musleabihf --release" >&2
  exit 1
fi

if [[ ! -f "$base_config" ]]; then
  echo "missing base config: $base_config" >&2
  exit 1
fi

echo "Checking adb device..."
adb devices

echo "Pushing binary..."
adb push "$local_bin" "$remote_bin"
adb shell "chmod +x '$remote_bin'"

tmp_config="$(mktemp)"
trap 'rm -f "$tmp_config"' EXIT

python3 - "$base_config" "$tmp_config" <<'PY'
from pathlib import Path
import sys

source = Path(sys.argv[1])
target = Path(sys.argv[2])
content = source.read_text()
content = content.replace('bind_addr = "192.168.4.1:80"', 'bind_addr = "0.0.0.0:80"')
target.write_text(content)
PY

echo "Pushing debug config..."
adb push "$tmp_config" "$remote_config"

echo "Starting debug daemon..."
adb shell "rm -f /tmp/wlan0-bootstrap-debug.log /tmp/wlan0-bootstrap-debug.pid; RUST_LOG=debug '$remote_bin' --config '$remote_config' >/tmp/wlan0-bootstrap-debug.log 2>&1 & echo \$! >/tmp/wlan0-bootstrap-debug.pid"

echo "Forwarding host port ${host_port} to device port 80..."
adb forward --remove "tcp:${host_port}" >/dev/null 2>&1 || true
adb forward "tcp:${host_port}" tcp:80

api="http://127.0.0.1:${host_port}"

echo "Waiting for provisioning API..."
for _ in $(seq 1 60); do
  if curl -fsS "${api}/api/status" >/tmp/wlan0-bootstrap-status.json 2>/dev/null; then
    cat /tmp/wlan0-bootstrap-status.json
    echo
    break
  fi

  if adb shell "test -s /tmp/wlan0-bootstrap-debug.log" >/dev/null 2>&1; then
    if adb shell "tail -n 20 /tmp/wlan0-bootstrap-debug.log | grep -q '^Error:'"; then
      adb shell "cat /tmp/wlan0-bootstrap-debug.log"
      exit 1
    fi
  fi

  sleep 1
done

payload="$(python3 - "$ssid" "$password" <<'PY'
import json
import sys

print(json.dumps({"ssid": sys.argv[1], "password": sys.argv[2]}))
PY
)"

echo "Submitting Wi-Fi credentials for SSID: ${ssid}"
curl -fsS -H "Content-Type: application/json" -d "$payload" "${api}/api/connect"
echo

echo "Polling status..."
for _ in $(seq 1 90); do
  status="$(curl -fsS "${api}/api/status")"
  echo "$status"

  state="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("state",""))' <<<"$status")"
  if [[ "$state" == "Connected" ]]; then
    echo "Connected."
    if [[ "$stop_after_connected" == "1" ]]; then
      echo "Stopping debug daemon after Connected..."
      adb shell "kill \$(cat /tmp/wlan0-bootstrap-debug.pid) 2>/dev/null || true"
    fi
    exit 0
  fi

  if [[ "$state" == "Failed" ]]; then
    reason="$(python3 -c 'import json,sys; data=json.load(sys.stdin); err=data.get("last_error") or {}; print(err.get("reason",""))' <<<"$status")"
    if [[ "$reason" != "no_known_network" && "$reason" != "scan_failed" ]]; then
      echo "Provisioning failed: ${reason}" >&2
      adb shell "tail -n 80 /tmp/wlan0-bootstrap-debug.log" || true
      exit 1
    fi
  fi

  sleep 1
done

echo "Timed out waiting for Connected. Leaving daemon running for inspection." >&2
adb shell "tail -n 80 /tmp/wlan0-bootstrap-debug.log" || true
exit 1
