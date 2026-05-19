# wlan0 bootstrap 设备测试流程

本文档用于在真实 Buildroot 设备上验证 `wlan0-bootstrap` 的完整功能。测试目标不是只确认二进制能运行，而是覆盖核心产品行为：

- Web UI / API 配网是否可用。
- 成功连接后是否持久化已知 Wi-Fi。
- 重启后是否能优先连接已知 Wi-Fi。
- 状态快照和 Unix socket 事件流是否能供外部程序消费。
- 失败路径是否能回到 Soft AP 配网。
- Buildroot 配置和 init 脚本是否可部署。

## 0. 测试前提

主机侧需要：

```bash
adb
curl
python3
cross
```

设备侧需要：

```sh
wpa_supplicant
hostapd
dnsmasq
ip
udhcpc
```

目标 Wi-Fi 要求：

- 准备一个可连接外网或局域网的 2.4GHz Wi-Fi。
- 记录 SSID 和密码。
- 确认测试设备所在位置能扫描到该 SSID。

注意：如果设备已有 `rkwifi_server`、系统 `wpa_supplicant`、NetworkManager、connman 或其他脚本管理 `wlan0`，默认配置下程序应报 `interface_busy` 并拒绝接管。完整接管测试前必须停止原有 owner，或在明确知道风险时使用 `ownership.force_takeover=true`。

## 1. 构建与部署

目标：确认 ARMv7 release 产物能在设备上运行。

主机执行：

```bash
env PATH=/home/hao/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/bin \
  /home/hao/.cargo/bin/cross build \
  --target=armv7-unknown-linux-musleabihf \
  --release

adb devices
adb push ./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap /root/
adb shell chmod +x /root/wlan0-bootstrap
adb shell /root/wlan0-bootstrap --help
```

通过标准：

- `adb devices` 中有且只有一个 `device`。
- `/root/wlan0-bootstrap --help` 输出用法。
- 二进制文件类型为 ARMv7 静态可执行文件。

主机可确认：

```bash
file target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap
```

## 2. 配置安装

目标：确认程序读取的是运行时配置，而不是无意使用 fallback。

主机执行：

```bash
adb shell mkdir -p /etc/wlan0-bootstrap
adb push ./buildroot/package/wlan0-bootstrap/config.toml /etc/wlan0-bootstrap/config.toml
adb shell sed -n '1,120p' /etc/wlan0-bootstrap/config.toml
```

通过标准：

- `/etc/wlan0-bootstrap/config.toml` 存在。
- `[ap].password` 至少 8 位。
- `[commands]` 中系统命令路径在设备上可找到。

设备确认：

```sh
which wpa_supplicant
which hostapd
which dnsmasq
which ip
which udhcpc
```

## 3. 接口 Owner 保护

目标：确认程序不会误接管已有网络管理进程。

设备先确认 owner：

```sh
ps -ef | grep -E 'rkwifi|wpa_supplicant|NetworkManager|connmand|udhcpc'
```

如果已有 `rkwifi_server` 或系统 `wpa_supplicant` 管理 `wlan0`，执行：

```sh
RUST_LOG=debug /root/wlan0-bootstrap
cat /run/wlan0-bootstrap/status.json
```

通过标准：

- 程序退出。
- `status.json` 中：

```json
{
  "state": "Failed",
  "last_error": {
    "reason": "interface_busy"
  }
}
```

- 不应启动第二套 `/run/wlan0-bootstrap` 的 `wpa_supplicant`、`hostapd`、`dnsmasq`。
- `wlan0` 不应残留 `192.168.4.1/24`。

## 4. Web UI / API 配网

目标：确认 Soft AP 配网路径完整可用。

测试方式有两种。

### 4.1 手动 Web UI 测试

先停止已有 owner，再启动程序：

```sh
RUST_LOG=debug /root/wlan0-bootstrap
```

手机或电脑连接 Soft AP：

```text
SSID: wlan0-bootstrap
Password: 配置文件中的 [ap].password
```

浏览器访问：

```text
http://192.168.4.1/
```

选择目标 Wi-Fi，输入密码。

通过标准：

- 手机能连接 Soft AP 并获得 `192.168.4.x` 地址。
- 页面能显示扫描列表。
- 提交后程序停止 AP，连接目标 Wi-Fi。
- 设备通过 `udhcpc` 获得目标网络 IP。
- `/run/wlan0-bootstrap/status.json` 进入 `Connected`。

### 4.2 ADB 自动 API 测试

这个测试不验证真实无线客户端能否关联 Soft AP；它验证同一套 Web API 配网逻辑，适合自动化回归。

主机执行：

```bash
.github/skills/debug-buildroot-adb/scripts/adb-provision-api.sh "目标SSID" "目标WiFi密码"
```

脚本行为：

- 推送二进制。
- 推送调试配置。
- 将 `bind_addr` 改为 `0.0.0.0:80`。
- 通过 `adb forward tcp:18080 tcp:80` 暴露设备 Web API。
- 调用 `/api/status`、`/api/connect`。
- 轮询直到 `Connected` 或失败。

通过标准：

- `/api/status` 可访问。
- `/api/connect` 返回 `accepted`。
- 最终状态为 `Connected`。
- 调试日志中能看到 `udhcpc` 获取 IP。

如果需要连接成功后继续保留 daemon 观察状态：

```bash
STOP_AFTER_CONNECTED=0 .github/skills/debug-buildroot-adb/scripts/adb-provision-api.sh "目标SSID" "目标WiFi密码"
```

## 5. 已知 Wi-Fi 持久化

目标：确认连接成功后会写入已知网络，并且重启后能自动连接。

第一次配网成功后检查：

```sh
cat /data/wlan0-bootstrap/networks.toml
```

通过标准：

- 文件存在。
- 有一条 `[[networks]]`。
- `ssid` 是刚连接成功的目标 Wi-Fi。
- `disabled = false`。
- `last_connected_at` 大于 0。

示例：

```toml
[[networks]]
ssid = "HomeWiFi"
security = "wpa-psk"
psk = "用户提交的密码字符串"
priority = 0
last_connected_at = 1770000000
disabled = false
```

当前阶段按项目决策保存可直接交给 `wpa_supplicant` 的密码字符串；不调用额外 PSK 派生工具，也不在程序内实现 PSK 派生。

重启自动连接测试：

```sh
killall wlan0-bootstrap
ip addr flush dev wlan0
RUST_LOG=debug /root/wlan0-bootstrap
```

通过标准：

- 程序启动后先扫描。
- 扫描到已知 SSID 后进入 `ConnectingKnown`。
- 不进入 Soft AP。
- DHCP 成功后进入 `Connected`。
- `last_connected_at` 被更新。

## 6. 状态快照测试

目标：确认外部程序可通过文件读取当前状态。

启动程序后，在关键阶段读取：

```sh
cat /run/wlan0-bootstrap/status.json
```

必须覆盖这些状态：

- `Preflight`
- `Scanning`
- `ProvisioningApStarting`
- `ProvisioningApRunning`
- `ProvisioningConnecting`
- `Connected`
- `Failed`

通过标准：

- JSON 可解析。
- `state` 与当前流程一致。
- `ssid` 在 AP 阶段为 AP SSID，在连接阶段为目标 SSID。
- `address` 在 AP 阶段为 Web 绑定地址，在连接成功后为设备 IP。
- 失败时 `last_error.reason` 为结构化枚举，例如 `interface_busy`、`dhcp_failed`、`ap_start_failed`。

主机可通过 ADB 拉取验证：

```bash
adb shell cat /run/wlan0-bootstrap/status.json | python3 -m json.tool
```

## 7. Unix Socket 事件流测试

目标：确认音频、屏幕、LED 等外部程序能订阅事件。

事件 socket：

```text
/run/wlan0-bootstrap/events.sock
```

设备上如果有 `socat`：

```sh
socat - UNIX-CONNECT:/run/wlan0-bootstrap/events.sock
```

设备上如果 BusyBox `nc` 支持 Unix socket：

```sh
nc -U /run/wlan0-bootstrap/events.sock
```

然后在另一个 shell 中触发状态变化，例如启动程序、提交 Wi-Fi、输入错误密码。

通过标准：

- 每行是一个 JSON 事件。
- 状态变化会输出 `state_changed`。
- 连接失败会输出 `connection_failed`。

示例事件：

```json
{"type":"state_changed","state":"ProvisioningApRunning","ssid":"wlan0-bootstrap","address":"192.168.4.1:80"}
{"type":"connection_failed","ssid":"HomeWiFi","reason":"wrong_password","message":"network_not_found_or_wrong_password"}
```

如果设备没有 `socat` 或 `nc -U`，需要临时放置一个小型 Unix socket 订阅工具；没有订阅工具时至少必须验证 `status.json` 快照。

## 8. 失败路径测试

### 8.1 错误 Wi-Fi 密码

通过 Web API 提交错误密码。

通过标准：

- 状态先进入 `ProvisioningConnecting`。
- 连接失败后进入 `Failed`，`last_error.reason` 为 `wrong_password` 或 `network_not_found`。
- Soft AP 重新启动。
- Web UI 可继续访问。

### 8.2 目标 Wi-Fi 不存在

提交一个不存在的 SSID。

通过标准：

- 连接失败。
- 状态有结构化失败原因。
- Soft AP 恢复。

### 8.3 DHCP 失败

准备一个能关联但不给 DHCP 的网络，或临时替换 `udhcpc` 命令为失败脚本。

通过标准：

- 状态为 `Failed`。
- `last_error.reason = "dhcp_failed"`。
- Soft AP 恢复。

### 8.4 AP 启动失败

让 `hostapd` 无法启动，例如设备驱动不支持 AP 模式或接口被占用。

通过标准：

- 程序不能把状态误报为 `ProvisioningApRunning`。
- `last_error.reason = "ap_start_failed"`。
- 不应残留 `dnsmasq` 或 `192.168.4.1/24`。

## 9. 断线重连测试

目标：确认连接成功后 daemon 不退出，而是监控连接。

连接成功后执行：

```sh
ip link set wlan0 down
sleep 3
ip link set wlan0 up
cat /run/wlan0-bootstrap/status.json
```

通过标准：

- 程序检测到不再是 `COMPLETED`。
- 状态进入 `Reconnecting`。
- 主循环重新扫描。
- 如果已知网络可用，应再次连接成功。

说明：当前设计是守护进程，成功连接后不会自动退出。调试脚本可以在看到 `Connected` 后主动 kill 进程，但产品运行不应退出。

## 10. Buildroot 集成测试

目标：确认 package、默认配置和 init 脚本可部署。

检查目标 rootfs：

```sh
ls -l /usr/bin/wlan0-bootstrap
ls -l /etc/wlan0-bootstrap/config.toml
ls -l /etc/init.d/S40wlan0-bootstrap
```

运行 init 脚本：

```sh
/etc/init.d/S40wlan0-bootstrap start
cat /var/log/wlan0-bootstrap.log
cat /run/wlan0-bootstrap/status.json
/etc/init.d/S40wlan0-bootstrap stop
```

通过标准：

- start 后有 daemon 进程。
- stop 后 daemon 退出。
- 日志和状态文件可读。
- 不依赖仓库目录或 `/root/wlan0-bootstrap`。

## 11. 回归检查清单

每次修改网络流程后至少执行：

```bash
cargo fmt --check
cargo check
cargo clippy -- -D warnings
cargo test
env PATH=/home/hao/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/bin \
  /home/hao/.cargo/bin/cross build \
  --target=armv7-unknown-linux-musleabihf \
  --release
```

设备侧至少执行：

```bash
adb push ./target/armv7-unknown-linux-musleabihf/release/wlan0-bootstrap /root/
adb shell /root/wlan0-bootstrap --help
adb shell cat /run/wlan0-bootstrap/status.json
```

完整设备回归至少覆盖：

- `interface_busy` 保护。
- Web API 自动配网。
- 手动手机连接 Soft AP。
- 已知网络持久化。
- 重启后自动连接已知网络。
- 状态快照。
- 事件 socket。
- 错误密码失败回退。
- AP 启动失败不误报。
- Buildroot init 脚本。

