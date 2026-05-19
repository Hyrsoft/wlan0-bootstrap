# AGENT.md

## 代码规范

所有 Rust 代码修改必须遵守 [docs/RUST_CODE_STYLE.md](docs/RUST_CODE_STYLE.md)。
所有 Git 提交信息必须遵守 [docs/GIT_COMMIT_CONVENTION.md](docs/GIT_COMMIT_CONVENTION.md)。

生成或审查代码时必须重点检查：

- 无意义 `clone`
- 滥用 `Arc<Mutex<_>>`
- production path 中的 `unwrap`、`expect`、`panic!`
- async 代码中的阻塞调用
- Java/Python 风格的 Service、Manager、Factory、Singleton 抽象
- 本可借用却强行接收 `String`、`Vec` 的 API
- 嵌入式敏感路径中的隐藏堆分配
- 缺少上下文的错误传播
- 没有 `SAFETY` 注释的 `unsafe`

如果确实需要例外，必须在代码附近或变更说明中解释原因。

## 中文注释规范

本项目面向嵌入式设备和系统工具编排，源码中的注释应使用中文，并优先解释“为什么这样做”和“边界在哪里”。

必须写中文注释的场景：

- 状态转换、主循环决策、失败回退和重连逻辑。
- 调用系统工具的位置，例如 `wpa_supplicant`、`hostapd`、`dnsmasq`、`ip`、`udhcpc`。
- 进程归属、socket 清理、force takeover 等可能影响设备网络状态的敏感操作。
- 持久化数据格式、原子写入、明文 Wi-Fi 密码等安全或兼容性取舍。
- async 中使用 `spawn_blocking`、锁、channel、timeout 等并发控制的位置。

不要写机械注释，例如“给变量赋值”“调用函数”。注释应帮助后续开发者理解硬件约束、Buildroot 环境假设、系统工具边界和失败处理策略。

## 项目方向

项目名称为 **wlan0 bootstrap**。

代码包、二进制和目录建议使用 kebab-case：

- 显示名称：`wlan0 bootstrap`
- crate/package/binary 名称：`wlan0-bootstrap`
- 仓库目录：`wlan0-bootstrap`

本项目的重构蓝图见 [docs/REFACTOR_BLUEPRINT.md](docs/REFACTOR_BLUEPRINT.md)。后续开发必须以该蓝图为准。

## 边界决策

本项目不是完整 NetworkManager 替代品，而是面向 Buildroot AIoT 设备的轻量 Wi-Fi bootstrap 守护进程。

必须坚持以下边界：

- 只管理一个 Wi-Fi 接口，默认是 `wlan0`。
- 只支持单射频 TDM 流程：先 STA 扫描，再根据结果决定连接或进入 Soft AP 配网。
- 不设计 AP+STA 并发模式。
- 不设计双网卡、多网卡或多后端抽象。
- 开机自启后先尝试连接已知网络。
- 扫描不到已知网络，或已知网络全部连接失败，才进入 Soft AP 配网。
- Web UI 只服务于配网流程，不做长期管理后台。
- 状态播报由本程序发布状态事件，音频、屏幕、LED 等提示由外部程序订阅并执行。

## 当前状态

当前代码已经从早期 Soft AP 配网原型推进到可编译的单接口 bootstrap 守护进程雏形：

- 支持 `/etc/wlan0-bootstrap/config.toml` 和 `--config`，内置配置仅作为 fallback。
- 启动后会加载已知 Wi-Fi 列表，先扫描并尝试连接已知网络，失败后进入 Soft AP 配网。
- 已知网络列表由程序维护，默认保存到 `/data/wlan0-bootstrap/networks.toml`。
- 不再执行全局 `killall -9`，默认检测到已有 `wpa_supplicant` 控制 socket 会报 `InterfaceBusy`。
- 状态快照写入 `/run/wlan0-bootstrap/status.json`，事件通过 Unix socket 发布。
- Web API 已提供 `/api/scan`、`/api/status`、`/api/connect`，真实连接结果通过状态轮询表达。
- 当前阶段不调用额外 PSK 派生工具，连接和自动重连都通过 `wpa-ctrl` 操作 `wpa_supplicant`。
- Buildroot package、默认配置和 init 脚本已经存在。

仍需重点关注的问题：

- 状态机仍是枚举加分散调用，不是集中式状态转移表。
- `ownership.force_takeover=true` 只允许移除已有控制 socket 并启动本程序的 `wpa_supplicant`，仍不应作为默认部署策略。
- 已知网络当前保存用户提交的密码字符串；这是为了保持系统工具调用边界，后续如需增强安全性再单独设计 PSK 派生或加密存储。
- Web UI 仍是配网页，不是长期管理后台；`/api/scan` 返回进入 AP 前的扫描缓存。
- 缺少真实 Buildroot 设备端到端验证和自动化测试覆盖。

## 后续优先级

优先级从高到低：

1. 在真实 Buildroot 设备上验证 STA 自动连接、Soft AP 配网、失败回退、重连和 init 脚本。
2. 明确单 wlan0 状态机，减少状态转换散落在不同异步函数中的问题。
3. 加强进程归属检测，区分 stale socket、外部 `wpa_supplicant` 和本程序启动的子进程。
4. 补充状态发布、Web API 和设备端失败路径测试。
5. 根据设备验证结果调整 Web UI 扫描刷新策略和配网空闲超时。

任何重构都应该小步推进，并优先保证可在真实 Buildroot 设备上验证。
