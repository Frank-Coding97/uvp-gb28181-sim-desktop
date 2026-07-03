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

/// 平台 → 设备的控制命令(Control)。GB28181 §A.2.4:PTZ/录像/布防/校时/重启/关键帧等。
/// 各命令是可选子元素,按出现的字段判断具体控制类型。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Control")]
pub struct Control {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// PTZ 云台控制码(8 字节十六进制串)。
    #[serde(rename = "PTZCmd", skip_serializing_if = "Option::is_none")]
    pub ptz_cmd: Option<String>,
    /// 强制关键帧,值 "Send"。
    #[serde(rename = "IFameCmd", skip_serializing_if = "Option::is_none")]
    pub iframe_cmd: Option<String>,
    /// 录像控制:Record / StopRecord。
    #[serde(rename = "RecordCmd", skip_serializing_if = "Option::is_none")]
    pub record_cmd: Option<String>,
    /// 布防/撤防:SetGuard / ResetGuard。
    #[serde(rename = "GuardCmd", skip_serializing_if = "Option::is_none")]
    pub guard_cmd: Option<String>,
    /// 报警复位。
    #[serde(rename = "AlarmCmd", skip_serializing_if = "Option::is_none")]
    pub alarm_cmd: Option<String>,
    /// 远程启动,值 "Boot"。
    #[serde(rename = "TeleBoot", skip_serializing_if = "Option::is_none")]
    pub tele_boot: Option<String>,
}

impl Control {
    /// 从 XML 解析控制命令。
    pub fn parse(xml: &str) -> Result<Self> {
        quick_xml::de::from_str(strip_xml_decl(xml))
            .map_err(|e| Error::Gb28181(format!("Control 解析失败: {e}")))
    }

    /// 人类可读的控制类型(日志/UI 用)。
    pub fn kind(&self) -> &'static str {
        if self.ptz_cmd.is_some() {
            "PTZ 云台控制"
        } else if self.iframe_cmd.is_some() {
            "强制关键帧"
        } else if self.record_cmd.is_some() {
            "录像控制"
        } else if self.guard_cmd.is_some() {
            "布防/撤防"
        } else if self.alarm_cmd.is_some() {
            "报警复位"
        } else if self.tele_boot.is_some() {
            "远程启动"
        } else {
            "未知控制"
        }
    }
}

/// 设备控制应答(设备 → 平台,DeviceControl 结果)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct ControlResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
}

impl ControlResponse {
    /// 构造 OK 应答。
    pub fn ok(device_id: impl Into<String>, sn: u32) -> Self {
        ControlResponse {
            cmd_type: "DeviceControl".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("ControlResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 移动位置通知(设备 → 平台,GPS 周期上报,MobilePosition 订阅)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct MobilePositionNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 时间(ISO8601)。
    #[serde(rename = "Time")]
    pub time: String,
    /// 经度(WGS-84)。
    #[serde(rename = "Longitude")]
    pub longitude: f64,
    /// 纬度(WGS-84)。
    #[serde(rename = "Latitude")]
    pub latitude: f64,
    /// 速度(km/h,可选)。
    #[serde(rename = "Speed", skip_serializing_if = "Option::is_none")]
    pub speed: Option<f64>,
}

impl MobilePositionNotify {
    /// 构造一条位置通知。
    pub fn new(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        lon: f64,
        lat: f64,
    ) -> Self {
        MobilePositionNotify {
            cmd_type: "MobilePosition".into(),
            sn,
            device_id: device_id.into(),
            time: time.into(),
            longitude: lon,
            latitude: lat,
            speed: Some(0.0),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("MobilePosition 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
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
    // ── 以下为 GB/T 28181-2022 相对 2016 新增的目录项字段(附录 A.2.1.9)。
    //     2016 版应答不输出这些字段(置 None)。──
    /// 摄像机安全能力等级代码(A/B/C,GB35114,2022 新增,可选)。
    #[serde(rename = "SecurityLevelCode", skip_serializing_if = "Option::is_none")]
    pub security_level_code: Option<String>,
    /// 设备 IPv4/IPv6 地址(2022 新增,可选)。
    #[serde(rename = "IPAddress", skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// 设备端口(2022 新增,可选)。
    #[serde(rename = "Port", skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
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

/// 报警通知(设备 → 平台,主动上报,FR-9)。
///
/// 对应 XML:
/// ```xml
/// <Notify>
///   <CmdType>Alarm</CmdType>
///   <SN>1</SN>
///   <DeviceID>3502...132</DeviceID>
///   <AlarmPriority>1</AlarmPriority>
///   <AlarmMethod>5</AlarmMethod>
///   <AlarmTime>2026-07-03T11:00:00</AlarmTime>
///   <AlarmDescription>移动侦测</AlarmDescription>
/// </Notify>
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct AlarmNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 报警级别 1-4(1 最高)。
    #[serde(rename = "AlarmPriority")]
    pub priority: u8,
    /// 报警方式:1 电话/2 设备/3 短信/4 GPS/5 视频/6 设备故障/7 其它。
    #[serde(rename = "AlarmMethod")]
    pub method: u8,
    /// 报警时间(ISO8601)。
    #[serde(rename = "AlarmTime")]
    pub time: String,
    /// 报警描述。
    #[serde(rename = "AlarmDescription", skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl AlarmNotify {
    /// 构造一条视频侦测报警(method=5)。
    pub fn video(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        desc: impl Into<String>,
    ) -> Self {
        AlarmNotify {
            cmd_type: "Alarm".into(),
            sn,
            device_id: device_id.into(),
            priority: 1,
            method: 5,
            time: time.into(),
            description: Some(desc.into()),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("AlarmNotify 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 录像文件项(RecordInfo 应答中一段录像)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct RecordItem {
    /// 通道 ID。
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 录像名称。
    #[serde(rename = "Name")]
    pub name: String,
    /// 开始时间(ISO8601)。
    #[serde(rename = "StartTime")]
    pub start_time: String,
    /// 结束时间(ISO8601)。
    #[serde(rename = "EndTime")]
    pub end_time: String,
    /// 录像类型:time(定时)/ alarm / manual。
    #[serde(rename = "Type")]
    pub kind: String,
}

/// 录像列表查询应答(RecordInfo,设备 → 平台,FR-10)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct RecordInfoResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 录像总数。
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "RecordList")]
    pub record_list: RecordList,
}

/// 录像列表容器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<RecordItem>,
}

impl RecordInfoResponse {
    /// 用设备 ID、SN、录像段构造应答。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<RecordItem>) -> Self {
        let num = items.len() as u32;
        RecordInfoResponse {
            cmd_type: "RecordInfo".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            record_list: RecordList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("RecordInfoResponse 序列化失败: {e}")))?;
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
            security_level_code: Some("A".into()),
            ip_address: Some("10.0.0.2".into()),
            port: Some(5060),
        }];
        let resp = CatalogResponse::new("34020000001320000001", 17, items);
        assert_eq!(resp.sum_num, 1);
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>Catalog</CmdType>"));
        assert!(xml.contains("<SecurityLevelCode>A</SecurityLevelCode>"));
        assert!(xml.contains("<IPAddress>10.0.0.2</IPAddress>"));
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

    #[test]
    fn 报警通知含字段() {
        let a = AlarmNotify::video("35020000001310000132", 7, "2026-07-03T11:00:00", "移动侦测");
        let xml = a.to_xml().unwrap();
        assert!(xml.contains("<CmdType>Alarm</CmdType>"));
        assert!(xml.contains("<AlarmMethod>5</AlarmMethod>"));
        assert!(xml.contains("<AlarmDescription>移动侦测</AlarmDescription>"));
        assert!(xml.contains("<DeviceID>35020000001310000132</DeviceID>"));
    }

    #[test]
    fn 录像列表应答含段() {
        let items = vec![RecordItem {
            device_id: "35020000001310000132".into(),
            name: "rec1".into(),
            start_time: "2026-07-03T10:00:00".into(),
            end_time: "2026-07-03T10:05:00".into(),
            kind: "time".into(),
        }];
        let resp = RecordInfoResponse::new("35020000001310000132", 12, items);
        assert_eq!(resp.sum_num, 1);
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>RecordInfo</CmdType>"));
        assert!(xml.contains("Num=\"1\""));
        assert!(xml.contains("<StartTime>2026-07-03T10:00:00</StartTime>"));
        assert!(xml.contains("<Type>time</Type>"));
    }

    #[test]
    fn 解析ptz控制并识别类型() {
        let xml = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
            <SN>5</SN><DeviceID>35020000001310000132</DeviceID>\
            <PTZCmd>A50F01000000FF</PTZCmd></Control>";
        let c = Control::parse(xml).unwrap();
        assert_eq!(c.cmd_type, "DeviceControl");
        assert_eq!(c.sn, 5);
        assert_eq!(c.ptz_cmd.as_deref(), Some("A50F01000000FF"));
        assert_eq!(c.kind(), "PTZ 云台控制");
    }

    #[test]
    fn 解析强制关键帧() {
        let xml = "<Control><CmdType>DeviceControl</CmdType><SN>1</SN>\
            <DeviceID>x</DeviceID><IFameCmd>Send</IFameCmd></Control>";
        let c = Control::parse(xml).unwrap();
        assert_eq!(c.kind(), "强制关键帧");
        let resp = ControlResponse::ok("x", 1).to_xml().unwrap();
        assert!(resp.contains("<Result>OK</Result>"));
    }

    #[test]
    fn 移动位置通知含经纬度() {
        let p = MobilePositionNotify::new(
            "35020000001310000001",
            3,
            "2026-07-03T11:00:00",
            116.397,
            39.908,
        );
        let xml = p.to_xml().unwrap();
        assert!(xml.contains("<CmdType>MobilePosition</CmdType>"));
        assert!(xml.contains("<Longitude>116.397</Longitude>"));
        assert!(xml.contains("<Latitude>39.908</Latitude>"));
    }
}
