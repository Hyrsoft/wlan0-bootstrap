# wlan0 bootstrap

`wlan0 bootstrap` 是一个面向 Buildroot AIoT 设备的轻量 Wi-Fi bootstrap 守护进程。

它只管理一个 Wi-Fi 接口，默认是 `wlan0`。核心流程固定为单射频 TDM：

1. 开机自启。
2. 加载运行时配置。
3. 加载已知 Wi-Fi 列表。
4. 启动 `wpa_supplicant`。
5. 先扫描附近 Wi-Fi。
6. 如果扫描到已知网络，自动连接并运行 DHCP。
7. 如果没有可用已知网络，或全部连接失败，进入 Soft AP 配网模式。
8. 用户通过 Web UI 提交新的 Wi-Fi。
9. 连接成功后保存到已知网络列表。
10. 状态通过本地文件和 Unix socket 事件流发布给外部程序。

本项目不是完整 NetworkManager 替代品，不支持 AP+STA 并发，不设计双网卡或多后端模式。

## 当前依赖

核心 Wi-Fi/AP/DHCP 功能仍调用系统工具：

- `wpa_supplicant`
- `hostapd`
- `dnsmasq`
- `ip`
- `udhcpc`

后续可以逐步替换部分实现，但当前重构阶段优先稳定外围结构。

## 配置

默认运行时配置路径：

```text
/etc/wlan0-bootstrap/config.toml
```

也可以通过命令行指定：

```bash
wlan0-bootstrap --config /path/to/config.toml
```

如果默认配置不存在，程序会使用编译进二进制的 fallback 配置，也就是仓库里的 `configs.toml`。

## 持久化数据

默认已知网络列表：

```text
/data/wlan0-bootstrap/networks.toml
```

默认运行时状态：

```text
/run/wlan0-bootstrap/status.json
/run/wlan0-bootstrap/events.sock
```

`status.json` 是当前状态快照。`events.sock` 是 newline-delimited JSON 事件流，供音频、屏幕、LED 等外部程序订阅。

主程序不再直接播放音频或控制提示硬件。

## Web 配网

Web UI 只在 Soft AP 配网模式中启动。

主要 API：

- `GET /api/scan`
- `GET /api/status`
- `POST /api/connect`

`POST /api/connect` 只表示请求已接收，真实连接结果通过 `/api/status` 查询。

## 构建

```bash
cargo build --release
```

交叉编译仍可使用 `cross`：

```bash
cross build \
  --target=armv7-unknown-linux-musleabihf \
  --release \
  --config 'target.armv7-unknown-linux-musleabihf.rustflags=["-C", "target-feature=+crt-static"]'
```

## 开发文档

- [AGENT.md](AGENT.md)：后续 AI/开发者入口说明。
- [docs/REFACTOR_BLUEPRINT.md](docs/REFACTOR_BLUEPRINT.md)：重构蓝图。
- [docs/RUST_CODE_STYLE.md](docs/RUST_CODE_STYLE.md)：Rust 代码规范。
- [docs/DEVICE_TEST_FLOW.md](docs/DEVICE_TEST_FLOW.md)：真实 Buildroot 设备测试流程。

## 验证

```bash
cargo check
cargo clippy -- -D warnings
```

## License

MIT
