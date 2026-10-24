use common::Result;
use serde::{Deserialize, Serialize};

use super::*;

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
        manscdp_to_xml(self)
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
    /// 不正常工作原因(可选)。
    #[serde(rename = "Reason", default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 是否编码:ON / OFF(可选)。
    #[serde(rename = "Encode", default, skip_serializing_if = "Option::is_none")]
    pub encode: Option<String>,
    /// 是否录像:ON / OFF(可选)。
    #[serde(rename = "Record", default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
    /// 设备时间和日期(可选)。
    ///
    /// 之前完全不输出。平台用它显示设备时钟、判断是否需要校时;我们本来就做了
    /// 网络校时,报一个真实值几乎零成本。
    #[serde(
        rename = "DeviceTime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub device_time: Option<String>,
    /// 报警设备状态列表(可选,A.2.6.6)。
    #[serde(
        rename = "Alarmstatus",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub alarm_status: Option<DeviceStatusAlarmList>,
}

/// [`DeviceStatusResponse`] 的报警设备状态列表(A.2.6.6)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStatusAlarmList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<DeviceStatusAlarmItem>,
}

/// 单个报警设备状态项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceStatusAlarmItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// ONDUTY / OFFDUTY / ALARM。
    #[serde(rename = "DutyStatus")]
    pub duty_status: String,
}

impl DeviceStatusResponse {
    /// 构造一条"在线且工作正常"的设备状态应答。
    ///
    /// `device_time` 传校时后的 ISO8601 时间;`alarm_channels` 为报警通道 ID 列表
    /// (按当前布防态给出 ALARM / OFFDUTY)。
    pub fn online(
        device_id: impl Into<String>,
        sn: u32,
        device_time: Option<String>,
        recording: bool,
        alarm_channels: &[String],
        alarming: bool,
    ) -> Self {
        let duty = if alarming { "ALARM" } else { "OFFDUTY" };
        let alarm_status = (!alarm_channels.is_empty()).then(|| DeviceStatusAlarmList {
            num: alarm_channels.len() as u32,
            items: alarm_channels
                .iter()
                .map(|id| DeviceStatusAlarmItem {
                    device_id: id.clone(),
                    duty_status: duty.into(),
                })
                .collect(),
        });
        DeviceStatusResponse {
            cmd_type: "DeviceStatus".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
            online: "ONLINE".into(),
            status: "OK".into(),
            reason: None,
            // 设备一直在编码(推流靠它),录像态跟随录像控制命令。
            encode: Some("ON".into()),
            record: Some(if recording { "ON" } else { "OFF" }.into()),
            device_time,
            alarm_status,
        }
    }
}

impl DeviceStatusResponse {
    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// 经度(可选)。GPS 报警(AlarmMethod=4)时应携带。
    #[serde(rename = "Longitude", default, skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    /// 纬度(可选)。
    #[serde(rename = "Latitude", default, skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    /// 扩展 Info:报警类型与类型参数(A.2.5.3)。
    ///
    /// 之前完全不输出 —— 视频报警(AlarmMethod=5)下平台只能显示"未知视频报警",
    /// 无法区分运动侦测 / 入侵 / 越界等,这正是 `AlarmType` 的作用。
    #[serde(rename = "Info", default, skip_serializing_if = "Option::is_none")]
    pub info: Option<AlarmNotifyInfo>,
}

/// 报警通知的扩展 Info(A.2.5.3)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlarmNotifyInfo {
    /// 报警类型。含义取决于 `AlarmMethod`:
    /// 方式 5(视频报警)下 1=人工 2=运动目标检测 3=遗留物 4=物体移除 5=绊线
    /// 6=入侵 7=逆行 8=徘徊 9=流量统计 10=密度 11=视频异常 12=快速移动 13=图像遮挡。
    #[serde(rename = "AlarmType")]
    pub alarm_type: u32,
    /// 报警类型扩展参数(入侵检测时携带 EventType:1 进入区域 / 2 离开区域)。
    #[serde(
        rename = "AlarmTypeParam",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub alarm_type_param: Option<AlarmTypeParam>,
}

/// 报警类型扩展参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmTypeParam {
    /// 1=进入区域;2=离开区域。
    #[serde(rename = "EventType")]
    pub event_type: u32,
}

/// 视频报警的报警类型(A.2.5.3,AlarmMethod=5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VideoAlarmType {
    /// 1 人工视频报警。
    Manual = 1,
    /// 2 运动目标检测报警(最常用)。
    MotionDetection = 2,
    /// 3 遗留物检测。
    ObjectLeft = 3,
    /// 4 物体移除检测。
    ObjectRemoved = 4,
    /// 5 绊线检测。
    TripwireCrossing = 5,
    /// 6 入侵检测。
    Intrusion = 6,
    /// 7 逆行检测。
    WrongWay = 7,
    /// 8 徘徊检测。
    Loitering = 8,
    /// 9 流量统计。
    FlowStatistics = 9,
    /// 10 密度检测。
    DensityDetection = 10,
    /// 11 视频异常检测。
    VideoAbnormal = 11,
    /// 12 快速移动。
    FastMoving = 12,
    /// 13 图像遮挡。
    Tampering = 13,
}

impl AlarmNotify {
    /// 构造一条视频侦测报警(AlarmMethod=5),默认类型为运动目标检测。
    pub fn video(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        desc: impl Into<String>,
    ) -> Self {
        Self::video_typed(
            device_id,
            sn,
            time,
            desc,
            VideoAlarmType::MotionDetection,
            None,
        )
    }

    /// 构造一条指定类型的视频报警。
    ///
    /// `position` 为可选经纬度。平台靠 `Info/AlarmType` 区分具体的视频报警类型,
    /// 不带这一项时只能显示成"未知视频报警"。
    pub fn video_typed(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        desc: impl Into<String>,
        alarm_type: VideoAlarmType,
        position: Option<(f64, f64)>,
    ) -> Self {
        AlarmNotify {
            cmd_type: "Alarm".into(),
            sn,
            device_id: device_id.into(),
            priority: 1,
            method: 5,
            time: time.into(),
            description: Some(desc.into()),
            longitude: position.map(|(lon, _)| lon),
            latitude: position.map(|(_, lat)| lat),
            info: Some(AlarmNotifyInfo {
                alarm_type: alarm_type as u32,
                alarm_type_param: None,
            }),
        }
    }

    /// 构造一条 GPS 报警(AlarmMethod=4),携带经纬度。
    pub fn gps(
        device_id: impl Into<String>,
        sn: u32,
        time: impl Into<String>,
        longitude: f64,
        latitude: f64,
    ) -> Self {
        AlarmNotify {
            cmd_type: "Alarm".into(),
            sn,
            device_id: device_id.into(),
            priority: 1,
            method: 4,
            time: time.into(),
            description: None,
            longitude: Some(longitude),
            latitude: Some(latitude),
            info: None,
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
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
    /// 文件路径名(可选)。
    #[serde(rename = "FilePath", default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// 录像地址(可选)。
    #[serde(rename = "Address", default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// 开始时间(ISO8601)。
    #[serde(rename = "StartTime")]
    pub start_time: String,
    /// 结束时间(ISO8601)。
    #[serde(rename = "EndTime")]
    pub end_time: String,
    /// 保密属性:0 不涉密 / 1 涉密。
    ///
    /// GB/T 28181-2022 A.2.1.10 里这是**必选**字段,之前完全没输出 ——
    /// 严格校验 schema 的平台会直接拒收整条录像应答。
    #[serde(rename = "Secrecy")]
    pub secrecy: u8,
    /// 录像类型:time(定时)/ alarm / manual。
    #[serde(rename = "Type")]
    pub kind: String,
    /// 录像触发者 ID(可选)。
    #[serde(
        rename = "RecorderID",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub recorder_id: Option<String>,
    /// 录像文件大小(字节,可选)。
    #[serde(rename = "FileSize", default, skip_serializing_if = "Option::is_none")]
    pub file_size: Option<String>,
    /// 码流类型:0 主码流 / 1 子码流1 …(可选)。
    #[serde(
        rename = "StreamNumber",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub stream_number: Option<u32>,
}

impl RecordItem {
    /// 构造一条不涉密的定时录像项(模拟器最常用的形态)。
    pub fn timed(
        device_id: impl Into<String>,
        name: impl Into<String>,
        start_time: impl Into<String>,
        end_time: impl Into<String>,
    ) -> Self {
        RecordItem {
            device_id: device_id.into(),
            name: name.into(),
            file_path: None,
            address: None,
            start_time: start_time.into(),
            end_time: end_time.into(),
            secrecy: 0,
            kind: "time".into(),
            recorder_id: None,
            file_size: None,
            stream_number: None,
        }
    }
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
    /// 设备/区域名称(GB28181-2022 A.2.6.7 必选),位于 SumNum 前。
    #[serde(rename = "Name")]
    pub name: String,
    /// 录像总数。
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    /// 录像列表容器。附录 M:空结果时不携带记录列表(见 CatalogResponse 的说明)。
    #[serde(rename = "RecordList", skip_serializing_if = "Option::is_none")]
    pub record_list: Option<RecordList>,
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
    /// 用设备 ID、SN、录像段构造应答(名称默认取 DeviceID)。
    pub fn new(device_id: impl Into<String>, sn: u32, items: Vec<RecordItem>) -> Self {
        let device_id = device_id.into();
        let name = device_id.clone();
        Self::with_name(device_id, name, sn, items)
    }

    /// 用设备 ID、名称、SN、录像段构造应答(A.2.6.7 Name 必选)。
    pub fn with_name(
        device_id: impl Into<String>,
        name: impl Into<String>,
        sn: u32,
        items: Vec<RecordItem>,
    ) -> Self {
        let num = items.len() as u32;
        RecordInfoResponse {
            cmd_type: "RecordInfo".into(),
            sn,
            device_id: device_id.into(),
            name: name.into(),
            sum_num: num,
            // 附录 M:空结果时不携带 <RecordList>(标准原文给的正是文件查询示例)。
            record_list: (!items.is_empty()).then_some(RecordList { num, items }),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 基本参数(ConfigDownload/BasicParam 的内容,GB/T 28181 附录 A.2.3)。
/// 设备返回自身注册/心跳相关的基本配置。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "BasicParam")]
pub struct BasicParam {
    /// 设备名称。
    #[serde(rename = "Name", default)]
    pub name: String,
    /// 注册有效期(秒)。配置修改命令可只带部分字段,故默认 0。
    #[serde(rename = "Expiration", default)]
    pub expiration: u32,
    /// 心跳间隔(秒)。
    #[serde(rename = "HeartBeatInterval", default)]
    pub heartbeat_interval: u32,
    /// 心跳超时次数(连续未应答判定掉线的阈值)。
    #[serde(rename = "HeartBeatCount", default)]
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
    /// 视频参数(ConfigType=VideoParamOpt 时携带,GB-2022)。
    #[serde(rename = "VideoParamOpt", skip_serializing_if = "Option::is_none")]
    pub video_param_opt: Option<VideoParamOpt>,
    /// 录像计划(ConfigType=VideoRecordPlan 时携带,GB-2022)。
    #[serde(rename = "VideoRecordPlan", skip_serializing_if = "Option::is_none")]
    pub video_record_plan: Option<VideoRecordPlan>,
    /// 报警录像(ConfigType=VideoAlarmRecord 时携带,GB-2022)。
    #[serde(rename = "VideoAlarmRecord", skip_serializing_if = "Option::is_none")]
    pub video_alarm_record: Option<VideoAlarmRecord>,
    /// 视频画面遮挡(ConfigType=PictureMask 时携带,GB-2022)。
    #[serde(rename = "PictureMask", skip_serializing_if = "Option::is_none")]
    pub picture_mask: Option<PictureMask>,
    /// 画面翻转(ConfigType=FrameMirror 时携带,GB-2022)。
    #[serde(rename = "FrameMirror", skip_serializing_if = "Option::is_none")]
    pub frame_mirror: Option<FrameMirror>,
    /// 报警上报开关(ConfigType=AlarmReport 时携带,GB-2022)。
    #[serde(rename = "AlarmReport", skip_serializing_if = "Option::is_none")]
    pub alarm_report: Option<AlarmReport>,
    /// 前端OSD配置(ConfigType=OSDConfig 时携带,GB-2022)。
    #[serde(rename = "OSDConfig", skip_serializing_if = "Option::is_none")]
    pub osd_config: Option<OSDConfig>,
    /// 图像抓拍配置(ConfigType=SnapShotConfig 时携带,GB-2022)。
    #[serde(rename = "SnapShotConfig", skip_serializing_if = "Option::is_none")]
    pub snap_shot_config: Option<SnapShotCfg>,
    /// 视频参数属性(ConfigType=VideoParamAttribute 时携带,GB-2022)。
    #[serde(
        rename = "VideoParamAttribute",
        skip_serializing_if = "Option::is_none"
    )]
    pub video_param_attribute: Option<VideoParamAttribute>,
    /// SVAC 编码配置(ConfigType=SVACEncodeConfig 时携带,占位)。
    #[serde(rename = "SVACEncodeConfig", skip_serializing_if = "Option::is_none")]
    pub svac_encode_config: Option<SvacConfig>,
    /// SVAC 解码配置(ConfigType=SVACDecodeConfig 时携带,占位)。
    #[serde(rename = "SVACDecodeConfig", skip_serializing_if = "Option::is_none")]
    pub svac_decode_config: Option<SvacConfig>,
}

/// 视频参数(ConfigDownload/VideoParamOpt 的内容,GB-2022)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "VideoParamOpt")]
pub struct VideoParamOpt {
    /// 支持的下载倍速,斜杠分隔(固定 "1/2/4",与上游一致)。
    #[serde(rename = "DownloadSpeed")]
    pub download_speed: String,
    /// 分辨率标签(如 "1920*1080")。
    #[serde(rename = "Resolution")]
    pub resolution: String,
}

/// 视频参数属性配置(VideoParamAttribute,GB-2022 A.2.1.13/A.2.3.2.5)。
/// 描述各码流的编码格式/分辨率/帧率/码率。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "VideoParamAttribute")]
pub struct VideoParamAttribute {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<VideoParamAttributeItem>,
}

/// 视频参数属性项(某一码流的参数,GB-2022 A.2.1.13)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct VideoParamAttributeItem {
    /// 视频流编号:0=主码流,1=子码流1,2=子码流2。
    #[serde(rename = "StreamNumber")]
    pub stream_number: u32,
    /// 视频编码格式(SDP f 字段规定)。
    #[serde(rename = "VideoFormat")]
    pub video_format: String,
    /// 分辨率当前配置值。
    #[serde(rename = "Resolution")]
    pub resolution: String,
    /// 帧率当前配置值。
    #[serde(rename = "FrameRate")]
    pub frame_rate: String,
    /// 码率类型配置值。
    #[serde(rename = "BitRateType")]
    pub bit_rate_type: String,
    /// 视频码率(固定码率时携带)。
    #[serde(rename = "VideoBitRate", skip_serializing_if = "Option::is_none")]
    pub video_bit_rate: Option<String>,
}

/// SVAC 编码/解码配置占位(GB-2022 A.2.1.21/22)。
/// SVAC 结构复杂(ROI 等),平台极少用;此处最小合规占位(仅 Result 语义)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SvacConfig {
    /// 配置标识占位(1=已配置)。
    #[serde(rename = "Configured", skip_serializing_if = "Option::is_none")]
    pub configured: Option<u32>,
}

/// 录像计划(ConfigDownload/VideoRecordPlan,GB-2022 A.2.3.2.6)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "VideoRecordPlan")]
pub struct VideoRecordPlan {
    /// 录像计划类型:0=定时录像,1=事件触发录像,2=手动录像。
    #[serde(rename = "RecordPlanType")]
    pub record_plan_type: u32,
}

/// 报警录像(ConfigDownload/VideoAlarmRecord,GB-2022 A.2.3.2.7)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "VideoAlarmRecord")]
pub struct VideoAlarmRecord {
    /// 报警录像时长(秒)。
    #[serde(rename = "Duration")]
    pub duration: u32,
}

/// 视频画面遮挡(ConfigDownload/PictureMask,GB-2022 A.2.3.2.8)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "PictureMask")]
pub struct PictureMask {
    /// 是否启用画面遮挡:0=关闭,1=开启。
    #[serde(rename = "Enabled")]
    pub enabled: u32,
}

/// 画面翻转(ConfigDownload/FrameMirror,GB-2022 A.2.1.23)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "FrameMirror")]
pub struct FrameMirror {
    /// 翻转模式:0=不启用,1=水平镜像,2=上下镜像,3=中心镜像。
    #[serde(rename = "Mode")]
    pub mode: u32,
}

/// 报警上报开关(ConfigDownload/AlarmReport,GB-2022 A.2.3.2.10)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "AlarmReport")]
pub struct AlarmReport {
    /// 是否启用报警上报:0=关闭,1=开启。
    #[serde(rename = "Enabled")]
    pub enabled: u32,
}

/// 前端OSD配置(ConfigDownload/OSDConfig,GB-2022 A.2.3.2.11)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "OSDConfig")]
pub struct OSDConfig {
    /// 绝对时间信息显示开关:0=关闭,1=开启。
    #[serde(rename = "TimeShowFlag")]
    pub time_show_flag: u32,
    /// OSD信息显示开关:0=关闭,1=开启。
    #[serde(rename = "OSDShowFlag")]
    pub osd_show_flag: u32,
}

/// 图像抓拍配置(ConfigDownload/SnapShotConfig,GB-2022 A.2.1.24)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "SnapShotConfig")]
pub struct SnapShotCfg {
    /// 连拍张数(1-10)。
    #[serde(rename = "SnapNum")]
    pub snap_num: u32,
    /// 单张抓拍间隔时间(秒)。
    #[serde(rename = "Interval")]
    pub interval: u32,
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
            video_param_opt: None,
            video_record_plan: None,
            video_alarm_record: None,
            picture_mask: None,
            frame_mirror: None,
            alarm_report: None,
            osd_config: None,
            snap_shot_config: None,
            video_param_attribute: None,
            svac_encode_config: None,
            svac_decode_config: None,
        }
    }

    /// 按 ConfigType(斜杠分隔可组合)构造应答,只输出被请求的块。
    /// 支持 GB28181-2022 附录 A.2.4.7 定义的全部 ConfigType。
    #[allow(clippy::too_many_arguments)]
    pub fn by_type(
        device_id: impl Into<String>,
        sn: u32,
        config_type: &str,
        name: impl Into<String>,
        expiration: u32,
        heartbeat_interval: u32,
        heartbeat_count: u32,
        resolution: impl Into<String>,
    ) -> Self {
        let lower = config_type.to_ascii_lowercase();
        // 缺省(空 ConfigType)按 BasicParam 处理,兼容旧行为。
        let want_basic = lower.is_empty() || lower.contains("basicparam");
        let want_video = lower.contains("videoparamopt");
        let want_record_plan = lower.contains("videorecordplan");
        let want_alarm_record = lower.contains("videoalarmrecord");
        let want_picture_mask = lower.contains("picturemask");
        let want_frame_mirror = lower.contains("framemirror");
        let want_alarm_report = lower.contains("alarmreport");
        let want_osd = lower.contains("osdconfig");
        let want_snapshot = lower.contains("snapshotconfig");
        let want_vpa = lower.contains("videoparamattribute");
        let want_svac_enc = lower.contains("svacencodeconfig");
        let want_svac_dec = lower.contains("svacdecodeconfig");
        let device_id = device_id.into();
        ConfigDownloadResponse {
            cmd_type: "ConfigDownload".into(),
            sn,
            device_id,
            result: "OK".into(),
            basic_param: want_basic.then(|| BasicParam {
                name: name.into(),
                expiration,
                heartbeat_interval,
                heartbeat_count,
            }),
            video_param_opt: want_video.then(|| VideoParamOpt {
                download_speed: "1/2/4".into(),
                resolution: resolution.into(),
            }),
            video_record_plan: want_record_plan.then_some(VideoRecordPlan {
                record_plan_type: 0,
            }),
            video_alarm_record: want_alarm_record.then_some(VideoAlarmRecord { duration: 30 }),
            picture_mask: want_picture_mask.then_some(PictureMask { enabled: 0 }),
            frame_mirror: want_frame_mirror.then_some(FrameMirror { mode: 0 }),
            alarm_report: want_alarm_report.then_some(AlarmReport { enabled: 1 }),
            osd_config: want_osd.then_some(OSDConfig {
                time_show_flag: 1,
                osd_show_flag: 1,
            }),
            snap_shot_config: want_snapshot.then_some(SnapShotCfg {
                snap_num: 1,
                interval: 1,
            }),
            video_param_attribute: want_vpa.then(|| VideoParamAttribute {
                num: 1,
                items: vec![VideoParamAttributeItem {
                    stream_number: 0,
                    video_format: "H.264".into(),
                    resolution: "1920*1080".into(),
                    frame_rate: "25".into(),
                    bit_rate_type: "CBR".into(),
                    video_bit_rate: Some("4096".into()),
                }],
            }),
            svac_encode_config: want_svac_enc.then_some(SvacConfig {
                configured: Some(0),
            }),
            svac_decode_config: want_svac_dec.then_some(SvacConfig {
                configured: Some(0),
            }),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
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
        manscdp_to_xml(self)
    }
}

/// 报警状态查询应答项(AlarmStatus,GB-2022 Item)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct AlarmStatusItem {
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 值守状态:ALARM(报警中)/ OFFDUTY(未布防)。
    #[serde(rename = "DutyStatus")]
    pub duty_status: String,
}

/// 报警状态查询应答(AlarmStatus,设备 → 平台)。
///
/// GB-2022 用 `Num`+`Item`(每报警通道一项 DutyStatus);
/// GB-2016 用单个 `NotNumber`(0/1)。用 [`AlarmStatusResponse::new`] 按版本构造。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct AlarmStatusResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "Result")]
    pub result: String,
    /// GB-2016:未处理报警数(0/1)。2022 版为 None。
    #[serde(rename = "NotNumber", skip_serializing_if = "Option::is_none")]
    pub not_number: Option<u32>,
    /// GB-2022:报警项数。2016 版为 None。
    #[serde(rename = "Num", skip_serializing_if = "Option::is_none")]
    pub num: Option<u32>,
    /// GB-2022:各报警通道值守状态。2016 版为空。
    #[serde(rename = "Item", default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<AlarmStatusItem>,
}

impl AlarmStatusResponse {
    /// 按版本构造。`alarming` 为当前是否处于报警/布防态。
    /// 2022 版每报警通道一项 DutyStatus;2016 版用 NotNumber。
    pub fn new(
        device_id: impl Into<String>,
        sn: u32,
        alarm_channels: &[String],
        alarming: bool,
        is_2022: bool,
    ) -> Self {
        let duty = if alarming { "ALARM" } else { "OFFDUTY" };
        if is_2022 {
            let items: Vec<AlarmStatusItem> = alarm_channels
                .iter()
                .map(|id| AlarmStatusItem {
                    device_id: id.clone(),
                    duty_status: duty.into(),
                })
                .collect();
            AlarmStatusResponse {
                cmd_type: "AlarmStatus".into(),
                sn,
                device_id: device_id.into(),
                result: "OK".into(),
                not_number: None,
                num: Some(items.len() as u32),
                items,
            }
        } else {
            AlarmStatusResponse {
                cmd_type: "AlarmStatus".into(),
                sn,
                device_id: device_id.into(),
                result: "OK".into(),
                not_number: Some(alarming as u32),
                num: None,
                items: Vec::new(),
            }
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 看守位查询应答(HomePositionQuery,设备 → 平台)。
///
/// ResetTime 固定 30(平台下发不落存);PresetIndex 表"有无看守位"(1/0),非真实编号
///(与上游 uvp-gb28181-sim 一致)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct HomePositionQueryResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 是否启用看守位(1/0)。
    #[serde(rename = "Enabled")]
    pub enabled: u8,
    /// 自动归位时间(秒),固定 30。
    #[serde(rename = "ResetTime")]
    pub reset_time: u32,
    /// 归位预置位标志(有看守位=1,否则 0)。
    #[serde(rename = "PresetIndex")]
    pub preset_index: u32,
}

impl HomePositionQueryResponse {
    /// 用是否启用/是否已设看守位构造。
    pub fn new(device_id: impl Into<String>, sn: u32, enabled: bool, has_home: bool) -> Self {
        HomePositionQueryResponse {
            cmd_type: "HomePositionQuery".into(),
            sn,
            device_id: device_id.into(),
            enabled: enabled as u8,
            reset_time: 30,
            preset_index: has_home as u32,
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 存储卡状态项(SDCardStatus Item)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct StorageCardItem {
    #[serde(rename = "CardNum")]
    pub card_num: u32,
    /// 状态:Normal / Abnormal / NoDisk 等。
    #[serde(rename = "Status")]
    pub status: String,
    /// 总容量(MB)。
    #[serde(rename = "TotalCapacity")]
    pub total_capacity: u64,
    /// 剩余容量(MB)。
    #[serde(rename = "RemainingSpace")]
    pub remaining_space: u64,
}

/// 存储卡列表(带 Num 属性)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default)]
    pub items: Vec<StorageCardItem>,
}

/// 存储卡状态查询应答(SDCardStatus,设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct StorageCardStatusResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "StorageList")]
    pub storage_list: StorageList,
}

impl StorageCardStatusResponse {
    /// 构造(模拟单张 32G 卡,余 24G,与上游一致)。
    pub fn mock(device_id: impl Into<String>, sn: u32) -> Self {
        let items = vec![StorageCardItem {
            card_num: 0,
            status: "Normal".into(),
            total_capacity: 32768,
            remaining_space: 24576,
        }];
        let num = items.len() as u32;
        StorageCardStatusResponse {
            cmd_type: "SDCardStatus".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            storage_list: StorageList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 巡航轨迹列表项(CruiseTrackListQuery Item)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CruiseTrackListItem {
    #[serde(rename = "GroupID")]
    pub group_id: u32,
    #[serde(rename = "Name")]
    pub name: String,
}

/// 巡航轨迹列表(带 Num 属性)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<CruiseTrackListItem>,
}

/// 巡航轨迹列表查询应答(CruiseTrackListQuery,设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct CruiseTrackListResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "TrackList")]
    pub track_list: TrackList,
}

impl CruiseTrackListResponse {
    /// 用轨迹号列表构造(名称固定 "巡航 {号}")。
    pub fn new(device_id: impl Into<String>, sn: u32, track_ids: &[u32]) -> Self {
        let items: Vec<CruiseTrackListItem> = track_ids
            .iter()
            .map(|&t| CruiseTrackListItem {
                group_id: t,
                name: format!("巡航 {t}"),
            })
            .collect();
        let num = items.len() as u32;
        CruiseTrackListResponse {
            cmd_type: "CruiseTrackListQuery".into(),
            sn,
            device_id: device_id.into(),
            sum_num: num,
            track_list: TrackList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// 巡航轨迹详情预置点(CruiseTrackQuery Item;Speed/DwellTime 固定 5/3)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Item")]
pub struct CruisePresetItem {
    #[serde(rename = "PresetID")]
    pub preset_id: u32,
    #[serde(rename = "Speed")]
    pub speed: u32,
    #[serde(rename = "DwellTime")]
    pub dwell_time: u32,
}

/// 巡航详情预置点列表(带 Num 属性)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CruisePresetList {
    #[serde(rename = "@Num")]
    pub num: u32,
    #[serde(rename = "Item", default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<CruisePresetItem>,
}

/// 巡航轨迹详情查询应答(CruiseTrackQuery,设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct CruiseTrackQueryResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    #[serde(rename = "GroupID")]
    pub group_id: u32,
    #[serde(rename = "SumNum")]
    pub sum_num: u32,
    #[serde(rename = "PresetList")]
    pub preset_list: CruisePresetList,
}

impl CruiseTrackQueryResponse {
    /// 用轨迹号与该轨迹的预置点列表构造(Speed/DwellTime 固定 5/3)。
    pub fn new(device_id: impl Into<String>, sn: u32, group_id: u32, preset_ids: &[u32]) -> Self {
        let items: Vec<CruisePresetItem> = preset_ids
            .iter()
            .map(|&p| CruisePresetItem {
                preset_id: p,
                speed: 5,
                dwell_time: 3,
            })
            .collect();
        let num = items.len() as u32;
        CruiseTrackQueryResponse {
            cmd_type: "CruiseTrackQuery".into(),
            sn,
            device_id: device_id.into(),
            group_id,
            sum_num: num,
            preset_list: CruisePresetList { num, items },
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}

/// PTZ 精准状态查询应答(PTZPosition,GB-2022,设备 → 平台)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Response")]
pub struct PtzPreciseStatusResponse {
    #[serde(rename = "CmdType")]
    pub cmd_type: String,
    #[serde(rename = "SN")]
    pub sn: u32,
    #[serde(rename = "DeviceID")]
    pub device_id: String,
    /// 水平角(度,%.2f)。
    #[serde(rename = "Pan")]
    pub pan: String,
    /// 俯仰角(度,%.2f)。
    #[serde(rename = "Tilt")]
    pub tilt: String,
    /// 变倍(≥1.00,%.2f)。
    #[serde(rename = "Zoom")]
    pub zoom: String,
}

impl PtzPreciseStatusResponse {
    /// 用当前姿态构造(格式化为 %.2f)。
    pub fn new(device_id: impl Into<String>, sn: u32, pan: f32, tilt: f32, zoom: f32) -> Self {
        PtzPreciseStatusResponse {
            cmd_type: "PTZPosition".into(),
            sn,
            device_id: device_id.into(),
            pan: format!("{pan:.2}"),
            tilt: format!("{tilt:.2}"),
            zoom: format!("{zoom:.2}"),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}
