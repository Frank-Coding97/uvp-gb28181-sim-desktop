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
    /// 订阅周期(秒)。仅移动位置订阅(SUBSCRIBE + MobilePosition)携带,
    /// 指示设备按此间隔周期上报位置 NOTIFY;普通查询不含此字段(None)。
    #[serde(rename = "Interval", default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
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
    /// 看守位控制(HomePosition):Enabled/ResetTime/PresetIndex 等,GB-2016 A.2.4.4。
    #[serde(rename = "HomePosition", skip_serializing_if = "Option::is_none")]
    pub home_position: Option<HomePosition>,
    /// 拉框放大/缩小(DragZoomIn/DragZoomOut),GB-2022 精确控制。
    #[serde(rename = "DragZoomIn", skip_serializing_if = "Option::is_none")]
    pub drag_zoom_in: Option<DragZoom>,
    #[serde(rename = "DragZoomOut", skip_serializing_if = "Option::is_none")]
    pub drag_zoom_out: Option<DragZoom>,
}

/// 看守位设置(HomePosition 子元素)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePosition {
    /// 是否启用看守位(1 启用 / 0 关闭)。
    #[serde(rename = "Enabled")]
    pub enabled: u8,
    /// 自动归位时间(秒)。
    #[serde(rename = "ResetTime", default)]
    pub reset_time: u32,
    /// 归位到的预置位编号。
    #[serde(rename = "PresetIndex", default)]
    pub preset_index: u32,
}

/// 拉框放大/缩小参数(DragZoom 子元素)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DragZoom {
    #[serde(rename = "Length", default)]
    pub length: u32,
    #[serde(rename = "Width", default)]
    pub width: u32,
    #[serde(rename = "MidPointX", default)]
    pub midpoint_x: u32,
    #[serde(rename = "MidPointY", default)]
    pub midpoint_y: u32,
    #[serde(rename = "LengthX", default)]
    pub length_x: u32,
    #[serde(rename = "LengthY", default)]
    pub length_y: u32,
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
        } else if self.home_position.is_some() {
            "看守位设置"
        } else if self.drag_zoom_in.is_some() {
            "拉框放大"
        } else if self.drag_zoom_out.is_some() {
            "拉框缩小"
        } else {
            "未知控制"
        }
    }

    /// 解析 PTZ 8 字节码(`ptz_cmd`,16 位十六进制)的预置位操作。
    ///
    /// GB/T 28181 附录 A.3.1:字节3(指令码)高 4 位为预置位操作类型
    /// (0x81 设置 / 0x82 调用 / 0x83 删除),字节5(数据2)为预置位编号。
    /// 非预置位指令或格式不符返回 None。
    pub fn preset_op(&self) -> Option<(PresetAction, u8)> {
        let hex = self.ptz_cmd.as_ref()?;
        let bytes = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        if bytes.len() < 6 {
            return None;
        }
        let action = match bytes[3] {
            0x81 => PresetAction::Set,
            0x82 => PresetAction::Call,
            0x83 => PresetAction::Delete,
            _ => return None,
        };
        Some((action, bytes[4]))
    }

    /// 解析 PTZ 8 字节码的方向/变倍运动(GB/T 28181 附录 A.3.1)。
    ///
    /// 字节结构:`A5 组合(0F) 地址 指令码 水平速度 垂直速度 变倍速度|校验`。
    /// 指令码(byte[3])位定义:bit0=上、bit1=下、bit2=左、bit3=右、bit4=放大、bit5=缩小;
    /// 全 0 为停止。byte[4]=水平速度、byte[5]=垂直速度、byte[6] 高 4 位=变倍速度(0-15)。
    /// 预置位指令(0x8x)返回 None(用 [`preset_op`](Self::preset_op) 解析)。
    /// 实测 WVP:左 A50F0104...、右 A50F0102... —— 见测试。
    pub fn ptz_motion(&self) -> Option<PtzMotion> {
        let hex = self.ptz_cmd.as_ref()?;
        let bytes = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        if bytes.len() < 7 {
            return None;
        }
        let cmd = bytes[3];
        if cmd & 0x80 != 0 {
            return None; // 预置位等扩展指令,不是运动
        }
        Some(PtzMotion {
            // 指令码位定义(GB/T 28181-2016 附录 A.3.1,实测 WVP 一致):
            // 右=0x01 左=0x02 下=0x04 上=0x08 放大=0x10 缩小=0x20。
            right: cmd & 0x01 != 0,
            left: cmd & 0x02 != 0,
            down: cmd & 0x04 != 0,
            up: cmd & 0x08 != 0,
            zoom_in: cmd & 0x10 != 0,
            zoom_out: cmd & 0x20 != 0,
            pan_speed: bytes[4],
            tilt_speed: bytes[5],
            zoom_speed: bytes[6] >> 4,
        })
    }
}

/// PTZ 方向/变倍运动(供 UI 云台动画;全 false 为停止)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct PtzMotion {
    pub up: bool,
    pub down: bool,
    pub left: bool,
    pub right: bool,
    pub zoom_in: bool,
    pub zoom_out: bool,
    /// 水平速度(0-255)。
    pub pan_speed: u8,
    /// 垂直速度(0-255)。
    pub tilt_speed: u8,
    /// 变倍速度(0-15)。
    pub zoom_speed: u8,
}

impl PtzMotion {
    /// 是否为停止(无任何方向/变倍)。
    pub fn is_stop(&self) -> bool {
        !(self.up || self.down || self.left || self.right || self.zoom_in || self.zoom_out)
    }
}

/// 预置位操作类型(PTZ 指令码解析结果)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetAction {
    /// 设置预置位。
    Set,
    /// 调用(转到)预置位。
    Call,
    /// 删除预置位。
    Delete,
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

/// 基本参数(ConfigDownload/BasicParam 的内容,GB/T 28181 附录 A.2.3)。
/// 设备返回自身注册/心跳相关的基本配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "BasicParam")]
pub struct BasicParam {
    /// 设备名称。
    #[serde(rename = "Name")]
    pub name: String,
    /// 注册有效期(秒)。
    #[serde(rename = "Expiration")]
    pub expiration: u32,
    /// 心跳间隔(秒)。
    #[serde(rename = "HeartBeatInterval")]
    pub heartbeat_interval: u32,
    /// 心跳超时次数(连续未应答判定掉线的阈值)。
    #[serde(rename = "HeartBeatCount")]
    pub heartbeat_count: u32,
}

/// 设备配置查询应答(ConfigDownload,设备 → 平台)。
///
/// 平台下发 `<Query><CmdType>ConfigDownload</CmdType>...<ConfigType>BasicParam</ConfigType></Query>`,
/// 设备回 `<Response><CmdType>ConfigDownload</CmdType>...<BasicParam>...</BasicParam></Response>`。
/// 目前仅实现最常用的 BasicParam;其它 ConfigType(视频参数/SVAC 等)按需扩展。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct ConfigDownloadResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
    /// 基本参数(ConfigType=BasicParam 时携带)。
    #[serde(rename = "BasicParam", skip_serializing_if = "Option::is_none")]
    pub basic_param: Option<BasicParam>,
}

impl ConfigDownloadResponse {
    /// 构造一个 BasicParam 配置应答。
    pub fn basic(
        device_id: impl Into<String>,
        sn: u32,
        name: impl Into<String>,
        expiration: u32,
        heartbeat_interval: u32,
        heartbeat_count: u32,
    ) -> Self {
        ConfigDownloadResponse {
            cmd_type: "ConfigDownload".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
            basic_param: Some(BasicParam {
                name: name.into(),
                expiration,
                heartbeat_interval,
                heartbeat_count,
            }),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("ConfigDownloadResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 预置位项(PresetQuery 应答中的一个预置位)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct PresetItem {
    /// 预置位编号。
    #[serde(rename = "PresetID")]
    pub preset_id: u32,
    /// 预置位名称。
    #[serde(rename = "PresetName")]
    pub preset_name: String,
}

/// 预置位列表容器(带 Num 属性)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresetList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<PresetItem>,
}

/// 预置位查询应答(PresetQuery,设备 → 平台,GB/T 28181 附录 A.2.5.5)。
///
/// 平台下发 `<Query><CmdType>PresetQuery</CmdType>...</Query>`,
/// 设备回 `<Response><CmdType>PresetQuery</CmdType>...<PresetList Num="N">...</PresetList></Response>`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct PresetQueryResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "PresetList")]
    pub preset_list: PresetList,
}

impl PresetQueryResponse {
    /// 用预置位列表构造应答。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<PresetItem>) -> Self {
        let num = items.len() as u32;
        PresetQueryResponse {
            cmd_type: "PresetQuery".into(),
            sn,
            device_id: device_id.into(),
            preset_list: PresetList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("PresetQueryResponse 序列化失败: {e}")))?;
        Ok(format!("{XML_DECL}{body}"))
    }
}

/// 目录订阅通知项:在目录项基础上多一个 `Event` 字段(ON/OFF/ADD/DEL/UPDATE)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CatalogNotifyItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Name")]
    pub name: String,
    /// 变更事件:ON(上线)/OFF(离线)/ADD/DEL/UPDATE。
    #[serde(rename = "Event")]
    pub event: String,
    #[serde(rename = "Status")]
    pub status: String,
}

/// 目录订阅通知列表容器。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogNotifyList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<CatalogNotifyItem>,
}

/// 目录订阅变更通知(设备 → 平台,`<Notify>` 根元素)。
///
/// 平台 `SUBSCRIBE + Catalog` 后,设备用本通知上报目录项的上线/离线/增删。
/// 与 [`CatalogResponse`] 的区别:根元素是 `Notify`(非 `Response`),每项带 `Event`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct CatalogNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "DeviceList")]
    pub device_list: CatalogNotifyList,
}

impl CatalogNotify {
    /// 用设备 ID、SN、变更项列表构造目录通知。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<CatalogNotifyItem>) -> Self {
        let num = items.len() as u32;
        CatalogNotify {
            cmd_type: "Catalog".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            device_list: CatalogNotifyList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        let body = quick_xml::se::to_string(self)
            .map_err(|e| Error::Gb28181(format!("CatalogNotify 序列化失败: {e}")))?;
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
    fn 配置查询应答含基本参数() {
        let resp = ConfigDownloadResponse::basic(
            "35020000001310000001",
            21,
            "UVP-Sim-Desktop",
            3600,
            60,
            3,
        );
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>ConfigDownload</CmdType>"));
        assert!(xml.contains("<SN>21</SN>"));
        assert!(xml.contains("<Result>OK</Result>"));
        assert!(xml.contains("<BasicParam>"));
        assert!(xml.contains("<Name>UVP-Sim-Desktop</Name>"));
        assert!(xml.contains("<Expiration>3600</Expiration>"));
        assert!(xml.contains("<HeartBeatInterval>60</HeartBeatInterval>"));
        assert!(xml.contains("<HeartBeatCount>3</HeartBeatCount>"));
    }

    #[test]
    fn 预置位查询应答含列表() {
        let resp = PresetQueryResponse::new(
            "35020000001310000001",
            88,
            vec![
                PresetItem {
                    preset_id: 1,
                    preset_name: "大门".into(),
                },
                PresetItem {
                    preset_id: 2,
                    preset_name: "停车场".into(),
                },
            ],
        );
        let xml = resp.to_xml().unwrap();
        assert!(xml.contains("<CmdType>PresetQuery</CmdType>"));
        assert!(xml.contains("<PresetList Num=\"2\">"));
        assert!(xml.contains("<PresetID>1</PresetID>"));
        assert!(xml.contains("<PresetName>大门</PresetName>"));
        assert!(xml.contains("<PresetID>2</PresetID>"));
    }

    #[test]
    fn 目录订阅通知含事件字段() {
        let notify = CatalogNotify::new(
            "35020000001310000001",
            5,
            vec![CatalogNotifyItem {
                device_id: "35020000001310000001".into(),
                name: "Camera-1".into(),
                event: "ON".into(),
                status: "ON".into(),
            }],
        );
        let xml = notify.to_xml().unwrap();
        assert!(xml.contains("<Notify>"));
        assert!(xml.contains("<CmdType>Catalog</CmdType>"));
        assert!(xml.contains("<SumNum>1</SumNum>"));
        assert!(xml.contains("<DeviceList Num=\"1\">"));
        assert!(xml.contains("<Event>ON</Event>"));
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
        assert_eq!(c.preset_op(), None); // 非预置位指令
    }

    #[test]
    fn 解析预置位设置调用删除() {
        // 字节3=指令码(81 设置/82 调用/83 删除),字节5=预置位号(0x03)。
        let mk = |cmd: &str| {
            Control::parse(&format!(
                "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
                 <SN>1</SN><DeviceID>d</DeviceID><PTZCmd>{cmd}</PTZCmd></Control>"
            ))
            .unwrap()
        };
        use PresetAction::*;
        assert_eq!(mk("A50F01810300EA").preset_op(), Some((Set, 3)));
        assert_eq!(mk("A50F01820300EB").preset_op(), Some((Call, 3)));
        assert_eq!(mk("A50F01830300EC").preset_op(), Some((Delete, 3)));
    }

    #[test]
    fn 解析ptz方向变倍_实测wvp码() {
        // 实测 WVP 下发码(horizonSpeed/verticalSpeed=150=0x96):
        let mk = |cmd: &str| {
            Control::parse(&format!(
                "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
                 <SN>1</SN><DeviceID>d</DeviceID><PTZCmd>{cmd}</PTZCmd></Control>"
            ))
            .unwrap()
        };
        // 实测 WVP:right=0x01 left=0x02 down=0x04 up=0x08(带标签抓包核对)。
        assert!(mk("A50F0100000000B5").ptz_motion().unwrap().is_stop()); // 停止
        let right = mk("A50F0101969610F2").ptz_motion().unwrap();
        assert!(right.right && !right.left && right.pan_speed == 0x96);
        let left = mk("A50F0102969610F3").ptz_motion().unwrap();
        assert!(left.left && !left.right);
        let down = mk("A50F0104969610F5").ptz_motion().unwrap();
        assert!(down.down && !down.up);
        let up = mk("A50F0108969610F9").ptz_motion().unwrap();
        assert!(up.up && !up.down);
        assert!(mk("A50F011096961001").ptz_motion().unwrap().zoom_in);
        assert!(mk("A50F012096961011").ptz_motion().unwrap().zoom_out);
        // 预置位码不算运动。
        assert_eq!(mk("A50F01810300EA").ptz_motion(), None);
    }

    #[test]
    fn 解析看守位设置() {
        let xml = "<?xml version=\"1.0\"?><Control><CmdType>DeviceControl</CmdType>\
            <SN>7</SN><DeviceID>d</DeviceID>\
            <HomePosition><Enabled>1</Enabled><ResetTime>60</ResetTime><PresetIndex>2</PresetIndex></HomePosition></Control>";
        let c = Control::parse(xml).unwrap();
        assert_eq!(c.kind(), "看守位设置");
        let hp = c.home_position.unwrap();
        assert_eq!(hp.enabled, 1);
        assert_eq!(hp.reset_time, 60);
        assert_eq!(hp.preset_index, 2);
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
