//! MANSCDP XML 消息编解码。
//!
//! 实现 docs/40-protocol/manscdp.md。M1 覆盖 Keepalive(心跳 Notify);
//! M2 补充 Catalog / DeviceInfo / DeviceStatus 查询解析与应答。用 quick-xml + serde。

use common::{Error, Result};
use serde::{Deserialize, Serialize};

/// XML 声明前缀(GB28181 惯例用 GB2312 编码声明,内容为 ASCII 安全)。
const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"GB2312\"?>\n";

/// 去掉 `<?xml ...?>` 声明,返回其后的元素部分。
fn strip_xml_decl(xml: &str) -> &str {
    match xml.find("?>") {
        Some(pos) => xml[pos + 2..].trim_start(),
        None => xml.trim_start(),
    }
}

/// 平台 → 设备的查询请求(Query),用于提取 CmdType 与 SN 以决定如何应答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Query")]
pub struct Query {
    /// 命令类型:Catalog / DeviceInfo / DeviceStatus / RecordInfo 等。
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    /// 序列号,应答须回显。
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 目标设备/通道 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
}

impl Query {
    /// 从 XML 解析查询。非 Query 根元素则返回错误。
    pub fn parse(xml: &str) -> Result<Self> {
        quick_xml::de::from_str(strip_xml_decl(xml))
            .map_err(|e| Error::Gb28181(format!("Query 解析失败: {e}")))
    }
}

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
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 从 XML 解析心跳(用于自检/测试往返;设备侧一般只发不收)。
pub fn parse_keepalive(xml: &str) -> Result<Keepalive> {
    quick_xml::de::from_str(strip_xml_decl(xml))
        .map_err(|e| Error::Gb28181(format!("Keepalive 解析失败: {e}")))
}

/// 目录通道项(GB-2022 常用字段)。设备目录查询应答中每个通道一项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CatalogItem {
    /// 通道国标 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 通道名称。
    #[serde(rename = "Name")]
    pub name: String,
    /// 厂商。
    #[serde(rename = "Manufacturer", skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// 型号。
    #[serde(rename = "Model", skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 行政区划。
    #[serde(rename = "CivilCode", skip_serializing_if = "Option::is_none")]
    pub civil_code: Option<String>,
    /// 是否为子设备/目录(0/1)。
    #[serde(rename = "Parental", skip_serializing_if = "Option::is_none")]
    pub parental: Option<u8>,
    /// 父设备/目录 ID。
    #[serde(rename = "ParentID", skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// 注册/在线状态:ON / OFF。
    #[serde(rename = "Status")]
    pub status: String,
}

/// 目录查询应答(设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct CatalogResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    /// 设备(根)ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 通道总数。
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    /// 通道列表容器。
    #[serde(rename = "DeviceList")]
    pub device_list: DeviceList,
}

/// 目录通道列表容器(带 Num 属性)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceList {
    /// 本次携带的通道数。
    #[serde(rename = "@Num")]
    pub num: u32,
    /// 通道项。
    #[serde(rename = "Item", default)]
    pub items: Vec<CatalogItem>,
}

impl CatalogResponse {
    /// 用设备 ID、SN、通道列表构造应答。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<CatalogItem>) -> Self {
        let num = items.len() as u32;
        CatalogResponse {
            cmd_type: "Catalog".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            device_list: DeviceList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("CatalogResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 设备信息查询应答(设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct DeviceInfoResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
    #[serde(rename = "DeviceName")]
    pub device_name: String,
    #[serde(rename = "Manufacturer")]
    pub manufacturer: String,
    #[serde(rename = "Model")]
    pub model: String,
    #[serde(rename = "Firmware")]
    pub firmware: String,
    /// 通道数。
    #[serde(rename = "Channel")]
    pub channel: u32,
}

impl DeviceInfoResponse {
    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("DeviceInfoResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 设备状态查询应答(设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct DeviceStatusResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
    /// 是否在线:ONLINE / OFFLINE。
    #[serde(rename = "Online")]
    pub online: String,
    /// 状态:OK / ERROR。
    #[serde(rename = "Status")]
    pub status: String,
}

impl DeviceStatusResponse {
    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("DeviceStatusResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
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

    #[test]
    fn 解析目录查询() {
        let xml = "<?xml version=\"1.0\"?>\n<Query>\
            <CmdType>Catalog</CmdType><SN>17</SN>\
            <DeviceID>34020000001320000001</DeviceID></Query>";
        let q = Query::parse(xml).unwrap();
        assert_eq!(q.cmd_type, "Catalog");
        assert_eq!(q.sn, 17);
        assert_eq!(q.device_id, "34020000001320000001");
    }

    #[test]
    fn 目录应答含通道与数量() {
        let items = vec![CatalogItem {
            device_id: "34020000001320000002".into(),
            name: "通道1".into(),
            manufacturer: Some("UVP".into()),
            model: Some("Sim".into()),
            civil_code: None,
            parental: Some(0),
            parent_id: Some("34020000001320000001".into()),
            status: "ON".into(),
        }];
        let resp = CatalogResponse::new("34020000001320000001", 17, items);
        assert_eq!(resp.sum_num, 1);
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>Catalog</CmdType>"));
        assert!(xml.contains("<SumNum>1</SumNum>"));
        assert!(xml.contains("Num=\"1\""));
        assert!(xml.contains("<DeviceID>34020000001320000002</DeviceID>"));
        assert!(xml.contains("<Status>ON</Status>"));
    }

    #[test]
    fn 设备信息应答字段() {
        let resp = DeviceInfoResponse {
            cmd_type: "DeviceInfo".into(),
            sn: 3,
            device_id: "34020000001320000001".into(),
            result: "OK".into(),
            device_name: "UVP-Sim".into(),
            manufacturer: "UVP".into(),
            model: "Desktop".into(),
            firmware: "0.1.0".into(),
            channel: 1,
        };
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>DeviceInfo</CmdType>"));
        assert!(xml.contains("<Manufacturer>UVP</Manufacturer>"));
        assert!(xml.contains("<Channel>1</Channel>"));
    }

    #[test]
    fn 设备状态应答字段() {
        let resp = DeviceStatusResponse {
            cmd_type: "DeviceStatus".into(),
            sn: 4,
            device_id: "34020000001320000001".into(),
            result: "OK".into(),
            online: "ONLINE".into(),
            status: "OK".into(),
        };
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<Online>ONLINE</Online>"));
    }
}
