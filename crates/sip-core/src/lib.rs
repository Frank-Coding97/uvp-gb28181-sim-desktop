//! 与 GB28181 无关的通用 SIP 协议栈。
//!
//! 分为四层:消息(解析/构造)、事务(重传/匹配)、传输(UDP 共享 socket)、认证(Digest)。
//! GB28181 语义不在此 crate,而在 `gb28181-protocol` / `gb28181-simulator`。

pub mod auth;
pub mod message;
pub mod sdp;
pub mod subject;
pub mod transaction;
pub mod transport;

// 常用类型再导出,方便上层 `use sip_core::{...}`。
pub use auth::{authorization, authorization_with_body, Algorithm, Challenge, Qop};
pub use message::{Headers, Method, Request, Response, SipMessage};
pub use sdp::{
    build_broadcast_offer, Direction, MediaFormat, SessionDescription, Setup, F_AUDIO_G711,
    F_VIDEO_H264, F_VIDEO_H265,
};
pub use subject::Subject;
pub use transaction::{client_transact, Admit, ServerTransactions, Timing, TxToken};
pub use transport::{
    Incoming, SipTrace, SipTransport, TcpTransport, TraceDir, TraceObserver, TransportStats,
    TransportStatsSnapshot, UdpTransport,
};
