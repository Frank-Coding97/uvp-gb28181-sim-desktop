//! 与 GB28181 无关的通用 SIP 协议栈。
//!
//! 分为四层:消息(解析/构造)、事务(状态机/重传)、传输(UDP/TCP)、认证(Digest)。
//! GB28181 语义不在此 crate,而在 `gb28181-protocol` / `gb28181-simulator`。
//!
//! M0 为可编译骨架,各子模块 M1 起逐步实现。

pub mod auth;
pub mod message;
pub mod transaction;
pub mod transport;
