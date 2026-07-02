# crate: common

**状态:已实现(M0)** · 依赖树最底层,无业务依赖。

## 职责

跨模块公共设施:统一错误、领域标识、传输类型、日志初始化。所有其它 crate 复用。

## 公开 API

```rust
pub use error::{Error, Result};
pub use types::{DeviceId, Transport};
pub mod logging;   // logging::init()
```

## 数据类型

- `Error`(thiserror 枚举):`Config` / `Sip` / `Gb28181` / `Media` / `Io`。变体分类用于压测失败归因(见 `20-architecture/data-model.md#失败归因分类`)。
- `Result<T> = std::result::Result<T, Error>`。
- `DeviceId`:20 位数字国标 ID,构造即校验(长度=20 且全数字),否则 `Error::Config`。
- `Transport`:`Udp`(默认)/ `Tcp`,`Display` 输出 `UDP`/`TCP`,serde 大写。

## 错误

自身即错误定义方。`DeviceId::new` 对非法输入返回 `Error::Config`。

## 依赖

thiserror、tracing、tracing-subscriber、serde。

## 里程碑

M0 完成。后续按需增补公共类型(如时间戳、分位数统计辅助)。

## 测试

已有:合法/非法 DeviceId、Transport 默认值。新增公共类型须配单测。
