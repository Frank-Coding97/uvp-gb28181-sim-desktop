use common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::*;

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
        manscdp_to_xml(self)
    }
}

/// 在线升级进度通知(DeviceUpgradeResult,设备 → 平台)。
///
/// 4 步进度 percent [0,30,60,100]。percent<100 → Result=0(进行中),
/// percent==100 → Result=1(成功);2=失败(定义但模拟不发)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct DeviceUpgradeResultNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
    #[serde(rename = "Firmware")]
    pub firmware: String,
    /// 0=进行中 1=成功 2=失败。
    #[serde(rename = "Result")]
    pub result: u8,
    /// 进度百分比(0-100)。
    #[serde(rename = "Percent")]
    pub percent: u8,
}

impl DeviceUpgradeResultNotify {
    /// 用会话/固件/进度构造(Result 由 percent 推导)。
    pub fn new(
        device_id: impl Into<String>,
        sn: u32,
        session_id: impl Into<String>,
        firmware: impl Into<String>,
        percent: u8,
    ) -> Self {
        DeviceUpgradeResultNotify {
            cmd_type: "DeviceUpgradeResult".into(),
            sn,
            device_id: device_id.into(),
            session_id: session_id.into(),
            firmware: firmware.into(),
            result: if percent >= 100 { 1 } else { 0 },
            percent,
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 抓拍完成通知(设备 → 平台,GB-2022 §9.5)。
///
/// 主格式:CmdType=Notify + SubCmd=SnapShot。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct SnapShotNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SubCmd")]
    pub sub_cmd: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SessionID")]
    pub session_id: String,
    #[serde(rename = "SnapShotID")]
    pub snap_shot_id: String,
    #[serde(rename = "Time")]
    pub time: String,
    #[serde(rename = "StoragePath")]
    pub storage_path: String,
}

impl SnapShotNotify {
    /// 构造抓拍完成通知。
    pub fn new(
        device_id: impl Into<String>,
        sn: u32,
        session_id: impl Into<String>,
        snap_shot_id: impl Into<String>,
        time: impl Into<String>,
        storage_path: impl Into<String>,
    ) -> Self {
        SnapShotNotify {
            cmd_type: "Notify".into(),
            sub_cmd: "SnapShot".into(),
            sn,
            device_id: device_id.into(),
            session_id: session_id.into(),
            snap_shot_id: snap_shot_id.into(),
            time: time.into(),
            storage_path: storage_path.into(),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 媒体状态通知(MediaStatus,设备 → 平台)。
///
/// NotifyType:121=历史媒体发送结束、122=录像异常、123=存储满。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct MediaStatusNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "NotifyType")]
    pub notify_type: u32,
}

impl MediaStatusNotify {
    /// 历史媒体文件发送结束(121)。
    pub fn finished(device_id: impl Into<String>, sn: u32) -> Self {
        Self::new(device_id, sn, 121)
    }

    /// 用 NotifyType 构造(121/122/123)。
    pub fn new(device_id: impl Into<String>, sn: u32, notify_type: u32) -> Self {
        MediaStatusNotify {
            cmd_type: "MediaStatus".into(),
            sn,
            device_id: device_id.into(),
            notify_type,
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 实时视音频回传通知(VideoUploadNotify,GB28181-2022 A.2.5.8)。
///
/// 设备主动上报"实时视音频已开始回传",供平台感知回传状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Notify")]
pub struct VideoUploadNotify {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 上报通知时间(ISO8601)。
    #[serde(rename = "Time")]
    pub time: String,
    /// 经度(可选)。
    #[serde(rename = "Longitude", skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// 纬度(可选)。
    #[serde(rename = "Latitude", skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
}

impl VideoUploadNotify {
    /// 构造通知(带可选经纬度)。
    pub fn new(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        position: Option<(f64, f64)>,
    ) -> Self {
        VideoUploadNotify {
            cmd_type: "VideoUploadNotify".into(),
            sn,
            device_id: device_id.into(),
            time: time.into(),
            longitude: position.map(|(lng, _)| lng),
            latitude: position.map(|(_, lat)| lat),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
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
    /// 故障设备列表(A.2.5.2)。
    ///
    /// 标准里 `Info` 是**必选**元素(只是其内的 DeviceID 可以 0 个),
    /// 之前完全不输出;严格校验 schema 的平台会拒收心跳,表现为设备一直离线。
    #[serde(rename = "Info")]
    pub info: KeepaliveInfo,
}

/// 心跳里的故障设备列表(A.2.5.2)。空列表表示所有通道正常。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct KeepaliveInfo {
    /// 故障设备/通道编码,可 0..n 个。
    #[serde(rename = "DeviceID", default, skip_serializing_if = "Vec::is_empty")]
    pub faulty_device_ids: Vec<String>,
}

impl Keepalive {
    /// 构造一个 OK 心跳(无故障通道)。
    pub fn ok(device_id: impl Into<String>, sn: u32) -> Self {
        Keepalive {
            cmd_type: "Keepalive".into(),
            sn,
            device_id: device_id.into(),
            status: "OK".into(),
            info: KeepaliveInfo::default(),
        }
    }

    /// 构造一个携带故障通道列表的心跳(有故障时 Status=ERROR)。
    pub fn with_faults(device_id: impl Into<String>, sn: u32, faulty: Vec<String>) -> Self {
        Keepalive {
            cmd_type: "Keepalive".into(),
            sn,
            device_id: device_id.into(),
            status: if faulty.is_empty() { "OK" } else { "ERROR" }.into(),
            info: KeepaliveInfo {
                faulty_device_ids: faulty,
            },
        }
    }

    /// 序列化为完整 XML(含 XML 声明)。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 从 XML 解析心跳(用于自检/测试往返;设备侧一般只发不收)。
pub fn parse_keepalive(xml: &str) -> Result<Keepalive> {
    quick_xml::de::from_str(strip_xml_decl(xml))
        .map_err(|e| Error::Gb28181(format!("Keepalive 解析失败: {e}")))
}
