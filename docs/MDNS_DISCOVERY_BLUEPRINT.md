# mDNS 设备发现蓝图

本文档用于指导后续在 `wlan0-bootstrap` 中实现 mDNS 设备发现能力。目标不是替换现有配网流程，而是补齐 AIoT / Buildroot 设备配网后的“最后一公里”体验：

```text
手机连接设备 Soft AP
用户输入目标 Wi-Fi
设备切换到 STA 并通过 DHCP 获取局域网 IP
Soft AP 消失
用户需要重新找到设备
```

mDNS 要解决的问题是：用户不需要记住 DHCP 分配的 IP，而是通过稳定的局域网名称访问设备。

```text
http://wlan-bootstrap-a1b2.local
```

## 1. 产品目标

实现后，设备联网成功后应同时提供两类发现信息：

- Web UI / 状态 API 显示当前 IP，例如 `http://192.168.1.88`。
- mDNS 发布稳定名称和 HTTP 服务，例如 `wlan-bootstrap-a1b2.local` 和 `_http._tcp.local`。

推荐用户体验：

```text
连接 Soft AP
打开配网页
提交目标 Wi-Fi
设备连接成功并获得 IP
配网页展示：
  设备已联网
  当前地址：http://192.168.1.88
  推荐地址：http://wlan-bootstrap-a1b2.local
以后优先使用 .local 名称访问
```

## 2. 职责边界

mDNS 发现属于 `wlan0-bootstrap` 的联网后状态发布能力，适合放进同一个 daemon：

```text
wlan0-bootstrap
├── Wi-Fi 状态机
├── DHCP 管理
├── Soft AP 配网
├── Web UI / API
├── 状态文件和事件 socket
└── mDNS responder
```

仍需坚持已有项目边界：

- Wi-Fi 连接继续通过 `wpa_supplicant` 和 `wpa-ctrl`。
- Soft AP 继续通过 `hostapd`。
- DHCP 继续通过 `udhcpc` 和系统 `ip` 命令读取地址。
- mDNS 只负责局域网发现，不负责连接 Wi-Fi、不负责 DHCP、不负责路由。
- mDNS 失败不应导致 Wi-Fi 连接失败；它只能影响“可发现性”状态。

## 3. 技术路线

### 3.1 首选：Rust 原生 mDNS responder

后续实现优先选择纯 Rust mDNS 库，例如 `mdns-sd` 这一类支持 responder 和 service publish 的 crate。

选择原则：

- 不依赖 D-Bus。
- 不依赖系统 avahi。
- 可静态链接到 ARMv7 musl 目标。
- 支持发布 hostname。
- 支持发布 `_http._tcp.local` 服务。
- 能显式注册当前 STA IP。
- mDNS worker 可以被 daemon 生命周期管理，连接断开时能停止或更新。

这个方向符合当前项目目标：小体积、少运行时依赖、适合 Buildroot。

### 3.2 备选：调用 avahi-daemon

如果原生 mDNS 在目标设备上不稳定，可以保留 avahi 方案作为产品集成备选：

```text
wlan0-bootstrap
连接成功
设置 hostname
启动或通知 avahi-daemon
```

优点：

- 协议实现成熟。
- 冲突检测、IPv6、缓存、服务发现更完整。

缺点：

- Buildroot 依赖更重。
- 通常需要 avahi、dbus、libdaemon 等额外包。
- 与本项目“轻量单 daemon”方向不完全一致。

因此 avahi 不作为第一阶段默认实现。

## 4. 命名策略

不要使用固定名称 `aiot-device.local`，多个设备在同一局域网会冲突。

推荐默认格式：

```text
wlan-bootstrap-<suffix>.local
```

`suffix` 的候选来源按优先级：

1. 配置文件显式指定。
2. `wlan0` MAC 地址后 4 到 6 位。
3. 设备序列号或 SoC ID 的短 hash。
4. 随机生成并持久化到 `/data/wlan0-bootstrap/device-id`。

示例：

```text
wlan-bootstrap-9629.local
```

命名规则：

- 只使用小写字母、数字和 `-`。
- 长度控制在 63 字符以内。
- 对外展示时带 `.local`。
- mDNS 内部 host name 使用完整结尾点，例如 `wlan-bootstrap-9629.local.`。

## 5. 配置设计

建议在配置中新增：

```toml
[discovery]
mdns_enabled = true
hostname_prefix = "wlan-bootstrap"
hostname = ""
http_service_enabled = true
http_service_type = "_http._tcp.local."
http_service_name = "wlan bootstrap"
```

字段说明：

- `mdns_enabled`：是否启用 mDNS。
- `hostname_prefix`：自动生成 hostname 时使用的前缀。
- `hostname`：用户显式指定时优先生效，不填则自动生成。
- `http_service_enabled`：是否发布 HTTP 服务。
- `http_service_type`：默认发布 `_http._tcp.local.`。
- `http_service_name`：服务实例名称，可包含设备短 ID。

第一阶段不要增加过多配置。TTL、TXT record、IPv6、接口白名单等可以等真实需求出现后再扩展。

## 6. 状态模型

现有状态快照应扩展发现信息，让 Web UI 和外部程序都能知道设备如何被重新访问。

建议在 `status.json` 中增加：

```json
{
  "state": "Connected",
  "ssid": "HomeWiFi",
  "address": "192.168.1.88",
  "hostname": "wlan-bootstrap-9629.local",
  "services": [
    {
      "kind": "http",
      "url": "http://wlan-bootstrap-9629.local",
      "port": 80
    }
  ],
  "discovery": {
    "mdns": "published",
    "last_error": null
  }
}
```

事件 socket 建议增加事件：

```json
{"type":"mdns_published","hostname":"wlan-bootstrap-9629.local","address":"192.168.1.88","port":80}
{"type":"mdns_failed","hostname":"wlan-bootstrap-9629.local","reason":"bind_failed"}
{"type":"mdns_stopped","hostname":"wlan-bootstrap-9629.local"}
```

重要原则：

- `Connected` 仍以 Wi-Fi 和 DHCP 成功为准。
- mDNS 发布失败不能把主状态改成 `Failed`。
- mDNS 错误应放在 discovery 子状态或独立事件里。

## 7. 生命周期

mDNS responder 应跟随 STA 连接生命周期。

### 7.1 启动时机

只在以下条件同时满足时发布：

- 状态进入 `Connected`。
- 已获得 IPv4 地址。
- Web 服务端口已知。
- `discovery.mdns_enabled = true`。

连接成功后流程：

```text
wpa_supplicant COMPLETED
udhcpc 成功
读取 wlan0 IPv4
更新 status.json
启动或更新 mDNS responder
发布 hostname 和 HTTP 服务
通知 Web UI / event socket
```

### 7.2 停止时机

以下场景必须停止或撤销发布：

- 断线进入 `Reconnecting`。
- 回到 Soft AP 配网。
- 程序 shutdown。
- IP 地址变化。
- Web 服务端口变化。

断线后不要继续发布旧 IP，否则用户会访问到错误地址。

### 7.3 IP 变化

如果 DHCP 续租导致 IP 变化，后续实现应重新发布：

```text
old 192.168.1.88
new 192.168.1.103
stop old mdns record
publish new mdns record
update status.json
emit mdns_published
```

第一阶段可以在检测到 `STATUS != COMPLETED` 后停止 mDNS，在重新连接成功后重新发布。

## 8. Web UI 交互

当前 Web UI 通过 `/api/status` 轮询连接状态。实现 mDNS 后，连接成功状态应展示：

```text
设备已联网

当前 IP：
http://192.168.1.88

推荐访问：
http://wlan-bootstrap-9629.local
```

注意事项：

- Android 浏览器对 `.local` 支持不稳定，不能只显示 mDNS 地址。
- 必须同时显示 IP 地址作为兜底。
- 文案应说明“手机需要切回同一个家庭 Wi-Fi 后访问”。
- AP 不要在连接成功瞬间让页面没有机会展示结果；应至少让 `/api/status` 返回成功结果。

后续可以考虑：

- 成功后倒计时提示用户切回家庭 Wi-Fi。
- 如果浏览器仍连在 Soft AP，上游局域网地址可能无法直接访问，这是单射频设备的天然限制。
- 更完整体验可引入 captive portal，但它应作为单独阶段实现。

## 9. Captive Portal 关系

captive portal 与 mDNS 是互补关系：

- captive portal 解决“用户连上 Soft AP 后如何自动打开配网页”。
- mDNS 解决“设备进入家庭 Wi-Fi 后如何再次找到设备”。

第一阶段先实现 mDNS 和成功页 IP 展示。captive portal 涉及 DNS 劫持、HTTP 302、手机连通性检测域名兼容，建议作为后续独立蓝图。

## 10. 平台兼容性

mDNS 的前提是设备和客户端处于同一个二层局域网，并且网络允许组播。

通常表现：

- macOS / iOS：体验最好。
- Linux：依赖 avahi 或 systemd-resolved，通常可用。
- Windows 10/11：多数环境可用。
- Android：不稳定，尤其部分国产系统或浏览器可能无法直接打开 `.local`。

可能失败的环境：

- 企业网禁用组播。
- 酒店 Wi-Fi 开启 AP isolation。
- 某些路由器关闭无线客户端互访。
- Android 热点或特殊移动路由环境。

因此产品策略必须是：

```text
mDNS
+ IP 显示
+ 后续可选 UDP discovery
```

不要把 mDNS 当成唯一发现机制。

## 11. 后续 UDP Discovery 预留

如果未来需要 Android 或 PC 工具稳定发现设备，可以增加 UDP discovery：

客户端广播：

```text
DISCOVER_WLAN_BOOTSTRAP
```

设备响应：

```json
{
  "name": "wlan-bootstrap-9629",
  "hostname": "wlan-bootstrap-9629.local",
  "ip": "192.168.1.88",
  "http_port": 80,
  "state": "Connected"
}
```

这不属于 mDNS 第一阶段实现，但设计状态模型时可以避免把发现能力写死为 mDNS。

## 12. 实现拆分建议

建议分阶段小步实现。

### 阶段 1：状态和配置

- 增加 `[discovery]` 配置。
- 增加 hostname 生成逻辑。
- 扩展 `status.json`，显示 `hostname` 和 HTTP URL。
- Web UI 连接成功时展示 IP 和 hostname。
- 不真正发 mDNS 包。

### 阶段 2：mDNS responder

- 新增 `src/discovery.rs`。
- 封装 `MdnsPublisher`。
- 在 `Connected` 后发布 hostname 和 `_http._tcp.local.`。
- 在断线、回 AP、shutdown 时停止发布。
- mDNS 发布失败只记录 discovery 错误，不影响 Wi-Fi 主状态。

建议接口：

```rust
pub struct DiscoveryInfo {
    pub hostname: String,
    pub address: String,
    pub http_port: u16,
}

pub struct MdnsPublisher {
    // 内部保存 responder handle，确保生命周期可控。
}

impl MdnsPublisher {
    pub fn new(config: DiscoveryConfig) -> Result<Self>;
    pub async fn publish(&mut self, info: DiscoveryInfo) -> Result<()>;
    pub async fn stop(&mut self);
}
```

### 阶段 3：真机验证

- 在 Buildroot 设备上连接 Wi-Fi。
- 确认 `status.json` 显示 `hostname`。
- 在 macOS/Linux 上测试：

```bash
ping wlan-bootstrap-9629.local
curl http://wlan-bootstrap-9629.local/api/status
```

- 验证断线后 mDNS 停止或更新。
- 验证重新 DHCP 后 IP 变化能重新发布。

### 阶段 4：兼容性增强

- 增加 TXT record，例如 model、version、interface。
- 增加 UDP discovery。
- 评估 captive portal。
- 评估 hostname 冲突处理策略。

## 13. 测试清单

主机侧：

- hostname 生成规则单元测试。
- 非法字符清理测试。
- 显式 hostname 优先级测试。
- status JSON 序列化测试。
- mDNS publisher 在重复 publish 时能先停旧记录。

设备侧：

- `Connected` 后能发布 `.local`。
- `curl http://<hostname>.local/api/status` 可用。
- 错误 Wi-Fi 密码不会发布 mDNS。
- DHCP 失败不会发布 mDNS。
- 断线进入 `Reconnecting` 后停止旧发布。
- 回到 Soft AP 后停止旧发布。
- shutdown 后不残留后台 mDNS 线程。

兼容性：

- macOS / iOS 访问 `.local`。
- Linux 使用 `avahi-resolve` 或 `getent hosts`。
- Windows 11 浏览器访问 `.local`。
- Android 上确认失败时仍能通过 IP 兜底。

## 14. 当前结论

本项目后续应优先实现：

```text
Web UI 显示 IP
+ 纯 Rust mDNS responder
+ status.json / events.sock 暴露发现状态
```

不要第一阶段引入 avahi/dbus，也不要把 mDNS 作为唯一发现方式。对于无屏 Buildroot AIoT 设备，稳定产品体验应是：

```text
首次配网靠 Soft AP
联网成功靠 IP 回显
长期访问靠 .local 名称
复杂客户端靠后续 UDP discovery
```
