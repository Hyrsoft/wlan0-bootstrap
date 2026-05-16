# Git 提交规范

本项目使用中文语义化 commit message。Agent 或开发者提交代码时必须遵守本规范。

## 格式

```text
<type>(<scope>): <中文描述>
```

要求：

- `type` 使用英文小写。
- `scope` 使用英文小写，表示影响范围。
- 冒号后使用一个空格。
- 描述必须使用中文。
- 描述使用祈使或概括语气，简洁说明本次提交做了什么。

## 常用 type

- `feat`：新增功能。
- `fix`：修复缺陷。
- `refactor`：重构代码，不改变用户可见行为。
- `docs`：文档变更。
- `test`：测试相关变更。
- `build`：构建系统、依赖、交叉编译、Buildroot 包变更。
- `ci`：持续集成相关变更。
- `chore`：维护性杂项变更。
- `style`：格式化或不影响逻辑的风格调整。

## 常用 scope

- `config`：运行时配置。
- `wifi`：Wi-Fi 扫描、连接、状态机。
- `ap`：Soft AP、hostapd、dnsmasq。
- `web`：Web API 或前端界面。
- `state`：状态快照、事件发布。
- `storage`：持久化数据、已知网络列表。
- `buildroot`：Buildroot package、init 脚本、部署配置。
- `docs`：项目文档。
- `repo`：仓库结构、命名、元数据。

## 示例

```text
docs(refactor): 添加重构相关文档
refactor(wifi): 引入单网卡启动连接流程
feat(state): 添加状态快照和事件发布
build(buildroot): 添加默认包配置和启动脚本
fix(web): 修复连接状态轮询失败提示
chore(repo): 删除旧音频播报实现
```

## Agent 提交要求

Agent 自动提交前必须：

1. 确认工作区没有无关改动。
2. 运行适合本次改动的验证命令。
3. 在最终回复中说明提交哈希和验证结果。
4. commit message 必须使用本规范，且描述必须为中文。
