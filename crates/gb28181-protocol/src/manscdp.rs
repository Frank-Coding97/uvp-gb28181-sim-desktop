//! MANSCDP XML 消息编解码。
//!
//! 实现 docs/40-protocol/manscdp.md。M1 覆盖 Keepalive(心跳 Notify);
//! Catalog / DeviceInfo 等在 M2 补充。用 quick-xml 的 serde 派生做序列化。

use common::{Error, Result};
use serde::{Deserialize, Serialize};

/// 心跳通知(设备 → 平台,周期发送)。
///
/// 对应 XML:
/// ```xml
/// <?xml version="1.0" encoding="GB2312"?>
/// <Notify>
///   <CmdType>Keepalive</CmdType>
///   <SN>1</SN>
///   <DeviceID>34020000001320000001</DeviceID>
///   <Status>OK</Status>
/// </Notify>
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct Keepalive {
    /// 命令类型,固定 "Keepalive"。
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    /// 序列号,每次递增。
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 本设备国标 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 状态,通常 "OK"。
    #[serde(rename = "Status")]
    pub status: String,
}

impl Keepalive {
    /// 构造一个 OK 心跳。
    pub fn ok(device_id: impl Into<String>, sn: u32) -> Self {
        Keepalive {
            cmd_type: "Keepalive".into(),
            sn,
            device_id: device_id.into(),
            status: "OK".into(),
        }
    }

    /// 序列化为完整 XML(含 XML 声明)。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("Keepalive 序列化失败: {e}")))?;
        Ok(format!("<?xml version=\"1.0\" encoding=\"GB2312\"?>\n{body}"))
    }
}

/// 从 XML 解析心跳(用于自检/测试往返;设备侧一般只发不收)。
pub fn parse_keepalive(xml: &str) -> Result<Keepalive> {
    let trimmed = strip_xml_decl(xml);
    quick_xml::de::from_str(trimmed)
        .map_err(|e| Error::Gb28181(format!("Keepalive 解析失败: {e}")))
}

/// 去掉 `<?xml ...?>` 声明,返回其后的元素部分。
fn strip_xml_decl(xml: &str) -> &str {
    match xml.find("?>") {
        Some(pos) => xml[pos + 2..].trim_start(),
        None => xml.trim_start(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 心跳序列化含关键字段() {
        let ka = Keepalive::ok("34020000001320000001", 5);
        let xml = ka.to_xml().unwrap();
        assert!(xml.contains("<CmdType>Keepalive</CmdType>"));
        assert!(xml.contains("<SN>5</SN>"));
        assert!(xml.contains("<DeviceID>34020000001320000001</DeviceID>"));
        assert!(xml.contains("<Status>OK</Status>"));
    }

    #[test]
    fn 心跳解析往返() {
        let ka = Keepalive::ok("34020000001320000001", 9);
        let xml = ka.to_xml().unwrap();
        let back = parse_keepalive(&xml).unwrap();
        assert_eq!(back, ka);
    }
}
