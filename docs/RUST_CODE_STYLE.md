# Rust 项目代码约束与风格规范（AI协同开发版）

适用于：

* Rust 项目
* AI 辅助生成代码
* 嵌入式 / 服务端 / CLI / 工具链
* 团队协作开发

目标：

* 保持 Rust idiomatic 风格
* 避免 Java/Python 风格污染
* 强化 ownership 与类型系统设计
* 提高可维护性与性能一致性

---

# 1. 总体设计原则

## 1.1 Ownership First

所有 API 设计优先考虑：

* ownership
* borrowing
* 生命周期
* 数据流向

禁止：

* 无意义 clone
* 全局共享可变状态
* “先 Arc<Mutex> 再说”

---

## 1.2 Composition Over Service Architecture

禁止 Java 风格：

```rust
UserService
OrderManager
GlobalContext
SystemManager
```

优先：

* module
* trait
* free function
* 数据驱动设计

---

## 1.3 数据优先于对象

Rust 不是 OOP 语言。

优先：

```rust
struct Packet {
    header: Header,
    payload: Vec<u8>,
}
```

而不是：

```rust
struct PacketManager {}
```

---

## 1.4 显式状态优于隐藏行为

优先：

```rust
enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}
```

避免：

* bool 状态组合
* 隐式 side effect
* 魔法状态切换

---

# 2. API 设计规范

---

## 2.1 优先 borrow API

错误示例：

```rust
fn process(data: Vec<String>)
```

正确示例：

```rust
fn process(data: &[String])
```

或：

```rust
fn process<I, S>(data: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
```

---

## 2.2 避免不必要 clone

禁止：

```rust
let x = value.clone();
```

除非：

* 明确 ownership 转移需求
* clone 成本可接受
* 有注释说明

优先：

* `&T`
* `&mut T`
* `Cow`
* move semantics

---

## 2.3 使用 AsRef / Into

优先：

```rust
fn open<P: AsRef<Path>>(path: P)
```

而不是：

```rust
fn open(path: String)
```

---

## 2.4 Result 优于 panic

禁止：

```rust
unwrap()
expect()
panic!()
```

在：

* library
* daemon
* embedded
* production path

中直接使用。

允许：

* test
* prototype
* 明确不可恢复错误

---

# 3. 并发规范

---

## 3.1 禁止滥用 Arc<Mutex<T>>

禁止：

```rust
Arc<Mutex<HashMap<...>>>
```

作为默认方案。

优先考虑：

* ownership transfer
* channel
* actor model
* immutable data
* RwLock
* lock-free

---

## 3.2 锁范围最小化

禁止：

```rust
let guard = mutex.lock().unwrap();
/* 大量逻辑 */
```

必须：

* 缩短 lock 生命周期
* 避免 await 持锁

---

## 3.3 async 禁止阻塞

禁止：

```rust
std::thread::sleep()
```

在 async 中使用。

必须：

```rust
tokio::time::sleep()
```

---

# 4. 错误处理规范

---

## 4.1 使用 thiserror

库代码：

```rust
#[derive(thiserror::Error)]
pub enum Error {
    #[error("invalid packet")]
    InvalidPacket,
}
```

---

## 4.2 anyhow 仅用于应用层

允许：

* CLI
* demo
* main.rs

禁止：

* library public API

---

## 4.3 错误必须携带上下文

优先：

```rust
.with_context(|| format!("failed to open {}", path.display()))
```

---

# 5. 模块与架构规范

---

## 5.1 模块优于巨型文件

禁止：

* 5000 行单文件
* “utils.rs 黑洞”

推荐：

```text
net/
    mod.rs
    tcp.rs
    udp.rs
    packet.rs
```

---

## 5.2 trait 用于能力抽象

正确：

```rust
trait Storage {
    fn read(&self);
}
```

避免：

* 为 mock 而抽象
* 无意义 interface

---

## 5.3 禁止 God Object

禁止：

```rust
struct AppContext {
    everything: Everything,
}
```

---

# 6. 性能规范

---

## 6.1 优先 stack allocation

避免：

```rust
Box<T>
Rc<T>
Arc<T>
```

除非：

* 生命周期必要
* 动态大小
* 共享 ownership

---

## 6.2 Vec 分配必须可解释

优先：

```rust
Vec::with_capacity(n)
```

---

## 6.3 避免字符串滥用

禁止：

```rust
String
```

作为所有 API 输入。

优先：

```rust
&str
```

---

## 6.4 iterator 优先

优先：

```rust
iter()
map()
filter()
fold()
```

而不是手写 index loop。

---

# 7. Embedded / Systems 特殊规范

---

## 7.1 禁止动态分配（可选）

对于 embedded：

```rust
#![no_std]
```

下：

禁止：

* Vec
* String
* Box

除非明确允许 allocator。

---

## 7.2 禁止隐藏 heap allocation

必须明确：

* allocation 点
* 内存生命周期

---

## 7.3 volatile / unsafe 必须注释

所有：

```rust
unsafe
```

必须解释：

* 为什么安全
* 内存约束
* 生命周期约束

---

# 8. Unsafe 规范

---

## 8.1 unsafe 最小化

必须：

* 封装在最小范围
* 提供 safe API

---

## 8.2 unsafe 必须有 SAFETY 注释

格式：

```rust
// SAFETY:
// buffer 来自 DMA 区域
// 长度已由硬件校验
unsafe {
}
```

---

# 9. AI 生成代码约束

---

## 9.1 禁止 Java 风格架构

禁止生成：

* Service
* Manager
* Singleton
* Factory
* Bean 风格

除非用户明确要求。

---

## 9.2 AI 代码必须二次审查

重点检查：

* clone
* Arc<Mutex>
* unwrap
* 生命周期
* allocation
* async 阻塞
* 错误传播

---

## 9.3 优先 idiomatic Rust

要求 AI：

* ownership-oriented
* borrow-friendly
* iterator-centric
* trait composition
* zero-cost abstraction

---

# 10. 推荐工具链

## 格式化

```bash
cargo fmt
```

---

## lint

```bash
cargo clippy -- -D warnings
```

---

## 安全检查

```bash
cargo audit
```

---

## 未使用依赖

```bash
cargo machete
```

---

# 11. 推荐代码风格

---

## 推荐

```rust
pub fn parse(data: &[u8]) -> Result<Packet> {
    if data.len() < HEADER_SIZE {
        return Err(Error::InvalidLength);
    }

    Ok(Packet::from_bytes(data))
}
```

特点：

* borrow API
* 无 clone
* 显式错误
* ownership 清晰

---

## 不推荐

```rust
pub struct PacketManager {}

impl PacketManager {
    pub fn parse_packet(&self, data: Vec<u8>) -> Result<Packet, String> {
        ...
    }
}
```

问题：

* Java 风格
* ownership 不合理
* String 错误
* 无意义对象化

---

# 12. 最终目标

Rust 代码应体现：

* ownership clarity
* type-driven design
* explicit state
* predictable performance
* minimal runtime cost
* fearless concurrency

而不是：

> “长得像 Rust 的 Java/Python”
