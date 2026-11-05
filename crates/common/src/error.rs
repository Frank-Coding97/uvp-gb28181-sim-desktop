//! 统一错误类型。
//!
//! 各 crate 复用本模块的 [`Error`] / [`Result`],上层(引擎/UI)可按变体分类处理。

use thiserror::Error;

/// 全局错误枚举。按来源分层,便于压测时统计失败原因 Top-N。
#[derive(Debug, Error)]
pub enum Error {
    /// 配置非法(字段缺失、取值越界、ID 格式错误等)。
    #[error("配置错误: {0}")]
    Config(String),

    /// SIP 协议层错误(解析失败、事务超时、认证失败等)。
    #[error("SIP 错误: {0}")]
    Sip(String),

    /// GB28181 应用层错误(MANSCDP XML、编码规则等)。
    #[error("GB28181 错误: {0}")]
    Gb28181(String),

    /// 媒体层错误(RTP/PS 封装、码流读取等)。
    #[error("媒体错误: {0}")]
    Media(String),

    /// 网络 / 文件 IO 错误。
    ///
    /// 显示文案不写「网络错误」：本变体承接的是所有 `std::io::Error`，
    /// 建目录、写文件失败也会走到这里。2026-11-05 就出现过录像目录被企业安全
    /// 客户端拦截（os error 5），界面上却显示「网络错误」，把人往网络方向带。
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 项目统一 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;
