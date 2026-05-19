---
name: debug-buildroot-adb
description: Deploy and debug this repository's wlan0-bootstrap binary on an ADB-connected Buildroot device. Use when the user asks to test on the device, enter adb shell, push target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap to /root/, run the daemon manually, inspect logs/status files, or debug Wi-Fi provisioning behavior on real hardware.
---

# Debug Buildroot ADB

## Core Workflow

Use this workflow for real-device debugging of `wlan0-bootstrap` on the Buildroot target.

1. Confirm the workspace is the `wlan0-bootstrap` repository.
2. Ensure the ARMv7 release binary exists:

```bash
test -x ./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap
```

If it is missing, build it before deploying:

```bash
env PATH=/home/hao/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/bin \
  /home/hao/.cargo/bin/cross build \
  --target=armv7-unknown-linux-musleabihf \
  --release
```

3. Check that exactly one ADB device is connected:

```bash
adb devices
```

If no device is listed, stop and report that the device is not connected or ADB is not authorized.

4. Push the binary to the device:

```bash
adb push ./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap /root/
```

5. Enter the device shell:

```bash
adb shell
```

6. On the device, make the binary executable and run focused checks:

```sh
chmod +x /root/wlan0-bootstrap
ls -lh /root/wlan0-bootstrap
/root/wlan0-bootstrap --help
```

## Device-Side Test Flow

For the full project-level test matrix, use `docs/DEVICE_TEST_FLOW.md`. It covers Web UI provisioning, known-network persistence, state snapshots, Unix socket events, failure paths, reconnect behavior, and Buildroot init integration.

Prefer running the daemon manually first so failures are visible in the terminal:

```sh
mkdir -p /data/wlan0-bootstrap /run/wlan0-bootstrap
RUST_LOG=debug /root/wlan0-bootstrap --config /etc/wlan0-bootstrap/config.toml
```

If the installed config is unavailable, inspect fallback behavior or push a known config separately only after confirming with the user.

Useful checks inside `adb shell`:

```sh
ip link show wlan0
which wpa_supplicant
which hostapd
which dnsmasq
which ip
which udhcpc
cat /run/wlan0-bootstrap/status.json
ls -la /run/wlan0-bootstrap /data/wlan0-bootstrap
cat /data/wlan0-bootstrap/networks.toml
```

Use the project boundary: Wi-Fi, AP, DHCP, and interface operations are performed by system tools (`wpa_supplicant`, `hostapd`, `dnsmasq`, `ip`, `udhcpc`). Do not replace them with custom implementations during debugging.

## Automated Web API Provisioning

Use `scripts/adb-provision-api.sh` when the user wants Codex to drive the Web provisioning API instead of manually connecting a phone to the AP and typing credentials.

The script:

1. Pushes the ARMv7 binary to `/root/wlan0-bootstrap`.
2. Pushes a debug config to `/root/wlan0-bootstrap-debug.toml`.
3. Rewrites the debug config `bind_addr` to `0.0.0.0:80`.
4. Starts the daemon through `adb shell`.
5. Runs `adb forward tcp:18080 tcp:80`.
6. Calls `/api/status`, `/api/connect`, and polls until `Connected` or failure.

Example:

```bash
.github/skills/debug-buildroot-adb/scripts/adb-provision-api.sh "HomeWiFi" "wifi-password"
```

This automates the HTTP API path. It does not validate that a phone can associate to the Soft AP at the 802.11 layer; use a real client for that specific AP association test.

If the script reports `interface_busy`, stop the existing device owner such as `rkwifi_server` or run with an explicit debug config only when takeover is intended.

## Failure Handling

- If `adb push` fails, check `adb devices`, device authorization, and available `/root` space.
- If the daemon reports `InterfaceBusy`, inspect `/run/wpa_supplicant/wlan0` and whether another network manager owns `wlan0`.
- If command preflight fails, verify the corresponding system tool exists on the Buildroot image.
- If Soft AP starts but the Web UI is unreachable, check `ip addr show wlan0`, `hostapd`, `dnsmasq`, and `/run/wlan0-bootstrap/status.json`.
- If connection succeeds but no IP appears, check `udhcpc` behavior and `ip -4 -o addr show dev wlan0`.

## Reporting

When reporting results, include:

- Host command used to build and push the binary.
- ADB device visibility.
- Device-side command output that proves the binary runs.
- Current `/run/wlan0-bootstrap/status.json` if available.
- The exact failing system tool command or status reason when debugging failures.
