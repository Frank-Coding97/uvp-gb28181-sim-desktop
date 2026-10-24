use common::{Error, Result};
use serde::{Deserialize, Serialize};

use super::*;

/// 平台 → 设备的控制命令(Control)。GB28181 §A.2.4:PTZ/录像/布防/校时/重启/关键帧等。
/// 各命令是可选子元素,按出现的字段判断具体控制类型。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
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
    /// PTZ 云台控制附加参数(GB28181-2022 A.2.3.1.2)。
    #[serde(rename = "PTZCmdParams", skip_serializing_if = "Option::is_none")]
    pub ptz_cmd_params: Option<PTZCmdParams>,
    /// 强制关键帧,值 "Send"。
    /// 2016 版拼写 `IFameCmd`(少个 r,多数设备沿用);GB28181-2022 A.2.3.1.7 为 `IFrameCmd`。
    /// 用 alias 同时接受两种拼写,序列化用 2016 拼写(WVP 已验证)。
    #[serde(
        rename = "IFameCmd",
        alias = "IFrameCmd",
        skip_serializing_if = "Option::is_none"
    )]
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
    /// 报警复位扩展信息(GB28181-2022 A.2.3.1.6):报警方式、报警类型。
    #[serde(rename = "Info", skip_serializing_if = "Option::is_none")]
    pub alarm_info: Option<AlarmInfo>,
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
    /// 精确云台控制(PTZPreciseCtrl),GB-2022:Pan/Tilt/Zoom 浮点。
    #[serde(rename = "PTZPreciseCtrl", skip_serializing_if = "Option::is_none")]
    pub ptz_precise_ctrl: Option<PtzPreciseCtrl>,
    /// 目标跟踪(TargetTrack),GB-2022:Mode/ObjectID/Speed。
    #[serde(rename = "TargetTrack", skip_serializing_if = "Option::is_none")]
    pub target_track: Option<TargetTrack>,
    /// 格式化 SD 卡(FormatSDCard):值为卡号或占位;卡号也可由 DiskNum 指定。
    #[serde(rename = "FormatSDCard", skip_serializing_if = "Option::is_none")]
    pub format_sd_card: Option<String>,
    /// 格式化卡号(与 FormatSDCard 配套,GB-2022)。
    #[serde(rename = "DiskNum", skip_serializing_if = "Option::is_none")]
    pub disk_num: Option<u32>,
    /// 平台下发抓拍(7.4 旧路径),值为任意占位串;触发后经 Alarm Notify 上报。
    #[serde(rename = "SnapShotCmd", skip_serializing_if = "Option::is_none")]
    pub snap_shot_cmd: Option<String>,
    /// 抓拍配置(GB-2022 §9.5):JPEG 经 HTTP 上传 + 完成 NOTIFY。
    #[serde(rename = "SnapShotConfig", skip_serializing_if = "Option::is_none")]
    pub snap_shot_config: Option<SnapShotConfig>,
    /// 在线升级(DeviceUpgrade):4 步进度 NOTIFY。
    #[serde(rename = "DeviceUpgrade", skip_serializing_if = "Option::is_none")]
    pub device_upgrade: Option<DeviceUpgrade>,
    // ── DeviceConfig 控制命令字段(GB28181-2022 A.2.3.2) ──
    /// 基本参数配置。
    #[serde(rename = "BasicParam", skip_serializing_if = "Option::is_none")]
    pub cfg_basic_param: Option<BasicParam>,
    /// 录像计划配置。
    #[serde(rename = "VideoRecordPlan", skip_serializing_if = "Option::is_none")]
    pub cfg_video_record_plan: Option<VideoRecordPlan>,
    /// 报警录像配置。
    #[serde(rename = "VideoAlarmRecord", skip_serializing_if = "Option::is_none")]
    pub cfg_video_alarm_record: Option<VideoAlarmRecord>,
    /// 视频画面遮挡配置。
    #[serde(rename = "PictureMask", skip_serializing_if = "Option::is_none")]
    pub cfg_picture_mask: Option<PictureMask>,
    /// 画面翻转配置。
    #[serde(rename = "FrameMirror", skip_serializing_if = "Option::is_none")]
    pub cfg_frame_mirror: Option<FrameMirror>,
    /// 报警上报开关配置。
    #[serde(rename = "AlarmReport", skip_serializing_if = "Option::is_none")]
    pub cfg_alarm_report: Option<AlarmReport>,
    /// 前端OSD配置。
    #[serde(rename = "OSDConfig", skip_serializing_if = "Option::is_none")]
    pub cfg_osd_config: Option<OSDConfig>,
    /// 视频参数属性配置(A.2.3.2.5)。
    #[serde(
        rename = "VideoParamAttribute",
        skip_serializing_if = "Option::is_none"
    )]
    pub cfg_video_param_attribute: Option<VideoParamAttribute>,
    /// SVAC 编码配置(A.2.3.2.3)。
    #[serde(rename = "SVACEncodeConfig", skip_serializing_if = "Option::is_none")]
    pub cfg_svac_encode: Option<SvacConfig>,
    /// SVAC 解码配置(A.2.3.2.4)。
    #[serde(rename = "SVACDecodeConfig", skip_serializing_if = "Option::is_none")]
    pub cfg_svac_decode: Option<SvacConfig>,
}

/// 抓拍配置参数(SnapShotConfig 子元素,GB-2022 §9.5)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapShotConfig {
    /// 会话标识(必填)。
    #[serde(rename = "SessionID", default)]
    pub session_id: String,
    /// 图片上传地址(必填,http/https)。
    #[serde(rename = "UploadURL", default)]
    pub upload_url: String,
    /// 抓拍张数(1-10,越界钳制)。
    #[serde(rename = "SnapNum", default)]
    pub snap_num: u32,
    /// 抓拍间隔(秒)。
    #[serde(rename = "Interval", default)]
    pub interval: u32,
}

impl SnapShotConfig {
    /// 会话标识与上传地址非空视为有效。
    pub fn is_valid(&self) -> bool {
        !self.session_id.is_empty() && !self.upload_url.is_empty()
    }

    /// 钳制后的抓拍张数(1-10)。
    pub fn clamped_num(&self) -> u32 {
        self.snap_num.clamp(1, 10)
    }
}

/// 在线升级参数(DeviceUpgrade 子元素,GB28181-2022 A.2.3.1.12)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceUpgrade {
    /// 固件版本。
    #[serde(rename = "Firmware", default)]
    pub firmware: String,
    /// 会话标识。
    #[serde(rename = "SessionID", default)]
    pub session_id: String,
    /// 固件文件地址。
    #[serde(rename = "FileURL", default)]
    pub file_url: String,
    /// 设备厂商(GB28181-2022 必选)。
    #[serde(
        rename = "Manufacturer",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub manufacturer: Option<String>,
}

/// 精确云台控制参数(PTZPreciseCtrl 子元素,GB-2022)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PtzPreciseCtrl {
    /// 水平角(度,0-360.00)。
    #[serde(rename = "Pan", default)]
    pub pan: f32,
    /// 俯仰角(度,-30~90)。
    #[serde(rename = "Tilt", default)]
    pub tilt: f32,
    /// 变倍(≥1.00)。
    #[serde(rename = "Zoom", default = "one_f32")]
    pub zoom: f32,
}

fn one_f32() -> f32 {
    1.0
}

/// 目标跟踪参数(TargetTrack 子元素,GB-2022 A.2.3.1.14)。
/// 平台可发结构体(Mode/ObjectID/Speed)或旧式纯文本 `<TargetTrack>Auto</TargetTrack>`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetTrack {
    /// 跟踪模式:Auto / Manual / Stop(白名单外忽略)。
    #[serde(rename = "Mode", default)]
    pub mode: String,
    /// 目标 ID(可选)。
    #[serde(rename = "ObjectID", default, skip_serializing_if = "Option::is_none")]
    pub object_id: Option<String>,
    /// 跟踪速度(1-255,可选)。
    #[serde(rename = "Speed", default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<u32>,
    /// 全景相机的全景通道 ID(可选)。
    #[serde(rename = "DeviceID2", default, skip_serializing_if = "Option::is_none")]
    pub device_id2: Option<String>,
    /// 全景图片大小、框选的区域坐标信息(手动跟踪时需要)。
    #[serde(
        rename = "TargetArea",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub target_area: Option<TargetArea>,
}

/// 目标跟踪区域坐标(TargetArea 子元素,GB28181-2022 A.2.3.1.14)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetArea {
    /// 全景播放窗口长度像素值。
    #[serde(rename = "Length", default)]
    pub length: u32,
    /// 全景播放窗口宽度像素值。
    #[serde(rename = "Width", default)]
    pub width: u32,
    /// 跟踪框中心的横轴坐标像素值。
    #[serde(rename = "MidPointX", default)]
    pub midpoint_x: u32,
    /// 跟踪框中心的纵轴坐标像素值。
    #[serde(rename = "MidPointY", default)]
    pub midpoint_y: u32,
    /// 跟踪框长度像素值。
    #[serde(rename = "LengthX", default)]
    pub length_x: u32,
    /// 跟踪框宽度像素值。
    #[serde(rename = "LengthY", default)]
    pub length_y: u32,
}

/// 报警复位扩展信息(AlarmCmd Info 子元素,GB28181-2022 A.2.3.1.6)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmInfo {
    /// 报警方式:0=全部,1=电话,2=设备,3=短信,4=GPS,5=视频,6=设备故障,7=其他。
    /// 可组合如 "1/2" 表示电话或设备报警。
    #[serde(
        rename = "AlarmMethod",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub alarm_method: Option<String>,
    /// 报警类型(含义取决于 AlarmMethod)。
    #[serde(rename = "AlarmType", default, skip_serializing_if = "Option::is_none")]
    pub alarm_type: Option<String>,
}

/// PTZ 云台控制附加参数(PTZCmdParams,GB28181-2022 A.2.3.1.2)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PTZCmdParams {
    /// 预置位名称(设置预置位时可选)。
    #[serde(
        rename = "PresetName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub preset_name: Option<String>,
    /// 巡航轨迹名称(最长32字节,巡航指令时可选)。
    #[serde(
        rename = "CruiseTrackName",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub cruise_track_name: Option<String>,
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
        } else if self.ptz_precise_ctrl.is_some() {
            "精确云台控制"
        } else if self.target_track.is_some() {
            "目标跟踪"
        } else if self.format_sd_card.is_some() {
            "格式化SD卡"
        } else if self.device_upgrade.is_some() {
            "在线升级"
        } else if self.snap_shot_config.is_some() {
            "抓拍配置"
        } else if self.snap_shot_cmd.is_some() {
            "抓拍"
        } else if self.cfg_basic_param.is_some()
            || self.cfg_video_record_plan.is_some()
            || self.cfg_video_alarm_record.is_some()
            || self.cfg_picture_mask.is_some()
            || self.cfg_frame_mirror.is_some()
            || self.cfg_alarm_report.is_some()
            || self.cfg_osd_config.is_some()
        {
            "设备配置"
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
        if bytes.len() < 7 {
            return None;
        }
        let action = match bytes[3] {
            0x81 => PresetAction::Set,
            0x82 => PresetAction::Call,
            0x83 => PresetAction::Delete,
            _ => return None,
        };
        // 预置位编号在字节5(数据2);字节4(数据1)恒为 0x01。
        // 实测 WVP:A50F0182 01 <presetId> 00 校验 —— presetId=1→..0101、=7→..0107。
        Some((action, bytes[5]))
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
            return None; // 预置位/巡航/辅助等扩展指令(0x8x),不是运动
        }
        if cmd & 0x40 != 0 {
            return None; // FI 指令(0x4x:光圈/聚焦),用 focus_op 解析,不是方向运动
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

    /// 解析 PTZCmd 8 字节码为字节数组(≥7 字节返回 Some)。
    fn ptz_bytes(&self) -> Option<Vec<u8>> {
        let hex = self.ptz_cmd.as_ref()?;
        let bytes = (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()?;
        (bytes.len() >= 7).then_some(bytes)
    }

    /// 12 位数据值:字节6(数据2)为低 8 位,字节7(组合码2)高 4 位为高 4 位。
    ///
    /// GB/T 28181-2022 表 A.9 注2 / 表 A.10 注2 都这样规定(巡航速度、停留时间、
    /// 扫描速度)。只取字节6 会把 >255 的值截断。
    fn data12(b: &[u8]) -> u32 {
        (b[5] as u32) | (((b[6] >> 4) as u32) << 8)
    }

    /// 解析巡航轨迹控制(PTZCmd 字节4 = 0x84~0x88,GB/T 28181-2022 表 A.9)。
    ///
    /// - `0x84` 加入巡航点:字节5=巡航组号,字节6=预置位号(01H-FFH)
    /// - `0x85` 删除巡航点:字节6=00H 时删除**整条**巡航
    /// - `0x86` 设置巡航速度:12 位(字节6 低 8 位 + 字节7 高 4 位)
    /// - `0x87` 设置巡航停留时间(秒):同样 12 位
    /// - `0x88` 开始巡航:字节5=巡航组号
    ///
    /// 停止巡航用 PTZ 停止指令(字节4 各 bit 全 0),不在本函数范围。
    pub fn cruise_op(&self) -> Option<CruiseOp> {
        let b = self.ptz_bytes()?;
        match b[3] {
            0x84 => Some(CruiseOp::SetPoint {
                track: b[4] as u32,
                preset: b[5] as u32,
            }),
            // 表 A.9 注2:字节6 为 00H 时删除整条巡航。
            0x85 if b[5] == 0 => Some(CruiseOp::DelTrack { track: b[4] as u32 }),
            0x85 => Some(CruiseOp::DelPoint {
                track: b[4] as u32,
                preset: b[5] as u32,
            }),
            0x86 => Some(CruiseOp::SetSpeed {
                track: b[4] as u32,
                speed: Self::data12(&b),
            }),
            0x87 => Some(CruiseOp::SetDwell {
                track: b[4] as u32,
                dwell: Self::data12(&b),
            }),
            0x88 => Some(CruiseOp::Start { track: b[4] as u32 }),
            _ => None,
        }
    }

    /// 解析自动扫描控制(PTZCmd 字节4 = 0x89 / 0x8A,GB/T 28181-2022 表 A.10)。
    ///
    /// - `0x89` + 字节6=00H → 开始自动扫描;=01H → 设左边界;=02H → 设右边界
    /// - `0x8A` → 设置扫描速度(12 位:字节6 低 8 位 + 字节7 高 4 位)
    ///
    /// 字节5 是扫描组号。停止扫描用 PTZ 停止指令。
    ///
    /// **注意**:这两个码之前被错当成"辅助开关",辅助开关实际是 0x8C/0x8D
    /// (表 A.11,2016 与 2022 一致)。
    pub fn scan_op(&self) -> Option<ScanOp> {
        let b = self.ptz_bytes()?;
        let group = b[4] as u32;
        match (b[3], b[5]) {
            (0x89, 0x00) => Some(ScanOp::Start { group }),
            (0x89, 0x01) => Some(ScanOp::SetLeftBoundary { group }),
            (0x89, 0x02) => Some(ScanOp::SetRightBoundary { group }),
            (0x89, _) => None,
            (0x8A, _) => Some(ScanOp::SetSpeed {
                group,
                speed: Self::data12(&b),
            }),
            _ => None,
        }
    }

    /// 解析辅助开关控制(PTZCmd 字节4 = 0x8C 开 / 0x8D 关,GB/T 28181-2022 表 A.11)。
    ///
    /// 字节5 为辅助开关编号,标准只明确 `1` 表示雨刷;
    /// 2=红外灯 3=加热 4=除雾 5=制冷 是海康/大华的事实约定。
    pub fn aux_op(&self) -> Option<AuxOp> {
        let b = self.ptz_bytes()?;
        let on = match b[3] {
            0x8C => true,
            0x8D => false,
            _ => return None,
        };
        Some(AuxOp {
            on,
            function: AuxFunction::from_index(b[4]),
            index: b[4],
        })
    }

    /// 解析 FI 指令(光圈/聚焦,GB/T 28181-2022 表 A.6)。
    ///
    /// 指令码(byte4/`bytes[3]`)的 bit6=1 标识 FI 指令(0x4X):
    /// bit3=光圈缩小、bit2=光圈放大、bit1=聚焦近、bit0=聚焦远;对应位清 0 即停止。
    /// 例:0x42 聚焦近、0x41 聚焦远、0x48 光圈缩小、0x44 光圈放大、0x40 停止、0x49 组合。
    /// byte5=聚焦速度、byte6=光圈速度。返回 None 表示非 FI 指令。
    pub fn fi_op(&self) -> Option<FiOp> {
        let b = self.ptz_bytes()?;
        let cmd = b[3];
        // 必须是 FI 指令:bit6=1(0x40),bit7=0。
        if cmd & 0x40 == 0 || cmd & 0x80 != 0 {
            return None;
        }
        Some(FiOp {
            iris_close: cmd & 0x08 != 0,
            iris_open: cmd & 0x04 != 0,
            focus_near: cmd & 0x02 != 0,
            focus_far: cmd & 0x01 != 0,
            focus_speed: b[4],
            iris_speed: b[5],
        })
    }
}

/// FI(光圈/聚焦)指令解析结果(GB/T 28181-2022 表 A.6)。全 false 为停止 FI 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FiOp {
    /// 光圈缩小(bit3)。
    pub iris_close: bool,
    /// 光圈放大(bit2)。
    pub iris_open: bool,
    /// 聚焦近(bit1)。
    pub focus_near: bool,
    /// 聚焦远(bit0)。
    pub focus_far: bool,
    /// 聚焦速度(byte5,0-255)。
    pub focus_speed: u8,
    /// 光圈速度(byte6,0-255)。
    pub iris_speed: u8,
}

impl FiOp {
    /// 是否为停止(无任何光圈/聚焦动作)。
    pub fn is_stop(&self) -> bool {
        !(self.iris_close || self.iris_open || self.focus_near || self.focus_far)
    }

    /// 人类可读描述(供 UI/日志)。
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if self.iris_close {
            parts.push("光圈−");
        }
        if self.iris_open {
            parts.push("光圈+");
        }
        if self.focus_near {
            parts.push("聚焦近");
        }
        if self.focus_far {
            parts.push("聚焦远");
        }
        if parts.is_empty() {
            "FI 停止".into()
        } else {
            parts.join("+")
        }
    }
}

/// 自动扫描控制操作(PTZCmd 0x89/0x8A 解析结果,GB/T 28181-2022 表 A.10)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOp {
    /// 开始自动扫描(画面从右向左移动)。
    Start { group: u32 },
    /// 设置自动扫描左边界。
    SetLeftBoundary { group: u32 },
    /// 设置自动扫描右边界。
    SetRightBoundary { group: u32 },
    /// 设置自动扫描速度(12 位)。
    SetSpeed { group: u32, speed: u32 },
}

/// 巡航轨迹控制操作(PTZCmd 0x84-0x88 解析结果)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CruiseOp {
    /// 增加巡航点。
    SetPoint { track: u32, preset: u32 },
    /// 删除巡航点。
    DelPoint { track: u32, preset: u32 },
    /// 删除整条巡航(表 A.9 注2:0x85 且字节6=00H)。
    DelTrack { track: u32 },
    /// 设置巡航速度。
    SetSpeed { track: u32, speed: u32 },
    /// 设置停留时间(秒)。
    SetDwell { track: u32, dwell: u32 },
    /// 启动巡航(track=0 表示停止)。
    Start { track: u32 },
}

/// 辅助控制功能(海康/大华事实标准索引)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuxFunction {
    /// 雨刷。
    Wiper,
    /// 红外灯。
    InfraredLight,
    /// 加热。
    Heater,
    /// 除雾。
    Defog,
    /// 制冷。
    Cooler,
    /// 未知辅助号。
    Unknown,
}

impl AuxFunction {
    /// 由辅助号映射功能。
    pub fn from_index(idx: u8) -> Self {
        match idx {
            1 => AuxFunction::Wiper,
            2 => AuxFunction::InfraredLight,
            3 => AuxFunction::Heater,
            4 => AuxFunction::Defog,
            5 => AuxFunction::Cooler,
            _ => AuxFunction::Unknown,
        }
    }

    /// 中文名(日志/UI 用)。
    pub fn label(&self) -> &'static str {
        match self {
            AuxFunction::Wiper => "雨刷",
            AuxFunction::InfraredLight => "红外灯",
            AuxFunction::Heater => "加热",
            AuxFunction::Defog => "除雾",
            AuxFunction::Cooler => "制冷",
            AuxFunction::Unknown => "未知辅助",
        }
    }
}

/// 辅助控制操作(PTZCmd 0x89/0x8A 解析结果)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuxOp {
    /// true=开,false=关。
    pub on: bool,
    /// 辅助功能。
    pub function: AuxFunction,
    /// 原始辅助号。
    pub index: u8,
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
    /// 构造 DeviceControl OK 应答。
    pub fn ok(device_id: impl Into<String>, sn: u32) -> Self {
        ControlResponse {
            cmd_type: "DeviceControl".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
        }
    }

    /// 构造 DeviceConfig OK 应答(GB28181-2022 A.2.6.8:CmdType=DeviceConfig)。
    pub fn config_ok(device_id: impl Into<String>, sn: u32) -> Self {
        ControlResponse {
            cmd_type: "DeviceConfig".into(),
            sn,
            device_id: device_id.into(),
            result: "OK".into(),
        }
    }

    /// 序列化为完整 XML。
    pub fn to_xml(&self) -> Result<String> {
        manscdp_to_xml(self)
    }
}
