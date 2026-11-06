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
    /// TCP —— 与 UDP 共用上层接口,底层为共享连接池(`sip_core::TcpTransport`)。
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

/// 音频流(语音广播/对讲下行)的媒体传输方式。
///
/// 与信令传输 [`Transport`] 相互独立:音频可以单独选择 UDP / TCP 主动 / TCP 被动。
/// 语义对齐移动端模拟器的 `AudioTransportType`:
/// - `TcpActive`(默认):设备是 TCP 客户端,offer 写 `a=setup:active`(NAT 后常用);
/// - `TcpPassive`:设备监听端口,offer 写 `a=setup:passive`,等平台连入;
/// - `Udp`:兼容性最好,但 NAT 后可能收不到平台下行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AudioTransport {
    /// UDP(兼容性兜底)。
    Udp,
    /// TCP 主动(设备连平台)。默认项。
    #[default]
    TcpActive,
    /// TCP 被动(设备监听,平台连入)。
    TcpPassive,
}

impl AudioTransport {
    /// 协议/日志用的稳定标识(`UDP` / `TCP_ACTIVE` / `TCP_PASSIVE`)。
    pub fn as_str(&self) -> &'static str {
        match self {
            AudioTransport::Udp => "UDP",
            AudioTransport::TcpActive => "TCP_ACTIVE",
            AudioTransport::TcpPassive => "TCP_PASSIVE",
        }
    }

    /// 是否为 TCP 媒体传输(决定 SDP 的 `TCP/RTP/AVP` 与 `a=setup` 行)。
    pub fn is_tcp(&self) -> bool {
        !matches!(self, AudioTransport::Udp)
    }
}

impl fmt::Display for AudioTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

/// 信令字符集编码(GB/T 28181 §6.10)。
/// 决定 MANSCDP XML 体的字节编码与 `<?xml encoding=...?>` 声明。
///
/// 两版口径不同,不是"升级一下默认值"这么简单:
///   - 2016 §6.10「联网系统与设备的 SIP 信令字符集**宜**采用 GB 2312 编码格式」—— 推荐项。
///   - 2022 §6.10「联网系统与设备的 SIP 信令字符集**应**采用 GB 18030 编码格式」—— 由「宜」升「应」。
///
/// 因此"声明 GB2312、实际发 UTF-8 字节"在 2022 下不是宽松实现,而是不合规:
/// 平台按声明挑解码器,拿 UTF-8 字节按 GBK 解,中文必乱码。声明与字节必须同源
/// (由 `gb28181_simulator::builder::encode_xml_body` 保证)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SignalingEncoding {
    /// 2016 口径(§6.10 宜)。实际编码用 **GBK** —— GB2312 的兼容超集,
    /// 严格 EUC-CN 子集编不出生僻字。与平台侧 `encodingForLabel` 取同一条口径。
    Gb2312,
    /// 2022 口径(§6.10 应),当前默认。GBK/GB2312 的超集,覆盖全部 Unicode。
    #[default]
    Gb18030,
    /// 非国标口径,仅作联调逃逸口:存量厂商大量实际发 UTF-8。
    /// 两版下都恒可选,但选择它即放弃 §6.10 合规性。
    Utf8,
}

impl SignalingEncoding {
    /// XML 声明里的 encoding 值。
    ///
    /// ⚠ GB2312 声明对应的是 GBK 字节(见枚举注释),平台侧同样把 GB2312 映射到 GBK 解码器。
    pub fn xml_name(&self) -> &'static str {
        match self {
            SignalingEncoding::Gb2312 => "GB2312",
            SignalingEncoding::Gb18030 => "GB18030",
            SignalingEncoding::Utf8 => "UTF-8",
        }
    }

    /// 该版本下 §6.10 的默认字符集。
    pub fn default_for(version: GbVersion) -> Self {
        match version {
            GbVersion::V2016 => SignalingEncoding::Gb2312,
            GbVersion::V2022 => SignalingEncoding::Gb18030,
        }
    }

    /// 该字符集在指定版本下是否**可选**。
    ///
    ///   - GB2312 只挂 2016:2022 下 GB18030 是它的超集,常用字字节完全一致,
    ///     再列一项只会制造等价选项。
    ///   - GB18030 只挂 2022:2016 平台不认这个声明名。
    ///   - UTF-8 两版都可选(逃逸口)。
    pub fn is_available_for(&self, version: GbVersion) -> bool {
        match self {
            SignalingEncoding::Gb2312 => version == GbVersion::V2016,
            SignalingEncoding::Gb18030 => version == GbVersion::V2022,
            SignalingEncoding::Utf8 => true,
        }
    }

    /// 实际出站字符集 = 保存值(该版本下可用时)或版本默认。
    ///
    /// 保存值与版本不匹配时**按版本默认兜底**,而不是照发:否则切到 2016 后仍会带着
    /// GB18030 声明出站,那是 2016 平台不认的编码名,联调现场表现为"对端收到但解析不出"。
    /// 兜底方向与 [`Self::default_for`] 一致,所以"跟随版本"无需额外开关。
    pub fn effective(self, version: GbVersion) -> Self {
        if self.is_available_for(version) {
            self
        } else {
            Self::default_for(version)
        }
    }

    /// 从字符串解析(大小写/连字符/下划线不敏感);未知回退 GB18030。
    ///
    /// 别名表与平台侧 `canonicalCharset` 对齐 —— 否则会出现"本端声明 GBK、平台不认"的哑火。
    pub fn from_str_lenient(s: &str) -> Self {
        let k = s.to_ascii_uppercase().replace(['-', '_'], "");
        match k.as_str() {
            "GB2312" | "GB231280" | "GB23121980" | "GBK" | "CP936" | "MS936" | "WINDOWS936" => {
                SignalingEncoding::Gb2312
            }
            "UTF8" | "UNICODE11UTF8" => SignalingEncoding::Utf8,
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
    fn 对讲流传输默认为_tcp主动() {
        assert_eq!(AudioTransport::default(), AudioTransport::TcpActive);
        assert_eq!(AudioTransport::default().as_str(), "TCP_ACTIVE");
        assert!(AudioTransport::TcpActive.is_tcp());
        assert!(!AudioTransport::Udp.is_tcp());
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

    #[test]
    fn 字符集默认随国标版本走() {
        // 2016 §6.10「宜 GB2312」,2022 §6.10「应 GB18030」—— 默认值不是常量而是版本的函数。
        assert_eq!(
            SignalingEncoding::default_for(GbVersion::V2016),
            SignalingEncoding::Gb2312
        );
        assert_eq!(
            SignalingEncoding::default_for(GbVersion::V2022),
            SignalingEncoding::Gb18030
        );
    }

    #[test]
    fn 字符集可选性按版本收窄() {
        // GB2312 只挂 2016,GB18030 只挂 2022,UTF-8 是两版共用的逃逸口。
        assert!(SignalingEncoding::Gb2312.is_available_for(GbVersion::V2016));
        assert!(!SignalingEncoding::Gb2312.is_available_for(GbVersion::V2022));
        assert!(!SignalingEncoding::Gb18030.is_available_for(GbVersion::V2016));
        assert!(SignalingEncoding::Gb18030.is_available_for(GbVersion::V2022));
        assert!(SignalingEncoding::Utf8.is_available_for(GbVersion::V2016));
        assert!(SignalingEncoding::Utf8.is_available_for(GbVersion::V2022));
    }

    #[test]
    fn 不匹配的字符集按版本默认兜底() {
        // 切到 2016 后仍存着 GB18030:必须兜底成 GB2312,否则带着 2016 不认的声明名出站。
        assert_eq!(
            SignalingEncoding::Gb18030.effective(GbVersion::V2016),
            SignalingEncoding::Gb2312
        );
        // 反向同理:2022 下存着 GB2312 会被升到超集 GB18030。
        assert_eq!(
            SignalingEncoding::Gb2312.effective(GbVersion::V2022),
            SignalingEncoding::Gb18030
        );
        // 匹配项与 UTF-8 逃逸口原样保留。
        assert_eq!(
            SignalingEncoding::Gb2312.effective(GbVersion::V2016),
            SignalingEncoding::Gb2312
        );
        assert_eq!(
            SignalingEncoding::Utf8.effective(GbVersion::V2016),
            SignalingEncoding::Utf8
        );
        assert_eq!(
            SignalingEncoding::Utf8.effective(GbVersion::V2022),
            SignalingEncoding::Utf8
        );
    }

    #[test]
    fn 字符集解析认得平台侧别名() {
        for label in ["GB2312", "gb2312", "GBK", "cp936", "MS936", "windows-936", "GB_2312-80"] {
            assert_eq!(
                SignalingEncoding::from_str_lenient(label),
                SignalingEncoding::Gb2312,
                "别名 {label} 应解析为 GB2312"
            );
        }
        assert_eq!(
            SignalingEncoding::from_str_lenient("utf8"),
            SignalingEncoding::Utf8
        );
        assert_eq!(
            SignalingEncoding::from_str_lenient("GB18030"),
            SignalingEncoding::Gb18030
        );
        // 未知值回退 GB18030(2022 口径),而不是 UTF-8。
        assert_eq!(
            SignalingEncoding::from_str_lenient("BIG5"),
            SignalingEncoding::Gb18030
        );
    }

    #[test]
    fn 字符集声明名与序列化一致() {
        assert_eq!(SignalingEncoding::Gb2312.xml_name(), "GB2312");
        assert_eq!(SignalingEncoding::Gb18030.xml_name(), "GB18030");
        assert_eq!(SignalingEncoding::Utf8.xml_name(), "UTF-8");
        // 配置以变体名落盘,前端按同一批字符串读写。
        assert_eq!(
            serde_json::to_string(&SignalingEncoding::Gb2312).unwrap(),
            "\"Gb2312\""
        );
    }
}
