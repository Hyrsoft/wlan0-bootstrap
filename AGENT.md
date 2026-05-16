# AGENT.md

## 代码规范

所有 Rust 代码修改必须遵守 [docs/RUST_CODE_STYLE.md](docs/RUST_CODE_STYLE.md)。

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

## 项目方向

项目计划重命名为 **wlan0 bootstrap**。

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

## 当前核心问题

当前代码仍是早期 Soft AP 配网原型，主要问题包括：

- 配置被编译进二进制。
- 启动时全局 `killall -9` 破坏性过强。
- 不维护已知 Wi-Fi 列表。
- 成功配网后直接退出，没有守护进程式生命周期。
- 状态机隐式散落在异步函数里。
- `/api/connect` 只能表示请求已接收，不能表达真实连接结果。
- 状态提示和音频播放耦合在主程序里。
- Buildroot 集成仍停留在 README 示例级别。

## 后续优先级

优先级从高到低：

1. 按蓝图整理运行时配置和持久化数据路径。
2. 引入已知网络列表，启动时自动连接。
3. 明确单 wlan0 状态机。
4. 移除全局破坏性清理，改为本程序拥有并管理自己的进程。
5. 引入状态快照和事件发布接口。
6. 把音频提示改成外部订阅者模型。
7. 更新 Web API 和 UI，使其反映真实连接状态。
8. 提供 Buildroot package、默认配置和 init 脚本。

任何重构都应该小步推进，并优先保证可在真实 Buildroot 设备上验证。
