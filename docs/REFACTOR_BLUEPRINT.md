# wlan0 bootstrap 重构蓝图

## 1. 项目命名

项目新名称：**wlan0 bootstrap**

建议命名约定：

- 显示名称：`wlan0 bootstrap`
- Rust crate/package 名称：`wlan0-bootstrap`
- 二进制名称：`wlan0-bootstrap`
- 仓库目录：`wlan0-bootstrap`

这个名字表达两个核心约束：

- `wlan0`：面向一个主 Wi-Fi 接口，而不是通用网络管理器。
- `bootstrap`：负责设备联网启动阶段和失败恢复阶段，而不是替代完整 NetworkManager。

## 2. 项目定位

`wlan0 bootstrap` 是一个面向 Buildroot AIoT 设备的轻量 Wi-Fi bootstrap 守护进程。

它负责：

- 开机自启后接管单个 Wi-Fi 接口。
- 扫描附近 Wi-Fi。
- 自动连接已经保存过的 Wi-Fi。
- 在无法连接已知 Wi-Fi 时进入 Soft AP 配网模式。
- 通过本地 Web UI 接收新的 Wi-Fi 凭据。
- 连接成功后保存该 Wi-Fi 到已知网络列表。
- 对外发布状态事件，供音频、屏幕、LED 等外部程序展示。

它不负责：

- 管理以太网、蜂窝、VPN、蓝牙或多网卡。
- 作为完整 NetworkManager 替代品。
- AP+STA 并发。
- 云配网。
- 长期复杂网络策略。
- 直接控制音频、屏幕或 LED 硬件。

## 3. 核心硬件假设

目标设备是运行 Buildroot 的 AIoT 设备，通常具备以下特征：

- 只有一个 Wi-Fi 射频。
- 不可靠支持 AP+STA 并发。
- 用户界面有限，甚至没有屏幕。
- rootfs 可能是只读的。
- 持久化分区可能是 `/data`、overlay 或产品自定义路径。
- 用户希望设备开机后自动联网，失败时才进入配网。

因此本项目必须选择唯一流程：

> 先 STA 扫描，再决定连接已知网络或进入 Soft AP 配网。

不要为 AP+STA 并发、双网卡或多 backend 预留抽象。对当前目标设备而言，那会增加复杂度而没有现实收益。

## 4. 唯一运行流程

启动后的完整流程如下：

1. 加载运行时配置。
2. 加载已知 Wi-Fi 列表。
3. 执行 preflight 检查：
   - 权限
   - 接口存在
   - 必要命令存在
   - 数据目录可写
   - 控制 socket 目录可用
4. 启动或连接本程序管理的 `wpa_supplicant`。
5. 将 `wlan0` 置于 STA 扫描状态。
6. 扫描附近 Wi-Fi。
7. 将扫描结果和已知 Wi-Fi 列表匹配。
8. 如果存在可用已知网络：
   - 按优先级、最近成功时间、信号强度排序。
   - 依次尝试连接。
   - 成功后运行 DHCP。
   - 进入 `Connected` 状态并持续监控。
9. 如果没有可用已知网络，或全部连接失败：
   - 停止 STA 连接尝试。
   - 启动 Soft AP。
   - 启动本地 DHCP/DNS。
   - 启动 Web 配网页面。
10. 用户提交新 Wi-Fi 后：
    - 停止 Web/AP/DHCP。
    - 尝试连接新 Wi-Fi。
    - 成功后保存到已知列表并进入 `Connected`。
    - 失败后恢复 Soft AP，并在 UI 中显示失败原因。

## 5. 运行时配置

配置文件不应编译进二进制。建议默认路径：

```text
/etc/wlan0-bootstrap/config.toml
```

命令行应支持：

```bash
wlan0-bootstrap --config /path/to/config.toml
```

编译期配置只能作为 fallback 默认值，不应作为产品部署配置。

配置建议包括：

```toml
[interface]
name = "wlan0"

[paths]
data_dir = "/data/wlan0-bootstrap"
run_dir = "/run/wlan0-bootstrap"
wpa_config = "/run/wlan0-bootstrap/wpa_supplicant.conf"
wpa_ctrl = "/run/wpa_supplicant"
hostapd_config = "/run/wlan0-bootstrap/hostapd.conf"

[ap]
ssid_prefix = "wlan0-bootstrap"
password = "change-me"
gateway_cidr = "192.168.4.1/24"
bind_addr = "192.168.4.1:80"
dhcp_range = "192.168.4.100,192.168.4.200,12h"
channel = 6

[timeouts]
scan_seconds = 10
connect_seconds = 30
dhcp_seconds = 20
provisioning_idle_seconds = 600

[commands]
wpa_supplicant = "wpa_supplicant"
hostapd = "hostapd"
dnsmasq = "dnsmasq"
ip = "ip"
udhcpc = "udhcpc"
```

后续可以增加配置项，但不要为未支持的模式预留过度抽象。

## 6. 已知网络列表

程序应该自己维护已知 Wi-Fi 列表，而不是把 `wpa_supplicant.conf` 当作唯一数据库。

建议默认路径：

```text
/data/wlan0-bootstrap/networks.toml
```

路径必须可配置，因为 Buildroot 设备的持久化分区不统一。

建议结构：

```toml
[[networks]]
ssid = "HomeWiFi"
security = "wpa-psk"
psk = "64-char-derived-psk"
priority = 10
last_connected_at = 1710000000
disabled = false
```

设计原则：

- 只有连接成功后才写入或更新列表。
- 同 SSID 更新已有记录，不无限追加。
- 文件写入必须原子化。
- 避免保存明文密码；WPA-PSK 网络优先保存派生后的 64 位 PSK。
- 支持删除、禁用、更新网络，但这可以后续实现。
- 连接选择应综合优先级、最近成功时间和扫描信号强度。

## 7. 进程所有权

一旦部署到设备上，`wlan0 bootstrap` 应该是 `wlan0` 的 owner。

这意味着：

- 不应同时运行 connman、NetworkManager 或其他脚本管理同一个接口。
- 程序可以启动并管理自己的 `wpa_supplicant`。
- 程序可以启动并管理自己的 `hostapd` 和 `dnsmasq`。
- 程序只能停止自己启动的进程。
- 不应默认执行全局 `killall -9`。

如果产品确实需要强制接管接口，必须通过显式配置开启，例如：

```toml
[ownership]
force_takeover = false
```

默认行为应该是检测到冲突后报错并发布状态事件。

## 8. 状态机

需要把当前隐式流程重构为显式状态机。

建议状态：

```rust
enum WifiState {
    Booting,
    Preflight,
    Scanning,
    ConnectingKnown,
    Connected,
    Reconnecting,
    ProvisioningApStarting,
    ProvisioningApRunning,
    ProvisioningConnecting,
    Failed,
    ShuttingDown,
}
```

状态机原则：

- 所有状态转换必须显式。
- 每次状态变化都更新状态快照。
- 每次状态变化都发布事件。
- 同一时间只能有一个连接尝试。
- 连接失败必须记录结构化原因。
- UI、日志和外部提示都基于同一份状态。

## 9. Web UI 和 API

Web UI 只在配网模式中启动。

建议 API：

- `GET /api/scan`
  - 返回进入 AP 前的扫描结果。
- `GET /api/status`
  - 返回当前状态、正在连接的 SSID、失败原因等。
- `POST /api/connect`
  - 提交新 Wi-Fi 凭据。

`/api/connect` 可以继续快速返回“请求已接收”，但 UI 必须通过 `/api/status` 展示真实连接结果。

连接失败后必须恢复 Soft AP，并让用户看到具体失败原因。

## 10. 状态播报设计

主程序不应该直接播放音频、控制屏幕或控制 LED。

主程序只负责发布状态。外部程序负责消费状态并执行提示动作。

推荐接口：

```text
/run/wlan0-bootstrap/status.json
/run/wlan0-bootstrap/events.sock
```

### 10.1 状态快照

`status.json` 保存当前状态，供后启动的外部程序读取。

示例：

```json
{
  "state": "ProvisioningApRunning",
  "ssid": "wlan0-bootstrap-1234",
  "address": "192.168.4.1",
  "last_error": null
}
```

写入必须原子化，避免读到半截文件。

### 10.2 事件流

`events.sock` 使用 Unix domain socket。事件格式采用 newline-delimited JSON。

示例：

```json
{"type":"state_changed","state":"Scanning"}
{"type":"known_network_found","ssid":"HomeWiFi","signal":86}
{"type":"connecting","ssid":"HomeWiFi"}
{"type":"connected","ssid":"HomeWiFi","ip":"192.168.1.23"}
{"type":"ap_started","ssid":"wlan0-bootstrap-1234","addr":"192.168.4.1"}
{"type":"connection_failed","ssid":"HomeWiFi","reason":"wrong_password"}
```

好处：

- 音频、屏幕、LED 程序可以独立开发和部署。
- 没有订阅者时主程序仍正常运行。
- 不需要主程序启动子进程。
- 比 D-Bus 更适合精简 Buildroot。
- 后续也可以增加一个小 CLI 工具读取状态。

## 11. 错误分类

连接和启动失败需要结构化分类。

建议错误原因：

- `command_missing`
- `permission_denied`
- `interface_missing`
- `interface_busy`
- `scan_failed`
- `no_known_network`
- `network_not_found`
- `wrong_password`
- `association_timeout`
- `dhcp_failed`
- `ap_start_failed`
- `storage_failed`
- `internal_error`

不要只把 `anyhow` 字符串透传到 UI。UI、日志和事件应该使用稳定的错误码加可读消息。

## 12. Buildroot 集成

本项目应该提供一等 Buildroot 集成。

需要补充：

- Buildroot package：
  - `Config.in`
  - `wlan0-bootstrap.mk`
  - 依赖声明
  - install rule
- BusyBox init 脚本：
  - `/etc/init.d/S40wlan0-bootstrap`
- 默认配置：
  - `/etc/wlan0-bootstrap/config.toml`
- 默认数据目录：
  - `/data/wlan0-bootstrap`
- 默认运行目录：
  - `/run/wlan0-bootstrap`

需要文档化的系统依赖：

- `wpa_supplicant`
- `hostapd`
- `dnsmasq`
- `ip` 或 BusyBox 兼容实现
- `udhcpc`

音频、屏幕、LED 不是主程序依赖。它们应该作为独立可选组件订阅状态事件。

## 13. 重构阶段

### Phase 1：定名和配置

- 将项目、crate、二进制重命名为 `wlan0-bootstrap`。
- 将仓库目录改为 `wlan0-bootstrap`。
- 引入运行时配置。
- 移除编译期配置作为唯一配置来源。
- 添加配置校验。

### Phase 2：已知网络和启动连接

- 增加 `networks.toml`。
- 启动时扫描并匹配已知网络。
- 成功连接后更新已知网络。
- 没有可用已知网络时进入 AP 配网。

### Phase 3：状态机和事件发布

- 引入显式 `WifiState`。
- 增加 `status.json`。
- 增加 `events.sock`。
- 移除主程序直接音频播放职责。

### Phase 4：Web 配网闭环

- 增加 `/api/status`。
- 让 UI 展示真实连接状态。
- 连接失败后恢复 AP 并展示失败原因。
- 成功后保存网络并进入 connected 状态。

### Phase 5：Buildroot 产品化

- 增加 Buildroot package。
- 增加 init 脚本。
- 增加默认配置。
- 增加目标设备部署文档。

### Phase 6：可靠性完善

- 移除全局破坏性清理。
- 引入进程 PID 管理。
- 改进错误分类。
- 增加单元测试和硬件测试清单。

## 14. 测试重点

必须覆盖：

- 首次启动，没有已知网络。
- 已有已知网络，自动连接成功。
- 已有多个已知网络，按优先级选择。
- 已知网络存在但密码错误。
- 已知网络扫描不到。
- DHCP 失败。
- 配网提交新 Wi-Fi 成功。
- 配网提交新 Wi-Fi 失败后恢复 AP。
- 断电时写 `networks.toml`。
- `/run/wlan0-bootstrap/status.json` 原子更新。
- 外部事件订阅者后启动时能读取当前状态。

## 15. 最终目标

最终系统应该满足：

- 设备开机后自动联网。
- 用户无需关心是否进入配网模式。
- 配网只在必要时出现。
- 主程序只管理 Wi-Fi bootstrap。
- 状态提示通过外部订阅者完成。
- Buildroot 集成简单、清晰、可复制。
- 代码保持 idiomatic Rust，符合 [RUST_CODE_STYLE.md](RUST_CODE_STYLE.md)。
