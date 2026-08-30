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
    /// TCP —— 预留枚举；当前 SIP 传输层尚未实现,上层会明确拒绝该配置。
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

/// GB/T 28181 协议版本。影响 MANSCDP XML 的字段集(2022 是 2016 的扩展超集)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GbVersion {
    /// GB/T 28181-2016。
    V2016,
    /// GB/T 28181-2022(默认,新增更多目录/能力字段)。
    #[default]
    V2022,
}

impl GbVersion {
    /// 是否为 2022 版(用于决定是否输出 2022 新增字段)。
    pub fn is_2022(&self) -> bool {
        matches!(self, GbVersion::V2022)
    }

    /// 协议版本标识字符串(附录 I,用于 SIP/XML 标注)。
    pub fn tag(&self) -> &'static str {
        match self {
            GbVersion::V2016 => "GB/T 28181-2016",
            GbVersion::V2022 => "GB/T 28181-2022",
        }
    }

    /// 2022 版规范性附录 I 定义的 SIP 注册协议版本标识。
    pub fn x_gb_ver(&self) -> &'static str {
        match self {
            GbVersion::V2016 => "2.0",
            GbVersion::V2022 => "3.0",
        }
    }
}

impl fmt::Display for GbVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.tag())
    }
}

/// 信令字符集编码(GB/T 28181-2022 §6.10 规定 GB18030;部分平台兼容 UTF-8)。
/// 决定 MANSCDP XML 体的字节编码与 `<?xml encoding=...?>` 声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SignalingEncoding {
    /// GB18030(国标 §6.10 规定,默认)。中文用双/四字节。
    #[default]
    Gb18030,
    /// UTF-8(部分平台兼容)。
    Utf8,
}

impl SignalingEncoding {
    /// XML 声明里的 encoding 值。
    pub fn xml_name(&self) -> &'static str {
        match self {
            SignalingEncoding::Gb18030 => "GB18030",
            SignalingEncoding::Utf8 => "UTF-8",
        }
    }

    /// 从字符串解析(大小写/连字符不敏感);未知回退 GB18030。
    pub fn from_str_lenient(s: &str) -> Self {
        let k = s.to_ascii_uppercase().replace(['-', '_'], "");
        match k.as_str() {
            "UTF8" => SignalingEncoding::Utf8,
            _ => SignalingEncoding::Gb18030,
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

    #[test]
    fn gb版本默认_2022() {
        assert_eq!(GbVersion::default(), GbVersion::V2022);
        assert!(GbVersion::V2022.is_2022());
        assert!(!GbVersion::V2016.is_2022());
        assert_eq!(GbVersion::V2016.tag(), "GB/T 28181-2016");
        assert_eq!(GbVersion::V2016.x_gb_ver(), "2.0");
        assert_eq!(GbVersion::V2022.x_gb_ver(), "3.0");
    }
}
