# wlan0-bootstrap 工作交接说明

本文档面向接手维护、部署和联调 `wlan0-bootstrap` 的同事。它说明这个项目能做什么、如何使用、适合哪些场景，以及排查问题时应该看哪些文件。

## 1. 项目定位

`wlan0-bootstrap` 是一个面向 Buildroot AIoT 设备的轻量 Wi-Fi bootstrap 守护进程。它负责让只有一个 Wi-Fi 接口的设备在开机后尽快联网；如果自动联网失败，则切换到 Soft AP 配网模式，让用户通过网页提交目标 Wi-Fi。

它的核心边界很明确：

- 只管理一个 Wi-Fi 接口，默认是 `wlan0`。
- 采用单射频 TDM 流程：STA 扫描/连接和 Soft AP 配网不会并发运行。
- 不是 NetworkManager、connman 或完整网络管理器的替代品。
- 不负责音频、屏幕、LED 等用户提示硬件，只发布状态快照和事件，由外部程序消费。
- Web UI 主要用于配网；联网后只保留只读状态 API 和页面，方便局域网发现和诊断。

当前实现依赖系统工具完成底层网络动作：

- `wpa_supplicant`：STA 扫描、关联和连接控制。
- `hostapd`：启动 Soft AP。
- `dnsmasq`：给 Soft AP 客户端分配地址并处理 captive portal 风格访问。
- `ip`：配置或清理接口 IPv4 地址。
- `udhcpc`：连接上游 Wi-Fi 后获取 DHCP 地址。

## 2. 它可以做什么

### 2.1 开机自动联网

程序启动后会加载已知 Wi-Fi 列表，扫描附近热点。如果扫描结果中存在已保存的 SSID，则按以下规则选择候选网络：

1. 只考虑未禁用的已知网络。
2. 只考虑本轮扫描确实出现的 SSID。
3. 优先级高的排前面。
4. 优先级相同则最近成功连接过的排前面。
5. 仍相同则当前信号更强的排前面。

连接成功后，程序会通过 `udhcpc` 获取 IPv4 地址，并进入 `Connected` 状态。

### 2.2 自动进入 Soft AP 配网

如果没有扫描到已知 Wi-Fi，或者所有已知 Wi-Fi 都连接失败，程序会启动 Soft AP。默认配置下：

- Soft AP SSID：`wlan0-bootstrap`
- Soft AP 密码：以运行时配置为准
- 网关地址：`192.168.4.1/24`
- Web UI 地址：`http://192.168.4.1/`

用户连接该 Soft AP 后，可以打开 Web UI，选择或输入目标 Wi-Fi，并提交密码。提交后程序会停止 Soft AP，切回 STA 模式连接目标 Wi-Fi。连接成功后会保存该 Wi-Fi，后续开机可自动连接。

### 2.3 失败回退

配网时如果目标 Wi-Fi 连接失败，例如密码错误、目标网络不可见、关联超时或 DHCP 失败，程序会：

- 发布失败状态和错误原因。
- 尝试刷新扫描缓存。
- 重新启动 Soft AP。
- 让用户继续通过 Web UI 修改密码或选择其他网络。

### 2.4 已知 Wi-Fi 持久化

连接成功的 Wi-Fi 会保存到：

```text
/data/wlan0-bootstrap/networks.toml
```

示例：

```toml
[[networks]]
ssid = "HomeWiFi"
security = "wpa-psk"
psk = "user-submitted-password"
priority = 0
last_connected_at = 1770000000
disabled = false
```

注意：当前阶段 `psk` 字段保存的是用户提交的密码字符串，或者历史数据里的 64 位 raw PSK。项目没有内置密码加密，也没有调用额外 PSK 派生工具。生产部署时要确保 `/data/wlan0-bootstrap` 的权限符合产品安全要求。

### 2.5 状态快照和事件流

程序会把当前状态写入：

```text
/run/wlan0-bootstrap/status.json
```

同时通过 Unix socket 发布 newline-delimited JSON 事件：

```text
/run/wlan0-bootstrap/events.sock
```

外部音频、屏幕、LED 或业务进程可以读取 `status.json` 获取当前状态，也可以订阅 `events.sock` 获取状态变化事件。

常见状态包括：

- `Booting`
- `Preflight`
- `Scanning`
- `ConnectingKnown`
- `Connected`
- `Reconnecting`
- `ProvisioningApStarting`
- `ProvisioningApRunning`
- `ProvisioningConnecting`
- `Failed`
- `ShuttingDown`

常见错误原因包括：

- `command_missing`
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

### 2.6 mDNS 局域网发现

设备成功连接上游 Wi-Fi 并拿到 IPv4 地址后，程序会继续启动只读 HTTP 服务，并通过 mDNS 发布：

- `<hostname>.local`
- `_http._tcp.local.`

默认 hostname 形如：

```text
wlan-bootstrap-a1b2c3.local
```

后缀来自首次启动生成的 6 位设备 ID，保存在：

```text
/data/wlan0-bootstrap/device-id
```

mDNS 发布失败不会影响 Wi-Fi 主连接，但错误会记录到 `status.json` 的 `discovery` 字段，并通过事件流发布。

## 3. 如何使用

### 3.1 配置文件

默认运行时配置路径：

```text
/etc/wlan0-bootstrap/config.toml
```

也可以启动时显式指定：

```sh
wlan0-bootstrap --config /path/to/config.toml
```

如果默认配置不存在，程序会使用编译进二进制的 fallback 配置，也就是仓库根目录的 `configs.toml`。产品部署不要依赖 fallback，应安装 `/etc/wlan0-bootstrap/config.toml`。

Buildroot 包内的默认配置位于：

```text
buildroot/package/wlan0-bootstrap/config.toml
```

关键配置项：

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

[timeouts]
scan_seconds = 10
connect_seconds = 30
dhcp_seconds = 20
provisioning_idle_seconds = 600

[ownership]
force_takeover = false
wpa_group = "netdev"
wpa_update_config = false

[discovery]
mdns_enabled = true
hostname_prefix = "wlan-bootstrap"
hostname = ""
http_service_enabled = true
```

配置注意事项：

- `[ap].password` 至少 8 位，否则配置校验会失败。
- `[ap].bind_addr` 决定 Web UI 监听地址和端口。常规 Soft AP 使用 `192.168.4.1:80`；ADB API 调试可临时改为 `0.0.0.0:80`。
- `ownership.force_takeover=false` 是默认安全策略。检测到已有 `wpa_supplicant`、`rkwifi_server`、NetworkManager 或 connman 管理 `wlan0` 时，程序会报 `interface_busy` 并退出。
- 只有明确确认本程序可以接管 `wlan0` 时，才考虑设置 `force_takeover=true`。
- `wpa_update_config=false` 时，`wpa_supplicant.conf` 只作为运行时控制入口，已知 Wi-Fi 数据仍由 `networks.toml` 管理。

### 3.2 本地构建

在仓库根目录执行：

```sh
cargo build --release
```

基础校验：

```sh
cargo check
cargo clippy -- -D warnings
```

交叉编译可使用 `cross`，示例：

```sh
cross build --target=armv7-unknown-linux-musleabihf --release
```

具体设备测试流程见：

```text
docs/DEVICE_TEST_FLOW.md
```

### 3.3 Buildroot 集成

项目已经提供 Buildroot package：

```text
buildroot/package/wlan0-bootstrap/
```

主要文件：

- `wlan0-bootstrap.mk`：构建和安装规则。
- `Config.in`：Buildroot 菜单配置入口。
- `config.toml`：安装到目标系统的默认运行配置。
- `S40wlan0-bootstrap`：SysV init 启动脚本。

安装后主要目标路径：

```text
/usr/bin/wlan0-bootstrap
/etc/wlan0-bootstrap/config.toml
/etc/init.d/S40wlan0-bootstrap
```

启动脚本会创建运行目录和数据目录，并以后台进程方式启动：

```sh
/etc/init.d/S40wlan0-bootstrap start
/etc/init.d/S40wlan0-bootstrap stop
/etc/init.d/S40wlan0-bootstrap restart
```

默认日志文件：

```text
/var/log/wlan0-bootstrap.log
```

### 3.4 手动运行

在设备上准备好配置后执行：

```sh
RUST_LOG=debug /usr/bin/wlan0-bootstrap --config /etc/wlan0-bootstrap/config.toml
```

如果是临时调试二进制，也可以放在 `/root/`：

```sh
RUST_LOG=debug /root/wlan0-bootstrap --config /etc/wlan0-bootstrap/config.toml
```

运行前建议确认：

```sh
which wpa_supplicant
which hostapd
which dnsmasq
which ip
which udhcpc
ip link show wlan0
ps -ef | grep -E 'rkwifi|wpa_supplicant|NetworkManager|connmand'
```

### 3.5 Web UI 使用

当设备进入 Soft AP 配网模式后：

1. 手机或电脑连接设备发出的 Soft AP。
2. 打开 `http://192.168.4.1/`。
3. 从扫描列表中选择目标 Wi-Fi，或手动输入 SSID。
4. 输入密码并提交。
5. 等待设备切回 STA 并连接目标网络。

如果连接失败，设备会恢复 Soft AP，页面可继续展示失败原因和新的扫描缓存。

### 3.6 HTTP API

Soft AP 配网期间可用 API：

#### 获取扫描缓存

```http
GET /api/scan
```

返回示例：

```json
[
  {
    "ssid": "HomeWiFi",
    "signal": 88,
    "security": "WPA2"
  }
]
```

说明：单射频设备进入 Soft AP 后不会同时做 STA 扫描，因此这里返回的是进入 AP 前的扫描缓存。

#### 获取状态

```http
GET /api/status
```

返回示例：

```json
{
  "state": "ProvisioningApRunning",
  "ssid": "wlan0-bootstrap",
  "address": "192.168.4.1:80",
  "hostname": "wlan-bootstrap-a1b2c3.local",
  "services": [],
  "discovery": {
    "mdns": "stopped",
    "last_error": null
  },
  "last_error": null,
  "device": null
}
```

#### 提交连接请求

```http
POST /api/connect
Content-Type: application/json

{
  "ssid": "HomeWiFi",
  "password": "wifi-password"
}
```

成功接收请求后返回：

```json
{
  "status": "accepted",
  "message": "connection request accepted"
}
```

注意：`POST /api/connect` 只表示请求已接收。真实连接结果要继续轮询 `/api/status`。

#### 查询后端类型

```http
GET /api/backend_kind
```

返回：

```json
{
  "kind": "single_radio_tdm"
}
```

联网后的只读 HTTP 服务提供：

- `GET /api/status`
- `GET /api/backend_kind`
- 静态页面资源

联网后不再提供 `/api/scan` 和 `/api/connect`。

## 4. 典型使用场景

### 4.1 首次出厂或用户首次开机

设备没有已知 Wi-Fi：

1. 程序启动。
2. 扫描不到可用已知网络。
3. 进入 Soft AP。
4. 用户连接 Soft AP，通过 Web UI 配网。
5. 成功后保存 Wi-Fi，设备接入用户局域网。
6. 后续可通过 mDNS hostname 重新发现设备。

### 4.2 用户换路由器或 Wi-Fi 密码变化

设备已有旧 Wi-Fi，但当前环境连接不上：

1. 程序扫描并尝试旧 Wi-Fi。
2. 连接失败后进入 Soft AP。
3. 用户重新提交新 Wi-Fi 或新密码。
4. 成功后更新 `networks.toml`。

### 4.3 设备移动到多个固定场所

设备保存过多个 Wi-Fi：

1. 开机后扫描当前环境。
2. 只挑选本轮扫描中出现的已知 SSID。
3. 按优先级、最近成功时间和信号排序连接。
4. 当前场所无已知 Wi-Fi 时进入 Soft AP。

### 4.4 生产或售后测试

工程人员可以通过 ADB 或串口运行程序，并使用 API 验证完整配网链路：

- 推送二进制和配置。
- 临时把 `[ap].bind_addr` 改为 `0.0.0.0:80`。
- 使用 `adb forward tcp:18080 tcp:80` 访问 Web API。
- 调用 `/api/connect` 并轮询 `/api/status`。

详细步骤见 `docs/DEVICE_TEST_FLOW.md`。

### 4.5 外部提示程序联动

外部程序不需要解析日志，可以直接消费状态：

- 启动时读取 `/run/wlan0-bootstrap/status.json`。
- 长期监听 `/run/wlan0-bootstrap/events.sock`。
- 根据状态变化播放提示音、显示二维码、闪烁 LED 或上报业务状态。

## 5. 运维和排障入口

### 5.1 常看文件

```text
/etc/wlan0-bootstrap/config.toml
/data/wlan0-bootstrap/networks.toml
/data/wlan0-bootstrap/device-id
/run/wlan0-bootstrap/status.json
/run/wlan0-bootstrap/events.sock
/run/wlan0-bootstrap/wpa_supplicant.conf
/run/wlan0-bootstrap/hostapd.conf
/var/log/wlan0-bootstrap.log
```

### 5.2 常用命令

查看当前状态：

```sh
cat /run/wlan0-bootstrap/status.json
```

查看已知网络：

```sh
cat /data/wlan0-bootstrap/networks.toml
```

查看 Wi-Fi 接口：

```sh
ip addr show wlan0
iw dev 2>/dev/null
```

查看是否有其他进程占用 `wlan0`：

```sh
ps -ef | grep -E 'rkwifi|wpa_supplicant|NetworkManager|connmand|hostapd|dnsmasq'
```

查看事件流：

```sh
nc -U /run/wlan0-bootstrap/events.sock
```

如果系统没有 `nc -U`，可以用产品侧已有的 Unix socket 工具或写一个很小的订阅程序读取 newline-delimited JSON。

### 5.3 常见问题

#### 启动后立刻失败，reason 是 `command_missing`

设备上缺少配置里指定的系统命令。确认这些命令存在并在 `PATH` 中，或者在 `[commands]` 中写绝对路径：

- `wpa_supplicant`
- `hostapd`
- `dnsmasq`
- `ip`
- `udhcpc`

#### reason 是 `interface_missing`

配置里的 `[interface].name` 不存在。确认设备实际 Wi-Fi 接口名，例如 `wlan0`、`wlan1`。

#### reason 是 `interface_busy`

程序检测到 `wlan0` 已由其他进程管理。常见进程包括：

- `rkwifi_server`
- 系统已有 `wpa_supplicant`
- NetworkManager
- connman

默认策略是拒绝接管，避免破坏产品系统网络。处理方式是停止原 owner，或在明确风险后设置 `ownership.force_takeover=true`。

#### Web UI 打不开

按顺序检查：

1. `status.json` 是否处于 `ProvisioningApRunning`。
2. 手机或电脑是否已连接 Soft AP。
3. 客户端是否拿到 `192.168.4.x` 地址。
4. 设备 `wlan0` 是否有 `192.168.4.1/24`。
5. `hostapd` 和 `dnsmasq` 是否仍在运行。
6. `[ap].bind_addr` 是否为 `192.168.4.1:80`。

#### 提交 Wi-Fi 后一直失败

优先看 `status.json` 的 `last_error.reason`：

- `wrong_password` 或 `network_not_found`：检查 SSID 和密码。
- `association_timeout`：检查信号、频段和 AP 兼容性。
- `dhcp_failed`：设备已关联但没拿到地址，检查路由器 DHCP、`udhcpc` 脚本和接口地址。
- `internal_error`：结合 `RUST_LOG=debug` 日志继续看 `wpa_supplicant` 控制命令结果。

#### 连接成功但无法通过 hostname 访问

检查：

- `status.json` 中 `discovery.mdns` 是否为 `Published`。
- `hostname` 是否符合预期。
- 局域网是否允许 mDNS。
- 访问端设备是否支持 `.local` 解析。
- `discovery.last_error` 是否有错误信息。

## 6. 重要限制和维护注意事项

- 当前不支持 AP+STA 并发。进入 Soft AP 后，`/api/scan` 返回的是之前的扫描缓存。
- 当前不支持多 Wi-Fi 接口或多网络后端。
- 当前不做全局网络管理，也不主动清理系统里所有网络进程。
- 当前已知 Wi-Fi 密码以可传给 `wpa_supplicant` 的字符串形式保存，需要由产品侧控制文件权限和数据分区安全。
- `force_takeover=true` 是敏感选项，不能作为默认量产策略。
- mDNS 是发现能力，不是主连接条件；mDNS 失败不代表 Wi-Fi 连接失败。
- Web UI 是配网入口，不应扩展成复杂长期管理后台，除非产品需求重新定义边界。
- 修改状态机、进程清理、接口地址和 `force_takeover` 相关逻辑前，应优先在真实 Buildroot 设备上验证。

## 7. 相关文档

- `README.md`：项目总览和快速入口。
- `AGENT.md`：后续开发者/AI 协作入口说明。
- `docs/DEVICE_TEST_FLOW.md`：真实 Buildroot 设备测试流程。
- `docs/BROADCOM_BCMDHD_NOTES.md`：RK + Broadcom bcmdhd 模式切换问题说明。
- `docs/MDNS_DISCOVERY_BLUEPRINT.md`：mDNS 发现设计说明。
- `docs/REFACTOR_BLUEPRINT.md`：重构蓝图。
- `docs/RUST_CODE_STYLE.md`：Rust 代码规范。
