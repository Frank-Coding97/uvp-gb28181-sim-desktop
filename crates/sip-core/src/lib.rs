//! 与 GB28181 无关的通用 SIP 协议栈。
//!
//! 分为四层:消息(解析/构造)、事务(重传/匹配)、传输(UDP 共享 socket)、认证(Digest)。
//! GB28181 语义不在此 crate,而在 `gb28181-protocol` / `gb28181-simulator`。

pub mod auth;
pub mod message;
pub mod sdp;
pub mod transaction;
pub mod transport;

// 常用类型再导出,方便上层 `use sip_core::{...}`。
pub use auth::{authorization, Challenge};
pub use message::{Headers, Method, Request, Response, SipMessage};
pub use sdp::{build_broadcast_offer, SessionDescription};
pub use transaction::{client_transact, Timing};
pub use transport::{Incoming, SipTrace, TraceDir, TraceObserver, UdpTransport};
