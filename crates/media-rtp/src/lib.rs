//! RTP / PS 媒体封装与推流引擎。
//!
//! 负责把码流(或伪码流)按 RTP 打包发送给上级平台。压测时按 `active_ratio`
//! 只让部分设备真推流,码率/帧率受控。
//!
//! - `rtp`:RTP 打包、SSRC/时间戳/序号管理、UDP 发送、发送统计
//! - `ps`:PS(Program Stream)封装,复用 H.264
//! - `source`:视频源抽象(空媒体 / 真实文件循环)
//! - `pusher`:把视频源→PS→RTP 按帧率推流的驱动

pub mod ps;
pub mod pusher;
pub mod rtp;
pub mod source;

pub use ps::PsMuxer;
pub use pusher::push_stream;
pub use rtp::{RtpSender, SendStats, CLOCK_HZ, PT_PS};
pub use source::{FileSource, Frame, NoneSource, VideoSource};

/// RTP 发送模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtpMode {
    /// UDP 数据报,每包一发。
    Udp,
    /// TCP 主动连接,RFC 4571 解帧(2 字节大端长度 + 包体)。
    TcpActive,
    /// TCP 被动监听,等平台连入。
    TcpPassive,
}
