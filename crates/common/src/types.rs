//! 公共领域类型。

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::{Error, Result};

/// SIP 信令传输方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum Transport {
    /// UDP —— GB28181 最常用,压测默认。
    #[default]
    Udp,
    /// TCP —— 大码流或跨网时使用。
    Tcp,
}

impl fmt::Display for Transport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Transport::Udp => write!(f, "UDP"),
            Transport::Tcp => write!(f, "TCP"),
        }
    }
}

/// GB28181 设备/通道国标 ID。
///
/// 20 位数字:中心编码(8) + 行业(2) + 类型(3) + 序号(7)。
/// 批量生成虚拟设备时按序号递增。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DeviceId(String);

impl DeviceId {
    /// 校验并构造。非 20 位或含非数字字符则返回 [`Error::Config`]。
    pub fn new(s: impl Into<String>) -> Result<Self> {
        let s = s.into();
        if s.len() != 20 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Config(format!("非法国标 ID(需 20 位数字): {s}")));
        }
        Ok(DeviceId(s))
    }

    /// 取底层字符串。
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 合法_id_通过校验() {
        let id = DeviceId::new("34020000001320000001").unwrap();
        assert_eq!(id.as_str(), "34020000001320000001");
    }

    #[test]
    fn 非法_id_被拒绝() {
        assert!(DeviceId::new("123").is_err()); // 长度不足
        assert!(DeviceId::new("3402000000132000000X").is_err()); // 含非数字
    }

    #[test]
    fn transport_默认为_udp() {
        assert_eq!(Transport::default(), Transport::Udp);
        assert_eq!(Transport::Udp.to_string(), "UDP");
    }
}
