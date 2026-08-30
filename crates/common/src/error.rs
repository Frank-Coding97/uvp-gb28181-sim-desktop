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

    /// SIP 协议层错误(解析失败、认证失败等)。
    #[error("SIP 错误: {0}")]
    Sip(String),

    /// SIP 事务超时:请求发出去了,但在 Timer B/F 之前没有收到最终响应。
    ///
    /// 与 [`Error::Sip`] 分开是为了**归因不再靠匹配错误文案**。之前压测按
    /// `msg.contains("超时")` 判定超时、`msg.contains("被拒")` 判定平台拒绝 ——
    /// 任何一次文案措辞调整都会静默地把失败归到 `Other`,而压测结论的可信度
    /// 恰恰建立在这个分类上。
    #[error("SIP 事务超时: {0}")]
    Timeout(String),

    /// 平台以非 2xx 最终响应拒绝了请求。
    ///
    /// 带上状态码是因为处置动作完全不同:401/403 是凭据或权限问题(再加压也没用),
    /// 486/503 才是"平台忙/过载"这类承载力信号。之前这些全被压成同一个
    /// "Rejected" 计数,报告里看不出该去修配置还是该降并发。
    #[error("平台拒绝: {status} {reason}")]
    Rejected {
        /// SIP 最终响应状态码。
        status: u16,
        /// 状态行原因短语 + 上下文(如"鉴权后注册被拒")。
        reason: String,
    },

    /// GB28181 应用层错误(MANSCDP XML、编码规则等)。
    #[error("GB28181 错误: {0}")]
    Gb28181(String),

    /// 媒体层错误(RTP/PS 封装、码流读取等)。
    #[error("媒体错误: {0}")]
    Media(String),

    /// 网络 I/O 错误。
    #[error("网络错误: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    /// 便捷构造平台拒绝错误。
    pub fn rejected(status: u16, reason: impl Into<String>) -> Self {
        Error::Rejected {
            status,
            reason: reason.into(),
        }
    }

    /// 拒绝时的状态码(其它变体为 None)。
    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Rejected { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// 项目统一 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拒绝错误保留状态码且文案可读() {
        let e = Error::rejected(503, "Service Unavailable(注册)");
        assert_eq!(e.status(), Some(503));
        assert!(e.to_string().contains("503"));
        assert!(e.to_string().contains("Service Unavailable"));
    }

    #[test]
    fn 其它变体没有状态码() {
        assert_eq!(Error::Timeout("REGISTER".into()).status(), None);
        assert_eq!(Error::Sip("解析失败".into()).status(), None);
        assert_eq!(Error::Config("端口越界".into()).status(), None);
    }
}
