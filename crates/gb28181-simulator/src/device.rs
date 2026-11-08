//! 单设备仿真:设备配置 + 注册/心跳状态机。
//!
//! 注册+Digest 鉴权、心跳保活。
//! 注册闭环:REGISTER(无鉴权)→ 平台 401 挑战 → REGISTER(带 Authorization)→ 200 OK。
//! 注册成功后按 `heartbeat_interval_secs` 周期发送 Keepalive。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use common::{DeviceId, Error, Result, Transport};
use gb28181_protocol::alarm::AlarmReport;
use gb28181_protocol::manscdp::Keepalive;
use serde::{Deserialize, Serialize};
use sip_core::{authorization, Challenge, SipTransport};

use crate::builder::{self, DialogIds};
use crate::firmware_upgrade::{
    self, SessionStatus, UpgradeDispatch, UpgradeResult, UpgradeStore, ValidatedUpgrade,
};

/// 设备初始内置的五个具名预置位(可被平台的预置位设置/删除命令修改)。
fn default_presets() -> std::collections::BTreeMap<u8, String> {
    [
        (1, "大门入口"),
        (2, "停车场"),
        (3, "接待大厅"),
        (4, "东侧通道"),
        (5, "西侧通道"),
    ]
    .into_iter()
    .map(|(id, name)| (id, name.to_string()))
    .collect()
}

/// 最小 HTTP PUT 上传 JPEG(抓拍上传用)。仅支持 http://(https 需 TLS,本工具不引入重依赖)。
///
/// 含基本 SSRF 防护:拒绝环回/链路本地/私网/组播地址(测试工具场景下上传目标应为受控采集服务)。
/// 用裸 TCP 手写请求,避免给压测链路(依赖本 crate)引入 HTTP 客户端。
async fn http_put_jpeg(url: &str, body: &[u8]) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| Error::Gb28181("抓拍上传仅支持 http:// URL".into()))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| Error::Gb28181("上传 URL 端口非法".into()))?,
        ),
        None => (authority.to_string(), 80),
    };
    reject_ssrf(&host)?;

    let req = format!(
        "PUT {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: image/jpeg\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| Error::Gb28181("上传连接超时".into()))?
    .map_err(|e| Error::Gb28181(format!("上传连接失败: {e}")))?;
    stream
        .write_all(req.as_bytes())
        .await
        .map_err(|e| Error::Gb28181(format!("上传写入失败: {e}")))?;
    stream
        .write_all(body)
        .await
        .map_err(|e| Error::Gb28181(format!("上传写入失败: {e}")))?;

    let mut resp = Vec::new();
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_to_end(&mut resp),
    )
    .await;
    // 解析状态行首行 "HTTP/1.1 2xx"。
    let head = String::from_utf8_lossy(&resp);
    let ok = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .map(|c| (200..300).contains(&c))
        .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(Error::Gb28181(format!(
            "上传响应非 2xx: {}",
            head.lines().next().unwrap_or("")
        )))
    }
}

/// 拒绝 SSRF 高危目标(环回/链路本地/私网/组播/未指定)。
fn reject_ssrf(host: &str) -> Result<()> {
    use std::net::IpAddr;
    // 仅对字面 IP 做判定;域名交由 DNS(受控内网场景)。
    if let Ok(ip) = host.parse::<IpAddr>() {
        let bad = match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_private()
                    || v4.is_multicast()
                    || v4.is_unspecified()
                    || v4.is_broadcast()
            }
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_multicast() || v6.is_unspecified(),
        };
        if bad {
            return Err(Error::Gb28181(format!("上传目标 {host} 属受限地址,已拒绝")));
        }
    }
    Ok(())
}

/// 单个虚拟设备的静态配置。压测时由 `scenario` 按序号批量生成。
#[derive(Debug, Clone)]
pub struct DeviceConfig {
    /// 本设备国标 ID。
    pub device_id: DeviceId,
    /// SIP 认证用户名(通常等于 device_id)。
    pub username: String,
    /// SIP 认证密码。
    pub password: String,
    /// 上级平台 SIP 服务地址 host。
    pub server_host: String,
    /// 上级平台 SIP 服务端口。
    pub server_port: u16,
    /// 上级平台国标 ID；为空时为兼容旧配置而回退到 server_domain。
    pub server_id: String,
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 信令传输方式。
    pub transport: Transport,
    /// 音频流(语音广播/对讲下行)的媒体传输方式。
    pub audio_transport: common::AudioTransport,
    /// 注册有效期(秒)，不得短于 3600。
    pub register_expires_secs: u32,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 连续心跳失败多少次后触发重注册。
    pub heartbeat_fail_threshold: u32,
    /// 通道列表(目录查询应答用)。
    pub channels: Vec<ChannelConfig>,
    /// 设备信息(DeviceInfo 查询应答用)。
    pub device_info: DeviceInfo,
    /// 视频源文件路径(H.264 Annex B,C 档);None 且无 light_bitrate 时不推流(A 档)。
    pub video_source: Option<String>,
    /// 推流帧率(fps)。
    pub video_fps: u32,
    /// Explicit desktop encoding profile; absent for legacy CLI/scenario callers.
    pub media_profile: Option<media_rtp::profile::MediaProfile>,
    /// B 档轻量伪流码率(kbps);Some 且无 video_source 时用 LightSource。
    pub light_bitrate_kbps: Option<u32>,
    /// GB28181 协议版本(影响 MANSCDP 应答字段集,2022 默认)。
    pub gb_version: common::GbVersion,
    /// 信令字符集编码(§6.10:GB18030 默认;部分平台用 UTF-8)。
    /// 决定 MANSCDP XML 体的字节编码与声明,修复中文乱码。
    pub signaling_encoding: common::SignalingEncoding,
}

/// 通道配置(对应目录查询中的一个 Item)。
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// 通道国标 ID(设备侧通道 ID 规则:设备 ID + 通道序号后缀)。
    pub channel_id: DeviceId,
    /// 通道名称。
    pub name: String,
    /// 状态:ON / OFF。
    pub status: String,
}

/// 设备信息(DeviceInfo 查询应答字段)。
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub device_name: String,
    pub manufacturer: String,
    pub model: String,
    pub firmware: String,
    /// 硬件版本（GB-2022 新增字段）。None 时不输出，兼容只认 2016 字段的平台。
    pub hardware_version: Option<String>,
    /// 安装地址（目录 Item 的 `<Address>`）。None 时不输出，保持旧报文不变。
    pub installation_address: Option<String>,
    /// 设备类型（DeviceInfo 应答的 `<DeviceType>`）。None 时不输出。
    pub device_type: Option<String>,
}

const REGISTER_RETRY_MIN_SECS: u64 = 60;
const REGISTER_RETRY_MAX_SECS: u64 = 300;
/// UDP Catalog XML 体工程预算。为 SIP 头和 IP/UDP 头预留约 500 字节，
/// 避免完整 SIP datagram 超过常见 1500 字节 MTU。
const CATALOG_UDP_BODY_MAX_BYTES: usize = 1000;
/// 完整 SIP UDP payload 的互操作预算。1200B 低于 IPv6 最小 MTU 扣除头部后的
/// 1232B，也兼容部分对较大 UDP SIP 报文处理不完整的平台。
const CATALOG_UDP_PAYLOAD_MAX_BYTES: usize = 1200;
const RECORD_INFO_PAGE_SIZE: usize = 5;
/// TCP 被动推流:200 OK 发出后等待平台连入的上限。
const TCP_PASSIVE_ACCEPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

fn register_retry_base() -> std::time::Duration {
    std::time::Duration::from_secs(REGISTER_RETRY_MIN_SECS)
}

fn next_register_retry(current: std::time::Duration) -> std::time::Duration {
    (current * 2).min(std::time::Duration::from_secs(REGISTER_RETRY_MAX_SECS))
}

fn query_response_timing() -> sip_core::Timing {
    // 0.4 + 0.8 + 1.6 + 1.6 = 4.4s，满足查询 Response 5 秒确认窗口。
    sip_core::Timing {
        t1: std::time::Duration::from_millis(400),
        t2: std::time::Duration::from_millis(1600),
        max_retransmits: 3,
    }
}

fn register_renew_after(expires_secs: u32) -> std::time::Duration {
    std::time::Duration::from_secs((u64::from(expires_secs) * 80 / 100).max(1))
}

fn record_heartbeat_failure(failures: &mut u32, threshold: u32) -> bool {
    *failures = failures.saturating_add(1);
    *failures >= threshold.max(1)
}

/// 设备状态(与 UI DTO 对齐)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceState {
    /// 离线。
    Disconnected,
    /// 注册中。
    Registering,
    /// 已注册。
    Registered,
    /// 推流中(M2)。
    InCall,
    /// 注册失败。
    Failed,
}

/// 最近一次平台目录查询活动，供桌面端在切页后回读。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogQueryActivity {
    pub target_id: String,
    pub result_count: usize,
    pub packet_count: usize,
    pub updated_at_ms: u64,
}

/// 设备仿真运行时状态机。持有配置 + 会话标识 + CSeq 计数 + 会话状态 + 事件观察者。
pub struct DeviceSimulator {
    config: DeviceConfig,
    /// 桌面端本地 OSD 样式；None 表示不烧录 OSD(CLI/压测)。
    osd_settings: std::sync::Mutex<Option<media_rtp::osd::OsdSettings>>,
    /// 平台经 DeviceConfig 下发的 OSDConfig。runtime_config 里的 osd_config
    /// 总有出厂默认值，无法区分「平台设置过」，所以单独记录。
    platform_osd: std::sync::Mutex<Option<gb28181_protocol::manscdp::OSDConfig>>,
    /// 注册后启动的唯一采集源；平台点播只订阅它，不再重复打开摄像头/文件。
    shared_media: tokio::sync::Mutex<Option<Arc<media_rtp::SharedMedia>>>,
    prepared_file: std::sync::Mutex<Option<media_rtp::file_profile::PreparedFileProfile>>,
    /// 可选的本地录像状态机，桌面端从 app_data_dir 注入。
    recording_service: std::sync::Mutex<Option<Arc<crate::recording::RecordingService>>>,
    ids: DialogIds,
    cseq: AtomicU32,
    /// 当前活跃的推流会话(INVITE → 推流中,BYE → 停止)。
    session: tokio::sync::Mutex<Option<PushSession>>,
    /// 事件观察者(上报注册/心跳/推流事件给压测指标)。默认 Noop。
    observer: Arc<dyn common::DeviceObserver>,
    /// 本端对外信令地址 (host, port),run() 启动时设置;应答 MESSAGE 填 Via/From 用。
    local_addr: std::sync::OnceLock<(String, u16)>,
    /// 当前位置(经度, 纬度)。移动位置订阅的周期 NOTIFY 上报此坐标;
    /// 由 report_position/set_position 更新。默认取一个内置坐标(北京)。
    position: std::sync::Mutex<(f64, f64)>,
    /// 活跃订阅表(CmdType → 订阅句柄)。平台 SUBSCRIBE 建立,退订/下线时清理。
    subscriptions: tokio::sync::Mutex<HashMap<String, Subscription>>,
    /// 四类订阅的运行期元数据(CmdType → 订阅者/有效期/最近通知结果)。
    /// 与 `subscriptions` 分开:后者只有 MobilePosition 有周期任务,元数据四类都有。
    subscription_meta: std::sync::Mutex<HashMap<String, SubscriptionMeta>>,
    /// 报警订阅对话(平台 `SUBSCRIBE + Alarm` 时记录:对话标识 + 平台地址)。
    /// 存在时,report_alarm 走对话内 NOTIFY;否则退化为独立 MESSAGE(如手动触发)。
    alarm_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// 目录订阅对话(平台 `SUBSCRIBE + Catalog` 时记录):CRUD 变更后据此发对话内增量 NOTIFY。
    catalog_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// PTZ 精准状态订阅对话(平台 `SUBSCRIBE + PTZPosition` 时记录)。
    ///
    /// 该订阅是**事件型**:平台构造订阅查询时按规范不带 `Interval`,设备只在精确姿态
    /// 真正发生变化时发一条对话内 NOTIFY。存在对话时才会上报,设备下线时清理。
    ptz_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// PTZ 精准状态已上报次数(供前端「活跃订阅面板」显示)。
    ptz_notify_count: std::sync::atomic::AtomicU32,
    /// 报警/目录/位置订阅已上报次数(供前端「活跃订阅面板」显示)。
    alarm_notify_count: std::sync::atomic::AtomicU32,
    catalog_notify_count: std::sync::atomic::AtomicU32,
    position_notify_count: std::sync::atomic::AtomicU32,
    /// 预置位表(编号 → 名称)。PTZ 预置位设置/删除命令更新,PresetQuery 返回。
    presets: std::sync::Mutex<std::collections::BTreeMap<u8, String>>,
    /// 预置位姿态表。与名称表分开保存，PresetQuery 协议只回名称，UI 快照需要姿态。
    preset_poses: std::sync::Mutex<std::collections::BTreeMap<u8, (f32, f32, f32)>>,
    /// 设备控制态(布防/看守位/巡航/精准姿态)。扩展查询与控制命令共同维护(FR-17/18)。
    control_state: std::sync::Mutex<ControlState>,
    /// 平台下发后已应用的会话级设备配置。ConfigDownload 从这里回读，不再返回固定值。
    runtime_config: std::sync::Mutex<gb28181_protocol::manscdp::ConfigDownloadResponse>,
    /// 最近一次影响运行时快照的变更来源与时间。
    runtime_change: std::sync::Mutex<RuntimeChange>,
    /// 多通道目录树(FR-34)。为空时用 `config.channels` 扁平列表出目录;
    /// 非空时用此树(含业务分组/虚拟组织/报警通道层级)出目录,支持 CRUD + 增量 NOTIFY。
    catalog_tree: std::sync::Mutex<Vec<gb28181_protocol::id_codec::CatalogNode>>,
    /// 上次推给平台的目录快照(用于计算增量 NOTIFY 的 diff)。
    catalog_snapshot: std::sync::Mutex<gb28181_protocol::manscdp::CatalogSnapshot>,
    /// 最近一次 Catalog Query 的目标、结果数和分包数。
    catalog_activity: std::sync::Mutex<Option<CatalogQueryActivity>>,
    /// 当前设备的固件版本和升级会话持久化状态。
    upgrade_store: Arc<UpgradeStore>,
    /// 同一设备同时只允许一个新的升级会话执行。
    active_upgrade: tokio::sync::Mutex<Option<String>>,
    /// 语音广播/对讲下行会话(平台 → 设备)。
    broadcast: Arc<crate::broadcast::BroadcastRunner>,
}

/// 自动扫描组(GB/T 28181 表 A.10)。边界是设备当时姿态的快照。
#[derive(Debug, Clone, Default)]
pub struct ScanGroupState {
    pub left: Option<(f32, f32, f32)>,
    pub right: Option<(f32, f32, f32)>,
    pub speed: Option<u32>,
}

/// 设备控制态。由扩展控制命令(布防/看守位/巡航/精准云台)更新,
/// 供扩展查询(AlarmStatus/HomePositionQuery/CruiseTrack*/PTZPosition)读回。
#[derive(Debug, Clone)]
pub struct ControlState {
    /// 是否处于布防态。GuardCmd 只更新本字段。
    pub guarded: bool,
    /// 是否正在报警(AlarmStatus 的 DutyStatus)。主动报警/报警复位更新本字段。
    pub alarming: bool,
    /// 看守位是否启用。
    pub home_enabled: bool,
    /// 是否已设看守位(HomePositionQuery 的 PresetIndex 标志)。
    pub home_set: bool,
    /// 看守位指向的预置位号。
    pub home_preset_index: Option<u32>,
    /// 看守位自动归位时间(秒)。
    pub home_reset_time: Option<u32>,
    /// 巡航轨迹:轨迹号 → 该轨迹的预置位号列表(增点/删点维护)。
    pub cruise_tracks: std::collections::BTreeMap<u32, Vec<u32>>,
    /// 巡航组级速度(12 位原值)。
    pub cruise_speed: std::collections::BTreeMap<u32, u32>,
    /// 巡航组级每点停留秒数。
    pub cruise_dwell: std::collections::BTreeMap<u32, u32>,
    /// 当前正在执行的巡航轨迹。
    pub active_cruise: Option<u32>,
    /// 平台设过边界的自动扫描组。
    pub scan_groups: std::collections::BTreeMap<u32, ScanGroupState>,
    /// 当前正在执行的扫描组。
    pub active_scan: Option<u32>,
    /// 最近一次精准云台姿态(pan 度, tilt 度, zoom 倍)。
    pub precise_pose: (f32, f32, f32),
    /// 当前姿态(前端积分后回写,作为预置位/扫描边界的真值)。
    pub pose: (f32, f32, f32),
    /// 当前聚焦行程(0~1)。
    pub focus: f32,
    /// 当前光圈行程(0~1)。
    pub iris: f32,
    /// 平台是否查询过存储卡。
    pub storage_queried: bool,
    /// 目标跟踪模式(Auto/Manual;None=未跟踪)。
    pub target_mode: Option<String>,
    /// 目标跟踪框（归一化 left/top/width/height，原点=画面左上角）。
    pub target_box: Option<(f32, f32, f32, f32)>,
    /// 最近一次拉框联动框（归一化 left/top/width/height）。
    pub drag_zoom_box: Option<(f32, f32, f32, f32)>,
}

impl Default for ControlState {
    fn default() -> Self {
        ControlState {
            guarded: false,
            alarming: false,
            home_enabled: true,
            home_set: false,
            home_preset_index: None,
            home_reset_time: None,
            cruise_tracks: std::collections::BTreeMap::new(),
            cruise_speed: std::collections::BTreeMap::new(),
            cruise_dwell: std::collections::BTreeMap::new(),
            active_cruise: None,
            scan_groups: std::collections::BTreeMap::new(),
            active_scan: None,
            precise_pose: (0.0, 0.0, 1.0),
            pose: (0.0, 0.0, 1.0),
            focus: 0.5,
            iris: 0.5,
            storage_queried: false,
            target_mode: None,
            target_box: None,
            drag_zoom_box: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PresetSnapshot {
    pub id: u8,
    pub name: String,
    pub pan: f32,
    pub tilt: f32,
    pub zoom: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CruiseSnapshot {
    pub id: u32,
    pub points: Vec<u32>,
    pub speed: Option<u32>,
    pub dwell: Option<u32>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanSnapshot {
    pub id: u32,
    pub left: Option<(f32, f32, f32)>,
    pub right: Option<(f32, f32, f32)>,
    pub speed: Option<u32>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageCardSnapshot {
    pub id: u32,
    pub name: String,
    pub capacity_mb: u32,
    pub free_mb: u32,
    pub status: String,
    pub progress: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MaskRegionSnapshot {
    pub seq: u32,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedPreset {
    pub id: u8,
    pub name: String,
    pub pan: f32,
    pub tilt: f32,
    pub zoom: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedCruise {
    pub id: u32,
    pub points: Vec<u32>,
    pub speed: Option<u32>,
    pub dwell: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedScan {
    pub id: u32,
    pub left: Option<(f32, f32, f32)>,
    pub right: Option<(f32, f32, f32)>,
    pub speed: Option<u32>,
}

/// 跨进程重启持久化的设备控制配置（不含瞬态目标/存储查询/运行态）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PersistedControlState {
    pub presets: Vec<PersistedPreset>,
    pub home_enabled: bool,
    pub home_preset_index: Option<u32>,
    pub home_reset_time: Option<u32>,
    pub cruise: Vec<PersistedCruise>,
    pub scan: Vec<PersistedScan>,
    /// 平台下发的 OSDConfig(未下发过为 None)。
    pub osd_config: Option<gb28181_protocol::manscdp::OSDConfig>,
    /// 平台下发的视频遮挡。
    pub picture_mask: Option<gb28181_protocol::manscdp::PictureMask>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OsdItemSnapshot {
    pub text: String,
    pub x: u32,
    pub y: u32,
}

/// 模拟器中心需要的设备控制快照。只读，不持锁执行 I/O。
#[derive(Debug, Clone, Serialize)]
pub struct ControlSnapshot {
    pub pan: f32,
    pub tilt: f32,
    pub zoom: f32,
    pub focus: f32,
    pub iris: f32,
    pub presets: Vec<PresetSnapshot>,
    pub home_enabled: bool,
    pub home_preset_index: Option<u32>,
    pub home_reset_time: Option<u32>,
    pub cruise: Vec<CruiseSnapshot>,
    pub scan: Vec<ScanSnapshot>,
    pub storage_queried: bool,
    pub storage_cards: Vec<StorageCardSnapshot>,
    pub mask_configured: bool,
    pub mask_on: bool,
    pub mask_regions: Vec<MaskRegionSnapshot>,
    pub osd_length: u32,
    pub osd_width: u32,
    pub osd_time_x: u32,
    pub osd_time_y: u32,
    pub osd_time_visible: bool,
    pub osd_text_visible: bool,
    pub osd_time_type: Option<u32>,
    pub osd_items: Vec<OsdItemSnapshot>,
    pub osd_configured: bool,
    pub guarded: bool,
    pub alarming: bool,
    pub target_mode: Option<String>,
    pub target_box: Option<(f32, f32, f32, f32)>,
    pub drag_zoom_box: Option<(f32, f32, f32, f32)>,
}

/// 一条活跃订阅的只读快照（供 UI 订阅卡片）。
///
/// 除「是否已订阅 / 已发几条」外，还带上运行期细节：谁订阅的、有效期多久、
/// 还剩多少秒、最近一次 NOTIFY 的结果与错误。移动端订阅面板展示的就是这一组，
/// 桌面端此前只有前三项，平台侧排障时看不出「订阅已过期但界面仍显示已订阅」。
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionSnapshot {
    pub kind: String,
    pub active: bool,
    /// `not_subscribed` / `subscribed` / `expired` / `cancelled` / `exception`。
    pub lifecycle: String,
    /// 发起订阅的平台地址（host:port），未订阅时为 None。
    pub subscriber: Option<String>,
    /// 平台在 SUBSCRIBE 里给的 Expires（秒）。
    pub expires_secs: u32,
    /// 距离过期的剩余秒数（已过期或无订阅为 0）。
    pub remaining_secs: u32,
    pub notify_count: u32,
    /// 最近一次 NOTIFY 的发起时刻（毫秒时间戳）。
    pub last_notify_at_ms: Option<u64>,
    /// 最近一次 NOTIFY 的 SIP 状态码，成功时写在这里。
    pub last_notify_result: Option<String>,
    /// 最近一次 NOTIFY 的错误描述，失败时写在这里。
    pub last_error: Option<String>,
    /// 仅 PTZPosition 有值：六字段姿态快照（A.2.6.15）。
    pub ptz_position: Option<PtzPositionSnapshot>,
}

/// PTZ 六字段姿态快照（GB/T 28181-2022 A.2.6.15）。
///
/// 光学三量（视场角 / 最大可视距离）由变焦换算而来，与报文里的取值同源，
/// 保证「界面看到的」和「平台收到的」不会互相矛盾。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct PtzPositionSnapshot {
    pub pan: f64,
    pub tilt: f64,
    pub zoom: f64,
    pub horizontal_field_angle: f64,
    pub vertical_field_angle: f64,
    pub max_view_distance: f64,
}

impl PtzPositionSnapshot {
    /// 由三轴姿态换算六字段。
    pub fn from_pose(pan: f32, tilt: f32, zoom: f32) -> Self {
        let optics = gb28181_protocol::ptz::PtzOptics::from_zoom(zoom);
        PtzPositionSnapshot {
            pan: f64::from(pan),
            tilt: f64::from(tilt),
            zoom: f64::from(zoom),
            horizontal_field_angle: f64::from(optics.horizontal_field_angle_deg),
            vertical_field_angle: f64::from(optics.vertical_field_angle_deg),
            max_view_distance: f64::from(optics.max_view_distance_m),
        }
    }
}

/// 一条订阅的运行期元数据（订阅者 / 有效期 / 最近通知结果）。
///
/// 与 [`Subscription`]（周期任务句柄）分开：后者只有 MobilePosition 用，
/// 而这里四类订阅都要记；且这份数据要能被快照读取，不能牵动任何任务。
#[derive(Debug, Clone, Default)]
struct SubscriptionMeta {
    subscriber: Option<String>,
    expires_secs: u32,
    subscribed_at_ms: u64,
    last_notify_at_ms: Option<u64>,
    last_notify_result: Option<String>,
    last_error: Option<String>,
}

impl SubscriptionMeta {
    /// 剩余秒数：首次订阅时按 `Expires` 起算，过期后恒为 0。
    fn remaining_secs(&self, now_ms: u64) -> u32 {
        if self.expires_secs == 0 {
            return 0;
        }
        let elapsed_ms = now_ms.saturating_sub(self.subscribed_at_ms);
        let expires_ms = u64::from(self.expires_secs) * 1_000;
        (expires_ms.saturating_sub(elapsed_ms) / 1_000).min(u64::from(u32::MAX)) as u32
    }
}

#[derive(Debug, Clone)]
struct RuntimeChange {
    kind: String,
    updated_at_ms: u64,
}

/// 桌面端只读的设备运行时真相快照。
#[derive(Debug, Clone)]
pub struct DeviceRuntimeSnapshot {
    pub guarded: bool,
    pub alarming: bool,
    pub longitude: f64,
    pub latitude: f64,
    pub name: String,
    pub expiration: u32,
    pub heartbeat_interval: u32,
    pub heartbeat_count: u32,
    pub video_record_plan_type: Option<u32>,
    pub alarm_record_duration: Option<u32>,
    pub picture_mask_enabled: Option<u32>,
    pub frame_mirror_mode: Option<u32>,
    pub alarm_report_enabled: Option<u32>,
    pub osd_time_show: Option<u32>,
    pub osd_show: Option<u32>,
    pub last_change: String,
    pub updated_at_ms: u64,
}

/// 解析遮挡区域 `Point=左,上,右,下`；格式错误或零面积的区域忽略。
fn mask_regions(mask: &gb28181_protocol::manscdp::PictureMask) -> Vec<MaskRegionSnapshot> {
    let Some(list) = mask.region_list.as_ref() else {
        return Vec::new();
    };
    list.items
        .iter()
        .filter_map(|item| {
            let parts = item
                .point
                .split(',')
                .map(|part| part.trim().parse::<u32>().ok())
                .collect::<Option<Vec<_>>>()?;
            if parts.len() != 4 {
                return None;
            }
            let (left, top, right, bottom) = (parts[0], parts[1], parts[2], parts[3]);
            (right > left && bottom > top).then_some(MaskRegionSnapshot {
                seq: item.seq,
                left,
                top,
                right,
                bottom,
            })
        })
        .collect()
}

/// 合并本地 OSD 样式、平台 OSDConfig 与遮挡，得到一次采集要烧录的内容。
///
/// - 平台下发过 OSDConfig：时间/文字的开关与坐标以平台为准，样式取本地。
/// - 未下发：时间左上、通道名右上(对齐移动端)。
/// - 斜铺水印只来自本地配置；遮挡仅在 `On=1` 时生效。
pub fn merge_osd_overlay(
    settings: &media_rtp::osd::OsdSettings,
    platform: Option<&gb28181_protocol::manscdp::OSDConfig>,
    mask: Option<&gb28181_protocol::manscdp::PictureMask>,
    channel_name: &str,
    output: (u32, u32),
) -> Option<media_rtp::osd::OsdOverlay> {
    use media_rtp::osd::*;
    let truncate = |text: &str| text.chars().take(OSD_TEXT_MAX_CHARS).collect::<String>();
    let (base_width, base_height, time, texts) = match platform {
        Some(osd) => {
            let base = (
                if osd.length > 0 { osd.length } else { output.0 },
                if osd.width > 0 { osd.width } else { output.1 },
            );
            let time = osd.time_visible().then(|| OsdTime {
                anchor: OsdAnchor::Pixel { x: osd.time_x, y: osd.time_y },
                format: if osd.time_type == Some(1) {
                    OsdTimeFormat::Chinese
                } else {
                    OsdTimeFormat::Dash
                },
                style: settings.timestamp.clone(),
            });
            let texts = if osd.text_visible() {
                osd.items
                    .iter()
                    .take(OSD_ITEMS_MAX)
                    .filter(|item| !item.text.is_empty())
                    .map(|item| OsdText {
                        text: truncate(&item.text),
                        anchor: OsdAnchor::Pixel { x: item.x, y: item.y },
                        style: settings.channel_name.clone(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            (base.0, base.1, time, texts)
        }
        None => {
            let time = settings.timestamp.enabled.then(|| OsdTime {
                anchor: OsdAnchor::Pixel { x: 6, y: 6 },
                format: OsdTimeFormat::Dash,
                style: settings.timestamp.clone(),
            });
            let texts = (settings.channel_name.enabled && !channel_name.is_empty())
                .then(|| OsdText {
                    text: truncate(channel_name),
                    anchor: OsdAnchor::TopRight,
                    style: settings.channel_name.clone(),
                })
                .into_iter()
                .collect();
            (output.0, output.1, time, texts)
        }
    };
    let watermark = (settings.watermark.enabled && !settings.watermark.text.trim().is_empty())
        .then(|| OsdText {
            text: truncate(&settings.watermark.text),
            anchor: OsdAnchor::TopRight,
            style: settings.watermark.clone(),
        });
    let masks = mask
        .filter(|mask| mask.on == 1)
        .map(|mask| {
            mask_regions(mask)
                .into_iter()
                .take(OSD_MASKS_MAX)
                .map(|region| OsdRect {
                    left: region.left,
                    top: region.top,
                    right: region.right,
                    bottom: region.bottom,
                })
                .collect()
        })
        .unwrap_or_default();
    let overlay = OsdOverlay {
        base_width,
        base_height,
        time,
        texts,
        watermark,
        mask_base: output,
        masks,
    };
    (!overlay.is_empty()).then_some(overlay)
}

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 订阅运行详情只跟踪这四类；SIP 里出现别的 Event 时不该在快照里凭空多一行。
const TRACKED_SUBSCRIPTIONS: [&str; 4] = ["MobilePosition", "Alarm", "Catalog", "PTZPosition"];

fn is_tracked_subscription(kind: &str) -> bool {
    TRACKED_SUBSCRIPTIONS.contains(&kind)
}

fn initial_runtime_config(
    config: &DeviceConfig,
) -> gb28181_protocol::manscdp::ConfigDownloadResponse {
    let mut response = gb28181_protocol::manscdp::ConfigDownloadResponse::by_type(
        config.device_id.as_str(),
        1,
        "BasicParam/VideoParamOpt/VideoRecordPlan/VideoAlarmRecord/PictureMask/FrameMirror/AlarmReport/OSDConfig/SnapShotConfig/VideoParamAttribute/SVACEncodeConfig/SVACDecodeConfig",
        config.device_info.device_name.clone(),
        config.register_expires_secs,
        config.heartbeat_interval_secs as u32,
        config.heartbeat_fail_threshold,
        "1920*1080",
    );
    if let Some(attribute) = response.video_param_attribute.as_mut() {
        for item in &mut attribute.items {
            item.frame_rate = config.video_fps.to_string();
            if let Some(media) = &config.media_profile {
                item.video_format = match media.video_codec {
                    media_rtp::profile::MediaVideoCodec::H264 => "H.264",
                    media_rtp::profile::MediaVideoCodec::H265 => "H.265",
                }
                .into();
                item.resolution = format!("{}*{}", media.width, media.height);
                item.frame_rate = media.video_fps.to_string();
                item.video_bit_rate = Some(media.bitrate_kbps.to_string());
            }
        }
    }
    if let Some(osd) = response.osd_config.as_mut() {
        let (width, height) = config
            .media_profile
            .as_ref()
            .map(|media| (media.width, media.height))
            .unwrap_or((1920, 1080));
        osd.length = width;
        osd.width = height;
        osd.time_x = 10;
        osd.time_y = 10;
        osd.items = config
            .channels
            .first()
            .map(|channel| {
                vec![gb28181_protocol::manscdp::OsdTextItem {
                    text: channel.name.clone(),
                    x: 10,
                    y: 34,
                }]
            })
            .unwrap_or_default();
    }
    response
}

/// 活跃的推流会话(推流任务句柄 + 停止信号)。
struct PushSession {
    /// 唯一会话标记，防止已结束的旧任务清理后续新会话。
    marker: Arc<()>,
    /// push_stream 任务。
    _task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端(drop 或 send 均可触发停流)。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 回放控制(倍速/暂停),供会话内 INFO 调整推流。
    control: Arc<media_rtp::PlaybackControl>,
    /// 是否为回放/下载会话(s=Playback/Download):结束时发 MediaStatus 121。
    is_history: bool,
    /// INVITE `t=` 描述的历史时间窗，供 Range 精确折算。
    history_duration_secs: u64,
}

/// 一条活跃订阅(周期 NOTIFY 任务句柄 + 停止信号)。
/// 丢弃或 send 停止信号即终止周期上报任务。
struct Subscription {
    /// 周期 NOTIFY 任务。
    _task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端。
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

impl DeviceSimulator {
    /// 用配置创建一个待运行的设备仿真实例(无观察者)。
    pub fn new(config: DeviceConfig) -> Self {
        let state_dir = std::env::var_os("UVP_SIM_UPGRADE_STATE_DIR").map(std::path::PathBuf::from);
        Self::new_with_observer_and_state_dir(config, Arc::new(common::NoopObserver), state_dir)
    }

    /// 创建带设备专属固件状态目录的仿真实例。目录下按设备 ID 写一个 JSON 文件。
    pub fn with_upgrade_state_dir(
        config: DeviceConfig,
        state_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new_with_observer_and_state_dir(
            config,
            Arc::new(common::NoopObserver),
            Some(state_dir.into()),
        )
    }

    /// 带事件观察者创建(压测时由编排器注入共享 Metrics)。
    pub fn with_observer(config: DeviceConfig, observer: Arc<dyn common::DeviceObserver>) -> Self {
        let state_dir = std::env::var_os("UVP_SIM_UPGRADE_STATE_DIR").map(std::path::PathBuf::from);
        Self::new_with_observer_and_state_dir(config, observer, state_dir)
    }

    /// 带观察者和设备专属固件状态目录创建仿真实例。
    pub fn with_observer_and_upgrade_state_dir(
        config: DeviceConfig,
        observer: Arc<dyn common::DeviceObserver>,
        state_dir: impl Into<std::path::PathBuf>,
    ) -> Self {
        Self::new_with_observer_and_state_dir(config, observer, Some(state_dir.into()))
    }

    fn new_with_observer_and_state_dir(
        config: DeviceConfig,
        observer: Arc<dyn common::DeviceObserver>,
        state_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let runtime_config = initial_runtime_config(&config);
        let broadcast = Arc::new(crate::broadcast::BroadcastRunner::new(
            config.clone(),
            Arc::clone(&observer),
        ));
        let upgrade_store = Arc::new(UpgradeStore::open(
            state_dir,
            config.device_id.as_str(),
            &config.device_info.firmware,
        ));
        Self {
            config,
            osd_settings: std::sync::Mutex::new(None),
            platform_osd: std::sync::Mutex::new(None),
            shared_media: tokio::sync::Mutex::new(None),
            prepared_file: std::sync::Mutex::new(None),
            recording_service: std::sync::Mutex::new(None),
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer,
            local_addr: std::sync::OnceLock::new(),
            position: std::sync::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            subscription_meta: std::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            catalog_dialog: tokio::sync::Mutex::new(None),
            ptz_dialog: tokio::sync::Mutex::new(None),
            ptz_notify_count: std::sync::atomic::AtomicU32::new(0),
            alarm_notify_count: std::sync::atomic::AtomicU32::new(0),
            catalog_notify_count: std::sync::atomic::AtomicU32::new(0),
            position_notify_count: std::sync::atomic::AtomicU32::new(0),
            presets: std::sync::Mutex::new(default_presets()),
            preset_poses: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            control_state: std::sync::Mutex::new(ControlState::default()),
            runtime_config: std::sync::Mutex::new(runtime_config),
            runtime_change: std::sync::Mutex::new(RuntimeChange {
                kind: "device_started".into(),
                updated_at_ms: epoch_millis(),
            }),
            catalog_tree: std::sync::Mutex::new(Vec::new()),
            catalog_snapshot: std::sync::Mutex::new(gb28181_protocol::manscdp::CatalogSnapshot {
                channels: std::collections::BTreeMap::new(),
            }),
            catalog_activity: std::sync::Mutex::new(None),
            upgrade_store,
            active_upgrade: tokio::sync::Mutex::new(None),
            broadcast,
        }
    }

    /// 注入语音广播的音频输出(桌面端在启动设备前注入;不注入则只统计不播放)。
    pub fn set_broadcast_audio_sink(
        &self,
        sink: Arc<dyn crate::broadcast::BroadcastAudioSink>,
    ) {
        self.broadcast.set_audio_sink(sink);
    }

    /// 本端信令 host(run 启动后有效,未设时回退 0.0.0.0)。
    fn local_host(&self) -> String {
        self.local_addr
            .get()
            .map(|(h, _)| h.clone())
            .unwrap_or_else(|| "0.0.0.0".into())
    }

    /// 本端信令端口(run 启动后有效,未设时回退 0)。
    fn local_port(&self) -> u16 {
        self.local_addr.get().map(|(_, p)| *p).unwrap_or(0)
    }

    /// 只读访问配置。
    pub fn config(&self) -> &DeviceConfig {
        &self.config
    }

    /// 当前设备固件版本。升级成功后该值会立即反映持久化的模拟版本。
    pub fn current_firmware(&self) -> String {
        self.upgrade_store.current_firmware()
    }

    /// 注入桌面端录像目录。重复注入时保留新 Store 的持久化真相。
    pub fn install_recording_store(&self, store: Arc<crate::recording::RecordingStore>) {
        if let Ok(mut service) = self.recording_service.lock() {
            *service = Some(Arc::new(crate::recording::RecordingService::new(store)));
        }
    }

    pub fn recording_service(&self) -> Option<Arc<crate::recording::RecordingService>> {
        self.recording_service
            .lock()
            .ok()
            .and_then(|service| service.clone())
    }

    pub async fn start_recording(
        &self,
        kind: crate::recording::RecordingKind,
        source: crate::recording::RecordingSource,
    ) -> Result<crate::recording::RecordingState> {
        let service = self
            .recording_service()
            .ok_or_else(|| Error::Media("录像存储未配置".into()))?;
        let media = self.shared_media.lock().await.clone();
        let channel_id = self
            .config
            .channels
            .first()
            .map(|channel| channel.channel_id.as_str().to_owned())
            .unwrap_or_else(|| self.config.device_id.as_str().to_owned());
        service
            .start(media, channel_id, kind, source, self.config.video_fps)
            .await
    }

    pub async fn stop_recording(&self) -> Result<Option<crate::recording::RecordingEntry>> {
        let service = self
            .recording_service()
            .ok_or_else(|| Error::Media("录像存储未配置".into()))?;
        service.stop().await
    }

    fn mark_runtime_change(&self, kind: impl Into<String>) {
        if let Ok(mut change) = self.runtime_change.lock() {
            change.kind = kind.into();
            change.updated_at_ms = epoch_millis();
        }
    }

    fn mark_alarm_active(&self) {
        if let Ok(mut state) = self.control_state.lock() {
            state.alarming = true;
        }
        self.mark_runtime_change("alarm_raised");
    }

    /// 读取当前设备实例的会话级运行时状态。只复制数据，不持锁执行 I/O。
    pub fn runtime_snapshot(&self) -> DeviceRuntimeSnapshot {
        let (guarded, alarming) = self
            .control_state
            .lock()
            .map(|state| (state.guarded, state.alarming))
            .unwrap_or((false, false));
        let (longitude, latitude) = self
            .position
            .lock()
            .map(|position| *position)
            .unwrap_or_default();
        let config = self.runtime_config.lock().ok();
        let basic = config.as_ref().and_then(|value| value.basic_param.as_ref());
        let change = self.runtime_change.lock().ok();
        DeviceRuntimeSnapshot {
            guarded,
            alarming,
            longitude,
            latitude,
            name: basic.map(|value| value.name.clone()).unwrap_or_default(),
            expiration: basic.map(|value| value.expiration).unwrap_or_default(),
            heartbeat_interval: basic
                .map(|value| value.heartbeat_interval)
                .unwrap_or_default(),
            heartbeat_count: basic.map(|value| value.heartbeat_count).unwrap_or_default(),
            video_record_plan_type: config
                .as_ref()
                .and_then(|value| value.video_record_plan.as_ref())
                .map(|value| value.record_plan_type),
            alarm_record_duration: config
                .as_ref()
                .and_then(|value| value.video_alarm_record.as_ref())
                .map(|value| value.duration),
            picture_mask_enabled: config
                .as_ref()
                .and_then(|value| value.picture_mask.as_ref())
                .map(|value| value.on),
            frame_mirror_mode: config
                .as_ref()
                .and_then(|value| value.frame_mirror.as_ref())
                .map(|value| value.mode),
            alarm_report_enabled: config
                .as_ref()
                .and_then(|value| value.alarm_report.as_ref())
                .map(|value| value.enabled),
            osd_time_show: config
                .as_ref()
                .and_then(|value| value.osd_config.as_ref())
                .map(|value| value.time_visible() as u32),
            osd_show: config
                .as_ref()
                .and_then(|value| value.osd_config.as_ref())
                .map(|value| value.text_visible() as u32),
            last_change: change
                .as_ref()
                .map(|value| value.kind.clone())
                .unwrap_or_default(),
            updated_at_ms: change
                .as_ref()
                .map(|value| value.updated_at_ms)
                .unwrap_or_default(),
        }
    }

    /// 前端积分出的当前姿态回写。预置位设置、扫描边界都以此为准。
    pub fn set_local_pose(&self, pan: f32, tilt: f32, zoom: f32, focus: f32, iris: f32) {
        if let Ok(mut state) = self.control_state.lock() {
            state.pose = (
                pan.clamp(-180.0, 180.0),
                tilt.clamp(-90.0, 90.0),
                zoom.clamp(1.0, 20.0),
            );
            state.focus = focus.clamp(0.0, 1.0);
            state.iris = iris.clamp(0.0, 1.0);
        }
    }

    /// 模拟器中心的一次性只读快照。
    pub fn control_snapshot(&self) -> ControlSnapshot {
        let state = self
            .control_state
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();

        let names = self
            .presets
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let poses = self
            .preset_poses
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let presets = names
            .into_iter()
            .map(|(id, name)| {
                let (pan, tilt, zoom) = poses.get(&id).copied().unwrap_or((0.0, 0.0, 1.0));
                PresetSnapshot { id, name, pan, tilt, zoom }
            })
            .collect();

        let cruise = state
            .cruise_tracks
            .iter()
            .map(|(id, points)| CruiseSnapshot {
                id: *id,
                points: points.clone(),
                speed: state.cruise_speed.get(id).copied(),
                dwell: state.cruise_dwell.get(id).copied(),
                active: state.active_cruise == Some(*id),
            })
            .collect();

        let scan = state
            .scan_groups
            .iter()
            .map(|(id, group)| ScanSnapshot {
                id: *id,
                left: group.left,
                right: group.right,
                speed: group.speed,
                active: state.active_scan == Some(*id),
            })
            .collect();

        let storage_cards = if state.storage_queried {
            vec![StorageCardSnapshot {
                id: 1,
                name: "SD Card 1".into(),
                capacity_mb: 32768,
                free_mb: 24576,
                status: "正常".into(),
                progress: None,
            }]
        } else {
            Vec::new()
        };

        let (mask_configured, mask_on, mask_regions) = {
            let config = self.runtime_config.lock().ok();
            let mask = config
                .as_ref()
                .and_then(|value| value.picture_mask.as_ref());
            let configured = mask.is_some();
            let on = mask.map(|value| value.on == 1).unwrap_or(false);
            let regions = mask.map(mask_regions).unwrap_or_default();
            (configured, on, regions)
        };

        let (osd_configured, osd_length, osd_width, osd_time_x, osd_time_y, osd_time_visible, osd_text_visible, osd_time_type, osd_items) = {
            let config = self.runtime_config.lock().ok();
            let osd = config
                .as_ref()
                .and_then(|value| value.osd_config.as_ref());
            (
                osd.is_some(),
                osd.map(|value| value.length).unwrap_or(1920),
                osd.map(|value| value.width).unwrap_or(1080),
                osd.map(|value| value.time_x).unwrap_or(10),
                osd.map(|value| value.time_y).unwrap_or(10),
                osd.map(|value| value.time_visible()).unwrap_or(false),
                osd.map(|value| value.text_visible()).unwrap_or(false),
                osd.and_then(|value| value.time_type),
                osd.map(|value| {
                    value
                        .items
                        .iter()
                        .map(|item| OsdItemSnapshot {
                            text: item.text.clone(),
                            x: item.x,
                            y: item.y,
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            )
        };

        ControlSnapshot {
            pan: state.pose.0,
            tilt: state.pose.1,
            zoom: state.pose.2,
            focus: state.focus,
            iris: state.iris,
            presets,
            home_enabled: state.home_enabled,
            home_preset_index: state.home_preset_index,
            home_reset_time: state.home_reset_time,
            cruise,
            scan,
            storage_queried: state.storage_queried,
            storage_cards,
            mask_configured,
            mask_on,
            mask_regions,
            osd_configured,
            osd_length,
            osd_width,
            osd_time_x,
            osd_time_y,
            osd_time_visible,
            osd_text_visible,
            osd_time_type,
            osd_items,
            guarded: state.guarded,
            alarming: state.alarming,
            target_mode: state.target_mode,
            target_box: state.target_box,
            drag_zoom_box: state.drag_zoom_box,
        }
    }

    /// 导出跨进程持久化的控制配置。
    pub fn export_persisted_control_state(&self) -> PersistedControlState {
        let snapshot = self.control_snapshot();
        PersistedControlState {
            presets: snapshot
                .presets
                .into_iter()
                .map(|preset| PersistedPreset {
                    id: preset.id,
                    name: preset.name,
                    pan: preset.pan,
                    tilt: preset.tilt,
                    zoom: preset.zoom,
                })
                .collect(),
            home_enabled: snapshot.home_enabled,
            home_preset_index: snapshot.home_preset_index,
            home_reset_time: snapshot.home_reset_time,
            cruise: snapshot
                .cruise
                .into_iter()
                .map(|track| PersistedCruise {
                    id: track.id,
                    points: track.points,
                    speed: track.speed,
                    dwell: track.dwell,
                })
                .collect(),
            scan: snapshot
                .scan
                .into_iter()
                .map(|group| PersistedScan {
                    id: group.id,
                    left: group.left,
                    right: group.right,
                    speed: group.speed,
                })
                .collect(),
            osd_config: self.platform_osd.lock().ok().and_then(|value| value.clone()),
            // 出厂默认(关闭、无区域)不落盘，保持旧文件格式干净。
            picture_mask: self
                .runtime_config
                .lock()
                .ok()
                .and_then(|config| config.picture_mask.clone())
                .filter(|mask| *mask != gb28181_protocol::manscdp::PictureMask::default()),
        }
    }

    /// 恢复跨进程持久化的控制配置；瞬态运行态(active/目标/查询)不恢复。
    pub fn import_persisted_control_state(&self, saved: &PersistedControlState) {
        if let Some(osd) = &saved.osd_config {
            if let Ok(mut platform) = self.platform_osd.lock() {
                *platform = Some(osd.clone());
            }
        }
        if let Ok(mut config) = self.runtime_config.lock() {
            if let Some(osd) = &saved.osd_config {
                config.osd_config = Some(osd.clone());
            }
            if let Some(mask) = &saved.picture_mask {
                config.picture_mask = Some(mask.clone());
            }
        }
        if let Ok(mut presets) = self.presets.lock() {
            presets.clear();
            for preset in &saved.presets {
                presets.insert(preset.id, preset.name.clone());
            }
        }
        if let Ok(mut poses) = self.preset_poses.lock() {
            poses.clear();
            for preset in &saved.presets {
                poses.insert(preset.id, (preset.pan, preset.tilt, preset.zoom));
            }
        }
        if let Ok(mut state) = self.control_state.lock() {
            state.home_enabled = saved.home_enabled;
            state.home_preset_index = saved.home_preset_index;
            state.home_reset_time = saved.home_reset_time;
            state.home_set = saved.home_preset_index.is_some();
            state.cruise_tracks.clear();
            state.cruise_speed.clear();
            state.cruise_dwell.clear();
            for track in &saved.cruise {
                state.cruise_tracks.insert(track.id, track.points.clone());
                if let Some(speed) = track.speed {
                    state.cruise_speed.insert(track.id, speed);
                }
                if let Some(dwell) = track.dwell {
                    state.cruise_dwell.insert(track.id, dwell);
                }
            }
            state.scan_groups.clear();
            for group in &saved.scan {
                state.scan_groups.insert(
                    group.id,
                    ScanGroupState {
                        left: group.left,
                        right: group.right,
                        speed: group.speed,
                    },
                );
            }
            state.active_cruise = None;
            state.active_scan = None;
        }
    }

    fn runtime_config_response(&self, query: &gb28181_protocol::manscdp::Query) -> Result<String> {
        let mut response = self
            .runtime_config
            .lock()
            .map_err(|_| Error::Gb28181("设备运行配置锁异常".into()))?
            .clone();
        response.sn = query.sn;
        response.device_id = query.device_id.clone();
        let lower = query
            .config_type
            .as_deref()
            .unwrap_or("BasicParam")
            .to_ascii_lowercase();
        let wants = |name: &str| lower.contains(name);
        if !(lower.is_empty() || wants("basicparam")) {
            response.basic_param = None;
        }
        if !wants("videoparamopt") {
            response.video_param_opt = None;
        }
        if !wants("videorecordplan") {
            response.video_record_plan = None;
        }
        if !wants("videoalarmrecord") {
            response.video_alarm_record = None;
        }
        if !wants("picturemask") {
            response.picture_mask = None;
        }
        if !wants("framemirror") {
            response.frame_mirror = None;
        }
        if !wants("alarmreport") {
            response.alarm_report = None;
        }
        if !wants("osdconfig") {
            response.osd_config = None;
        }
        if !wants("snapshotconfig") {
            response.snap_shot_config = None;
        }
        if !wants("videoparamattribute") {
            response.video_param_attribute = None;
        }
        if !wants("svacencodeconfig") {
            response.svac_encode_config = None;
        }
        if !wants("svacdecodeconfig") {
            response.svac_decode_config = None;
        }
        response.to_xml()
    }

    fn apply_device_config(&self, ctrl: &gb28181_protocol::manscdp::Control) -> Result<bool> {
        // Encoding changes must be applied to the capture pipeline, never just the query DTO.
        if ctrl.cfg_video_param_attribute.is_some() {
            return Err(Error::Gb28181(
                "媒体参数需停止设备后在音视频配置中修改".into(),
            ));
        }
        let mut current = self
            .runtime_config
            .lock()
            .map_err(|_| Error::Gb28181("设备运行配置锁异常".into()))?;
        let mut changed = false;
        if let Some(incoming) = &ctrl.cfg_basic_param {
            if let Some(basic) = current.basic_param.as_mut() {
                if !incoming.name.trim().is_empty() {
                    basic.name = incoming.name.clone();
                }
                if incoming.expiration > 0 {
                    basic.expiration = incoming.expiration;
                }
                if incoming.heartbeat_interval > 0 {
                    basic.heartbeat_interval = incoming.heartbeat_interval;
                }
                if incoming.heartbeat_count > 0 {
                    basic.heartbeat_count = incoming.heartbeat_count;
                }
            } else {
                current.basic_param = Some(incoming.clone());
            }
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_video_record_plan {
            current.video_record_plan = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_video_alarm_record {
            current.video_alarm_record = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_picture_mask {
            current.picture_mask = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_frame_mirror {
            current.frame_mirror = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_alarm_report {
            current.alarm_report = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_osd_config {
            current.osd_config = Some(value.clone());
            if let Ok(mut platform) = self.platform_osd.lock() {
                *platform = Some(value.clone());
            }
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_svac_encode {
            current.svac_encode_config = Some(value.clone());
            changed = true;
        }
        if let Some(value) = &ctrl.cfg_svac_decode {
            current.svac_decode_config = Some(value.clone());
            changed = true;
        }
        drop(current);
        if changed {
            self.mark_runtime_change("device_config_applied");
        }
        Ok(changed)
    }

    /// 设置本地 OSD 样式。必须在启动设备前调用；None 表示不烧录 OSD。
    pub fn set_osd_settings(&self, settings: Option<media_rtp::osd::OsdSettings>) {
        *self.osd_settings.lock().unwrap() = settings;
    }

    /// 当前要烧进实时采集画面的 OSD(本地样式 + 平台 OSDConfig + 遮挡)。
    pub fn osd_overlay(&self) -> Option<media_rtp::osd::OsdOverlay> {
        let settings = self.osd_settings.lock().ok()?.clone()?;
        let platform = self.platform_osd.lock().ok()?.clone();
        let mask = self
            .runtime_config
            .lock()
            .ok()
            .and_then(|config| config.picture_mask.clone());
        let output = self
            .config
            .media_profile
            .as_ref()
            .map(|media| (media.width, media.height))
            .unwrap_or((1920, 1080));
        let channel_name = self
            .config
            .channels
            .first()
            .map(|channel| channel.name.as_str())
            .unwrap_or_default();
        merge_osd_overlay(&settings, platform.as_ref(), mask.as_ref(), channel_name, output)
    }

    /// Install a verified immutable file output prepared before REGISTER.
    pub fn install_prepared_file(
        &self,
        prepared: media_rtp::file_profile::PreparedFileProfile,
    ) -> std::result::Result<(), String> {
        if self.config.media_profile.as_ref() != Some(prepared.profile()) {
            return Err("文件准备结果与设备媒体配置不一致".into());
        }
        *self
            .prepared_file
            .lock()
            .map_err(|_| "文件准备状态锁异常")? = Some(prepared);
        Ok(())
    }

    /// 注册成功后启动唯一媒体采集源。采集失败不影响 SIP 注册，但会通过观察者上报。
    async fn start_shared_media(&self) -> Result<bool> {
        if let Some(profile) = &self.config.media_profile {
            profile.validate().map_err(Error::Media)?;
            if profile.video_fps != self.config.video_fps {
                return Err(Error::Media("媒体配置与兼容FPS冲突".into()));
            }
        }
        {
            let mut active = self.shared_media.lock().await;
            if active.as_ref().is_some_and(|media| media.is_alive()) {
                return Ok(true);
            }
            if let Some(stale) = active.take() {
                stale.stop();
            }
        }
        let Some(path) = self.config.video_source.clone() else {
            let Some(kbps) = self.config.light_bitrate_kbps else {
                return Ok(false);
            };
            let source = Box::new(media_rtp::LightSource::new(kbps, self.config.video_fps))
                as Box<dyn media_rtp::VideoSource>;
            let media = media_rtp::start_shared_media(source, self.config.video_fps);
            *self.shared_media.lock().await = Some(media);
            return self.wait_for_media_ready().await;
        };
        let fps = self.config.video_fps;
        let profile = self.config.media_profile.clone();
        let prepared = self
            .prepared_file
            .lock()
            .map_err(|_| Error::Media("文件准备状态锁异常".into()))?
            .clone();
        // 本地预览不再由采集源内部旁路,改为桌面端订阅共享媒体总线(media-pipeline-v2 P4)。
        // OSD 只烧进实时采集；文件循环不重新编码。
        let osd = path.starts_with("live:").then(|| self.osd_overlay()).flatten();
        let source = tokio::task::spawn_blocking(move || {
            if path.starts_with("live:") {
                let capture = match profile {
                    Some(profile) => {
                        media_rtp::LiveSource::capture_with_profile_osd(&path, profile, None, osd)
                    }
                    None => media_rtp::LiveSource::capture(&path, fps, None),
                };
                capture
                    .map(|source| Box::new(source) as Box<dyn media_rtp::VideoSource>)
                    .map_err(|error| Error::Media(format!("实时采集失败: {error}")))
            } else if profile.is_some() {
                let source = prepared
                    .ok_or_else(|| Error::Media("文件尚未按媒体配置准备，请重新启动设备".into()))?
                    .open_source(true)
                    .map_err(Error::Media)?;
                Ok(Box::new(source) as Box<dyn media_rtp::VideoSource>)
            } else {
                media_rtp::FileSource::from_path_av(&path, fps)
                    .map(|source| Box::new(source) as Box<dyn media_rtp::VideoSource>)
                    .map_err(|error| Error::Media(format!("加载视频源失败: {error}")))
            }
        })
        .await
        .map_err(|error| Error::Media(format!("媒体源启动任务异常: {error}")))??;
        let media = media_rtp::start_shared_media(source, fps);
        *self.shared_media.lock().await = Some(media);
        self.wait_for_media_ready().await
    }

    async fn wait_for_media_ready(&self) -> Result<bool> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let status = {
                let media = self.shared_media.lock().await;
                media.as_ref().map(|media| {
                    if media.has_frames() {
                        Some(Ok(true))
                    } else if let Some(error) = media.capture_error() {
                        Some(Err(Error::Media(error)))
                    } else if !media.is_alive() {
                        Some(Ok(false))
                    } else {
                        None
                    }
                })
            };
            if let Some(result) = status.flatten() {
                return result;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Media(
                    "视频采集启动超时：10 秒内没有产生编码帧".into(),
                ));
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    /// 停止注册期唯一采集源，释放摄像头/屏幕和编码器。
    pub async fn stop_shared_media(&self) {
        if let Some(media) = self.shared_media.lock().await.take() {
            if !media.stop_and_wait(std::time::Duration::from_secs(3)).await {
                tracing::warn!("共享媒体在 3 秒内未完成 owned child 回收");
            }
        }
    }

    /// 只重建本地预览 worker；注册、主采集与平台推流保持不动。
    /// 订阅当前共享媒体(本地预览用)。采集未启动或已停止时返回 `None`。
    pub async fn subscribe_shared_media(&self) -> Option<media_rtp::SharedVideoSource> {
        self.shared_media
            .lock()
            .await
            .as_ref()
            .filter(|media| media.is_alive())
            .map(|media| media.subscribe())
    }

    /// 当前采集画面的 JPEG:取共享媒体缓存的最近一个带参数集关键帧解码。
    ///
    /// 平台 SnapShotConfig 抓拍与桌面端本地抓拍共用,不依赖本地预览是否开启。
    pub async fn current_keyframe_jpeg(&self) -> Result<Vec<u8>> {
        let current = self
            .shared_media
            .lock()
            .await
            .as_ref()
            .and_then(|media| media.latest_config_frame());
        let Some((frame, codec)) = current else {
            return Err(Error::Media("尚无可解码的当前关键帧，请确认设备已启动采集".into()));
        };
        tokio::task::spawn_blocking(move || media_rtp::frame_to_jpeg(&frame, codec))
            .await
            .map_err(|error| Error::Media(format!("抓拍转码任务异常: {error}")))?
    }

    pub async fn retry_preview(&self) -> bool {
        self.shared_media
            .lock()
            .await
            .as_ref()
            .is_some_and(|media| media.retry_preview())
    }

    /// 查询采集源是否已经真正产出过编码帧，供桌面端刷新页面后恢复状态。
    pub async fn capture_ready(&self) -> bool {
        self.shared_media
            .lock()
            .await
            .as_ref()
            .is_some_and(|media| media.has_frames())
    }

    /// 设置本端信令地址(仅供测试/非 run 场景手动注入应答用;run() 会自动设置)。
    #[doc(hidden)]
    pub fn set_local_addr_for_test(&self, host: &str, port: u16) {
        let _ = self.local_addr.set((host.to_string(), port));
    }

    /// 载入一个内置目录模板(single/nvr-8ch/civil-3x2/large-16ch/business-platform),
    /// 替换现有目录树(FR-34)。business-platform 用于模拟下级平台业务分组。
    pub fn load_catalog_template(&self, template: &str) {
        let channel_name = self
            .config
            .channels
            .first()
            .map(|c| c.name.as_str())
            .unwrap_or("");
        let tree = gb28181_protocol::id_codec::catalog_template(
            template,
            self.config.device_id.as_str(),
            &self.config.device_info.device_name,
            &self.config.server_domain,
            channel_name,
        );
        *self.catalog_tree.lock().unwrap() = tree;
    }

    /// 直接设置目录树节点(FR-34)。首节点应为根设备。
    pub fn set_catalog_tree(
        &self,
        nodes: Vec<gb28181_protocol::id_codec::CatalogNode>,
    ) -> std::result::Result<(), String> {
        gb28181_protocol::id_codec::validate_catalog_tree(&nodes)?;
        *self.catalog_tree.lock().unwrap() = nodes;
        Ok(())
    }

    /// 当前目录树快照(空表示用 config.channels 扁平列表)。
    pub fn catalog_tree(&self) -> Vec<gb28181_protocol::id_codec::CatalogNode> {
        self.catalog_tree.lock().unwrap().clone()
    }

    /// 新增/更新一个目录通道节点(按 id 覆盖)。返回是否为新增。
    pub fn upsert_channel(
        &self,
        node: gb28181_protocol::id_codec::CatalogNode,
    ) -> std::result::Result<bool, String> {
        let mut tree = self.catalog_tree.lock().unwrap();
        let mut candidate = tree.clone();
        let added = if let Some(existing) = candidate.iter_mut().find(|n| n.id == node.id) {
            *existing = node;
            false
        } else {
            candidate.push(node);
            true
        };
        gb28181_protocol::id_codec::validate_catalog_tree(&candidate)?;
        *tree = candidate;
        Ok(added)
    }

    /// 删除一个目录节点及其全部后代。返回删除节点数。
    pub fn remove_channel(&self, id: &str) -> std::result::Result<usize, String> {
        let mut tree = self.catalog_tree.lock().unwrap();
        let Some(target) = tree.iter().find(|node| node.id == id) else {
            return Ok(0);
        };
        if target.id == target.parent_id {
            return Err("不能删除目录根节点".into());
        }
        let mut removed = std::collections::HashSet::from([id.to_string()]);
        loop {
            let before = removed.len();
            for node in tree.iter() {
                if removed.contains(&node.parent_id) {
                    removed.insert(node.id.clone());
                }
            }
            if removed.len() == before {
                break;
            }
        }
        let before = tree.len();
        let candidate: Vec<_> = tree
            .iter()
            .filter(|node| !removed.contains(&node.id))
            .cloned()
            .collect();
        gb28181_protocol::id_codec::validate_catalog_tree(&candidate)?;
        let count = before - candidate.len();
        *tree = candidate;
        Ok(count)
    }

    /// 最近一次平台目录查询活动。
    pub fn catalog_activity(&self) -> Option<CatalogQueryActivity> {
        self.catalog_activity
            .lock()
            .ok()
            .and_then(|value| value.clone())
    }

    /// 返回可发布的完整目录节点。根设备节点(parent==self)不作为订阅通道项。
    fn catalog_publishable(&self) -> Vec<gb28181_protocol::id_codec::CatalogNode> {
        use gb28181_protocol::id_codec::{catalog_business_group_id, catalog_parent_id};
        let tree = self.effective_catalog_tree();
        tree.iter()
            .filter(|node| node.id != node.parent_id)
            .map(|node| {
                let mut item = node.clone();
                item.business_group_id = catalog_business_group_id(&tree, node);
                item.parent_id = catalog_parent_id(&tree, node).unwrap_or_default();
                item
            })
            .collect()
    }

    /// 本设备注册用的 Call-ID(用于在共享传输上注册接收路由)。
    pub fn call_id(&self) -> &str {
        &self.ids.call_id
    }

    fn next_cseq(&self) -> u32 {
        self.cseq.fetch_add(1, Ordering::Relaxed)
    }

    /// 执行完整注册流程(REGISTER→401→带鉴权 REGISTER→200)。
    ///
    /// `transport` 为共享 UDP 传输;`local_host/local_port` 为本端对外地址(填 Via/Contact);
    /// 成功返回 [`DeviceState::Registered`],鉴权/拒绝失败返回 [`DeviceState::Failed`] 对应错误。
    pub async fn register(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        use common::{DeviceEvent, FailureKind};
        self.observer.on_event(DeviceEvent::RegisterAttempt);
        let result = self.register_inner(transport, local_host, local_port).await;
        match &result {
            Ok(_) => {
                self.observer.on_event(DeviceEvent::RegisterSuccess);
                self.observer.on_event(DeviceEvent::CaptureStarting);
                match self.start_shared_media().await {
                    Ok(true) => self.observer.on_event(DeviceEvent::CaptureReady),
                    Ok(false) => self.observer.on_event(DeviceEvent::CaptureStopped),
                    Err(error) => {
                        // 初始化失败不能留下一个“活着但不可用”的采集源，否则下一次重注册
                        // 会误判为 ready，页面也会永远订阅到坏总线。
                        self.stop_shared_media().await;
                        self.observer
                            .on_event(DeviceEvent::CaptureFailure(error.to_string()));
                        self.observer.on_event(DeviceEvent::RuntimeError {
                            scope: "capture".into(),
                            message: error.to_string(),
                        });
                    }
                }
            }
            Err(e) => {
                // 按错误信息归因:超时 → Timeout,含状态码拒绝 → Rejected,其余 Other。
                let kind = match e {
                    Error::Sip(msg) if msg.contains("超时") => FailureKind::Timeout,
                    Error::Sip(msg) if msg.contains("被拒") => FailureKind::Rejected,
                    _ => FailureKind::Other,
                };
                self.observer.on_event(DeviceEvent::RegisterFailure {
                    kind,
                    message: e.to_string(),
                });
            }
        }
        result
    }

    /// 注册核心逻辑(不含事件上报)。
    async fn register_inner(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| {
                Error::Config(format!(
                    "平台地址非法: {}:{}",
                    self.config.server_host, self.config.server_port
                ))
            })?;
        let mut rx = transport.register(self.ids.call_id.clone());
        let timing = sip_core::Timing::default();

        // 首发 REGISTER(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(
            &self.config,
            &self.ids,
            cseq1,
            local_host,
            local_port,
            None,
            self.config.register_expires_secs,
        );
        let resp = sip_core::client_transact(transport, dst, &req1, &mut rx, timing).await?;

        match resp.status {
            200 => Ok(DeviceState::Registered),
            401 => {
                // 解析挑战,带 Authorization 重发。
                let wa = resp
                    .headers
                    .get("WWW-Authenticate")
                    .ok_or_else(|| Error::Sip("401 缺少 WWW-Authenticate".into()))?;
                let challenge = Challenge::parse(wa)?;
                let auth = authorization(
                    &challenge,
                    &self.config.username,
                    &self.config.password,
                    "REGISTER",
                    &req1.uri,
                );
                let cseq2 = self.next_cseq();
                let req2 = builder::register(
                    &self.config,
                    &self.ids,
                    cseq2,
                    local_host,
                    local_port,
                    Some(&auth),
                    self.config.register_expires_secs,
                );
                let resp2 =
                    sip_core::client_transact(transport, dst, &req2, &mut rx, timing).await?;
                if resp2.status == 200 {
                    // 网络校时:用平台 200 OK 的 Date 头校准本地时钟偏移(FR)。
                    if let Some(date) = resp2.headers.get("Date") {
                        if let Some(off) = common::clock::sync_from_date_header(date) {
                            tracing::info!(platform_date = %date, offset_secs = off, "网络校时完成");
                        }
                    }
                    Ok(DeviceState::Registered)
                } else {
                    Err(Error::Sip(format!(
                        "鉴权后注册被拒: {} {}",
                        resp2.status, resp2.reason
                    )))
                }
            }
            other => Err(Error::Sip(format!("注册被拒: {} {}", other, resp.reason))),
        }
    }

    /// 向平台注销(GB28181-2022 §9.1.2.2):发 REGISTER Expires=0。
    ///
    /// 设备下线时主动注销,平台立即置离线,无需等心跳超时。与注册同样处理 401 鉴权挑战。
    /// 尽力而为:失败仅记录警告(下线流程不因注销失败而阻塞)。
    pub async fn unregister(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        // 用独立 Call-ID,避免与运行中的注册路由冲突。
        let ids = DialogIds::new();
        let mut rx = transport.register(ids.call_id.clone());
        let timing = sip_core::Timing::default();

        // 首发 REGISTER Expires=0(无鉴权)。
        let cseq1 = self.next_cseq();
        let req1 = builder::register(&self.config, &ids, cseq1, local_host, local_port, None, 0);
        let result = async {
            let resp = sip_core::client_transact(transport, dst, &req1, &mut rx, timing).await?;
            match resp.status {
                200 => Ok(()),
                401 => {
                    let wa = resp
                        .headers
                        .get("WWW-Authenticate")
                        .ok_or_else(|| Error::Sip("注销 401 缺少 WWW-Authenticate".into()))?;
                    let challenge = Challenge::parse(wa)?;
                    let auth = authorization(
                        &challenge,
                        &self.config.username,
                        &self.config.password,
                        "REGISTER",
                        &req1.uri,
                    );
                    let cseq2 = self.next_cseq();
                    let req2 = builder::register(
                        &self.config,
                        &ids,
                        cseq2,
                        local_host,
                        local_port,
                        Some(&auth),
                        0,
                    );
                    let resp2 =
                        sip_core::client_transact(transport, dst, &req2, &mut rx, timing).await?;
                    if resp2.status == 200 {
                        Ok(())
                    } else {
                        Err(Error::Sip(format!("注销被拒: {}", resp2.status)))
                    }
                }
                other => Err(Error::Sip(format!("注销被拒: {other}"))),
            }
        }
        .await;
        transport.unregister(&ids.call_id);
        result
    }

    /// 发送一条无对话的 MANSCDP MESSAGE(心跳/报警/位置等)并等平台响应。
    ///
    /// 每次事务用**全新** [`DialogIds`](独立 Call-ID):同一设备的心跳、报警、
    /// 位置上报可能并发(周期任务 + 手动触发),若共用注册 Call-ID 会在共享传输上
    /// 互相覆盖接收路由、抢走对方的响应导致误判超时。独立 Call-ID 彻底隔离,
    /// 事务结束即注销路由。
    async fn send_message_xml(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<u16> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(&self.config, &ids, cseq, local_host, local_port, xml);
        let mut rx = transport.register(ids.call_id.clone());
        let result =
            sip_core::client_transact(transport, dst, &req, &mut rx, sip_core::Timing::default())
                .await;
        transport.unregister(&ids.call_id);
        Ok(result?.status)
    }

    /// 发送一条通知 MESSAGE 但**不等待响应**(fire-and-forget)。
    ///
    /// 用于纯告知性、平台可能不 ACK 的通知(如 VideoUploadNotify —— 部分平台
    /// 不支持该 CmdType 直接丢弃)。发出即返回,不因平台不响应而阻塞重传。
    async fn send_message_xml_oneshot(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        xml: &str,
    ) -> Result<()> {
        let dst: SocketAddr = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse()
            .map_err(|_| Error::Config("平台地址非法".into()))?;
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let req = builder::message_xml(&self.config, &ids, cseq, local_host, local_port, xml);
        transport
            .send_to(&sip_core::SipMessage::Request(req), dst)
            .await
    }

    /// 发送一次心跳(MESSAGE + Keepalive XML)。返回平台响应状态码。
    pub async fn send_keepalive(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        let ka = Keepalive::ok(self.config.device_id.as_str(), sn);
        let xml = ka.to_xml()?;
        let status = self
            .send_message_xml(transport, local_host, local_port, &xml)
            .await?;
        let resp_status = status;
        if resp_status == 200 {
            self.observer.on_event(common::DeviceEvent::HeartbeatOk);
        } else {
            self.observer.on_event(common::DeviceEvent::HeartbeatFail);
        }
        Ok(resp_status)
    }

    /// 订阅状态变化统一出口：前端「活跃订阅面板」靠它点亮四类订阅。
    fn emit_subscription(&self, kind: &str, active: bool, notify_count: u32) {
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: kind.into(),
                active,
                notify_count,
            });
    }

    /// 查询四类订阅的当前状态（页面刷新/重新进入时补一次基线，避免只依赖事件）。
    pub async fn subscription_snapshot(&self) -> Vec<SubscriptionSnapshot> {
        let mobile_active = self
            .subscriptions
            .lock()
            .await
            .contains_key("MobilePosition");
        let alarm_active = self.alarm_dialog.lock().await.is_some();
        let catalog_active = self.catalog_dialog.lock().await.is_some();
        let ptz_active = self.ptz_dialog.lock().await.is_some();
        let ptz_position = ptz_active.then(|| {
            let (pan, tilt, zoom) = self.precise_pose();
            PtzPositionSnapshot::from_pose(pan, tilt, zoom)
        });
        vec![
            self.build_subscription_snapshot(
                "MobilePosition",
                mobile_active,
                self.position_notify_count
                    .load(std::sync::atomic::Ordering::Relaxed),
                None,
            ),
            self.build_subscription_snapshot(
                "Alarm",
                alarm_active,
                self.alarm_notify_count
                    .load(std::sync::atomic::Ordering::Relaxed),
                None,
            ),
            self.build_subscription_snapshot(
                "Catalog",
                catalog_active,
                self.catalog_notify_count
                    .load(std::sync::atomic::Ordering::Relaxed),
                None,
            ),
            self.build_subscription_snapshot(
                "PTZPosition",
                ptz_active,
                self.ptz_notify_count
                    .load(std::sync::atomic::Ordering::Relaxed),
                ptz_position,
            ),
        ]
    }

    /// 组装单条订阅快照。生命周期由「是否活跃」+「有效期是否走完」共同决定：
    /// 平台订阅到点未续订时，设备这边其实已不再上报，但界面若只看 `active`
    /// 会一直显示"已订阅"，排障时最容易走错方向。
    fn build_subscription_snapshot(
        &self,
        kind: &str,
        active: bool,
        notify_count: u32,
        ptz_position: Option<PtzPositionSnapshot>,
    ) -> SubscriptionSnapshot {
        let now_ms = epoch_millis();
        let meta = self
            .subscription_meta
            .lock()
            .ok()
            .and_then(|metas| metas.get(kind).cloned())
            .unwrap_or_default();
        let ever_subscribed = meta.subscribed_at_ms > 0;
        let remaining_secs = if active { meta.remaining_secs(now_ms) } else { 0 };
        let lifecycle = if active && remaining_secs > 0 {
            "subscribed"
        } else if active {
            "expired"
        } else if meta.last_error.is_some() {
            // 通知失败后又没了订阅对话，优先暴露异常而不是"已取消"。
            "exception"
        } else if ever_subscribed {
            "cancelled"
        } else {
            "not_subscribed"
        };
        SubscriptionSnapshot {
            kind: kind.to_string(),
            active,
            lifecycle: lifecycle.to_string(),
            subscriber: active.then(|| meta.subscriber.clone()).flatten(),
            expires_secs: meta.expires_secs,
            remaining_secs,
            notify_count,
            last_notify_at_ms: meta.last_notify_at_ms,
            last_notify_result: meta.last_notify_result.clone(),
            last_error: meta.last_error.clone(),
            ptz_position,
        }
    }

    /// 记录一条订阅已建立（SUBSCRIBE 到达且 Expires > 0）。
    fn mark_subscribed(&self, kind: &str, subscriber: String, expires_secs: u32) {
        if !is_tracked_subscription(kind) {
            return;
        }
        let Ok(mut metas) = self.subscription_meta.lock() else {
            return;
        };
        let entry = metas.entry(kind.to_string()).or_default();
        entry.subscriber = Some(subscriber);
        entry.expires_secs = expires_secs;
        entry.subscribed_at_ms = epoch_millis();
        // 重新订阅是新的生命周期起点，旧的通知痕迹不该留到新周期里。
        entry.last_notify_at_ms = None;
        entry.last_notify_result = None;
        entry.last_error = None;
    }

    /// 记录一条订阅被取消（平台退订 Expires:0 / 设备下线）。
    fn mark_unsubscribed(&self, kind: &str) {
        if !is_tracked_subscription(kind) {
            return;
        }
        let Ok(mut metas) = self.subscription_meta.lock() else {
            return;
        };
        if let Some(entry) = metas.get_mut(kind) {
            // 保留 subscribed_at_ms，让快照能区分"曾经订阅过"和"从未订阅"。
            entry.expires_secs = 0;
            entry.subscriber = None;
        }
    }

    /// 记录一次 NOTIFY 结果。成功时清掉旧错误，避免界面挂着已修复的报错。
    fn record_notify_outcome(&self, kind: &str, outcome: &Result<u16>) {
        if !is_tracked_subscription(kind) {
            return;
        }
        let Ok(mut metas) = self.subscription_meta.lock() else {
            return;
        };
        let entry = metas.entry(kind.to_string()).or_default();
        entry.last_notify_at_ms = Some(epoch_millis());
        match outcome {
            Ok(status) => {
                entry.last_notify_result = Some(status.to_string());
                entry.last_error = None;
            }
            Err(error) => {
                entry.last_error = Some(error.to_string());
            }
        }
    }

    /// 主动上报一条视频侦测报警(FR-9)。设备 → 平台 MESSAGE + Alarm Notify XML。
    /// `description` 为报警描述;返回平台响应状态码。
    pub async fn report_alarm(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
    ) -> Result<u16> {
        let report = AlarmReport::video(description);
        self.report_alarm_report(transport, local_host, local_port, &report)
            .await
    }

    /// 按报警单上报(类型/等级/方式/经纬度/描述/时间偏移均可指定)。
    ///
    /// 与 [`Self::report_alarm`] 共用同一条上报路径:订阅对话存在时走 NOTIFY,
    /// 否则退化为独立 MESSAGE —— 这条分支决定了平台能否在 Event:Alarm 订阅里
    /// 收到结构化报警,两条路径不能分叉。
    pub async fn report_alarm_report(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        report: &AlarmReport,
    ) -> Result<u16> {
        let outcome = self
            .report_alarm_report_inner(transport, local_host, local_port, report)
            .await;
        self.record_notify_outcome("Alarm", &outcome);
        outcome
    }

    async fn report_alarm_report_inner(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        report: &AlarmReport,
    ) -> Result<u16> {
        // 报警是设备本地已经发生的事实，不取决于平台是否成功应答通知。
        self.mark_alarm_active();
        let sn = self.next_cseq();
        // 时间戳由协议层统一按校时后的挂钟生成("N 分钟前"只挪时间戳,不延迟发送)。
        let alarm = report.to_notify(self.config.device_id.as_str(), sn)?;
        let xml = alarm.to_xml()?;

        // 若存在报警订阅对话:在对话内以 SIP NOTIFY 上报(GB28181 订阅语义);
        // 否则(如手动触发、平台未订阅)退化为独立 MESSAGE(全新 Call-ID)。
        let alarm_dialog = self.alarm_dialog.lock().await.clone();
        if let Some((dialog, sub_dst)) = alarm_dialog {
            let cseq = self.next_cseq();
            let req = builder::notify_in_dialog(
                &self.config,
                &dialog,
                cseq,
                local_host,
                local_port,
                &xml,
            );
            let mut rx = transport.register(dialog.call_id.clone());
            let result = sip_core::client_transact(
                transport,
                sub_dst,
                &req,
                &mut rx,
                sip_core::Timing::default(),
            )
            .await;
            transport.unregister(&dialog.call_id);
            let status = result?.status;
            let n = self
                .alarm_notify_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            self.emit_subscription("Alarm", true, n);
            return Ok(status);
        }

        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 本机复位报警:清掉 `alarming` 运行态标记。
    ///
    /// 只改设备本地状态,不向平台发任何报文 —— 平台侧的报警单由平台自己闭合,
    /// 设备能做的是让下一次 DeviceStatus 查询回到「无报警」。
    pub fn reset_alarm(&self) {
        if let Ok(mut state) = self.control_state.lock() {
            state.alarming = false;
        }
        self.mark_runtime_change("alarm_reset");
    }

    /// 把定时录像计划开关同步到 ConfigDownload 回读。
    ///
    /// 平台查 `VideoRecordPlan` 时看到的是设备**实际在执行**的计划类型：
    /// 计划已关闭却仍回 0（定时录像），平台会以为设备还在按表录像。
    /// 关闭时回 2（手动录像）—— 这是"当前没有生效的定时计划"最接近的真话。
    pub fn set_record_plan_enabled(&self, enabled: bool) {
        if let Ok(mut response) = self.runtime_config.lock() {
            if let Some(plan) = response.video_record_plan.as_mut() {
                plan.record_plan_type = if enabled { 0 } else { 2 };
            }
        }
    }

    /// 平台是否已建立 Event:Alarm 订阅对话。
    ///
    /// 决定报警走订阅内 NOTIFY 还是独立 MESSAGE；桌面端把这条信息记进报警历史，
    /// 排查"平台没收到报警"时先看它比翻报文快。
    pub async fn has_alarm_subscription(&self) -> bool {
        self.alarm_dialog.lock().await.is_some()
    }

    /// 主动上报实时视音频回传通知(VideoUploadNotify,GB28181-2022 A.2.5.8)。
    /// 设备 → 平台独立 MESSAGE,携带当前位置。
    ///
    /// **fire-and-forget**:该通知为告知性,部分平台(如 WVP)不支持此 CmdType 会
    /// 直接丢弃不回 200,故不等待响应,发出即返回,避免无谓的事务重传阻塞。
    pub async fn report_video_upload(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<()> {
        let sn = self.next_cseq();
        let time = common::clock::synced_iso8601();
        let pos = self.position.lock().map(|p| *p).ok();
        let notify = gb28181_protocol::manscdp::VideoUploadNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            pos,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml_oneshot(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报一条移动位置(GPS,MobilePosition NOTIFY)。设备 → 平台。
    /// 位置订阅场景下由周期任务调用;也可手动触发。返回平台响应状态码。
    pub async fn report_position(
        &self,
        transport: &Arc<SipTransport>,
        local_host: &str,
        local_port: u16,
        longitude: f64,
        latitude: f64,
    ) -> Result<u16> {
        // 记录当前坐标,供位置订阅的周期 NOTIFY 复用。
        if let Ok(mut p) = self.position.lock() {
            *p = (longitude, latitude);
        }
        self.mark_runtime_change("position_updated");
        let sn = self.next_cseq();
        // 位置上报时间用校时后的 ISO8601(与平台时间对齐)。
        let time = common::clock::synced_iso8601();
        let notify = gb28181_protocol::manscdp::MobilePositionNotify::new(
            self.config.device_id.as_str(),
            sn,
            time,
            longitude,
            latitude,
        );
        let xml = notify.to_xml()?;
        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 在订阅对话内向平台回发目录变更通知(Catalog Notify)。
    ///
    /// 平台 `SUBSCRIBE + Catalog` 后调用:把本设备全部通道以 `Event=ON` 上报,
    /// 使平台目录树同步。走对话内 SIP NOTIFY(沿用订阅 Call-ID + 反转 From/To +
    /// Event/Subscription-State 头),`dst` 为平台来源地址。
    async fn notify_catalog(
        &self,
        transport: &Arc<SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        sn: u32,
    ) -> Result<u16> {
        let outcome = self
            .notify_catalog_inner(transport, dst, dialog, sn)
            .await;
        self.record_notify_outcome("Catalog", &outcome);
        outcome
    }

    async fn notify_catalog_inner(
        &self,
        transport: &Arc<SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        sn: u32,
    ) -> Result<u16> {
        use gb28181_protocol::manscdp::{CatalogNotifyItem, CatalogSnapshot};
        // 全量目录:多通道树优先,否则扁平通道。全量后刷新快照基线供后续增量 diff。
        let publishable = self.catalog_publishable();
        let items: Vec<CatalogNotifyItem> = publishable
            .iter()
            .map(|node| CatalogNotifyItem {
                device_id: node.id.clone(),
                name: node.name.clone(),
                civil_code: node.civil_code.clone(),
                parental: Some(node.node_type.parental()),
                parent_id: (!node.parent_id.is_empty()).then(|| node.parent_id.clone()),
                business_group_id: node.business_group_id.clone(),
                event: "ON".into(),
                status: node.status.clone(),
            })
            .collect();
        *self.catalog_snapshot.lock().unwrap() =
            CatalogSnapshot::from_nodes(publishable.iter().cloned());
        let pages = self.catalog_notifies_by_udp_budget(
            sn,
            items,
            dialog,
            CATALOG_UDP_BODY_MAX_BYTES,
            CATALOG_UDP_PAYLOAD_MAX_BYTES,
        )?;
        let status = self.send_catalog_notify_pages(transport, dst, dialog, pages).await?;
        let n = self
            .catalog_notify_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.emit_subscription("Catalog", true, n);
        Ok(status)
    }

    /// 目录变更后推送增量 NOTIFY(FR-32/34)。
    ///
    /// 计算当前目录相对上次快照的 diff(ADD/DEL/UPDATE/ON/OFF),若有变更且存在活跃目录订阅,
    /// 在订阅对话内发一条只含变更项的 Catalog NOTIFY,并刷新快照基线。无订阅/无变更时不发。
    /// CRUD(upsert_channel/remove_channel)或状态切换后调用。
    pub async fn notify_catalog_changed(&self, transport: &Arc<SipTransport>) -> Result<()> {
        use gb28181_protocol::manscdp::CatalogSnapshot;
        let next = CatalogSnapshot::from_nodes(self.catalog_publishable());
        let changes = {
            let snap = self.catalog_snapshot.lock().unwrap();
            snap.diff(&next)
        };
        if changes.is_empty() {
            return Ok(());
        }
        // 刷新基线(无论有无订阅,快照都应跟上当前目录)。
        *self.catalog_snapshot.lock().unwrap() = next;

        let dialog = self.catalog_dialog.lock().await.clone();
        let Some((dialog, dst)) = dialog else {
            return Ok(()); // 无目录订阅,平台会在下次全量查询时同步
        };
        let sn = self.next_cseq();
        let pages = self.catalog_notifies_by_udp_budget(
            sn,
            changes,
            &dialog,
            CATALOG_UDP_BODY_MAX_BYTES,
            CATALOG_UDP_PAYLOAD_MAX_BYTES,
        )?;
        let outcome = self
            .send_catalog_notify_pages(transport, dst, &dialog, pages)
            .await;
        self.record_notify_outcome("Catalog", &outcome);
        outcome?;
        let n = self
            .catalog_notify_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.emit_subscription("Catalog", true, n);
        Ok(())
    }

    /// 串行发送目录 NOTIFY 分片；每片等待事务确认，失败时记录并继续剩余分片。
    async fn send_catalog_notify_pages(
        &self,
        transport: &Arc<SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        pages: Vec<gb28181_protocol::manscdp::CatalogNotify>,
    ) -> Result<u16> {
        let mut last_status = None;
        let mut last_error = None;
        for page in pages {
            let xml = page.to_xml()?;
            let cseq = self.next_cseq();
            let request = builder::notify_in_dialog(
                &self.config,
                dialog,
                cseq,
                &self.local_host(),
                self.local_port(),
                &xml,
            );
            let mut rx = transport.register(dialog.call_id.clone());
            let result = sip_core::client_transact(
                transport,
                dst,
                &request,
                &mut rx,
                query_response_timing(),
            )
            .await;
            transport.unregister(&dialog.call_id);
            match result {
                Ok(response) => last_status = Some(response.status),
                Err(error) => {
                    tracing::warn!(error = %error, "Catalog NOTIFY 分片事务失败，继续发送后续分片");
                    last_error = Some(error.to_string());
                }
            }
        }
        match (last_status, last_error) {
            (Some(status), _) => Ok(status),
            (None, Some(error)) => Err(Error::Sip(error)),
            (None, None) => Err(Error::Gb28181("Catalog NOTIFY 未生成分片".into())),
        }
    }

    /// 启动(或重建)移动位置订阅的周期上报任务。
    ///
    /// 平台下发 `SUBSCRIBE + MobilePosition`(携 Interval)后调用:按 `interval` 秒
    /// 周期把当前坐标以 MobilePosition NOTIFY 上报给平台,直到订阅被替换或设备下线。
    /// `interval` 缺省/为 0 时取 5 秒兜底。同一 CmdType 再次订阅会替换旧任务。
    async fn start_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        interval: Option<u64>,
    ) {
        let secs = interval.filter(|s| *s > 0).unwrap_or(5);
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        let local_host = self.local_host();
        let local_port = self.local_port();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(secs));
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break, // 订阅被替换/取消。
                    _ = ticker.tick() => {
                        let (lon, lat) = sim.position.lock().map(|p| *p).unwrap_or((0.0, 0.0));
                        let outcome =
                            sim.report_position(&tp, &local_host, local_port, lon, lat).await;
                        // 先记运行详情再判分支：失败路径过去只打日志，界面看不到原因。
                        sim.record_notify_outcome("MobilePosition", &outcome);
                        match outcome {
                            Ok(_) => {
                                let n = sim
                                    .position_notify_count
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                                    + 1;
                                sim.emit_subscription("MobilePosition", true, n);
                            }
                            Err(e) => {
                                tracing::warn!(error=%e, "位置订阅周期上报失败");
                            }
                        }
                    }
                }
            }
        });

        // 插入订阅表(替换旧任务:旧 stop_tx 被 drop → 旧任务在下次 select 时退出)。
        let mut subs = self.subscriptions.lock().await;
        subs.insert(
            "MobilePosition".to_string(),
            Subscription {
                _task: task,
                stop_tx,
            },
        );
        self.position_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.emit_subscription("MobilePosition", true, 0);
        tracing::info!(interval = secs, "移动位置订阅已建立,开始周期上报");
    }

    /// 登记(或替换)PTZ 精准状态**事件订阅**,并补推一次当前姿态(GB28181-2022 §9.11.2)。
    ///
    /// 平台下发 `SUBSCRIBE + PTZPosition` 后调用。该订阅是事件型:平台按规范构造订阅
    /// 查询时**不带 `Interval`**(只有移动位置 MobilePosition 才带上报周期),设备应只在
    /// 精确姿态发生变化时发对话内 NOTIFY。因此这里不启动任何周期任务,只记录对话供
    /// 后续变化时上报;订阅建立后补推一条当前姿态,给平台一个基线读数。
    async fn register_ptz_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        dialog: builder::NotifyDialog,
        dst: SocketAddr,
    ) {
        *self.ptz_dialog.lock().await = Some((dialog.clone(), dst));
        self.ptz_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: "PTZPosition".into(),
                active: true,
                notify_count: 0,
            });
        tracing::info!("PTZ 精准位置事件订阅已建立,姿态变化时上报");

        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        // 事件型订阅不保证后续一定再有变化,初值先推一条;NOTIFY 走事务要等平台确认。
        tokio::spawn(async move {
            if let Err(e) = sim.notify_ptz_position(&tp, dst, &dialog).await {
                tracing::warn!(error=%e, "PTZ 精准位置初始上报失败");
            }
        });
    }

    /// 读取当前精确姿态(Pan/Tilt/Zoom)。
    fn precise_pose(&self) -> (f32, f32, f32) {
        self.control_state
            .lock()
            .map(|s| s.precise_pose)
            .unwrap_or((0.0, 0.0, 1.0))
    }

    /// 精确姿态确实变了、且存在 PTZ 订阅时,异步发一条对话内 NOTIFY。
    ///
    /// `before` 为本次控制命令处理前的姿态;未变化则不发(事件型语义,替代原先的
    /// 无条件周期上报)。无订阅时静默返回,平台会在下次订阅或查询时同步。
    async fn notify_ptz_if_pose_changed(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        before: (f32, f32, f32),
    ) {
        if self.precise_pose() == before {
            return;
        }
        let Some((dialog, dst)) = self.ptz_dialog.lock().await.clone() else {
            return;
        };
        self.mark_runtime_change("ptz_position_changed");
        let sim = Arc::clone(self);
        let tp = Arc::clone(transport);
        // NOTIFY 走事务要等平台确认,不能占用入站信令循环。
        tokio::spawn(async move {
            if let Err(e) = sim.notify_ptz_position(&tp, dst, &dialog).await {
                tracing::warn!(error=%e, "PTZ 精准位置变化上报失败");
            }
        });
    }

    /// 在订阅对话内发一条 PTZ 精准状态 NOTIFY(GB28181-2022 §9.11.2)。
    ///
    /// 必须是 SIP **NOTIFY** 方法 + `<Notify>` 根元素:平台按方法分流,只有 NOTIFY 才进
    /// 通知入口(`OnPTZNotify`)落库;用 MESSAGE 承载会被当成"查询应答"去关联 operation,
    /// 关联不上就丢弃。
    pub async fn notify_ptz_position(
        &self,
        transport: &Arc<SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
    ) -> Result<u16> {
        let outcome = self
            .notify_ptz_position_inner(transport, dst, dialog)
            .await;
        self.record_notify_outcome("PTZPosition", &outcome);
        outcome
    }

    async fn notify_ptz_position_inner(
        &self,
        transport: &Arc<SipTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
    ) -> Result<u16> {
        let (pan, tilt, zoom) = self.precise_pose();
        let sn = self.next_cseq();
        let notify = gb28181_protocol::manscdp::PtzPreciseNotify::new(
            self.config.device_id.as_str(),
            sn,
            common::clock::synced_iso8601(),
            pan,
            tilt,
            zoom,
        );
        let xml = notify.to_xml()?;
        let cseq = self.next_cseq();
        let request = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
        );
        let mut rx = transport.register(dialog.call_id.clone());
        let result = sip_core::client_transact(
            transport,
            dst,
            &request,
            &mut rx,
            query_response_timing(),
        )
        .await;
        transport.unregister(&dialog.call_id);
        let status = result?.status;
        let n = self
            .ptz_notify_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.observer
            .on_event(common::DeviceEvent::SubscriptionChanged {
                kind: "PTZPosition".into(),
                active: true,
                notify_count: n,
            });
        Ok(status)
    }

    /// 停止全部活跃订阅(设备下线/重注册时调,避免残留周期任务)。
    async fn stop_subscriptions(&self) {
        let mut subs = self.subscriptions.lock().await;
        for (_, sub) in subs.drain() {
            let _ = sub.stop_tx.send(());
        }
        *self.alarm_dialog.lock().await = None;
        *self.catalog_dialog.lock().await = None;
        // PTZ 事件订阅同样随设备下线失效:清对话后
        // notify_ptz_if_pose_changed 会静默跳过,不再向上报(平台侧订阅也会被标 expired)。
        *self.ptz_dialog.lock().await = None;
        // 四类订阅的运行详情一起收尾:设备下线后界面不该还挂着"已订阅 + 剩余 N 秒"。
        for kind in TRACKED_SUBSCRIPTIONS {
            self.mark_unsubscribed(kind);
        }
        self.ptz_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.alarm_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.catalog_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        self.position_notify_count
            .store(0, std::sync::atomic::Ordering::Relaxed);
        for kind in ["MobilePosition", "Alarm", "Catalog", "PTZPosition"] {
            self.emit_subscription(kind, false, 0);
        }
    }

    /// 处理一条入站请求:OPTIONS 简单回 200;MESSAGE 解析 XML 查询并应答;
    /// SUBSCRIBE 回 200 并按需启动周期 NOTIFY。返回是否已应答(true=已回,false=暂不处理)。
    ///
    /// 取 `self: &Arc<Self>`(而非 `&self`):移动位置订阅需要把设备句柄交给后台
    /// 周期上报任务,任务生命周期独立于单次入站处理。
    pub async fn answer_inbound(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        incoming: &sip_core::Incoming,
    ) -> Result<bool> {
        if let sip_core::SipMessage::Request(req) = &incoming.message {
            match req.method {
                sip_core::Method::Options => {
                    let resp = sip_core::SipMessage::Response(builder::response_ok(req));
                    transport.send_to(&resp, incoming.from).await?;
                    Ok(true)
                }
                sip_core::Method::Message => {
                    // GB28181 规范:先对查询 MESSAGE 回空 200 OK(事务应答),
                    // 再由设备发一条独立的 MESSAGE 把应答 XML 送回平台(新事务)。
                    let ack = sip_core::SipMessage::Response(builder::response_ok(req));
                    transport.send_to(&ack, incoming.from).await?;

                    // body 可能是查询(Query)或控制(Control)。按**根元素**区分,
                    // 不能靠 parse 成功与否(quick-xml 忽略根名,Query 会误吞 Control)。
                    // 按信令编码解码(GB18030/UTF-8),修复中文乱码。
                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        let body_str = body_str.as_str();
                        let is_control =
                            body_str.contains("<Control>") || body_str.contains("<Control ");
                        if is_control {
                            if let Ok(ctrl) = gb28181_protocol::manscdp::Control::parse(body_str) {
                                // 精确云台控制会改变姿态。记录处理前的值,处理完后据此判断
                                // 是否需要向 PTZ 事件订阅推一条 NOTIFY(事件型:只在变化时上报)。
                                let pose_before = self.precise_pose();
                                let (xml, upgrade_dispatch) = if ctrl.device_upgrade.is_some() {
                                    self.handle_control_with_dispatch(&ctrl).await?
                                } else {
                                    (self.handle_control(&ctrl).await?, None)
                                };
                                self.send_reply_message(transport, incoming.from, &xml)
                                    .await?;
                                // 平台命令时间线:上报语义摘要(控制类,含方向/变倍/预置位等细节)。
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "control".into(),
                                        summary: format!(
                                            "平台控制:{} → 已应答",
                                            Self::control_detail(&ctrl)
                                        ),
                                    });
                                // 抓拍/升级等需设备主动发 NOTIFY 的命令:回 200/结果后异步执行。
                                self.spawn_control_side_effects(transport, &ctrl, upgrade_dispatch);
                                self.notify_ptz_if_pose_changed(transport, pose_before)
                                    .await;
                            }
                        } else if body_str.contains("Broadcast") {
                            // 语音广播:回 Broadcast Response(OK/ERROR),接受则反向发 INVITE。
                            if let Ok(bc) =
                                gb28181_protocol::manscdp::BroadcastNotify::parse(body_str)
                            {
                                self.handle_broadcast(transport, &bc, incoming.from).await?;
                                self.observer
                                    .on_event(common::DeviceEvent::PlatformCommand {
                                        kind: "broadcast".into(),
                                        summary: "平台语音广播请求 → 已应答".into(),
                                    });
                            }
                        } else if let Ok(query) = gb28181_protocol::manscdp::Query::parse(body_str)
                        {
                            if let Ok(packets) = self.handle_query_packets(&query) {
                                // 查询 SIP 200 已回；业务 Response 在后台串行发送，避免某片平台
                                // 确认缓慢时阻塞同一入站循环里的后续 Query/INVITE/BYE。
                                let simulator = Arc::clone(self);
                                let reply_transport = Arc::clone(transport);
                                let reply_to = incoming.from;
                                let cmd_type = query.cmd_type.clone();
                                let summary = self.query_summary(&cmd_type);
                                tokio::spawn(async move {
                                    // 附录 M:同 SN 多响应串行发送，每包等待上一条 MESSAGE 的 SIP 响应。
                                    for xml in packets {
                                        match simulator
                                            .send_query_response_message(
                                                &reply_transport,
                                                reply_to,
                                                &xml,
                                            )
                                            .await
                                        {
                                            Ok(200..=299) => {}
                                            Ok(status) => {
                                                tracing::warn!(status, cmd_type = %cmd_type,
                                                    "查询 Response MESSAGE 未获平台 2xx 确认，停止后续分片");
                                                break;
                                            }
                                            Err(error) => {
                                                tracing::warn!(error = %error, cmd_type = %cmd_type,
                                                    "查询 Response MESSAGE 事务失败，停止后续分片");
                                                break;
                                            }
                                        }
                                    }
                                    // 平台命令时间线:上报语义摘要(查询类)。
                                    simulator.observer.on_event(
                                        common::DeviceEvent::PlatformCommand {
                                            kind: "query".into(),
                                            summary,
                                        },
                                    );
                                });
                            }
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Invite => {
                    // 解析 SDP,启动推流,回 200 OK。
                    self.handle_invite(transport, req, incoming.from).await
                }
                sip_core::Method::Bye => {
                    // 语音广播对话的 BYE:结束广播并回 200 OK。
                    if self.broadcast.handle_bye(req).await {
                        let resp = sip_core::SipMessage::Response(builder::response_ok(req));
                        transport.send_to(&resp, incoming.from).await?;
                        return Ok(true);
                    }
                    // 停止推流,回 200 OK。
                    self.handle_bye(transport, req, incoming.from).await
                }
                sip_core::Method::Cancel => {
                    // 取消点播:停掉可能已起的推流,回 200 OK(与 BYE 同处理)。
                    self.handle_bye(transport, req, incoming.from).await
                }
                sip_core::Method::Ack => {
                    // ACK 无需响应,但标志会话已确认。
                    Ok(true)
                }
                sip_core::Method::Subscribe => {
                    // 平台订阅(Catalog/MobilePosition/Alarm):先回 200 OK 建立订阅。
                    // 用固定 to_tag,使 200 的 To-tag 与后续对话内 NOTIFY 的 From-tag 一致。
                    let local_tag = builder::rand_token("");
                    let resp = sip_core::SipMessage::Response(builder::response_ok_tagged(
                        req, &local_tag,
                    ));
                    transport.send_to(&resp, incoming.from).await?;

                    {
                        let body_str =
                            builder::decode_xml_body(&req.body, self.config.signaling_encoding);
                        if let Ok(query) = gb28181_protocol::manscdp::Query::parse(&body_str) {
                            // 从 SUBSCRIBE + 本端 tag 构建订阅对话(供对话内 NOTIFY 用)。
                            let dialog = builder::NotifyDialog {
                                call_id: req.headers.call_id().unwrap_or_default().to_string(),
                                local_tag,
                                remote_from: req
                                    .headers
                                    .get("From")
                                    .unwrap_or_default()
                                    .to_string(),
                                event: req
                                    .headers
                                    .get("Event")
                                    .unwrap_or(&query.cmd_type)
                                    .to_string(),
                                expires: req
                                    .headers
                                    .get("Expires")
                                    .and_then(|s| s.trim().parse().ok())
                                    .unwrap_or(3600),
                            };
                            // Expires: 0 是退订：清掉对应对话/周期任务并通知 UI。
                            if dialog.expires == 0 {
                                match query.cmd_type.as_str() {
                                    "MobilePosition" => {
                                        let sub = self
                                            .subscriptions
                                            .lock()
                                            .await
                                            .remove("MobilePosition");
                                        if let Some(sub) = sub {
                                            let _ = sub.stop_tx.send(());
                                        }
                                    }
                                    "Alarm" => {
                                        *self.alarm_dialog.lock().await = None;
                                    }
                                    "Catalog" => {
                                        *self.catalog_dialog.lock().await = None;
                                    }
                                    "PTZPosition" => {
                                        *self.ptz_dialog.lock().await = None;
                                    }
                                    _ => {}
                                }
                                self.emit_subscription(&query.cmd_type, false, 0);
                                self.mark_unsubscribed(&query.cmd_type);
                                return Ok(true);
                            }
                            // 记录订阅者与有效期，供「订阅运行详情」显示谁订的、还剩多久。
                            self.mark_subscribed(
                                &query.cmd_type,
                                incoming.from.to_string(),
                                dialog.expires,
                            );
                            match query.cmd_type.as_str() {
                                // 移动位置订阅:启动周期位置 NOTIFY(独立 MESSAGE 路径)。
                                "MobilePosition" => {
                                    self.start_position_subscription(transport, query.interval)
                                        .await;
                                }
                                // 目录订阅:记录对话供 CRUD 增量 NOTIFY,并回发一条全量目录 NOTIFY。
                                "Catalog" => {
                                    *self.catalog_dialog.lock().await =
                                        Some((dialog.clone(), incoming.from));
                                    self.catalog_notify_count
                                        .store(0, std::sync::atomic::Ordering::Relaxed);
                                    self.emit_subscription("Catalog", true, 0);
                                    let sim = Arc::clone(self);
                                    let tp = Arc::clone(transport);
                                    let sn = query.sn;
                                    let dst = incoming.from;
                                    // 独立任务发送:notify 走事务会等待平台响应,不占用入站循环。
                                    tokio::spawn(async move {
                                        if let Err(e) =
                                            sim.notify_catalog(&tp, dst, &dialog, sn).await
                                        {
                                            tracing::warn!(error=%e, "目录订阅通知失败");
                                        }
                                    });
                                }
                                // 报警订阅:记录对话,后续 report_alarm 走对话内 NOTIFY。
                                "Alarm" => {
                                    *self.alarm_dialog.lock().await = Some((dialog, incoming.from));
                                    self.alarm_notify_count
                                        .store(0, std::sync::atomic::Ordering::Relaxed);
                                    self.emit_subscription("Alarm", true, 0);
                                }
                                // PTZ 精准位置事件订阅(GB28181-2022 §9.11.1/9.11.2):
                                // 事件型 —— 只登记订阅对话,姿态变化时发对话内 NOTIFY,不周期上报。
                                "PTZPosition" => {
                                    self.register_ptz_position_subscription(
                                        transport,
                                        dialog,
                                        incoming.from,
                                    )
                                    .await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(true)
                }
                sip_core::Method::Info => {
                    // 会话内回放控制(MANSRTSP):PLAY(Scale=N)调速 / 恢复,PAUSE 暂停。
                    self.handle_info(transport, req, incoming.from).await
                }
                _ => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    /// 处理会话内 INFO(MANSRTSP 回放控制):按体首行 PLAY/PAUSE 调整推流。
    ///
    /// 体形如 `PLAY MANSRTSP/1.0\r\nCSeq: n\r\nScale: 2.0\r\n\r\n`(倍速/恢复)
    /// 或 `PAUSE MANSRTSP/1.0\r\nCSeq: n\r\nPauseTime: now\r\n\r\n`(暂停)。
    /// 无活跃会话时也回 200(避免平台重传)。
    async fn handle_info(
        &self,
        transport: &Arc<SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        if let Ok(body) = std::str::from_utf8(&req.body) {
            let head = body.trim_start();
            let mut guard = self.session.lock().await;
            let teardown = head.starts_with("TEARDOWN");
            let mut notify_finished = false;
            if teardown {
                if let Some(session) = guard.take() {
                    notify_finished = session.is_history;
                    let _ = session.stop_tx.send(());
                    self.observer.on_event(common::DeviceEvent::StreamStop);
                    tracing::info!("回放控制:结束");
                }
            } else if let Some(session) = guard.as_ref() {
                if head.starts_with("PAUSE") {
                    session.control.pause();
                    tracing::info!("回放控制:暂停");
                } else if head.starts_with("PLAY") {
                    // 取 Scale 行(倍速);缺省视为 1.0(恢复正常播放)。
                    let scale = body
                        .lines()
                        .find_map(|l| {
                            l.split_once(':')
                                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Scale"))
                                .and_then(|(_, v)| v.trim().parse::<f32>().ok())
                        })
                        .unwrap_or(1.0);
                    // 取 Range 行(拖动定位,§9.8):`Range: npt=<start>-<end>`。
                    // start 为回放起点秒偏移;按 INVITE t= 时间段折算千分比位置。
                    if let Some(permille) =
                        Self::parse_range_permille_for(body, session.history_duration_secs)
                    {
                        session.control.seek_permille(permille);
                        tracing::info!(permille, "回放控制:拖动定位");
                    }
                    session.control.set_speed(scale);
                    session.control.resume();
                    tracing::info!(scale, "回放控制:播放/倍速");
                }
            }
            drop(guard);
            if notify_finished {
                self.notify_media_finished(transport).await;
            }
        }
        let resp = sip_core::SipMessage::Response(builder::response_ok(req));
        transport.send_to(&resp, from).await?;
        Ok(true)
    }

    /// 解析 MANSRTSP PLAY 的 `Range: npt=<start>-<end>` 头,折算为流内千分比位置(0..=1000)。
    ///
    /// GB28181 回放拖动:npt 为回放起点秒偏移。模拟器循环推短文件,无真实录像时间轴,
    /// 故把 start 对一个名义 3600s 时间窗取模折算千分比(保证拖动可见地改变起播位置)。
    /// 返回 None 表示无 Range 或无法解析。
    #[cfg(test)]
    fn parse_range_permille(body: &str) -> Option<u32> {
        Self::parse_range_permille_for(body, 3600)
    }

    fn parse_range_permille_for(body: &str, duration_secs: u64) -> Option<u32> {
        let range = body.lines().find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.trim().eq_ignore_ascii_case("Range"))
                .map(|(_, v)| v.trim().to_string())
        })?;
        // 形如 "npt=10-" / "npt=10-20" / "npt=now-"。取 start。
        let npt = range.split_once('=').map(|(_, v)| v).unwrap_or(&range);
        let start = npt.split('-').next()?.trim();
        if start.eq_ignore_ascii_case("now") {
            return None; // now 表示从当前继续,不定位。
        }
        let secs: f64 = start.parse().ok()?;
        let duration = duration_secs.max(1) as f64;
        let permille = ((secs.clamp(0.0, duration) / duration) * 1000.0) as u32;
        Some(permille.min(1000))
    }

    async fn notify_media_finished(&self, transport: &Arc<SipTransport>) {
        let (host, port) = (self.local_host(), self.local_port());
        let sn = self.next_cseq();
        if let Ok(xml) = gb28181_protocol::manscdp::MediaStatusNotify::finished(
            self.config.device_id.as_str(),
            sn,
        )
        .to_xml()
        {
            let _ = self.send_message_xml(transport, &host, port, &xml).await;
        }
        tracing::info!("回放/下载结束,发 MediaStatus 121");
    }

    /// 控制命令的详细人类可读摘要(供 UI 命令时间线):方向/变倍/预置位/巡航/辅助等。
    fn control_detail(ctrl: &gb28181_protocol::manscdp::Control) -> String {
        use gb28181_protocol::manscdp::{CruiseOp, PresetAction};
        // PTZ 方向/变倍。
        if let Some(m) = ctrl.ptz_motion() {
            if !m.is_stop() {
                let mut parts = Vec::new();
                if m.up {
                    parts.push("上");
                }
                if m.down {
                    parts.push("下");
                }
                if m.left {
                    parts.push("左");
                }
                if m.right {
                    parts.push("右");
                }
                if m.zoom_in {
                    parts.push("放大");
                }
                if m.zoom_out {
                    parts.push("缩小");
                }
                let dir = if parts.is_empty() {
                    "云台动作".to_string()
                } else {
                    parts.join("+")
                };
                return format!(
                    "PTZ {dir}(水平{} 垂直{} 变倍{})",
                    m.pan_speed, m.tilt_speed, m.zoom_speed
                );
            }
            return "PTZ 停止".into();
        }
        // 预置位。
        if let Some((action, idx)) = ctrl.preset_op() {
            let a = match action {
                PresetAction::Set => "设置",
                PresetAction::Call => "调用",
                PresetAction::Delete => "删除",
            };
            return format!("PTZ {a}预置位 {idx}");
        }
        // 巡航。
        if let Some(op) = ctrl.cruise_op() {
            return match op {
                CruiseOp::SetPoint { track, preset } => format!("巡航{track}增点(预置{preset})"),
                CruiseOp::DelPoint { track, preset } => format!("巡航{track}删点(预置{preset})"),
                CruiseOp::SetSpeed { track, speed } => format!("巡航{track}设速{speed}"),
                CruiseOp::SetDwell { track, dwell } => format!("巡航{track}停留{dwell}s"),
                CruiseOp::Start { track } => {
                    if track == 0 {
                        "巡航停止".into()
                    } else {
                        format!("巡航{track}启动")
                    }
                }
            };
        }
        // 自动扫描(表 A.10)。
        if let Some(scan) = ctrl.scan_op() {
            use gb28181_protocol::manscdp::ScanOp;
            return match scan {
                ScanOp::Start { group } => format!("扫描{group}启动"),
                ScanOp::SetLeft { group } => format!("扫描{group}设左边界"),
                ScanOp::SetRight { group } => format!("扫描{group}设右边界"),
                ScanOp::SetSpeed { group, speed } => format!("扫描{group}设速{speed}"),
            };
        }
        // 辅助控制。
        if let Some(aux) = ctrl.aux_op() {
            return format!(
                "辅助{}{}",
                aux.function.label(),
                if aux.on { "开" } else { "关" }
            );
        }
        // FI 光圈/聚焦。
        if let Some(fi) = ctrl.fi_op() {
            return fi.label();
        }
        // 其它:录像/布防/复位/看守位/精准云台/升级/抓拍/配置等,用 kind()。
        if let Some(v) = ctrl.record_cmd.as_deref() {
            return format!("录像控制:{v}");
        }
        if let Some(g) = &ctrl.guard_cmd {
            return format!("布防/撤防:{g}");
        }
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            return format!("精确云台 Pan{} Tilt{} Zoom{}", p.pan, p.tilt, p.zoom);
        }
        if let Some(hp) = &ctrl.home_position {
            return format!(
                "看守位设置:Enabled{} Preset{} Reset{}",
                hp.enabled, hp.preset_index, hp.reset_time
            );
        }
        if let Some(t) = &ctrl.target_track {
            return format!("目标跟踪:{}", t.mode);
        }
        if ctrl.format_sd_card.is_some() {
            return format!("格式化SD卡:Disk{}", ctrl.disk_num.unwrap_or(0));
        }
        if let Some(upgrade) = &ctrl.device_upgrade {
            return format!("在线升级:{}", upgrade.firmware);
        }
        ctrl.kind().to_string()
    }

    /// 查询命令的人类可读摘要(供 UI 命令时间线)。
    fn query_summary(&self, cmd_type: &str) -> String {
        let ch_count = {
            let tree = self.catalog_tree.lock().unwrap();
            if tree.is_empty() {
                self.config.channels.len()
            } else {
                tree.iter()
                    .filter(|n| {
                        n.node_type == gb28181_protocol::id_codec::CatalogNodeType::VideoChannel
                    })
                    .count()
            }
        };
        match cmd_type {
            "Catalog" => format!("查询目录 → 回 {ch_count} 通道"),
            "DeviceInfo" => "查询设备信息 → 已应答".into(),
            "DeviceStatus" => "查询设备状态 → 在线".into(),
            "MobilePosition" => "查询移动位置 → 已应答当前坐标".into(),
            "RecordInfo" => "检索录像 → 回录像列表".into(),
            "ConfigDownload" => "查询设备配置 → 已应答".into(),
            "PresetQuery" => "查询预置位 → 回预置位列表".into(),
            "HomePositionQuery" => "查询看守位 → 已应答".into(),
            "SDCardStatus" => "查询存储卡状态 → 已应答".into(),
            "CruiseTrackListQuery" => "查询巡航轨迹列表 → 已应答".into(),
            "CruiseTrackQuery" => "查询巡航轨迹 → 已应答".into(),
            "PTZPosition" => "查询 PTZ 精准状态 → 已应答".into(),
            other => format!("查询 {other} → 已应答"),
        }
    }

    fn effective_catalog_tree(&self) -> Vec<gb28181_protocol::id_codec::CatalogNode> {
        use gb28181_protocol::id_codec::{CatalogNode, CatalogNodeType};
        let tree = self.catalog_tree.lock().unwrap();
        if !tree.is_empty() {
            return tree.clone();
        }
        let root_id = self.config.device_id.as_str();
        let mut nodes = vec![CatalogNode::new(
            root_id,
            CatalogNodeType::Device,
            &self.config.device_info.device_name,
            root_id,
        )];
        nodes.extend(self.config.channels.iter().map(|channel| {
            let mut node = CatalogNode::new(
                channel.channel_id.as_str(),
                CatalogNodeType::VideoChannel,
                &channel.name,
                root_id,
            );
            node.status = channel.status.clone();
            node
        }));
        nodes
    }

    fn catalog_query_packets(
        &self,
        query: &gb28181_protocol::manscdp::Query,
    ) -> Result<Vec<String>> {
        use gb28181_protocol::{
            id_codec::{catalog_business_group_id, catalog_parent_id, select_catalog_nodes},
            manscdp::CatalogItem,
        };
        let tree = self.effective_catalog_tree();
        let selected = select_catalog_nodes(&tree, &query.device_id);
        let is_2022 = self.config.gb_version.is_2022();
        let items: Vec<CatalogItem> = selected
            .iter()
            .map(|node| CatalogItem {
                device_id: node.id.clone(),
                name: node.name.clone(),
                manufacturer: Some(self.config.device_info.manufacturer.clone()),
                model: Some(self.config.device_info.model.clone()),
                civil_code: node.civil_code.clone(),
                // 设备级安装位置：同一台设备下的目录项共用，未配置时 None 不输出。
                address: self.config.device_info.installation_address.clone(),
                parental: Some(node.node_type.parental()),
                parent_id: catalog_parent_id(&tree, node),
                business_group_id: catalog_business_group_id(&tree, node),
                status: node.status.clone(),
                security_level_code: is_2022.then(|| "A".to_string()),
                ip_address: None,
                port: None,
            })
            .collect();
        let result_count = items.len();
        let responses = self.catalog_responses_by_udp_budget(
            &query.device_id,
            query.sn,
            items,
            CATALOG_UDP_BODY_MAX_BYTES,
            CATALOG_UDP_PAYLOAD_MAX_BYTES,
        )?;
        let packet_count = responses.len();
        *self.catalog_activity.lock().unwrap() = Some(CatalogQueryActivity {
            target_id: query.device_id.clone(),
            result_count,
            packet_count,
            updated_at_ms: epoch_millis(),
        });
        responses
            .into_iter()
            .map(|response| response.to_xml())
            .collect()
    }

    /// 按实际信令编码后的 XML 与完整 SIP UDP payload 字节数贪心装包。
    ///
    /// GB/T 28181-2022 附录 M 允许目录多响应，并要求同一请求的响应保持相同 SN；
    /// 1000 字节是 UDP 实现的工程预算，不是标准规定的固定数值。
    fn catalog_responses_by_udp_budget(
        &self,
        device_id: &str,
        sn: u32,
        items: Vec<gb28181_protocol::manscdp::CatalogItem>,
        max_body_bytes: usize,
        max_datagram_bytes: usize,
    ) -> Result<Vec<gb28181_protocol::manscdp::CatalogResponse>> {
        use gb28181_protocol::manscdp::CatalogResponse;

        fn response(
            device_id: &str,
            sn: u32,
            total: u32,
            items: Vec<gb28181_protocol::manscdp::CatalogItem>,
        ) -> CatalogResponse {
            let mut response = CatalogResponse::new(device_id, sn, items);
            response.sum_num = total;
            response
        }

        let total = items.len() as u32;
        let encoded_sizes = |response: &CatalogResponse| -> Result<(usize, usize)> {
            let xml = response.to_xml()?;
            let body_len = builder::encode_xml_body(&xml, self.config.signaling_encoding).len();
            // CSeq 用最大十进制长度，保证运行时任意 CSeq 都不会超过测量值。
            let request = builder::message_xml(
                &self.config,
                &self.ids,
                u32::MAX,
                &self.local_host(),
                self.local_port(),
                &xml,
            );
            let datagram_len = sip_core::SipMessage::Request(request).to_bytes().len();
            Ok((body_len, datagram_len))
        };
        let fits = |response: &CatalogResponse| -> Result<bool> {
            let (body_len, datagram_len) = encoded_sizes(response)?;
            Ok(body_len <= max_body_bytes && datagram_len <= max_datagram_bytes)
        };

        if items.is_empty() {
            let empty = response(device_id, sn, 0, Vec::new());
            let (body_len, datagram_len) = encoded_sizes(&empty)?;
            if body_len > max_body_bytes || datagram_len > max_datagram_bytes {
                return Err(Error::Gb28181(format!(
                    "Catalog 空响应 body={body_len}B/datagram={datagram_len}B 超过 UDP 预算 body={max_body_bytes}B/datagram={max_datagram_bytes}B"
                )));
            }
            return Ok(vec![empty]);
        }

        let mut pages = Vec::new();
        let mut current = Vec::new();
        for item in items {
            current.push(item);
            let candidate = response(device_id, sn, total, current.clone());
            if fits(&candidate)? {
                continue;
            }

            let overflow = current.pop().expect("刚加入的目录项必然存在");
            if current.is_empty() {
                let oversized = response(device_id, sn, total, vec![overflow]);
                let (body_len, datagram_len) = encoded_sizes(&oversized)?;
                return Err(Error::Gb28181(format!(
                    "单个 Catalog 目录项 {} 编码后响应 body={body_len}B/datagram={datagram_len}B，超过 UDP 预算 body={max_body_bytes}B/datagram={max_datagram_bytes}B",
                    oversized
                        .device_list
                        .as_ref()
                        .and_then(|list| list.items.first())
                        .map(|item| item.device_id.as_str())
                        .unwrap_or("unknown")
                )));
            }

            pages.push(response(device_id, sn, total, std::mem::take(&mut current)));
            current.push(overflow);
            let single = response(device_id, sn, total, current.clone());
            let (body_len, datagram_len) = encoded_sizes(&single)?;
            if body_len > max_body_bytes || datagram_len > max_datagram_bytes {
                return Err(Error::Gb28181(format!(
                    "单个 Catalog 目录项 {} 编码后响应 body={body_len}B/datagram={datagram_len}B，超过 UDP 预算 body={max_body_bytes}B/datagram={max_datagram_bytes}B",
                    single
                        .device_list
                        .as_ref()
                        .and_then(|list| list.items.first())
                        .map(|item| item.device_id.as_str())
                        .unwrap_or("unknown")
                )));
            }
        }
        if !current.is_empty() {
            pages.push(response(device_id, sn, total, current));
        }
        Ok(pages)
    }

    /// Catalog NOTIFY 使用与查询 Response 相同的 UDP 预算，并按实际对话内 SIP 头测量。
    fn catalog_notifies_by_udp_budget(
        &self,
        sn: u32,
        items: Vec<gb28181_protocol::manscdp::CatalogNotifyItem>,
        dialog: &builder::NotifyDialog,
        max_body_bytes: usize,
        max_datagram_bytes: usize,
    ) -> Result<Vec<gb28181_protocol::manscdp::CatalogNotify>> {
        use gb28181_protocol::manscdp::{CatalogNotify, CatalogNotifyItem};

        fn notify(
            sn: u32,
            total: u32,
            items: Vec<CatalogNotifyItem>,
            device_id: &str,
        ) -> CatalogNotify {
            let mut notify = CatalogNotify::new(device_id, sn, items);
            notify.sum_num = total;
            notify
        }

        let total = items.len() as u32;
        let encoded_sizes = |notify: &CatalogNotify| -> Result<(usize, usize)> {
            let xml = notify.to_xml()?;
            let body_len = builder::encode_xml_body(&xml, self.config.signaling_encoding).len();
            let request = builder::notify_in_dialog(
                &self.config,
                dialog,
                u32::MAX,
                &self.local_host(),
                self.local_port(),
                &xml,
            );
            let datagram_len = sip_core::SipMessage::Request(request).to_bytes().len();
            Ok((body_len, datagram_len))
        };
        let fits = |notify: &CatalogNotify| -> Result<bool> {
            let (body_len, datagram_len) = encoded_sizes(notify)?;
            Ok(body_len <= max_body_bytes && datagram_len <= max_datagram_bytes)
        };

        if items.is_empty() {
            let empty = notify(sn, 0, Vec::new(), self.config.device_id.as_str());
            if !fits(&empty)? {
                return Err(Error::Gb28181("Catalog NOTIFY 空报文超过 UDP 预算".into()));
            }
            return Ok(vec![empty]);
        }

        let mut pages = Vec::new();
        let mut current = Vec::new();
        for item in items {
            current.push(item);
            let candidate = notify(sn, total, current.clone(), self.config.device_id.as_str());
            if fits(&candidate)? {
                continue;
            }

            let overflow = current.pop().expect("刚加入的目录通知项必然存在");
            if current.is_empty() {
                return Err(Error::Gb28181(format!(
                    "单个 Catalog NOTIFY 目录项 {} 超过 UDP 预算",
                    overflow.device_id
                )));
            }
            pages.push(notify(
                sn,
                total,
                std::mem::take(&mut current),
                self.config.device_id.as_str(),
            ));
            current.push(overflow);
            let single = notify(sn, total, current.clone(), self.config.device_id.as_str());
            if !fits(&single)? {
                return Err(Error::Gb28181(format!(
                    "单个 Catalog NOTIFY 目录项 {} 超过 UDP 预算",
                    single.device_list.items[0].device_id
                )));
            }
        }
        if !current.is_empty() {
            pages.push(notify(sn, total, current, self.config.device_id.as_str()));
        }
        Ok(pages)
    }

    /// 处理查询并返回一条或多条独立 MESSAGE XML。
    fn handle_query_packets(
        &self,
        query: &gb28181_protocol::manscdp::Query,
    ) -> Result<Vec<String>> {
        match query.cmd_type.as_str() {
            "Catalog" => self.catalog_query_packets(query),
            "RecordInfo" => self.record_info_packets(query),
            _ => self.handle_query(query).map(|xml| vec![xml]),
        }
    }

    fn record_info_packets(&self, query: &gb28181_protocol::manscdp::Query) -> Result<Vec<String>> {
        use crate::recording::{RecordingKind, RecordingQuery};
        use gb28181_protocol::manscdp::{RecordInfoResponse, RecordItem};

        let parse_time = |value: Option<&String>| {
            value
                .and_then(|value| common::clock::parse_iso8601(value))
                .and_then(|seconds| u64::try_from(seconds).ok())
                .map(|seconds| seconds.saturating_mul(1_000))
        };
        let kind = match query.record_type.as_deref().map(str::trim) {
            None | Some("") | Some("all") | Some("ALL") => None,
            Some(value) if value.eq_ignore_ascii_case("time") => Some(RecordingKind::Time),
            Some(value) if value.eq_ignore_ascii_case("alarm") => Some(RecordingKind::Alarm),
            Some(value) if value.eq_ignore_ascii_case("manual") => Some(RecordingKind::Manual),
            Some(value) => return Err(Error::Gb28181(format!("RecordInfo Type 不支持: {value}"))),
        };
        let entries = self
            .recording_service()
            .map(|service| {
                service.store().query(&RecordingQuery {
                    start_time_ms: parse_time(query.start_time.as_ref()),
                    end_time_ms: parse_time(query.end_time.as_ref()),
                    channel_id: Some(query.device_id.clone()),
                    kind,
                })
            })
            .unwrap_or_default();
        let total = entries.len() as u32;
        let items: Vec<RecordItem> = entries
            .into_iter()
            .map(|entry| RecordItem {
                device_id: entry.channel_id.clone(),
                name: entry
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| entry.id.clone()),
                start_time: entry.start_time,
                end_time: entry.end_time,
                kind: entry.kind.as_str().into(),
                file_path: Some(entry.path.to_string_lossy().into_owned()),
                address: Some(self.local_host()),
                secrecy: Some(0),
                recorder_id: Some(self.config.device_id.as_str().to_owned()),
            })
            .collect();
        if items.is_empty() {
            return Ok(vec![RecordInfoResponse::page(
                &query.device_id,
                self.config.device_info.device_name.clone(),
                query.sn,
                0,
                Vec::new(),
            )
            .to_xml()?]);
        }
        items
            .chunks(RECORD_INFO_PAGE_SIZE)
            .map(|page| {
                RecordInfoResponse::page(
                    &query.device_id,
                    self.config.device_info.device_name.clone(),
                    query.sn,
                    total,
                    page.to_vec(),
                )
                .to_xml()
            })
            .collect()
    }

    /// 处理 Query,返回应答 XML。
    fn handle_query(&self, query: &gb28181_protocol::manscdp::Query) -> Result<String> {
        use gb28181_protocol::manscdp::*;
        match query.cmd_type.as_str() {
            "Catalog" => self
                .catalog_query_packets(query)?
                .into_iter()
                .next()
                .ok_or_else(|| Error::Gb28181("目录查询未生成应答".into())),
            "DeviceInfo" => {
                let resp = DeviceInfoResponse {
                    cmd_type: "DeviceInfo".into(),
                    sn: query.sn,
                    device_id: query.device_id.clone(),
                    result: "OK".into(),
                    device_name: self.config.device_info.device_name.clone(),
                    device_type: self.config.device_info.device_type.clone(),
                    manufacturer: self.config.device_info.manufacturer.clone(),
                    model: self.config.device_info.model.clone(),
                    firmware: self.current_firmware(),
                    hardware_version: self
                        .config
                        .device_info
                        .hardware_version
                        .clone()
                        .filter(|value| !value.trim().is_empty()),
                    channel: self.config.channels.len() as u32,
                };
                resp.to_xml()
            }
            "DeviceStatus" => {
                let resp = DeviceStatusResponse {
                    cmd_type: "DeviceStatus".into(),
                    sn: query.sn,
                    device_id: query.device_id.clone(),
                    result: "OK".into(),
                    online: "ONLINE".into(),
                    status: "OK".into(),
                };
                resp.to_xml()
            }
            "MobilePosition" => {
                let (longitude, latitude) = self
                    .position
                    .lock()
                    .map(|position| *position)
                    .map_err(|_| Error::Gb28181("设备位置状态锁异常".into()))?;
                MobilePositionResponse::new(
                    &query.device_id,
                    query.sn,
                    common::clock::synced_iso8601(),
                    longitude,
                    latitude,
                )
                .to_xml()
            }
            "RecordInfo" => self
                .record_info_packets(query)?
                .into_iter()
                .next()
                .ok_or_else(|| Error::Gb28181("RecordInfo 未生成应答".into())),
            "ConfigDownload" => {
                // 从当前会话已应用配置回读，只返回 ConfigType 请求的块。
                self.runtime_config_response(query)
            }
            "PresetQuery" => {
                // 预置位查询:返回当前预置位表(可被 PTZ 预置位设置/删除命令动态修改)。
                let items: Vec<PresetItem> = self
                    .presets
                    .lock()
                    .map(|p| {
                        p.iter()
                            .map(|(id, name)| PresetItem {
                                preset_id: *id as u32,
                                preset_name: name.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let resp = PresetQueryResponse::new(&query.device_id, query.sn, items);
                resp.to_xml()
            }
            "AlarmStatus" => {
                // 报警状态查询(FR-17):GB-2022 每报警通道一项 DutyStatus,GB-2016 用 NotNumber。
                let alarming = self
                    .control_state
                    .lock()
                    .map(|s| s.alarming)
                    .unwrap_or(false);
                // 报警通道:取设备各通道 ID(模拟器无独立报警通道时用查询目标 ID 兜底)。
                let channels: Vec<String> = if self.config.channels.is_empty() {
                    vec![query.device_id.clone()]
                } else {
                    self.config
                        .channels
                        .iter()
                        .map(|c| c.channel_id.to_string())
                        .collect()
                };
                let resp = AlarmStatusResponse::new(
                    &query.device_id,
                    query.sn,
                    &channels,
                    alarming,
                    self.config.gb_version.is_2022(),
                );
                resp.to_xml()
            }
            "HomePositionQuery" => {
                // 看守位查询(FR-17):ResetTime 固定 30,PresetIndex 为"有无看守位"标志。
                let (enabled, has_home) = self
                    .control_state
                    .lock()
                    .map(|s| (s.home_enabled, s.home_set))
                    .unwrap_or((true, false));
                HomePositionQueryResponse::new(&query.device_id, query.sn, enabled, has_home)
                    .to_xml()
            }
            "SDCardStatus" => {
                // 存储卡状态查询(FR-17):模拟单张 32G 卡余 24G。
                if let Ok(mut s) = self.control_state.lock() {
                    s.storage_queried = true;
                }
                StorageCardStatusResponse::mock(&query.device_id, query.sn).to_xml()
            }
            "CruiseTrackListQuery" => {
                // 巡航轨迹列表查询(FR-17):返回已配置的轨迹号。
                let tracks: Vec<u32> = self
                    .control_state
                    .lock()
                    .map(|s| s.cruise_tracks.keys().copied().collect())
                    .unwrap_or_default();
                CruiseTrackListResponse::new(&query.device_id, query.sn, &tracks).to_xml()
            }
            "CruiseTrackQuery" => {
                // 巡航轨迹详情查询(FR-17):读 GroupID(缺省 1),返回该轨迹预置点与组级速度/停留。
                let group = query.group_id.unwrap_or(1);
                let (presets, speed, dwell) = self
                    .control_state
                    .lock()
                    .map(|s| {
                        (
                            s.cruise_tracks.get(&group).cloned().unwrap_or_default(),
                            s.cruise_speed.get(&group).copied().unwrap_or(5),
                            s.cruise_dwell.get(&group).copied().unwrap_or(3),
                        )
                    })
                    .unwrap_or_default();
                CruiseTrackQueryResponse::new_with_params(
                    &query.device_id,
                    query.sn,
                    group,
                    &presets,
                    speed,
                    dwell,
                )
                .to_xml()
            }
            "PTZPosition" => {
                // PTZ 精准状态查询(FR-17,GB-2022):返回最近精准云台姿态。
                let (pan, tilt, zoom) = self
                    .control_state
                    .lock()
                    .map(|s| s.precise_pose)
                    .unwrap_or((0.0, 0.0, 1.0));
                PtzPreciseStatusResponse::new(&query.device_id, query.sn, pan, tilt, zoom).to_xml()
            }
            _ => Err(Error::Gb28181(format!(
                "未实现的查询类型: {}",
                query.cmd_type
            ))),
        }
    }

    /// 处理设备控制命令并返回业务应答 XML。升级命令还会返回一次性副作用调度结果。
    async fn handle_control(&self, ctrl: &gb28181_protocol::manscdp::Control) -> Result<String> {
        Ok(self.handle_control_with_dispatch(ctrl).await?.0)
    }

    /// 处理设备控制命令(PTZ/关键帧/录像/布防/复位/重启),返回应答 XML。
    /// 语义:模拟器接受并回 Result=OK;强制关键帧会触发当前推流立即发关键帧
    ///(当前 FileSource/LightSource 本就周期性带关键帧,记录日志即可)。
    async fn handle_control_with_dispatch(
        &self,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) -> Result<(String, Option<UpgradeDispatch>)> {
        if ctrl.cfg_video_param_attribute.is_some() {
            let response = gb28181_protocol::manscdp::ControlResponse {
                cmd_type: "DeviceConfig".into(),
                sn: ctrl.sn,
                device_id: ctrl.device_id.clone(),
                result: "ERROR".into(),
            };
            return Ok((response.to_xml()?, None));
        }
        if ctrl.device_upgrade.is_some() {
            return self.handle_upgrade_control(ctrl).await;
        }
        tracing::info!(kind = %ctrl.kind(), sn = ctrl.sn, "收到设备控制");
        if let Some(ptz) = &ctrl.ptz_cmd {
            // PTZ 8 字节码:识别预置位设置/调用/删除并更新预置位表;其余为方向/变倍。
            use gb28181_protocol::manscdp::PresetAction;
            match ctrl.preset_op() {
                Some((PresetAction::Set, idx)) => {
                    let name = ctrl
                        .ptz_cmd_params
                        .as_ref()
                        .and_then(|params| params.preset_name.as_deref())
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    if let Ok(mut p) = self.presets.lock() {
                        p.insert(idx, name.clone());
                    }
                    let pose = self
                        .control_state
                        .lock()
                        .map(|state| state.pose)
                        .unwrap_or((0.0, 0.0, 1.0));
                    if let Ok(mut poses) = self.preset_poses.lock() {
                        poses.insert(idx, pose);
                    }
                    tracing::info!(preset = idx, preset_name = %name, "预置位设置");
                }
                Some((PresetAction::Delete, idx)) => {
                    if let Ok(mut p) = self.presets.lock() {
                        p.remove(&idx);
                    }
                    if let Ok(mut poses) = self.preset_poses.lock() {
                        poses.remove(&idx);
                    }
                    tracing::info!(preset = idx, "预置位删除");
                }
                Some((PresetAction::Call, idx)) => {
                    let name = self
                        .presets
                        .lock()
                        .ok()
                        .and_then(|presets| presets.get(&idx).cloned())
                        .unwrap_or_else(|| format!("预置位{idx}"));
                    let pose = self
                        .preset_poses
                        .lock()
                        .ok()
                        .and_then(|poses| poses.get(&idx).copied());
                    tracing::info!(preset = idx, preset_name = %name, "预置位调用(转到)");
                    self.observer
                        .on_event(common::DeviceEvent::PtzPresetCall { preset: idx, name, pose });
                }
                None => {
                    // 优先级:巡航/辅助(0x8x)→ FI 光圈聚焦(0x4x)→ 方向/变倍运动。
                    if let Some(op) = ctrl.cruise_op() {
                        self.apply_cruise_op(op);
                    } else if let Some(scan) = ctrl.scan_op() {
                        self.apply_scan_op(scan);
                    } else if let Some(aux) = ctrl.aux_op() {
                        tracing::info!(func = aux.function.label(), on = aux.on, "辅助控制");
                    } else if let Some(fi) = ctrl.fi_op() {
                        tracing::info!(fi = %fi.label(), "FI 光圈/聚焦控制");
                    } else if let Some(m) = ctrl.ptz_motion() {
                        // 方向/变倍运动:解析后上报观察者(供 UI 云台动画)。
                        tracing::info!(ptz = %ptz, up=m.up, down=m.down, left=m.left, right=m.right,
                            zoom_in=m.zoom_in, zoom_out=m.zoom_out, "PTZ 云台运动");
                        self.observer.on_event(common::DeviceEvent::Ptz {
                            up: m.up,
                            down: m.down,
                            left: m.left,
                            right: m.right,
                            zoom_in: m.zoom_in,
                            zoom_out: m.zoom_out,
                            pan_speed: m.pan_speed,
                            tilt_speed: m.tilt_speed,
                            zoom_speed: m.zoom_speed,
                        });
                        if m.is_stop() {
                            if let Ok(mut state) = self.control_state.lock() {
                                state.active_cruise = None;
                                state.active_scan = None;
                            }
                        }
                    } else {
                        tracing::info!(ptz = %ptz, "PTZ 云台控制(模拟接受)");
                    }
                }
            }
        }
        // 精确云台控制(GB-2022):记录姿态供 PTZPosition 读回。
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            tracing::info!(pan = p.pan, tilt = p.tilt, zoom = p.zoom, "精确云台控制");
            if let Ok(mut s) = self.control_state.lock() {
                s.precise_pose = (p.pan, p.tilt, p.zoom);
                s.pose = (p.pan, p.tilt, p.zoom);
            }
        }
        // 拉框联动：记录平台给出的播放窗口/框像素，供模拟器中心画线框。
        if let Some(zoom) = ctrl.drag_zoom_in.as_ref().or(ctrl.drag_zoom_out.as_ref()) {
            if zoom.length > 0 && zoom.width > 0 && zoom.length_x > 0 && zoom.length_y > 0 {
                let left = (zoom.midpoint_x as f32 - zoom.length_x as f32 / 2.0)
                    / zoom.length as f32;
                let top = (zoom.midpoint_y as f32 - zoom.length_y as f32 / 2.0)
                    / zoom.width as f32;
                if let Ok(mut s) = self.control_state.lock() {
                    s.drag_zoom_box = Some((
                        left.clamp(0.0, 1.0),
                        top.clamp(0.0, 1.0),
                        (zoom.length_x as f32 / zoom.length as f32).clamp(0.0, 1.0),
                        (zoom.length_y as f32 / zoom.width as f32).clamp(0.0, 1.0),
                    ));
                }
            }
        }
        // 目标跟踪:白名单模式外忽略,仍应答 200/OK。
        if let Some(t) = &ctrl.target_track {
            let mode = t.mode.trim();
            if matches!(mode, "Auto" | "Manual" | "Stop") {
                let target_box = t.target_area.as_ref().and_then(|area| {
                    if area.length == 0 || area.width == 0 || area.length_x == 0 || area.length_y == 0 {
                        return None;
                    }
                    let left = (area.midpoint_x as f32 - area.length_x as f32 / 2.0)
                        / area.length as f32;
                    let top = (area.midpoint_y as f32 - area.length_y as f32 / 2.0)
                        / area.width as f32;
                    Some((
                        left.clamp(0.0, 1.0),
                        top.clamp(0.0, 1.0),
                        (area.length_x as f32 / area.length as f32).clamp(0.0, 1.0),
                        (area.length_y as f32 / area.width as f32).clamp(0.0, 1.0),
                    ))
                });
                if let Ok(mut s) = self.control_state.lock() {
                    s.target_mode = if mode == "Stop" { None } else { Some(mode.to_string()) };
                    s.target_box = if mode == "Stop" { None } else { target_box };
                }
                tracing::info!(mode, object = ?t.object_id, "目标跟踪");
            } else {
                tracing::info!(mode, "目标跟踪(模式未识别,忽略)");
            }
        }
        // 格式化 SD 卡:模拟设备无实际存储,仅记录应答。
        if ctrl.format_sd_card.is_some() {
            let disk = ctrl.disk_num.unwrap_or(0);
            tracing::info!(disk, "格式化 SD 卡(模拟接受)");
        }
        // 布防/撤防只更新 Guarded，不再冒充正在报警。
        if let Some(g) = &ctrl.guard_cmd {
            let arm = g.eq_ignore_ascii_case("SetGuard");
            if let Ok(mut s) = self.control_state.lock() {
                s.guarded = arm;
            }
            self.mark_runtime_change(if arm { "guard_set" } else { "guard_reset" });
            tracing::info!(guard = %g, "布防/撤防");
        }
        // 报警复位：兼容移动端已验证的 0/2/ResetAlarm；其它值只留痕。
        if let Some(command) = ctrl.alarm_cmd.as_deref() {
            let numeric = command.parse::<u32>().ok();
            let reset =
                matches!(numeric, Some(0 | 2)) || command.eq_ignore_ascii_case("ResetAlarm");
            if reset {
                if let Ok(mut state) = self.control_state.lock() {
                    state.alarming = false;
                }
                self.mark_runtime_change("alarm_reset");
            }
            tracing::info!(alarm = command, reset, "报警控制");
        }
        // 看守位设置:更新启用/已设标志(供 HomePositionQuery 读回)。
        if let Some(hp) = &ctrl.home_position {
            if let Ok(mut s) = self.control_state.lock() {
                s.home_enabled = hp.enabled != 0;
                s.home_set = hp.enabled != 0;
                s.home_preset_index = (hp.preset_index != 0).then_some(hp.preset_index);
                s.home_reset_time = Some(hp.reset_time);
            }
            tracing::info!(enabled = hp.enabled, preset = hp.preset_index, "看守位设置");
        }
        if ctrl.iframe_cmd.is_some() {
            tracing::info!("强制关键帧(下一帧起带 IDR)");
        }
        if let Some(command) = ctrl.record_cmd.as_deref() {
            if self.recording_service().is_none() {
                tracing::info!(command, "未注入录像存储，保持纯信令模拟应答");
            } else if command.eq_ignore_ascii_case("Record") {
                self.start_recording(
                    crate::recording::RecordingKind::Manual,
                    crate::recording::RecordingSource::Platform,
                )
                .await?;
                tracing::info!("平台录像已启动");
            } else if command.eq_ignore_ascii_case("StopRecord") {
                self.stop_recording().await?;
                tracing::info!("平台录像已停止");
            }
        }
        // DeviceConfig 控制命令(GB28181-2022 A.2.3.2):应用到当前会话配置真相。
        // CmdType=DeviceConfig 时应答须为 DeviceConfig(A.2.6.8),而非 DeviceControl。
        let is_device_config = ctrl.cmd_type == "DeviceConfig"
            || ctrl.cfg_basic_param.is_some()
            || ctrl.cfg_video_record_plan.is_some()
            || ctrl.cfg_video_alarm_record.is_some()
            || ctrl.cfg_picture_mask.is_some()
            || ctrl.cfg_frame_mirror.is_some()
            || ctrl.cfg_alarm_report.is_some()
            || ctrl.cfg_osd_config.is_some()
            || ctrl.cfg_video_param_attribute.is_some()
            || ctrl.cfg_svac_encode.is_some()
            || ctrl.cfg_svac_decode.is_some();
        if ctrl.cfg_basic_param.is_some() {
            tracing::info!("设备配置:基本参数(模拟接受)");
        }
        if ctrl.cfg_video_record_plan.is_some() {
            tracing::info!("设备配置:录像计划(模拟接受)");
        }
        if ctrl.cfg_video_alarm_record.is_some() {
            tracing::info!("设备配置:报警录像(模拟接受)");
        }
        if ctrl.cfg_picture_mask.is_some() {
            tracing::info!("设备配置:画面遮挡(模拟接受)");
        }
        if ctrl.cfg_frame_mirror.is_some() {
            tracing::info!("设备配置:画面翻转(模拟接受)");
        }
        if ctrl.cfg_alarm_report.is_some() {
            tracing::info!("设备配置:报警上报开关(模拟接受)");
        }
        if let Some(osd) = &ctrl.cfg_osd_config {
            let time_show = osd.time_visible();
            let osd_show = osd.text_visible();
            tracing::info!(time_show, osd_show, "设备配置:前端OSD(已应用)");
            // 上报观察者:供 UI 展示"平台下发 OSD 配置、设备已应用"(国标 A.2.3.2.11)。
            self.observer.on_event(common::DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            });
        }
        if is_device_config {
            self.apply_device_config(ctrl)?;
        }
        let resp = if is_device_config {
            gb28181_protocol::manscdp::ControlResponse::config_ok(&ctrl.device_id, ctrl.sn)
        } else {
            gb28181_protocol::manscdp::ControlResponse::ok(&ctrl.device_id, ctrl.sn)
        };
        Ok((resp.to_xml()?, None))
    }

    /// 校验并登记一次升级会话。入站 MESSAGE 的 SIP 200 已经由调用方先行发送，
    /// 这里生成的 DeviceControl Response 只表示设备是否接受本次升级任务；最终结果
    /// 必须等下载、校验、模拟应用和重新注册完成后另发 DeviceUpgradeResult Notify。
    async fn handle_upgrade_control(
        &self,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) -> Result<(String, Option<UpgradeDispatch>)> {
        let rejected = |reason: &str| {
            tracing::warn!(sn = ctrl.sn, %reason, "设备固件升级请求拒绝");
            gb28181_protocol::manscdp::ControlResponse {
                cmd_type: "DeviceControl".into(),
                sn: ctrl.sn,
                device_id: self.config.device_id.as_str().into(),
                result: "ERROR".into(),
            }
            .to_xml()
            .map(|xml| (xml, None))
        };

        let upgrade = match firmware_upgrade::validate_request(
            &self.config.gb_version,
            self.config.device_id.as_str(),
            &self.config.device_info.manufacturer,
            ctrl,
        ) {
            Ok(upgrade) => upgrade,
            Err(reason) => return rejected(&reason),
        };
        tracing::info!(
            sn = ctrl.sn,
            session_id = %upgrade.session_id,
            firmware = %upgrade.firmware,
            "收到 DeviceUpgrade 请求"
        );

        let dispatch = if let Some(existing) = self.upgrade_store.lookup(&upgrade.session_id) {
            if existing.fingerprint != upgrade.fingerprint {
                return rejected("同一 SessionID 的请求参数不一致");
            }
            match existing.status {
                SessionStatus::Success | SessionStatus::Error => UpgradeDispatch::Replay {
                    session_id: upgrade.session_id.clone(),
                    session: existing,
                },
                SessionStatus::InProgress => {
                    let active = self.active_upgrade.lock().await;
                    if active.as_deref() == Some(upgrade.session_id.as_str()) {
                        UpgradeDispatch::AlreadyRunning
                    } else {
                        return rejected("升级会话处于未完成状态，等待设备恢复处理");
                    }
                }
            }
        } else {
            let mut active = self.active_upgrade.lock().await;
            if active.is_some() {
                return rejected("设备正在执行其他固件升级");
            }
            if let Err(error) = self.upgrade_store.begin(&upgrade) {
                tracing::error!(%error, "固件升级 InProgress 状态持久化失败");
                return rejected("升级状态无法持久化");
            }
            *active = Some(upgrade.session_id.clone());
            UpgradeDispatch::Start(upgrade)
        };

        let response =
            gb28181_protocol::manscdp::ControlResponse::ok(self.config.device_id.as_str(), ctrl.sn)
                .to_xml()?;
        Ok((response, Some(dispatch)))
    }

    /// 应用巡航轨迹控制操作到设备控制态(供 CruiseTrack* 查询读回)。
    fn apply_cruise_op(&self, op: gb28181_protocol::manscdp::CruiseOp) {
        use gb28181_protocol::manscdp::CruiseOp;
        let Ok(mut s) = self.control_state.lock() else {
            return;
        };
        match op {
            CruiseOp::SetPoint { track, preset } => {
                let pts = s.cruise_tracks.entry(track).or_default();
                if !pts.contains(&preset) {
                    pts.push(preset);
                }
                tracing::info!(track, preset, "巡航增点");
            }
            CruiseOp::DelPoint { track, preset } => {
                if let Some(pts) = s.cruise_tracks.get_mut(&track) {
                    pts.retain(|&p| p != preset);
                }
                tracing::info!(track, preset, "巡航删点");
            }
            CruiseOp::SetSpeed { track, speed } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_speed.insert(track, speed);
                tracing::info!(track, speed, "巡航设速");
            }
            CruiseOp::SetDwell { track, dwell } => {
                s.cruise_tracks.entry(track).or_default();
                s.cruise_dwell.insert(track, dwell);
                tracing::info!(track, dwell, "巡航设停留");
            }
            CruiseOp::Start { track } => {
                if track == 0 {
                    s.active_cruise = None;
                    tracing::info!("巡航停止");
                } else {
                    s.cruise_tracks.entry(track).or_default();
                    s.active_cruise = Some(track);
                    // 巡航与扫描互斥：同一套机械不能同时跑两个自主行为。
                    s.active_scan = None;
                    tracing::info!(track, "巡航启动");
                }
            }
        }
    }

    /// 应用自动扫描控制(表 A.10)。设边界只快照当前姿态，不移动镜头。
    fn apply_scan_op(&self, op: gb28181_protocol::manscdp::ScanOp) {
        use gb28181_protocol::manscdp::ScanOp;
        let Ok(mut state) = self.control_state.lock() else {
            return;
        };
        match op {
            ScanOp::SetLeft { group } => {
                let pose = state.pose;
                let group_state = state.scan_groups.entry(group).or_default();
                group_state.left = Some(pose);
                tracing::info!(group, "扫描设左边界");
            }
            ScanOp::SetRight { group } => {
                let pose = state.pose;
                let group_state = state.scan_groups.entry(group).or_default();
                group_state.right = Some(pose);
                tracing::info!(group, "扫描设右边界");
            }
            ScanOp::SetSpeed { group, speed } => {
                let group_state = state.scan_groups.entry(group).or_default();
                group_state.speed = Some(speed);
                tracing::info!(group, speed, "扫描设速度");
            }
            ScanOp::Start { group } => {
                state.scan_groups.entry(group).or_default();
                state.active_scan = Some(group);
                // 扫描与巡航互斥。
                state.active_cruise = None;
                tracing::info!(group, "扫描启动");
            }
        }
    }

    /// 处理语音广播(FR-36):校验 TargetID,回 Broadcast Response,接受则反向发 INVITE。
    ///
    /// 设备为**主叫 UAC**:向平台发 `m=audio ... a=recvonly` 的 INVITE 收 G.711A 下行,
    /// 收到 200 OK 后回 ACK,并按配置的音频流传输方式(UDP / TCP 主动 / TCP 被动)收流播放。
    async fn handle_broadcast(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        bc: &gb28181_protocol::manscdp::BroadcastNotify,
        from: SocketAddr,
    ) -> Result<()> {
        // TargetID 须为本设备或其某通道。
        let owned = bc.target_id == self.config.device_id.as_str()
            || self
                .config
                .channels
                .iter()
                .any(|c| c.channel_id.as_str() == bc.target_id);
        let busy = owned && self.broadcast.is_active();
        let resp = if !owned {
            gb28181_protocol::manscdp::BroadcastResponse::error(
                self.config.device_id.as_str(),
                bc.sn,
                "target mismatch",
            )
        } else if busy {
            gb28181_protocol::manscdp::BroadcastResponse::error(
                self.config.device_id.as_str(),
                bc.sn,
                "busy",
            )
        } else {
            gb28181_protocol::manscdp::BroadcastResponse::ok(&bc.target_id, bc.sn)
        };
        // 协议顺序:先以独立 MESSAGE 回 Broadcast Response,再发起反向 INVITE。
        let xml = resp.to_xml()?;
        self.send_reply_message(transport, from, &xml).await?;

        if !owned {
            tracing::warn!(target = %bc.target_id, "语音广播 TargetID 不属本设备,拒绝");
            return Ok(());
        }
        if busy {
            tracing::warn!(target = %bc.target_id, "语音广播忙,拒绝新的广播会话");
            return Ok(());
        }
        if !self.broadcast.start(
            Arc::clone(transport),
            self.local_host(),
            self.local_port(),
            bc.clone(),
        ) {
            tracing::warn!(target = %bc.target_id, "语音广播会话启动失败");
        }
        Ok(())
    }

    /// 触发需设备主动 NOTIFY/上报的控制副作用(固件升级结果、抓拍上传)。
    /// 已在入站循环里回过 200/结果,这里异步执行不阻塞应答。
    fn spawn_control_side_effects(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        ctrl: &gb28181_protocol::manscdp::Control,
        upgrade_dispatch: Option<UpgradeDispatch>,
    ) {
        // 在线升级:下载/校验/应用/重新注册后只发一次最终结果 Notify。
        if let Some(dispatch) = upgrade_dispatch {
            if !matches!(dispatch, UpgradeDispatch::AlreadyRunning) {
                let dev = Arc::clone(self);
                let tp = Arc::clone(transport);
                tokio::spawn(async move {
                    dev.run_upgrade_dispatch(&tp, dispatch).await;
                });
            }
        }
        // 抓拍配置(GB-2022):HTTP 上传 + 完成 NOTIFY。
        if let Some(cfg) = &ctrl.snap_shot_config {
            if cfg.is_valid() {
                let dev = Arc::clone(self);
                let tp = Arc::clone(transport);
                let cfg = cfg.clone();
                tokio::spawn(async move {
                    dev.run_snapshot_upload(&tp, &cfg).await;
                });
            } else {
                tracing::warn!("SnapShotConfig 缺 SessionID/UploadURL,忽略");
            }
        }
        // 抓拍(旧路径):经 Alarm Notify 上报。
        if ctrl.snap_shot_cmd.is_some() {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            tokio::spawn(async move {
                let (h, p) = (dev.local_host(), dev.local_port());
                let _ = dev.report_alarm(&tp, &h, p, "SnapShot").await;
            });
        }
    }

    /// 执行已登记的升级会话，成功路径重新注册并持久化版本后发送最终 Notify。
    async fn run_upgrade_dispatch(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        dispatch: UpgradeDispatch,
    ) {
        let clear_active = matches!(&dispatch, UpgradeDispatch::Start(_));
        let (session_id, result) = match dispatch {
            UpgradeDispatch::Start(upgrade) => {
                let mut result = self.execute_upgrade(&upgrade).await;
                if result.success {
                    // 将状态文件中的成功提交作为模拟刷写的原子点；只有重新 REGISTER
                    // 成功后才提交并对平台宣告 OK。这样 REGISTER 失败或其窗口内进程崩溃
                    // 都不会留下一个已确认的成功结果。
                    let (host, port) = (self.local_host(), self.local_port());
                    if let Err(error) = self.register(transport, &host, port).await {
                        tracing::warn!(%error, "固件升级后重新注册失败，升级结果改为 ERROR");
                        result = UpgradeResult {
                            firmware: self.current_firmware(),
                            success: false,
                            failure_reason: Some("03".into()),
                        };
                        let _ = self.upgrade_store.complete(&upgrade.session_id, &result);
                    } else {
                        if let Err(error) =
                            self.upgrade_store.complete(&upgrade.session_id, &result)
                        {
                            tracing::error!(%error, "固件版本持久化失败，升级结果改为 ERROR");
                            result = UpgradeResult {
                                firmware: self.current_firmware(),
                                success: false,
                                failure_reason: Some("03".into()),
                            };
                            let _ = self.upgrade_store.complete(&upgrade.session_id, &result);
                        }
                    }
                } else if let Err(error) = self.upgrade_store.complete(&upgrade.session_id, &result)
                {
                    tracing::error!(%error, "固件升级失败状态持久化失败");
                }
                (upgrade.session_id, result)
            }
            UpgradeDispatch::Replay {
                session_id,
                session,
            } => (session_id, session.result()),
            UpgradeDispatch::AlreadyRunning => return,
        };

        self.send_upgrade_result(transport, &result, &session_id)
            .await;

        if clear_active {
            let mut active = self.active_upgrade.lock().await;
            if active.as_deref() == Some(session_id.as_str()) {
                *active = None;
            }
        }
    }

    async fn execute_upgrade(&self, upgrade: &ValidatedUpgrade) -> UpgradeResult {
        let body = match firmware_upgrade::download(&upgrade.file_url).await {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(
                    session_id = %upgrade.session_id,
                    reason = error.reason_code(),
                    error = %error.message(),
                    "固件下载失败"
                );
                return UpgradeResult {
                    firmware: self.current_firmware(),
                    success: false,
                    failure_reason: Some(error.reason_code().into()),
                };
            }
        };
        match firmware_upgrade::verify_package(
            &body,
            &self.config.device_info.manufacturer,
            &self.config.device_info.model,
            &upgrade.firmware,
        ) {
            Ok(firmware) => {
                tracing::info!(
                    session_id = %upgrade.session_id,
                    firmware = %firmware,
                    "固件包校验通过，模拟设备版本已准备更新"
                );
                UpgradeResult {
                    firmware,
                    success: true,
                    failure_reason: None,
                }
            }
            Err(error) => {
                tracing::warn!(session_id = %upgrade.session_id, %error, "固件包校验失败");
                UpgradeResult {
                    firmware: self.current_firmware(),
                    success: false,
                    failure_reason: Some("02".into()),
                }
            }
        }
    }

    async fn send_upgrade_result(
        &self,
        transport: &Arc<SipTransport>,
        result: &UpgradeResult,
        session_id: &str,
    ) {
        let sn = self.next_cseq();
        let notify = if result.success {
            gb28181_protocol::manscdp::DeviceUpgradeResultNotify::success(
                self.config.device_id.as_str(),
                sn,
                session_id,
                &result.firmware,
            )
        } else {
            gb28181_protocol::manscdp::DeviceUpgradeResultNotify::error(
                self.config.device_id.as_str(),
                sn,
                session_id,
                &result.firmware,
                result.failure_reason.as_deref().unwrap_or("99"),
            )
        };
        let Ok(xml) = notify.to_xml() else {
            tracing::error!(session_id, "固件升级结果通知序列化失败");
            return;
        };
        // 平台可能先用 503 表示结果落库尚未完成；每次调用都会创建独立的
        // SIP 事务，最多补发两次。重试只覆盖事务超时和 5xx，4xx/其它错误
        // 直接结束，且整个过程不会重新下载或执行升级。
        for attempt in 0..3u8 {
            let outcome = self
                .send_message_xml(transport, &self.local_host(), self.local_port(), &xml)
                .await;
            let retryable = match outcome {
                Ok(status) if (200..300).contains(&status) => {
                    tracing::info!(
                        session_id,
                        status,
                        attempt = attempt + 1,
                        "固件升级结果通知已确认"
                    );
                    false
                }
                Ok(status) if (500..600).contains(&status) => {
                    tracing::warn!(
                        session_id,
                        status,
                        attempt = attempt + 1,
                        "固件升级结果通知收到 5xx"
                    );
                    true
                }
                Ok(status) => {
                    tracing::warn!(
                        session_id,
                        status,
                        attempt = attempt + 1,
                        "固件升级结果通知未获 2xx"
                    );
                    false
                }
                Err(error) => {
                    let retryable = matches!(
                        &error,
                        Error::Sip(message) if message.contains("事务超时") || message.contains("超时")
                    );
                    tracing::warn!(
                        session_id,
                        %error,
                        attempt = attempt + 1,
                        retryable,
                        "固件升级结果通知发送失败"
                    );
                    retryable
                }
            };
            if !retryable || attempt == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs((attempt + 1) as u64)).await;
        }
    }

    /// 抓拍上传(GB-2022):串行拍 N 张，从共享采集流的最近可解码关键帧生成 JPEG。
    async fn run_snapshot_upload(
        &self,
        transport: &Arc<SipTransport>,
        cfg: &gb28181_protocol::manscdp::SnapShotConfig,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        let num = cfg.clamped_num();
        for idx in 1..=num {
            let ts = common::clock::synced_iso8601();
            let snap_id = format!("{}_{idx}", ts.replace([':', '-'], "").replace('.', ""));
            let url = if cfg.upload_url.ends_with('/') {
                format!("{}{snap_id}.jpg", cfg.upload_url)
            } else {
                cfg.upload_url.clone()
            };
            let jpeg = match self.current_keyframe_jpeg().await {
                Ok(jpeg) => Some(jpeg),
                Err(error) => {
                    tracing::warn!(%error, "当前帧转 JPEG 失败，本张抓拍跳过");
                    None
                }
            };
            if let Some(jpeg) = jpeg {
                match http_put_jpeg(&url, &jpeg).await {
                    Ok(()) => {
                        let sn = self.next_cseq();
                        let notify = gb28181_protocol::manscdp::SnapShotNotify::new(
                            self.config.device_id.as_str(),
                            sn,
                            &cfg.session_id,
                            &snap_id,
                            &ts,
                            &url,
                        );
                        if let Ok(xml) = notify.to_xml() {
                            let _ = self.send_message_xml(transport, &h, p, &xml).await;
                        }
                        tracing::info!(snap_id, "抓拍上传成功");
                    }
                    Err(e) => tracing::warn!(error = %e, url, "抓拍上传失败"),
                }
            }
            self.observer.on_event(common::DeviceEvent::Progress {
                kind: "snapshot".into(),
                current: idx,
                total: num,
                percent: idx * 100 / num,
            });
            if idx < num && cfg.interval > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(cfg.interval as u64)).await;
            }
        }
    }

    /// 向平台发送一条应答/通知 MESSAGE(查询应答、控制应答、位置通知等复用)。
    async fn send_reply_message(
        &self,
        transport: &Arc<SipTransport>,
        to: SocketAddr,
        xml: &str,
    ) -> Result<()> {
        let cseq = self.next_cseq();
        let reply = builder::message_xml(
            &self.config,
            &self.ids,
            cseq,
            &self.local_host(),
            self.local_port(),
            xml,
        );
        transport
            .send_to(&sip_core::SipMessage::Request(reply), to)
            .await
    }

    /// 发送独立的查询 Response MESSAGE，并等待平台事务确认。
    ///
    /// 每片使用独立 Call-ID，避免和注册、心跳及相邻分片争抢同一接收路由；调用方按
    /// 返回顺序发送下一片，并在单片失败时继续 best-effort 发送剩余片。
    async fn send_query_response_message(
        &self,
        transport: &Arc<SipTransport>,
        to: SocketAddr,
        xml: &str,
    ) -> Result<u16> {
        let ids = DialogIds::new();
        let cseq = self.next_cseq();
        let request = builder::message_xml(
            &self.config,
            &ids,
            cseq,
            &self.local_host(),
            self.local_port(),
            xml,
        );
        let mut rx = transport.register(ids.call_id.clone());
        let result =
            sip_core::client_transact(transport, to, &request, &mut rx, query_response_timing())
                .await;
        transport.unregister(&ids.call_id);
        Ok(result?.status)
    }

    /// 处理 INVITE:解析平台 SDP,启动推流,回 200 OK(带本端 SDP)。
    async fn handle_invite(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        use std::net::IpAddr;

        // 解析平台 SDP(提取 RTP 目标地址与端口)。
        let body_str = std::str::from_utf8(&req.body)
            .map_err(|_| Error::Sip("INVITE body 非 UTF-8".into()))?;
        let platform_sdp = sip_core::SessionDescription::parse(body_str)?;
        // 打完整 SDP,便于诊断 TCP 方向(active/passive)/端口/SSRC 等推流问题。
        tracing::info!(sdp = %body_str.replace('\r', "").replace('\n', " | "), "INVITE:平台完整 SDP");
        tracing::info!(proto=%platform_sdp.media.proto, "INVITE SDP 媒体协议");
        let rtp_host: IpAddr = platform_sdp
            .connection
            .addr
            .parse()
            .map_err(|_| Error::Sip("SDP c= 地址非法".into()))?;
        let rtp_port = platform_sdp.media.port;
        let rtp_dst = SocketAddr::new(rtp_host, rtp_port);

        // SSRC 必须用平台 SDP 的 y= 行指定值(GB28181),平台据此识别本条流。
        // 缺失时退化为设备 ID hash。
        let ssrc = platform_sdp.media.ssrc.unwrap_or_else(|| {
            self.config
                .device_id
                .as_str()
                .bytes()
                .fold(0u32, |a, b| a.wrapping_add(b as u32))
        });
        tracing::info!(%rtp_dst, ssrc, "INVITE:开始向平台推流");

        // 启动推流任务(若配置了视频源)。
        let mut sess_guard = self.session.lock().await;
        if sess_guard.is_some() {
            let busy =
                sip_core::SipMessage::Response(builder::response_status(req, 486, "Busy Here"));
            transport.send_to(&busy, from).await?;
            return Ok(true);
        }
        // 实时设备启动可能涉及 TCC 授权和最多 5 秒首帧等待，先发 100 防止
        // 平台在准备期间把事务误判为无响应。
        let trying = sip_core::SipMessage::Response(builder::response_status(req, 100, "Trying"));
        transport.send_to(&trying, from).await?;

        // 传输角色:UDP / 设备主动连平台 / 设备被动等平台连入(由 offer 的 a=setup 推导)。
        let media_transport = platform_sdp.device_media_transport();
        tracing::info!(?media_transport, setup = ?platform_sdp.setup, "INVITE:媒体传输角色");

        // o= 用户名/索引通道均取 INVITE Request-URI 的 user 部分。
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());

        // 直播订阅共享媒体；回放/下载只能从录像索引创建有限文件源。
        let fps = self.config.video_fps;
        let is_history = platform_sdp.is_playback() || platform_sdp.is_download();
        let source: Option<Box<dyn media_rtp::VideoSource>> = if is_history {
            let service = self.recording_service();
            let entries = service
                .as_ref()
                .map(|service| {
                    service.store().query(&crate::recording::RecordingQuery {
                        start_time_ms: (platform_sdp.timing.start > 0)
                            .then(|| platform_sdp.timing.start.saturating_mul(1_000)),
                        end_time_ms: (platform_sdp.timing.stop > 0)
                            .then(|| platform_sdp.timing.stop.saturating_mul(1_000)),
                        channel_id: Some(channel.to_owned()),
                        kind: None,
                    })
                })
                .unwrap_or_default();
            if entries.is_empty() {
                let not_found = sip_core::SipMessage::Response(builder::response_status(
                    req,
                    404,
                    "Recording Not Found",
                ));
                transport.send_to(&not_found, from).await?;
                self.observer.on_event(common::DeviceEvent::RuntimeError {
                    scope: "history".into(),
                    message: "回放/下载时间窗内无完成录像".into(),
                });
                return Ok(true);
            }
            let paths: Vec<_> = entries.into_iter().map(|entry| entry.path).collect();
            match media_rtp::FileSource::from_paths_once(&paths) {
                Ok(source) => Some(Box::new(source)),
                Err(error) => {
                    tracing::warn!(%error, "历史录像文件加载失败");
                    let unavailable = sip_core::SipMessage::Response(builder::response_status(
                        req,
                        480,
                        "Recording Unavailable",
                    ));
                    transport.send_to(&unavailable, from).await?;
                    return Ok(true);
                }
            }
        } else {
            self.shared_media
                .lock()
                .await
                .as_ref()
                .map(|media| Box::new(media.subscribe()) as Box<dyn media_rtp::VideoSource>)
        };
        if source.is_none()
            && (self.config.video_source.is_some() || self.config.light_bitrate_kbps.is_some())
        {
            self.observer.on_event(common::DeviceEvent::RuntimeError {
                scope: "capture".into(),
                message: "平台点播时共享媒体源尚未就绪".into(),
            });
            let unavailable = sip_core::SipMessage::Response(builder::response_status(
                req,
                480,
                "Temporarily Unavailable",
            ));
            transport.send_to(&unavailable, from).await?;
            return Ok(true);
        }

        // 先构造并发送 200 OK(带本端 SDP,SSRC 回显平台值)。
        // TCP-PASSIVE 下平台收到 200 后才开始监听,故推流必须在 200 之后再连接。
        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(rtp_host));
        // 设备被动:先绑定监听端口,answer 里写真实端口,平台收到 200 OK 后连入。
        // 只有确实要推流时才绑定,避免无源会话白占端口。
        let passive_listener = if media_transport == sip_core::MediaTransport::TcpPassive
            && source.is_some()
        {
            // 绑定通配地址:local_host 可能是 NAT 外部地址,不一定在本机网卡上。
            let bind_ip: IpAddr = if local_ip.is_ipv4() {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            };
            match tokio::net::TcpListener::bind(SocketAddr::new(bind_ip, 0)).await {
                Ok(listener) => Some(listener),
                Err(error) => {
                    tracing::warn!(%error, %local_ip, "TCP 被动推流监听端口绑定失败");
                    let unavailable = sip_core::SipMessage::Response(
                        builder::response_status(req, 488, "Not Acceptable Here"),
                    );
                    transport.send_to(&unavailable, from).await?;
                    return Ok(true);
                }
            }
        } else {
            None
        };
        // RFC 4145:设备主动时 answer 使用 discard port 9,实际媒体目标仍为
        // 平台 offer 中的 c=/m= 地址;设备被动时写真实监听端口。
        let answer_port = match media_transport {
            sip_core::MediaTransport::Udp => 0,
            sip_core::MediaTransport::TcpActive => 9,
            sip_core::MediaTransport::TcpPassive => passive_listener
                .as_ref()
                .and_then(|listener| listener.local_addr().ok())
                .map_or(9, |addr| addr.port()),
        };
        let local_sdp = sip_core::SessionDescription::new_device_answer(
            channel,
            local_ip,
            answer_port,
            ssrc,
            &platform_sdp,
        );
        let sdp_body = local_sdp.to_string();

        let mut resp = builder::response_ok(req);
        resp.headers.set("Content-Type", "application/sdp");
        resp.body = sdp_body.as_bytes().to_vec();
        transport
            .send_to(&sip_core::SipMessage::Response(resp), from)
            .await?;

        // 200 OK 已发,启动推流(TCP 模式给平台一点时间建监听 + 收 ACK)。
        // 带回放控制:回放时平台可经会话内 INFO 调整倍速/暂停,直播则保持 1.0x。
        if let Some(source) = source {
            let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
            let (start_tx, start_rx) = tokio::sync::oneshot::channel();
            let control = media_rtp::PlaybackControl::new();
            // 录像下载会话(s=Download + a=downloadspeed:N):按 N 倍速推流。
            if platform_sdp.is_download() {
                if let Some(spd) = platform_sdp.download_speed {
                    control.set_speed(spd as f32);
                    tracing::info!(speed = spd, "INVITE:录像下载模式,按倍速推流");
                }
            }
            let control_task = Arc::clone(&control);
            let simulator = Arc::clone(self);
            let completion_transport = Arc::clone(transport);
            let marker = Arc::new(());
            let task_marker = Arc::clone(&marker);
            let task = tokio::spawn(async move {
                // 确保 PushSession 已写入槽位后再允许任务结束并清理，避免启动失败竞态。
                if start_rx.await.is_err() {
                    return;
                }
                let target = match (media_transport, passive_listener) {
                    (sip_core::MediaTransport::TcpPassive, Some(listener)) => {
                        media_rtp::RtpTarget::TcpPassive {
                            listener,
                            accept_timeout: TCP_PASSIVE_ACCEPT_TIMEOUT,
                        }
                    }
                    (sip_core::MediaTransport::TcpActive, _)
                    | (sip_core::MediaTransport::TcpPassive, None) => {
                        // 主动模式给平台一点时间建监听并收 ACK。
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        media_rtp::RtpTarget::TcpActive(rtp_dst)
                    }
                    (sip_core::MediaTransport::Udp, _) => media_rtp::RtpTarget::Udp(rtp_dst),
                };
                // GB28181 的音频复用在 PS 内部,SDP 没有可靠的"不要音频"信号,
                // 直播与回放都按源能力带音频;PushOptions 保留按会话关闭的能力。
                let result = media_rtp::push_stream_to(
                    source,
                    target,
                    media_rtp::PushOptions {
                        ssrc,
                        fps,
                        with_audio: true,
                    },
                    control_task,
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await;
                if let Err(error) = &result {
                    tracing::warn!(%error, "推流结束(错误)");
                    simulator
                        .observer
                        .on_event(common::DeviceEvent::RuntimeError {
                            scope: "stream".into(),
                            message: error.to_string(),
                        });
                }
                let mut session = simulator.session.lock().await;
                let owns_slot = session
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(&active.marker, &task_marker));
                if owns_slot {
                    *session = None;
                }
                drop(session);
                if owns_slot {
                    simulator.observer.on_event(common::DeviceEvent::StreamStop);
                    if is_history {
                        simulator.notify_media_finished(&completion_transport).await;
                    }
                }
            });
            *sess_guard = Some(PushSession {
                marker,
                _task: task,
                stop_tx,
                control,
                is_history,
                history_duration_secs: platform_sdp
                    .timing
                    .stop
                    .saturating_sub(platform_sdp.timing.start),
            });
            self.observer.on_event(common::DeviceEvent::StreamStart);
            let _ = start_tx.send(());
        }
        drop(sess_guard);
        let mode = if platform_sdp.is_download() {
            "下载"
        } else if platform_sdp.is_playback() {
            "回放"
        } else {
            "点播"
        };
        self.observer
            .on_event(common::DeviceEvent::PlatformCommand {
                kind: "invite".into(),
                summary: format!("平台{mode} INVITE → 200 OK,开始推流"),
            });
        Ok(true)
    }

    /// 处理 BYE:停止推流,回 200 OK。回放/下载会话结束后补发 MediaStatus 121。
    async fn handle_bye(
        self: &Arc<Self>,
        transport: &Arc<SipTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        let mut sess_guard = self.session.lock().await;
        let was_history = sess_guard.as_ref().map(|s| s.is_history).unwrap_or(false);
        let had_session = if let Some(session) = sess_guard.take() {
            let _ = session.stop_tx.send(()); // 触发停流
            true
        } else {
            false
        };
        drop(sess_guard);
        if had_session {
            self.observer.on_event(common::DeviceEvent::StreamStop);
        }

        let resp = sip_core::SipMessage::Response(builder::response_ok(req));
        transport.send_to(&resp, from).await?;

        // 回放/下载会话结束:补发 MediaStatus 121(历史媒体文件发送结束)。
        if was_history {
            self.notify_media_finished(transport).await;
        }
        Ok(true)
    }

    /// 运行设备:注册 → 启动入站应答(OPTIONS)→ 周期心跳,直到 `shutdown` 完成。
    ///
    /// 心跳连续失败 `max_hb_fail` 次判定掉线,触发重注册(指数退避 backoff→2×,上限 backoff_max)。
    /// 这是设备"保持在线"的主循环(心跳保活与掉线重注册)。
    pub async fn run(
        self: Arc<Self>,
        transport: Arc<SipTransport>,
        local_host: String,
        local_port: u16,
        shutdown: impl std::future::Future<Output = ()>,
    ) {
        use std::time::Duration;
        tokio::pin!(shutdown);

        // 记录本端信令地址(应答 MESSAGE / NOTIFY 构造 Via/From/Contact 用)。
        let _ = self.local_addr.set((local_host.clone(), local_port));

        // 入站请求应答任务:注册设备 ID + 全部通道 ID(点播 INVITE 的
        // Request-URI 是通道 ID,需一并注册才能收到)。
        let mut aors = vec![self.config.device_id.as_str().to_string()];
        aors.extend(
            self.config
                .channels
                .iter()
                .map(|c| c.channel_id.as_str().to_string()),
        );
        // 多通道目录树的视频通道 ID 也要注册,否则平台点播子通道的 INVITE 收不到(问题1)。
        {
            let tree = self.catalog_tree.lock().unwrap();
            aors.extend(
                tree.iter()
                    .filter(|n| {
                        n.node_type == gb28181_protocol::id_codec::CatalogNodeType::VideoChannel
                    })
                    .map(|n| n.id.clone()),
            );
        }
        aors.sort();
        aors.dedup();
        let mut inbound = transport.register_inbound_many(aors);
        let inbound_sim = self.clone();
        let inbound_tp = transport.clone();
        let inbound_task = tokio::spawn(async move {
            while let Some(inc) = inbound.recv().await {
                if let sip_core::SipMessage::Request(r) = &inc.message {
                    tracing::info!(method=%r.method, uri=%r.uri, "收到入站请求");
                }
                if let Err(e) = inbound_sim.answer_inbound(&inbound_tp, &inc).await {
                    tracing::warn!(error=%e, "入站请求应答失败");
                }
            }
        });

        // 初次注册(失败则退避重试)。
        let backoff_base = register_retry_base();
        let mut backoff = backoff_base;
        while (self.register(&transport, &local_host, local_port).await).is_err() {
            tokio::select! {
                _ = &mut shutdown => { inbound_task.abort(); return; }
                _ = tokio::time::sleep(backoff) => {
                    backoff = next_register_retry(backoff);
                }
            }
        }
        tracing::info!(device=%self.config.device_id, "注册成功");

        // 心跳主循环 + 注册续约(FR-32)。
        // 按配置有效期的 80% 主动重注册续约。
        let interval = Duration::from_secs(self.config.heartbeat_interval_secs.max(1));
        let renew_after = register_renew_after(self.config.register_expires_secs);
        let mut renew_ticker = tokio::time::interval(renew_after);
        renew_ticker.reset(); // 跳过启动即触发
        let max_hb_fail = self.config.heartbeat_fail_threshold.max(1);
        let mut hb_fail = 0u32;
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                _ = renew_ticker.tick() => {
                    // 到期前主动续约,避免掉线后被动重注册。
                    tracing::info!(device=%self.config.device_id, "注册续约(到期前 80%)");
                    let _ = self.register(&transport, &local_host, local_port).await;
                }
                _ = tokio::time::sleep(interval) => {
                    match self.send_keepalive(&transport, &local_host, local_port).await {
                        Ok(200) => hb_fail = 0,
                        _ => {
                            if record_heartbeat_failure(&mut hb_fail, max_hb_fail) {
                                tracing::warn!(device=%self.config.device_id, "心跳连续失败,重注册");
                                hb_fail = 0;
                                let mut b = backoff_base;
                                while self.register(&transport, &local_host, local_port).await.is_err() {
                                    tokio::select! {
                                        _ = &mut shutdown => { inbound_task.abort(); return; }
                                        _ = tokio::time::sleep(b) => { b = next_register_retry(b); }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        inbound_task.abort();
        self.stop_subscriptions().await;
        // 先结束语音广播(尽力向平台发 BYE),再注销设备。
        if let Ok(dst) = format!("{}:{}", self.config.server_host, self.config.server_port)
            .parse::<SocketAddr>()
        {
            self.broadcast
                .stop(Arc::clone(&transport), dst, local_host.clone(), local_port)
                .await;
        }
        if let Some(service) = self.recording_service() {
            if let Err(error) = service.stop().await {
                tracing::warn!(%error, "设备下线时录像收尾失败");
            }
        }
        self.stop_shared_media().await;
        // 主动向平台注销(REGISTER Expires=0,§9.1.2.2):平台立即置离线,不等心跳超时。
        if let Err(e) = self.unregister(&transport, &local_host, local_port).await {
            tracing::warn!(device=%self.config.device_id, error=%e, "注销失败(下线仍继续)");
        } else {
            tracing::info!(device=%self.config.device_id, "已向平台注销");
        }
        transport.unregister_inbound(self.config.device_id.as_str());
    }
}

#[cfg(test)]
mod osd_merge_tests {
    use super::*;
    use gb28181_protocol::manscdp::{
        OSDConfig, OsdTextItem, PictureMask, PictureMaskRegion, PictureMaskRegionList,
    };
    use media_rtp::osd::*;

    #[test]
    fn 未下发平台配置时时间左上通道名右上() {
        let overlay = merge_osd_overlay(&OsdSettings::default(), None, None, "大门", (1280, 720))
            .expect("默认有时间和通道名");
        assert_eq!(overlay.time.unwrap().anchor, OsdAnchor::Pixel { x: 6, y: 6 });
        assert_eq!(overlay.texts.len(), 1);
        assert_eq!(overlay.texts[0].text, "大门");
        assert_eq!(overlay.texts[0].anchor, OsdAnchor::TopRight);
        assert!(overlay.watermark.is_none() && overlay.masks.is_empty());
    }

    #[test]
    fn 平台配置覆盖坐标和开关并保留本地样式() {
        let mut settings = OsdSettings::default();
        settings.timestamp.size = OsdSize::Large;
        let platform = OSDConfig {
            length: 704,
            width: 576,
            time_x: 100,
            time_y: 50,
            time_enable: Some(1),
            time_type: Some(1),
            text_enable: Some(1),
            sum_num: 2,
            items: vec![
                OsdTextItem { text: "一号".into(), x: 10, y: 500 },
                OsdTextItem { text: "".into(), x: 0, y: 0 },
            ],
            ..Default::default()
        };
        let overlay =
            merge_osd_overlay(&settings, Some(&platform), None, "大门", (1920, 1080)).unwrap();
        assert_eq!((overlay.base_width, overlay.base_height), (704, 576));
        let time = overlay.time.unwrap();
        assert_eq!(time.anchor, OsdAnchor::Pixel { x: 100, y: 50 });
        assert_eq!(time.format, OsdTimeFormat::Chinese);
        assert_eq!(time.style.size, OsdSize::Large);
        // 空文字行忽略，本地通道名不再额外绘制。
        assert_eq!(overlay.texts.len(), 1);
        assert_eq!(overlay.texts[0].text, "一号");
    }

    #[test]
    fn 平台全部关闭且无水印无遮挡时不烧录() {
        let platform = OSDConfig {
            length: 1920,
            width: 1080,
            time_enable: Some(0),
            text_enable: Some(0),
            ..Default::default()
        };
        assert!(merge_osd_overlay(&OsdSettings::default(), Some(&platform), None, "x", (1920, 1080)).is_none());
    }

    #[test]
    fn 遮挡仅在开启时生效且使用媒体分辨率坐标() {
        let mut settings = OsdSettings::default();
        settings.timestamp.enabled = false;
        settings.channel_name.enabled = false;
        let mut mask = PictureMask {
            on: 0,
            sum_num: 1,
            region_list: Some(PictureMaskRegionList {
                num: 1,
                items: vec![PictureMaskRegion { seq: 1, point: "10,20,110,220".into() }],
            }),
        };
        assert!(merge_osd_overlay(&settings, None, Some(&mask), "", (1280, 720)).is_none());
        mask.on = 1;
        let overlay = merge_osd_overlay(&settings, None, Some(&mask), "", (1280, 720)).unwrap();
        assert_eq!(overlay.mask_base, (1280, 720));
        assert_eq!(overlay.masks, vec![OsdRect { left: 10, top: 20, right: 110, bottom: 220 }]);
    }

    #[test]
    fn 平台osd与遮挡跨重启持久化() {
        let config = test_device_config_for_osd();
        let device = DeviceSimulator::new(config.clone());
        let platform = OSDConfig { length: 704, width: 576, time_x: 8, ..Default::default() };
        let mask = PictureMask { on: 1, sum_num: 0, region_list: None };
        let saved = PersistedControlState {
            osd_config: Some(platform.clone()),
            picture_mask: Some(mask.clone()),
            ..Default::default()
        };
        device.import_persisted_control_state(&saved);
        let exported = device.export_persisted_control_state();
        assert_eq!(exported.osd_config, Some(platform));
        assert_eq!(exported.picture_mask, Some(mask));
        // 旧格式文件(无新字段)仍可读取。
        let legacy: PersistedControlState = serde_json::from_str(r#"{"presets":[]}"#).unwrap();
        assert!(legacy.osd_config.is_none() && legacy.picture_mask.is_none());
        // 未下发过时不落盘遮挡默认值。
        let fresh = DeviceSimulator::new(config).export_persisted_control_state();
        assert!(fresh.osd_config.is_none() && fresh.picture_mask.is_none());
    }

    fn test_device_config_for_osd() -> DeviceConfig {
        super::tests::test_cfg("127.0.0.1", 5060)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_core::{Headers, Response, SipMessage};

    #[tokio::test]
    async fn media_remote_config_rejection_returns_error_without_mutation() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let before = sim.runtime_config.lock().unwrap().clone();
        let mut ctrl = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>19</SN><DeviceID>d</DeviceID></Control>",
        )
        .unwrap();
        ctrl.cfg_video_param_attribute = before.video_param_attribute.clone();
        let xml = sim.handle_control(&ctrl).await.unwrap();
        assert!(xml.contains("<Result>ERROR</Result>"));
        assert_eq!(sim.runtime_config.lock().unwrap().clone(), before);
    }

    #[test]
    fn media_profile_runtime_query_uses_selected_values() {
        let mut config = test_cfg("127.0.0.1", 5060);
        config.video_fps = 20;
        config.media_profile = Some(media_rtp::profile::MediaProfile {
            width: 640,
            height: 480,
            video_fps: 20,
            bitrate_kbps: 600,
            video_codec: media_rtp::profile::MediaVideoCodec::H265,
            ..Default::default()
        });
        let response = initial_runtime_config(&config);
        let item = &response.video_param_attribute.unwrap().items[0];
        assert_eq!(item.video_format, "H.265");
        assert_eq!(item.resolution, "640*480");
        assert_eq!(item.frame_rate, "20");
        assert_eq!(item.video_bit_rate.as_deref(), Some("600"));
    }

    #[test]
    fn subscription_remaining_secs_covers_fresh_halfway_and_expired() {
        let meta = SubscriptionMeta {
            subscriber: Some("10.0.0.9:5060".into()),
            expires_secs: 3_600,
            subscribed_at_ms: 1_000_000,
            ..Default::default()
        };
        // 刚订阅：剩余就是完整的有效期。
        assert_eq!(meta.remaining_secs(1_000_000), 3_600);
        // 走了一半：向下取整，不四舍五入（宁可少报一秒也不提前宣告过期）。
        assert_eq!(meta.remaining_secs(1_000_000 + 1_800_000), 1_800);
        // 已过期：饱和到 0，不能下溢成天文数字。
        assert_eq!(meta.remaining_secs(1_000_000 + 3_600_000 * 2), 0);
        // 客户端时钟早于订阅时刻（对时回拨）：同样饱和到 0。
        assert_eq!(meta.remaining_secs(0), 3_600);
    }

    #[test]
    fn subscription_without_expires_has_no_remaining_secs() {
        // Expires: 0 是退订语义，不该算出「剩余 0 秒但仍在订阅」的假象。
        let meta = SubscriptionMeta {
            subscriber: Some("10.0.0.9:5060".into()),
            expires_secs: 0,
            subscribed_at_ms: 1_000_000,
            ..Default::default()
        };
        assert_eq!(meta.remaining_secs(1_000_000), 0);
    }

    #[test]
    fn ptz_position_snapshot_carries_six_fields() {
        let snapshot = PtzPositionSnapshot::from_pose(30.0, -12.5, 2.0);
        assert_eq!(snapshot.pan, 30.0);
        assert_eq!(snapshot.tilt, -12.5);
        assert_eq!(snapshot.zoom, 2.0);
        // 光学三量按变焦换算，与报文里的取值同源。
        assert_eq!(snapshot.horizontal_field_angle, 30.0);
        assert_eq!(snapshot.vertical_field_angle, 18.0);
        assert_eq!(snapshot.max_view_distance, 600.0);
    }

    #[tokio::test]
    async fn fresh_simulator_reports_no_subscriptions_with_zero_remaining() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let snapshots = sim.subscription_snapshot().await;
        assert_eq!(snapshots.len(), 4);
        for snapshot in &snapshots {
            assert!(!snapshot.active, "{} 未订阅时不应显示活跃", snapshot.kind);
            assert_eq!(snapshot.lifecycle, "not_subscribed", "{}", snapshot.kind);
            assert_eq!(snapshot.remaining_secs, 0);
            assert!(snapshot.subscriber.is_none());
            assert!(snapshot.last_error.is_none());
        }
        // PTZ 未订阅时不该带姿态，否则界面会显示一份"没有订阅来源"的读数。
        let ptz = snapshots
            .iter()
            .find(|item| item.kind == "PTZPosition")
            .expect("PTZPosition 快照必须存在");
        assert!(ptz.ptz_position.is_none());
    }

    #[test]
    fn mark_subscribed_then_cancel_switches_lifecycle() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.mark_subscribed("Catalog", "10.0.0.9:5060".into(), 3_600);
        let subscribed = sim.build_subscription_snapshot("Catalog", true, 0, None);
        assert_eq!(subscribed.lifecycle, "subscribed");
        assert_eq!(subscribed.expires_secs, 3_600);
        assert_eq!(subscribed.subscriber.as_deref(), Some("10.0.0.9:5060"));
        assert!(subscribed.remaining_secs > 0);

        sim.mark_unsubscribed("Catalog");
        let cancelled = sim.build_subscription_snapshot("Catalog", false, 0, None);
        // 退订后既不是"从未订阅"，也不该继续显示剩余时间。
        assert_eq!(cancelled.lifecycle, "cancelled");
        assert_eq!(cancelled.remaining_secs, 0);
        assert!(cancelled.subscriber.is_none());
    }

    #[test]
    fn notify_failure_shows_exception_and_recovery_clears_it() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.mark_subscribed("Alarm", "10.0.0.9:5060".into(), 3_600);
        sim.record_notify_outcome("Alarm", &Err(common::Error::Gb28181("平台无应答".into())));
        let failed = sim.build_subscription_snapshot("Alarm", false, 0, None);
        assert_eq!(failed.lifecycle, "exception");
        assert!(failed
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("平台无应答"));

        // 下一次成功必须清掉旧错误：否则界面会长期挂着已被修复的报错。
        sim.record_notify_outcome("Alarm", &Ok(200));
        let recovered = sim.build_subscription_snapshot("Alarm", true, 0, None);
        assert!(recovered.last_error.is_none());
        assert_eq!(recovered.last_notify_result.as_deref(), Some("200"));
        assert!(recovered.last_notify_at_ms.is_some());
    }

    #[test]
    fn untracked_event_kind_is_ignored() {
        // SIP 里可能冒出自定义 Event，别让它污染四类订阅的运行详情。
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.mark_subscribed("Presence", "10.0.0.9:5060".into(), 600);
        assert!(sim.subscription_meta.lock().unwrap().get("Presence").is_none());
    }

    pub(super) fn test_cfg(host: &str, port: u16) -> DeviceConfig {
        DeviceConfig {
            device_id: DeviceId::new("34020000001320000001").unwrap(),
            username: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: host.into(),
            server_port: port,
            server_id: "34020000002000000001".into(),
            server_domain: "3402000000".into(),
            transport: Transport::Udp,
            audio_transport: common::AudioTransport::Udp,
            register_expires_secs: 3_600,
            heartbeat_interval_secs: 60,
            heartbeat_fail_threshold: 3,
            channels: vec![],
            device_info: DeviceInfo {
                device_name: "Test Device".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1.0".into(),
                hardware_version: None,
                installation_address: None,
                device_type: None,
            },
            video_source: None,
            media_profile: None,
            video_fps: 30,
            light_bitrate_kbps: None,
            gb_version: common::GbVersion::V2022,
            signaling_encoding: common::SignalingEncoding::Gb18030,
        }
    }

    /// mock 平台:收到首个 REGISTER 回 401(带挑战),第二个回 200。
    async fn mock_platform_register(
        platform: Arc<SipTransport>,
        device_addr: SocketAddr,
        call_id: String,
        expected_expires: u32,
    ) {
        let mut prx = platform.register(call_id.clone());
        let expected_expires = expected_expires.to_string();
        let mut seen = 0;
        let mut first_via: Option<String> = None;
        let mut first_from: Option<String> = None;
        let mut first_cseq: Option<u32> = None;
        while let Some(inc) = prx.recv().await {
            if let SipMessage::Request(req) = inc.message {
                let cseq = req.headers.cseq().unwrap_or("1 REGISTER").to_string();
                let via = req
                    .headers
                    .get("Via")
                    .expect("REGISTER 缺少 Via")
                    .to_string();
                let from = req
                    .headers
                    .get("From")
                    .expect("REGISTER 缺少 From")
                    .to_string();
                let cseq_number = req.headers.cseq_number().expect("REGISTER 缺少 CSeq");
                assert_eq!(req.headers.get("Expires"), Some(expected_expires.as_str()));
                let mut h = Headers::new();
                h.set("Call-ID", call_id.clone());
                h.set("CSeq", cseq);
                let resp = if seen == 0 {
                    assert!(req.headers.get("Authorization").is_none());
                    assert_eq!(req.headers.get("X-GB-Ver"), Some("3.0"));
                    assert!(req.uri.starts_with("sip:34020000002000000001@"));
                    first_via = Some(via);
                    first_from = Some(from);
                    first_cseq = Some(cseq_number);
                    h.set(
                        "WWW-Authenticate",
                        r#"Digest realm="3402000000", nonce="abc123""#,
                    );
                    Response {
                        status: 401,
                        reason: "Unauthorized".into(),
                        headers: h,
                        body: Vec::new(),
                    }
                } else {
                    let authorization = req
                        .headers
                        .get("Authorization")
                        .expect("鉴权 REGISTER 缺少 Authorization");
                    assert!(authorization.contains(&format!("uri=\"{}\"", req.uri)));
                    assert_eq!(req.headers.get("X-GB-Ver"), Some("3.0"));
                    assert_eq!(req.headers.get("Call-ID"), Some(call_id.as_str()));
                    assert_eq!(first_from.as_deref(), Some(from.as_str()));
                    assert_ne!(first_via.as_deref(), Some(via.as_str()));
                    assert_eq!(Some(cseq_number), first_cseq.map(|value| value + 1));
                    Response {
                        status: 200,
                        reason: "OK".into(),
                        headers: h,
                        body: Vec::new(),
                    }
                };
                seen += 1;
                let _ = platform
                    .send_to(&SipMessage::Response(resp), device_addr)
                    .await;
                if seen == 2 {
                    return;
                }
            }
        }
    }

    #[tokio::test]
    async fn 注册闭环_401后带鉴权成功() {
        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", platform_addr.port()));
        let platform_task = tokio::spawn(mock_platform_register(
            platform_tp.clone(),
            device_addr,
            sim.call_id().to_string(),
            3_600,
        ));

        let state = sim
            .register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        assert_eq!(state, DeviceState::Registered);
        assert!(!sim.capture_ready().await, "未配置视频源时不应伪造采集就绪");
        platform_task.await.unwrap();
    }

    #[tokio::test]
    async fn 注册使用配置有效期() {
        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let mut config = test_cfg("127.0.0.1", platform_addr.port());
        config.register_expires_secs = 7_200;
        let sim = DeviceSimulator::new(config);
        let platform_task = tokio::spawn(mock_platform_register(
            platform_tp,
            device_addr,
            sim.call_id().to_string(),
            7_200,
        ));

        sim.register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        platform_task.await.unwrap();
    }

    #[test]
    fn 注册策略满足重试下限和续约比例() {
        assert_eq!(register_retry_base(), std::time::Duration::from_secs(60));
        assert_eq!(
            next_register_retry(std::time::Duration::from_secs(240)),
            std::time::Duration::from_secs(300)
        );
        assert_eq!(
            register_renew_after(7_200),
            std::time::Duration::from_secs(5_760)
        );
    }

    #[test]
    fn 心跳失败阈值按配置且成功会清零() {
        let mut failures = 0;
        for _ in 0..4 {
            assert!(!record_heartbeat_failure(&mut failures, 5));
        }
        assert!(record_heartbeat_failure(&mut failures, 5));
        failures = 0;
        assert!(!record_heartbeat_failure(&mut failures, 5));
        assert_eq!(failures, 1);
    }

    #[tokio::test]
    async fn 注册失败后的重试等待可被立即停止() {
        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let mut requests = platform_tp.register(sim.call_id().to_string());
        let platform_task = tokio::spawn(async move {
            let incoming = requests.recv().await.expect("应收到首次 REGISTER");
            let SipMessage::Request(request) = incoming.message else {
                panic!("应为 REGISTER 请求");
            };
            let mut headers = Headers::new();
            headers.set("Call-ID", request.headers.call_id().unwrap_or_default());
            headers.set("CSeq", request.headers.cseq().unwrap_or("1 REGISTER"));
            platform_tp
                .send_to(
                    &SipMessage::Response(Response {
                        status: 403,
                        reason: "Forbidden".into(),
                        headers,
                        body: Vec::new(),
                    }),
                    device_addr,
                )
                .await
                .unwrap();
        });

        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let run = tokio::spawn(sim.run(
            device_tp,
            "127.0.0.1".into(),
            device_addr.port(),
            async move {
                let _ = stop_rx.await;
            },
        ));
        platform_task.await.unwrap();
        stop_tx.send(()).unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(1), run)
            .await
            .expect("停止不应等待 60 秒重试计时")
            .unwrap();
    }

    #[tokio::test]
    async fn 注册成功后_先启动采集并等待首帧再报告就绪() {
        use common::{DeviceEvent, DeviceObserver};
        use std::sync::Mutex;

        struct Cap(Mutex<Vec<&'static str>>);
        impl DeviceObserver for Cap {
            fn on_event(&self, event: DeviceEvent) {
                let name = match event {
                    DeviceEvent::RegisterSuccess => "registered",
                    DeviceEvent::CaptureStarting => "capture-starting",
                    DeviceEvent::CaptureReady => "capture-ready",
                    _ => return,
                };
                self.0.lock().unwrap().push(name);
            }
        }

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();
        let mut cfg = test_cfg("127.0.0.1", platform_addr.port());
        cfg.light_bitrate_kbps = Some(128);
        let events = Arc::new(Cap(Mutex::new(Vec::new())));
        let sim = DeviceSimulator::with_observer(cfg, events.clone());
        let platform_task = tokio::spawn(mock_platform_register(
            platform_tp,
            device_addr,
            sim.call_id().to_string(),
            3_600,
        ));

        assert_eq!(
            sim.register(&device_tp, "127.0.0.1", device_addr.port())
                .await
                .unwrap(),
            DeviceState::Registered
        );
        assert!(sim.capture_ready().await);
        assert_eq!(
            events.0.lock().unwrap().as_slice(),
            &["registered", "capture-starting", "capture-ready"]
        );
        sim.stop_shared_media().await;
        platform_task.await.unwrap();
    }

    /// 平台 offer `a=setup:active`:设备在 200 OK 里写真实监听端口 + `setup:passive`,
    /// 平台连入后收到 RFC 4571 分帧的 RTP。
    #[tokio::test]
    async fn invite_tcp_被动_answer回填监听端口且平台连入后收到rtp() {
        use sip_core::{Method, Request};
        use tokio::io::AsyncReadExt;

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();
        let mut cfg = test_cfg("127.0.0.1", platform_addr.port());
        cfg.light_bitrate_kbps = Some(128);
        let sim = Arc::new(DeviceSimulator::with_observer(
            cfg,
            Arc::new(common::NoopObserver),
        ));
        let platform_task = tokio::spawn(mock_platform_register(
            platform_tp.clone(),
            device_addr,
            sim.call_id().to_string(),
            3_600,
        ));
        sim.register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        platform_task.await.unwrap();

        let sdp = "v=0\r\n\
o=34020000002000000001 0 0 IN IP4 127.0.0.1\r\n\
s=Play\r\n\
c=IN IP4 127.0.0.1\r\n\
t=0 0\r\n\
m=video 40000 TCP/RTP/AVP 96\r\n\
a=rtpmap:96 PS/90000\r\n\
a=recvonly\r\n\
a=setup:active\r\n\
a=connection:new\r\n\
y=0100000099\r\n";
        let mut h = Headers::new();
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKinvp");
        h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "inv-passive-1");
        h.set("CSeq", "1 INVITE");
        h.set("Content-Type", "application/sdp");
        let invite = Request {
            method: Method::Invite,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: sdp.as_bytes().to_vec(),
        };
        let mut presp = platform_tp.register("inv-passive-1");
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(invite),
            from: platform_addr,
        };
        assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

        // 跳过 100 Trying,取 200 OK 的 SDP。
        let answer = loop {
            let got = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
                .await
                .expect("超时未收到 INVITE 应答")
                .expect("应答通道关闭");
            if let SipMessage::Response(resp) = got.message {
                if resp.status == 200 {
                    break String::from_utf8(resp.body).unwrap();
                }
            }
        };
        assert!(answer.contains("a=setup:passive\r\n"), "answer: {answer}");
        let answer_sdp = sip_core::SessionDescription::parse(&answer).unwrap();
        let port = answer_sdp.media.port;
        assert_ne!(port, 9, "设备被动时必须写真实监听端口");

        let mut platform_media = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("平台连入设备监听端口失败");
        let mut len = [0u8; 2];
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            platform_media.read_exact(&mut len),
        )
        .await
        .expect("超时未收到 RTP")
        .unwrap();
        let mut pkt = vec![0u8; u16::from_be_bytes(len) as usize];
        platform_media.read_exact(&mut pkt).await.unwrap();
        assert_eq!(pkt[0], 0x80);
        assert_eq!(&pkt[8..12], &100_000_099u32.to_be_bytes());

        sim.stop_shared_media().await;
    }

    #[tokio::test]
    async fn options_回_200() {
        use sip_core::{Method, Request};

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 平台构造发往设备 AOR 的 OPTIONS,并注册接收响应的队列。
        let mut h = Headers::new();
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKopt");
        h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "opt-call-1");
        h.set("CSeq", "1 OPTIONS");
        let options = Request {
            method: Method::Options,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: Vec::new(),
        };
        let mut presp = platform_tp.register("opt-call-1");

        // 设备侧应答一条入站请求。
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(options),
            from: platform_addr,
        };
        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // 平台应收到 200 OK(Call-ID 回显)。
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match got.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert_eq!(r.headers.call_id(), Some("opt-call-1"));
            }
            _ => panic!("应为响应"),
        }
    }

    #[tokio::test]
    async fn 目录查询应答含通道() {
        use sip_core::Method;

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let mut cfg = test_cfg("127.0.0.1", platform_addr.port());
        cfg.channels.push(ChannelConfig {
            channel_id: DeviceId::new("34020000001320000132").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        });
        let sim = Arc::new(DeviceSimulator::new(cfg));

        let query_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Catalog</CmdType><SN>999</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;

        let mut h = Headers::new();
        h.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKcat");
        h.set("From", "<sip:34020000002000000001@3402>;tag=p2");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "cat-call-1");
        h.set("CSeq", "2 MESSAGE");
        h.set("Content-Type", "Application/MANSCDP+xml");
        let query_msg = sip_core::Request {
            method: Method::Message,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: query_xml.as_bytes().to_vec(),
        };
        let mut presp = platform_tp.register("cat-call-1");

        let incoming = sip_core::Incoming {
            message: SipMessage::Request(query_msg),
            from: platform_addr,
        };
        // 应答 MESSAGE 的 Request-URI 指向平台域,平台按 AOR 收独立应答。
        let mut preply = platform_tp.register_inbound("34020000002000000001");

        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // (1) 平台先收到对查询的空 200 OK(事务应答)。
        let ack = tokio::time::timeout(std::time::Duration::from_secs(2), presp.recv())
            .await
            .expect("超时")
            .expect("关闭");
        match ack.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.body.is_empty(), "查询 200 OK 应为空 body");
            }
            _ => panic!("应先收到 200 响应"),
        }

        // (2) 平台再收到独立的 Catalog 应答 MESSAGE。
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
            .await
            .expect("超时未收到应答 MESSAGE")
            .expect("关闭");
        match reply.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Message);
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<CmdType>Catalog</CmdType>"));
                assert!(body_str.contains("<SN>999</SN>"));
                assert!(body_str.contains("34020000001320000132")); // 通道 ID
                assert!(body_str.contains("<Name>Camera-1</Name>"));
            }
            _ => panic!("应为独立 MESSAGE 应答"),
        }
    }

    #[tokio::test]
    async fn catalog分片等待每片确认后串行发送() {
        use sip_core::Method;

        let device_tp = Arc::new(SipTransport::bind_udp("127.0.0.1:0").await.unwrap());
        let platform_tp = Arc::new(SipTransport::bind_udp("127.0.0.1:0").await.unwrap());
        let platform_addr = platform_tp.local_addr().unwrap();
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));
        sim.load_catalog_template("nvr-8ch");

        let query_xml = format!(
            "<?xml version=\"1.0\"?><Query><CmdType>Catalog</CmdType><SN>1000</SN><DeviceID>{}</DeviceID></Query>",
            sim.config().device_id
        );
        let mut headers = Headers::new();
        headers.set("Via", "SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bKserial");
        headers.set("From", "<sip:34020000002000000001@3402>;tag=platform");
        headers.set("To", format!("<sip:{}@3402>", sim.config().device_id));
        headers.set("Call-ID", "catalog-serial-query");
        headers.set("CSeq", "2 MESSAGE");
        headers.set("Content-Type", "Application/MANSCDP+xml");
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(sip_core::Request {
                method: Method::Message,
                uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
                headers,
                body: query_xml.into_bytes(),
            }),
            from: platform_addr,
        };

        let mut query_ack = platform_tp.register("catalog-serial-query");
        let mut responses = platform_tp.register_inbound("34020000002000000001");
        let answer_task = tokio::spawn({
            let sim = sim.clone();
            let device_tp = device_tp.clone();
            async move { sim.answer_inbound(&device_tp, &incoming).await }
        });

        let ack = tokio::time::timeout(std::time::Duration::from_secs(1), query_ack.recv())
            .await
            .expect("查询 SIP 200 超时")
            .expect("查询 SIP 200 队列关闭");
        assert!(matches!(ack.message, SipMessage::Response(ref r) if r.status == 200));

        let first = tokio::time::timeout(std::time::Duration::from_secs(1), responses.recv())
            .await
            .expect("首片 Catalog Response 超时")
            .expect("Catalog Response 队列关闭");
        let SipMessage::Request(first_request) = first.message else {
            panic!("应收到 Catalog Response MESSAGE");
        };
        assert_eq!(first_request.method, Method::Message);
        let first_body =
            builder::decode_xml_body(&first_request.body, sim.config().signaling_encoding);
        let mut item_count = first_body.matches("<Item>").count();
        let mut packet_count = 1usize;

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(80), responses.recv())
                .await
                .is_err(),
            "上一片未收到 200 OK 前不得发送下一片"
        );
        platform_tp
            .send_to(
                &SipMessage::Response(builder::response_ok(&first_request)),
                first.from,
            )
            .await
            .unwrap();

        while item_count < 11 {
            let packet = tokio::time::timeout(std::time::Duration::from_secs(1), responses.recv())
                .await
                .expect("后续 Catalog Response 超时")
                .expect("Catalog Response 队列关闭");
            let SipMessage::Request(request) = packet.message else {
                panic!("应收到 Catalog Response MESSAGE");
            };
            let body = builder::decode_xml_body(&request.body, sim.config().signaling_encoding);
            assert!(body.contains("<SN>1000</SN>"));
            assert!(body.contains("<SumNum>11</SumNum>"));
            item_count += body.matches("<Item>").count();
            packet_count += 1;
            platform_tp
                .send_to(
                    &SipMessage::Response(builder::response_ok(&request)),
                    packet.from,
                )
                .await
                .unwrap();
        }

        assert_eq!(item_count, 11);
        assert!(packet_count > 1);
        assert!(answer_task.await.unwrap().unwrap());
    }

    #[test]
    fn 目录应答按gb版本差异化字段() {
        use gb28181_protocol::manscdp::Query;

        let query = Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            interval: None,
            group_id: None,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
        };
        let ch = ChannelConfig {
            channel_id: DeviceId::new("35020000001310000132").unwrap(),
            name: "Camera-1".into(),
            status: "ON".into(),
        };

        // 2022:输出 SecurityLevelCode。
        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.channels.push(ch.clone());
        cfg.gb_version = common::GbVersion::V2022;
        let xml = DeviceSimulator::new(cfg).handle_query(&query).unwrap();
        assert!(
            xml.contains("<SecurityLevelCode>"),
            "2022 应含 SecurityLevelCode"
        );

        // 2016:不输出该 2022 新增字段。
        let mut cfg16 = test_cfg("127.0.0.1", 5060);
        cfg16.channels.push(ch);
        cfg16.gb_version = common::GbVersion::V2016;
        let xml16 = DeviceSimulator::new(cfg16).handle_query(&query).unwrap();
        assert!(
            !xml16.contains("SecurityLevelCode"),
            "2016 不应含 2022 新增字段"
        );
    }

    #[test]
    fn 回放range解析折算千分比() {
        // npt=1800- → 1800/3600 = 500‰。
        let p = DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=1800-\r\n");
        assert_eq!(p, Some(500));
        // npt=now- → 不定位。
        assert_eq!(
            DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nRange: npt=now-\r\n"),
            None
        );
        // 无 Range → None。
        assert_eq!(
            DeviceSimulator::parse_range_permille("PLAY MANSRTSP/1.0\r\nScale: 2.0\r\n"),
            None
        );
    }

    /// OSD 配置命令(国标 A.2.3.2.11):平台下发 DeviceConfig+OSDConfig,
    /// 设备回 DeviceConfig 应答 + 上报 OsdConfig 事件(供 UI 展示已应用)。
    #[tokio::test]
    async fn osd配置命令_应答并上报事件() {
        use common::{DeviceEvent, DeviceObserver};
        use std::sync::atomic::{AtomicBool, Ordering};
        // 捕获 OsdConfig 事件的观察者。
        struct Cap {
            got: Arc<AtomicBool>,
        }
        impl DeviceObserver for Cap {
            fn on_event(&self, e: DeviceEvent) {
                if let DeviceEvent::OsdConfig {
                    time_show,
                    osd_show,
                } = e
                {
                    assert!(time_show && osd_show);
                    self.got.store(true, Ordering::SeqCst);
                }
            }
        }
        let got = Arc::new(AtomicBool::new(false));
        let sim = DeviceSimulator::with_observer(
            test_cfg("127.0.0.1", 5060),
            Arc::new(Cap { got: got.clone() }),
        );
        let ctrl = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>1</SN><DeviceID>d</DeviceID>\
             <OSDConfig><TimeShowFlag>1</TimeShowFlag><OSDShowFlag>1</OSDShowFlag></OSDConfig></Control>",
        )
        .unwrap();
        let xml = sim.handle_control(&ctrl).await.unwrap();
        // 应答 CmdType=DeviceConfig(A.2.6.8),且事件已上报。
        assert!(xml.contains("<CmdType>DeviceConfig</CmdType>"));
        assert!(got.load(Ordering::SeqCst), "应上报 OsdConfig 事件");
    }

    /// 扩展查询(FR-17):AlarmStatus/HomePositionQuery/StorageCard/CruiseTrack*/PTZPreciseStatus。
    #[test]
    fn 扩展查询应答_各cmdtype() {
        let mk = |cmd: &str, group: Option<u32>| gb28181_protocol::manscdp::Query {
            cmd_type: cmd.into(),
            sn: 1,
            device_id: "34020000001320000001".into(),
            interval: None,
            group_id: group,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
        };
        let mut cfg = test_cfg("127.0.0.1", 5060);
        cfg.channels.push(ChannelConfig {
            channel_id: DeviceId::new("34020000001320000001").unwrap(),
            name: "Ch".into(),
            status: "ON".into(),
        });
        let sim = DeviceSimulator::new(cfg);

        // 预置一条巡航轨迹 1 = [1,3],并置布防态,便于验证读回。
        {
            let mut s = sim.control_state.lock().unwrap();
            s.alarming = true;
            s.home_set = true;
            s.cruise_tracks.insert(1, vec![1, 3]);
        }

        let alarm = sim.handle_query(&mk("AlarmStatus", None)).unwrap();
        assert!(alarm.contains("<CmdType>AlarmStatus</CmdType>"));
        assert!(alarm.contains("<DutyStatus>ALARM</DutyStatus>"));

        let home = sim.handle_query(&mk("HomePositionQuery", None)).unwrap();
        assert!(home.contains("<ResetTime>30</ResetTime>"));
        assert!(home.contains("<PresetIndex>1</PresetIndex>"));

        let sd = sim.handle_query(&mk("SDCardStatus", None)).unwrap();
        assert!(sd.contains("<TotalCapacity>32768</TotalCapacity>"));

        let list = sim.handle_query(&mk("CruiseTrackListQuery", None)).unwrap();
        assert!(list.contains("<TrackList Num=\"1\">"));
        assert!(list.contains("<Name>巡航 1</Name>"));

        let detail = sim.handle_query(&mk("CruiseTrackQuery", Some(1))).unwrap();
        assert!(detail.contains("<GroupID>1</GroupID>"));
        assert!(detail.contains("<PresetID>3</PresetID>"));

        let precise = sim.handle_query(&mk("PTZPosition", None)).unwrap();
        assert!(precise.contains("<Zoom>1.00</Zoom>"));
    }

    #[test]
    fn ssrf_拒绝受限地址() {
        assert!(reject_ssrf("127.0.0.1").is_err());
        assert!(reject_ssrf("169.254.1.1").is_err());
        assert!(reject_ssrf("192.168.1.10").is_err());
        assert!(reject_ssrf("10.0.0.1").is_err());
        assert!(reject_ssrf("224.0.0.1").is_err());
        // 公网 IP 与域名放行(受控内网场景)。
        assert!(reject_ssrf("203.0.113.9").is_ok());
        assert!(reject_ssrf("upload.example.com").is_ok());
    }

    #[tokio::test]
    async fn 抓拍上传_协议格式与2xx判定() {
        // 起一个本地 TCP 服务模拟采集端(回 200),用 127.0.0.1 直连绕过 SSRF(仅测协议)。
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            req
        });
        // http_put_jpeg 对 127.0.0.1 会被 SSRF 拒绝(安全设计),此处直接建连验证 PUT 报文格式与 2xx。
        let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
        let head = format!(
            "PUT /snap.jpg HTTP/1.1\r\nHost: {addr}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            jpeg.len()
        );
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(head.as_bytes()).await.unwrap();
        c.write_all(jpeg).await.unwrap();
        let mut resp = Vec::new();
        let _ = c.read_to_end(&mut resp).await;
        assert!(String::from_utf8_lossy(&resp).contains("200"));

        let req = srv.await.unwrap();
        assert!(req.starts_with("PUT /snap.jpg HTTP/1.1"));
        assert!(req.contains("Content-Type: image/jpeg"));

        // 且 http_put_jpeg 对环回目标必须拒绝(SSRF 防护生效)。
        assert!(http_put_jpeg(&format!("http://{addr}/x.jpg"), jpeg)
            .await
            .is_err());
    }

    /// 多通道目录(FR-34):载入模板后 Catalog 查询枚举全部节点,含层级 ParentID/Parental。
    #[test]
    fn 多通道目录_模板与crud() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.load_catalog_template("nvr-8ch");
        let q = gb28181_protocol::manscdp::Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: sim.config().device_id.to_string(),
            interval: None,
            group_id: None,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
        };
        let packets = sim.handle_query_packets(&q).unwrap();
        let xml = packets.join("");
        // 查询设备编码返回设备本身及其后代:根 + 业务分组 + 虚拟组织 + 8 视频通道。
        assert_eq!(xml.matches("<Item>").count(), 11);
        assert!(xml.contains("<Name>NVR-8</Name>"));
        assert!(xml.contains("<Name>Uvp-Camera-1</Name>"));
        assert!(xml.contains("<Parental>1</Parental>")); // 业务分组
        assert!(xml.contains("<Parental>0</Parental>")); // 视频通道

        // CRUD:新增一个通道 → 树 +1;删除 → 复原。
        let node = gb28181_protocol::id_codec::CatalogNode::new(
            "34020000001320000099",
            gb28181_protocol::id_codec::CatalogNodeType::VideoChannel,
            "新增通道",
            sim.config().device_id.to_string(),
        );
        assert!(sim.upsert_channel(node.clone()).unwrap());
        assert!(!sim.upsert_channel(node).unwrap()); // 再次 upsert 为更新非新增
        assert_eq!(sim.remove_channel("34020000001320000099").unwrap(), 1);
        assert_eq!(sim.remove_channel("34020000001320000099").unwrap(), 0); // 已删
    }

    #[test]
    fn catalog查询按目标子树应答并记录活动() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.load_catalog_template("nvr-8ch");
        let group = sim
            .catalog_tree()
            .into_iter()
            .find(|node| {
                node.node_type == gb28181_protocol::id_codec::CatalogNodeType::BusinessGroup
            })
            .unwrap();
        let q = gb28181_protocol::manscdp::Query {
            cmd_type: "Catalog".into(),
            sn: 88,
            device_id: group.id.clone(),
            interval: None,
            group_id: None,
            config_type: None,
            start_time: None,
            end_time: None,
            record_type: None,
            indistinct_query: None,
            stream_number: None,
        };
        let packets = sim.handle_query_packets(&q).unwrap();
        assert!(packets.len() > 1);
        assert!(
            packets
                .iter()
                .all(|xml| xml.contains("<SumNum>10</SumNum>"))
        );
        assert_eq!(
            packets
                .iter()
                .map(|xml| xml.matches("<Item>").count())
                .sum::<usize>(),
            10
        );
        assert!(
            packets
                .iter()
                .all(|xml| !xml.contains(&sim.config().device_info.device_name))
        );
        let activity = sim.catalog_activity().unwrap();
        assert_eq!(activity.target_id, group.id);
        assert_eq!(activity.result_count, 10);
        assert_eq!(activity.packet_count, packets.len());
    }

    #[test]
    fn 下级平台业务目录查询在两版本映射分组与父节点() {
        use gb28181_protocol::id_codec::CatalogNodeType;

        for version in [common::GbVersion::V2016, common::GbVersion::V2022] {
            let mut cfg = test_cfg("127.0.0.1", 5060);
            cfg.gb_version = version;
            let sim = DeviceSimulator::new(cfg);
            sim.load_catalog_template("business-platform");
            let tree = sim.catalog_tree();
            let root_id = sim.config().device_id.to_string();
            let group = tree
                .iter()
                .find(|node| node.node_type == CatalogNodeType::BusinessGroup)
                .unwrap();
            let top_org = tree
                .iter()
                .find(|node| {
                    node.node_type == CatalogNodeType::VirtualOrg && node.parent_id == group.id
                })
                .unwrap();
            let nested_org = tree
                .iter()
                .find(|node| {
                    node.node_type == CatalogNodeType::VirtualOrg && node.parent_id == top_org.id
                })
                .unwrap();
            let channel = tree
                .iter()
                .find(|node| {
                    node.node_type == CatalogNodeType::VideoChannel
                        && node.parent_id == nested_org.id
                })
                .unwrap();
            let query = gb28181_protocol::manscdp::Query {
                cmd_type: "Catalog".into(),
                sn: 117,
                device_id: root_id.clone(),
                interval: None,
                group_id: None,
                config_type: None,
                start_time: None,
                end_time: None,
                record_type: None,
                indistinct_query: None,
                stream_number: None,
            };
            let xml = sim.handle_query_packets(&query).unwrap().join("");
            let item_for = |id: &str| {
                xml.split("<Item>")
                    .skip(1)
                    .find(|item| item.contains(&format!("<DeviceID>{id}</DeviceID>")))
                    .unwrap()
                    .split("</Item>")
                    .next()
                    .unwrap()
            };
            let group_item = item_for(&group.id);
            let top_org_item = item_for(&top_org.id);
            let nested_org_item = item_for(&nested_org.id);
            let channel_item = item_for(&channel.id);

            assert!(group_item.contains(&format!("<ParentID>{root_id}</ParentID>")));
            assert!(!top_org_item.contains("<ParentID>"));
            assert!(
                top_org_item.contains(&format!("<BusinessGroupID>{}</BusinessGroupID>", group.id))
            );
            assert!(nested_org_item.contains(&format!("<ParentID>{}</ParentID>", top_org.id)));
            assert!(
                nested_org_item
                    .contains(&format!("<BusinessGroupID>{}</BusinessGroupID>", group.id))
            );
            assert!(
                channel_item.contains(&format!("<ParentID>{root_id}/{}</ParentID>", nested_org.id))
            );
            assert!(
                channel_item.contains(&format!("<BusinessGroupID>{}</BusinessGroupID>", group.id))
            );
            if version == common::GbVersion::V2016 {
                assert!(!channel_item.contains("<SecurityLevelCode>"));
            }
        }
    }

    #[test]
    fn 下级平台业务目录全量通知使用与查询相同的目录映射() {
        use gb28181_protocol::id_codec::CatalogNodeType;

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.load_catalog_template("business-platform");
        let tree = sim.catalog_tree();
        let root_id = sim.config().device_id.to_string();
        let group = tree
            .iter()
            .find(|node| node.node_type == CatalogNodeType::BusinessGroup)
            .unwrap();
        let top_org = tree
            .iter()
            .find(|node| {
                node.node_type == CatalogNodeType::VirtualOrg && node.parent_id == group.id
            })
            .unwrap();
        let nested_org = tree
            .iter()
            .find(|node| {
                node.node_type == CatalogNodeType::VirtualOrg && node.parent_id == top_org.id
            })
            .unwrap();
        let channel = tree
            .iter()
            .find(|node| {
                node.node_type == CatalogNodeType::VideoChannel && node.parent_id == nested_org.id
            })
            .unwrap();
        let publishable = sim.catalog_publishable();
        let wire_node = |id: &str| publishable.iter().find(|node| node.id == id).unwrap();

        assert_eq!(wire_node(&top_org.id).parent_id, "");
        assert_eq!(
            wire_node(&top_org.id).business_group_id.as_deref(),
            Some(group.id.as_str())
        );
        assert_eq!(
            wire_node(&channel.id).parent_id,
            format!("{root_id}/{}", nested_org.id)
        );
        assert_eq!(
            wire_node(&channel.id).business_group_id.as_deref(),
            Some(group.id.as_str())
        );

        let snapshot = gb28181_protocol::manscdp::CatalogSnapshot::from_nodes(publishable);
        let changes = gb28181_protocol::manscdp::CatalogSnapshot::from_nodes(Vec::<
            gb28181_protocol::id_codec::CatalogNode,
        >::new())
        .diff(&snapshot);
        let notify_xml = gb28181_protocol::manscdp::CatalogNotify::new(&root_id, 118, changes)
            .to_xml()
            .unwrap();
        assert!(!notify_xml.contains(&format!(
            "<DeviceID>{}</DeviceID><Name>{}</Name><CivilCode>",
            top_org.id, top_org.name
        )));
        assert!(notify_xml.contains(&format!("<ParentID>{root_id}/{}</ParentID>", nested_org.id)));
        assert!(notify_xml.contains(&format!("<BusinessGroupID>{}</BusinessGroupID>", group.id)));
    }

    #[test]
    fn catalog_udp响应按编码后字节预算分片() {
        const MAX_BODY_BYTES: usize = 1000;
        const MAX_UDP_PAYLOAD_BYTES: usize = 1200;

        for encoding in [
            common::SignalingEncoding::Gb18030,
            common::SignalingEncoding::Utf8,
        ] {
            let mut cfg = test_cfg("127.0.0.1", 5060);
            cfg.signaling_encoding = encoding;
            let sim = DeviceSimulator::new(cfg);
            sim.load_catalog_template("nvr-8ch");
            let q = gb28181_protocol::manscdp::Query {
                cmd_type: "Catalog".into(),
                sn: 99,
                device_id: sim.config().device_id.to_string(),
                interval: None,
                group_id: None,
                config_type: None,
                start_time: None,
                end_time: None,
                record_type: None,
                indistinct_query: None,
                stream_number: None,
            };

            let packets = sim.handle_query_packets(&q).unwrap();
            assert!(packets.len() > 1, "11 个目录项不应继续塞进一个 UDP MESSAGE");

            let mut item_count = 0usize;
            for xml in &packets {
                let encoded = builder::encode_xml_body(xml, sim.config().signaling_encoding);
                assert!(
                    encoded.len() <= MAX_BODY_BYTES,
                    "Catalog body {}B 超过 {}B 预算",
                    encoded.len(),
                    MAX_BODY_BYTES
                );
                assert!(xml.contains("<SN>99</SN>"));
                assert!(xml.contains("<SumNum>11</SumNum>"));
                item_count += xml.matches("<Item>").count();

                let request = builder::message_xml(
                    sim.config(),
                    &DialogIds::new(),
                    1,
                    "127.0.0.1",
                    5060,
                    xml,
                );
                let datagram = sip_core::SipMessage::Request(request).to_bytes();
                assert!(
                    datagram.len() <= MAX_UDP_PAYLOAD_BYTES,
                    "完整 SIP UDP payload {}B 超过 {}B",
                    datagram.len(),
                    MAX_UDP_PAYLOAD_BYTES
                );
            }
            assert_eq!(item_count, 11);

            let activity = sim.catalog_activity().unwrap();
            assert_eq!(activity.result_count, 11);
            assert_eq!(activity.packet_count, packets.len());
        }
    }

    #[test]
    fn catalog单项超过udp预算时拒绝发送() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let item = gb28181_protocol::manscdp::CatalogItem {
            device_id: "34020000001320000099".into(),
            name: "超长目录项".repeat(300),
            manufacturer: None,
            model: None,
            civil_code: None,
            address: None,
            parental: Some(0),
            parent_id: Some(sim.config().device_id.to_string()),
            business_group_id: None,
            status: "ON".into(),
            security_level_code: Some("A".into()),
            ip_address: None,
            port: None,
        };
        let error = sim
            .catalog_responses_by_udp_budget(
                sim.config().device_id.as_str(),
                1,
                vec![item],
                CATALOG_UDP_BODY_MAX_BYTES,
                CATALOG_UDP_PAYLOAD_MAX_BYTES,
            )
            .unwrap_err();
        assert!(error.to_string().contains("34020000001320000099"));
        assert!(error.to_string().contains("超过 UDP 预算"));
    }

    #[test]
    fn catalog_notify同样按完整udp报文预算分片() {
        use gb28181_protocol::manscdp::CatalogNotifyItem;

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.load_catalog_template("nvr-8ch");
        let items: Vec<CatalogNotifyItem> = sim
            .catalog_publishable()
            .iter()
            .map(|node| CatalogNotifyItem {
                device_id: node.id.clone(),
                name: node.name.clone(),
                civil_code: node.civil_code.clone(),
                parental: Some(node.node_type.parental()),
                parent_id: (!node.parent_id.is_empty()).then(|| node.parent_id.clone()),
                business_group_id: node.business_group_id.clone(),
                event: "ON".into(),
                status: node.status.clone(),
            })
            .collect();
        let total = items.len();
        let dialog = builder::NotifyDialog {
            call_id: "catalog-subscription-call".into(),
            local_tag: "device-tag".into(),
            remote_from: "<sip:34020000002000000001@3402>;tag=platform-tag".into(),
            event: "Catalog".into(),
            expires: 3600,
        };

        let pages = sim
            .catalog_notifies_by_udp_budget(
                77,
                items,
                &dialog,
                CATALOG_UDP_BODY_MAX_BYTES,
                CATALOG_UDP_PAYLOAD_MAX_BYTES,
            )
            .unwrap();
        assert!(pages.len() > 1);
        assert_eq!(
            pages
                .iter()
                .map(|page| page.device_list.items.len())
                .sum::<usize>(),
            total
        );
        for page in pages {
            assert_eq!(page.sn, 77);
            assert_eq!(page.sum_num as usize, total);
            let xml = page.to_xml().unwrap();
            let body = builder::encode_xml_body(&xml, sim.config().signaling_encoding);
            assert!(body.len() <= CATALOG_UDP_BODY_MAX_BYTES);
            let request =
                builder::notify_in_dialog(sim.config(), &dialog, 1, "127.0.0.1", 5060, &xml);
            assert!(
                sip_core::SipMessage::Request(request).to_bytes().len()
                    <= CATALOG_UDP_PAYLOAD_MAX_BYTES
            );
        }
    }

    /// 语音广播(FR-36):TargetID 属本设备回 Broadcast Response OK。
    #[tokio::test]
    async fn 语音广播_目标匹配回ok() {
        use sip_core::{Headers, Method, Request, SipMessage};
        let device_tp = std::sync::Arc::new(SipTransport::bind_udp("127.0.0.1:0").await.unwrap());
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let sim = std::sync::Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let body = format!(
            "<?xml version=\"1.0\"?><Notify><CmdType>Broadcast</CmdType><SN>1</SN>\
             <SourceID>34020000002000000001</SourceID><TargetID>{}</TargetID></Notify>",
            sim.config().device_id
        );
        let mut h = Headers::new();
        h.set("From", "<sip:34020000002000000001@3402>;tag=p1");
        h.set("To", "<sip:34020000001320000001@3402>");
        h.set("Call-ID", "bc-call-1");
        h.set("CSeq", "1 MESSAGE");
        h.set("Content-Type", "Application/MANSCDP+xml");
        let msg = Request {
            method: Method::Message,
            uri: format!("sip:{}@127.0.0.1", sim.config().device_id),
            headers: h,
            body: body.into_bytes(),
        };
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(msg),
            from: platform_addr,
        };
        // 广播 Response 走独立 MESSAGE,平台按 AOR 收。
        let mut preply = platform_tp.register_inbound("34020000002000000001");
        sim.answer_inbound(&device_tp, &incoming).await.unwrap();

        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), preply.recv())
            .await
            .expect("超时未收到广播应答")
            .expect("关闭");
        match reply.message {
            SipMessage::Request(r) => {
                let s = std::str::from_utf8(&r.body).unwrap();
                assert!(s.contains("<CmdType>Broadcast</CmdType>"));
                assert!(s.contains("<Result>OK</Result>"));
            }
            _ => panic!("应为独立 MESSAGE"),
        }
    }

    /// 扩展控制(FR-18):巡航增点→列表查询可见;精确云台→精准状态查询读回;布防→报警状态。
    #[tokio::test]
    async fn 扩展控制_更新状态供查询读回() {
        use gb28181_protocol::manscdp::Control;
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));

        // 巡航增点:轨迹 1 加预置 5(byte3=0x84 track=1 preset=5)。
        let cruise = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 1,
            ptz_cmd: Some("A50F0184010500".into()),
            ..Default::default()
        };
        sim.handle_control(&cruise).await.unwrap();
        assert!(sim.control_state.lock().unwrap().cruise_tracks[&1].contains(&5));

        // 精确云台:pan=90 tilt=10 zoom=2。
        let precise = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 2,
            ptz_precise_ctrl: Some(gb28181_protocol::manscdp::PtzPreciseCtrl {
                pan: 90.0,
                tilt: 10.0,
                zoom: 2.0,
            }),
            ..Default::default()
        };
        sim.handle_control(&precise).await.unwrap();
        assert_eq!(
            sim.control_state.lock().unwrap().precise_pose,
            (90.0, 10.0, 2.0)
        );

        // 布防只更新 guarded，不冒充报警。
        let guard = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 3,
            guard_cmd: Some("SetGuard".into()),
            ..Default::default()
        };
        sim.handle_control(&guard).await.unwrap();
        let state = sim.control_state.lock().unwrap();
        assert!(state.guarded);
        assert!(!state.alarming);
    }

    #[tokio::test]
    async fn 移动位置订阅_回200并周期上报位置notify() {
        use sip_core::{Method, Request};

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        // 设备平台地址指向 mock 平台;server_domain 为平台 AOR。
        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 平台构造 MobilePosition 订阅(Interval=1)发往设备 AOR。
        let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>MobilePosition</CmdType><SN>7</SN><DeviceID>34020000001320000001</DeviceID><Interval>1</Interval></Query>"#;
        let mut sub_headers = Headers::new();
        sub_headers.set("Call-ID", "sub-call-1");
        sub_headers.set("CSeq", "1 SUBSCRIBE");
        sub_headers.set("Event", "presence");
        let sub_req = Request {
            method: Method::Subscribe,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: sub_headers,
            body: sub_xml.as_bytes().to_vec(),
        };
        // 订阅的 200 OK 回给 incoming.from,平台按 Call-ID 收。
        let mut psub = platform_tp.register("sub-call-1");
        // 位置 NOTIFY(MESSAGE)Request-URI 指向平台域,平台按 AOR 收。
        let mut pnotify = platform_tp.register_inbound("34020000002000000001");

        let incoming = sip_core::Incoming {
            message: SipMessage::Request(sub_req),
            from: platform_addr,
        };
        let answered = sim.answer_inbound(&device_tp, &incoming).await.unwrap();
        assert!(answered);

        // (1) 平台先收到 SUBSCRIBE 的 200 OK。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        match ok.message {
            SipMessage::Response(r) => assert_eq!(r.status, 200),
            _ => panic!("应先收到 200 响应"),
        }

        // (2) 周期任务的首个 tick 立即触发,平台应收到一条 MobilePosition NOTIFY。
        let notify = tokio::time::timeout(std::time::Duration::from_secs(3), pnotify.recv())
            .await
            .expect("超时未收到位置 NOTIFY")
            .expect("关闭");
        match notify.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Message);
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<CmdType>MobilePosition</CmdType>"));
            }
            _ => panic!("应为 MobilePosition NOTIFY MESSAGE"),
        }

        // 订阅表应登记该订阅。
        assert!(sim
            .subscriptions
            .lock()
            .await
            .contains_key("MobilePosition"));
        let snapshot = sim.subscription_snapshot().await;
        let mobile = snapshot
            .iter()
            .find(|item| item.kind == "MobilePosition")
            .expect("位置订阅快照应存在");
        assert!(mobile.active);

        // 停止订阅,任务应退出(不再 panic/泄漏)。
        sim.stop_subscriptions().await;
        assert!(sim.subscriptions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn 报警订阅_回200并记录对话供对话内notify() {
        use sip_core::{Method, Request};

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>Alarm</CmdType><SN>9</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;
        let mut h = Headers::new();
        h.set("Call-ID", "alarm-sub-1");
        h.set("CSeq", "1 SUBSCRIBE");
        h.set(
            "From",
            "<sip:34020000002000000001@34020000002000000001>;tag=plat7",
        );
        h.set("To", "<sip:34020000001320000001@34020000002000000001>");
        h.set("Event", "Alarm");
        h.set("Expires", "3600");
        let sub_req = Request {
            method: Method::Subscribe,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: sub_xml.as_bytes().to_vec(),
        };
        let mut psub = platform_tp.register("alarm-sub-1");

        let incoming = sip_core::Incoming {
            message: SipMessage::Request(sub_req),
            from: platform_addr,
        };
        assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());

        // 平台收到订阅 200(To 带本端 tag)。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), psub.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        match ok.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.headers.get("To").unwrap().contains("tag="));
            }
            _ => panic!("应先收到 200"),
        }

        // 报警订阅对话已记录(后续 report_alarm 将走对话内 NOTIFY)。
        let stored = sim.alarm_dialog.lock().await.clone();
        let (dialog, dst) = stored.expect("应记录报警订阅对话");
        assert_eq!(dialog.call_id, "alarm-sub-1");
        assert_eq!(dialog.event, "Alarm");
        assert_eq!(dialog.expires, 3600);
        assert!(dialog.remote_from.contains("tag=plat7"));
        assert_eq!(dst, platform_addr);
        let snapshot = sim.subscription_snapshot().await;
        let alarm = snapshot
            .iter()
            .find(|item| item.kind == "Alarm")
            .expect("报警订阅快照应存在");
        assert!(alarm.active);

        // 下线清理:对话应被清除。
        sim.stop_subscriptions().await;
        assert!(sim.alarm_dialog.lock().await.is_none());
    }

    /// 平台侧对设备发来的请求回 200,结束设备的事务并抑制重传。
    async fn ack_platform_request(platform: &Arc<SipTransport>, incoming: &sip_core::Incoming) {
        let SipMessage::Request(req) = &incoming.message else {
            return;
        };
        let mut h = Headers::new();
        h.set("Call-ID", req.headers.call_id().unwrap_or_default().to_string());
        h.set("CSeq", req.headers.cseq().unwrap_or_default().to_string());
        let resp = SipMessage::Response(Response {
            status: 200,
            reason: "OK".into(),
            headers: h,
            body: Vec::new(),
        });
        let _ = platform.send_to(&resp, incoming.from).await;
    }

    /// 平台下发 PTZPosition 事件订阅(GB-2022 事件型语义,**不带 Interval**)。
    async fn subscribe_ptz_position(
        sim: &Arc<DeviceSimulator>,
        device_tp: &Arc<SipTransport>,
        platform_addr: SocketAddr,
        call_id: &str,
    ) {
        use sip_core::{Method, Request};

        let sub_xml = r#"<?xml version="1.0"?>
<Query><CmdType>PTZPosition</CmdType><SN>1</SN><DeviceID>34020000001320000001</DeviceID></Query>"#;
        let mut h = Headers::new();
        h.set("Call-ID", call_id);
        h.set("CSeq", "1 SUBSCRIBE");
        h.set("From", "<sip:34020000002000000001@34020000002000000001>;tag=plat9");
        h.set("To", "<sip:34020000001320000001@34020000002000000001>");
        h.set("Event", "PTZPosition");
        h.set("Expires", "3600");
        let sub_req = Request {
            method: Method::Subscribe,
            uri: "sip:34020000001320000001@127.0.0.1".into(),
            headers: h,
            body: sub_xml.as_bytes().to_vec(),
        };
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(sub_req),
            from: platform_addr,
        };
        assert!(sim.answer_inbound(device_tp, &incoming).await.unwrap());
    }

    #[tokio::test]
    async fn ptz位置订阅_回200记录对话且不启动周期上报() {
        use sip_core::Method;

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 对话内 NOTIFY 沿用订阅 Call-ID,平台按同一 Call-ID 收(200 响应与入站 NOTIFY 同路由)。
        let mut pdlg = platform_tp.register("ptz-sub-1");
        subscribe_ptz_position(&sim, &device_tp, platform_addr, "ptz-sub-1").await;

        // (1) 订阅 200。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), pdlg.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        match ok.message {
            SipMessage::Response(r) => {
                assert_eq!(r.status, 200);
                assert!(r.headers.get("To").unwrap().contains("tag="));
            }
            _ => panic!("应先收到 200"),
        }

        // 对话已登记供后续变化时上报。
        let stored = sim.ptz_dialog.lock().await.clone();
        let (dialog, dst) = stored.expect("应记录 PTZ 订阅对话");
        assert_eq!(dialog.call_id, "ptz-sub-1");
        assert_eq!(dialog.event, "PTZPosition");
        assert_eq!(dialog.expires, 3600);
        assert!(dialog.remote_from.contains("tag=plat9"));
        assert_eq!(dst, platform_addr);
        let snapshot = sim.subscription_snapshot().await;
        let ptz = snapshot
            .iter()
            .find(|item| item.kind == "PTZPosition")
            .expect("PTZ 订阅快照应存在");
        assert!(ptz.active);

        // (2) 事件型订阅建立后补推一次当前姿态:必须是 NOTIFY + <Notify> 根元素,
        //     而不是旧的 MESSAGE 周期上报(后者会被平台当查询应答丢弃)。
        let notify = tokio::time::timeout(std::time::Duration::from_secs(2), pdlg.recv())
            .await
            .expect("超时未收到初始姿态 NOTIFY")
            .expect("关闭");
        match &notify.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Notify);
                assert_eq!(r.headers.call_id(), Some("ptz-sub-1"));
                assert_eq!(r.headers.get("Event"), Some("PTZPosition"));
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<CmdType>PTZPosition</CmdType>"));
                assert!(body_str.contains("<Pan>0.00</Pan>"));
                assert!(!body_str.contains("<Interval>"));
            }
            _ => panic!("应为对话内 NOTIFY 请求"),
        }
        ack_platform_request(&platform_tp, &notify).await;

        // (3) 事件型订阅不占周期任务:订阅表里不应有 PTZPosition 周期任务。
        assert!(!sim.subscriptions.lock().await.contains_key("PTZPosition"));
        // 短时间内不应再收到第二条(证明没有 5 秒周期上报)。
        let extra = tokio::time::timeout(std::time::Duration::from_millis(800), pdlg.recv()).await;
        assert!(extra.is_err(), "事件型订阅不应周期上报");

        // 下线清理:对话应被清除。
        sim.stop_subscriptions().await;
        assert!(sim.ptz_dialog.lock().await.is_none());
    }

    #[tokio::test]
    async fn ptz姿态变化_发对话内notify_未变化不发() {
        use sip_core::{Method, Request};

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        let mut pdlg = platform_tp.register("ptz-sub-2");
        subscribe_ptz_position(&sim, &device_tp, platform_addr, "ptz-sub-2").await;

        // 订阅 200 + 初始姿态 NOTIFY,均消费掉(并把初始 NOTIFY 应答掉以免重传)。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), pdlg.recv())
            .await
            .expect("超时未收到订阅 200")
            .expect("关闭");
        assert!(matches!(ok.message, SipMessage::Response(_)));
        let initial = tokio::time::timeout(std::time::Duration::from_secs(2), pdlg.recv())
            .await
            .expect("超时未收到初始姿态 NOTIFY")
            .expect("关闭");
        ack_platform_request(&platform_tp, &initial).await;

        // 平台下发精确云台控制(改姿态):pan=90 tilt=10 zoom=2。
        let control_xml = r#"<?xml version="1.0"?>
<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>34020000001320000001</DeviceID><PTZPreciseCtrl><Pan>90.00</Pan><Tilt>10.00</Tilt><Zoom>2.00</Zoom></PTZPreciseCtrl></Control>"#;
        let mut h = Headers::new();
        h.set("Call-ID", "ctl-msg-1");
        h.set("CSeq", "2 MESSAGE");
        h.set(
            "From",
            "<sip:34020000002000000001@34020000002000000001>;tag=plat9",
        );
        let incoming = sip_core::Incoming {
            message: SipMessage::Request(Request {
                method: Method::Message,
                uri: "sip:34020000001320000001@127.0.0.1".into(),
                headers: h,
                body: control_xml.as_bytes().to_vec(),
            }),
            from: platform_addr,
        };
        assert!(sim.answer_inbound(&device_tp, &incoming).await.unwrap());
        assert_eq!(
            sim.control_state.lock().unwrap().precise_pose,
            (90.0, 10.0, 2.0)
        );

        // 姿态确实变了 ⇒ 应发一条对话内 NOTIFY,携带新姿态。
        let changed = tokio::time::timeout(std::time::Duration::from_secs(2), pdlg.recv())
            .await
            .expect("姿态变化后超时未收到 NOTIFY")
            .expect("关闭");
        match &changed.message {
            SipMessage::Request(r) => {
                assert_eq!(r.method, Method::Notify);
                assert_eq!(r.headers.call_id(), Some("ptz-sub-2"));
                let body_str = std::str::from_utf8(&r.body).unwrap();
                assert!(body_str.contains("<Pan>90.00</Pan>"));
                assert!(body_str.contains("<Tilt>10.00</Tilt>"));
                assert!(body_str.contains("<Zoom>2.00</Zoom>"));
            }
            _ => panic!("应为对话内 NOTIFY 请求"),
        }
        ack_platform_request(&platform_tp, &changed).await;

        // 同一姿态再下发一次:未变化 ⇒ 不应再上报(事件型语义)。
        let mut h2 = Headers::new();
        h2.set("Call-ID", "ctl-msg-2");
        h2.set("CSeq", "3 MESSAGE");
        h2.set("From", "<sip:34020000002000000001@34020000002000000001>;tag=plat9");
        let incoming2 = sip_core::Incoming {
            message: SipMessage::Request(Request {
                method: Method::Message,
                uri: "sip:34020000001320000001@127.0.0.1".into(),
                headers: h2,
                body: control_xml.as_bytes().to_vec(),
            }),
            from: platform_addr,
        };
        assert!(sim.answer_inbound(&device_tp, &incoming2).await.unwrap());
        let repeat = tokio::time::timeout(std::time::Duration::from_millis(800), pdlg.recv()).await;
        assert!(repeat.is_err(), "姿态未变化不应上报 NOTIFY");
    }

    #[tokio::test]
    async fn info回放控制_调速与暂停恢复并回200() {
        use sip_core::{Method, Request};

        let device_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_tp = SipTransport::bind_udp("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();

        let sim = Arc::new(DeviceSimulator::new(test_cfg(
            "127.0.0.1",
            platform_addr.port(),
        )));

        // 注入一个活跃会话(不真正推流:任务只等待停止信号)。
        let control = media_rtp::PlaybackControl::new();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _ = stop_rx.await;
        });
        *sim.session.lock().await = Some(PushSession {
            marker: Arc::new(()),
            _task: task,
            stop_tx,
            control: control.clone(),
            is_history: false,
            history_duration_secs: 3_600,
        });

        // 构造 INFO 请求的辅助闭包。
        let make_info = |body: &str| {
            let mut h = Headers::new();
            h.set("Call-ID", "info-call-1");
            h.set("CSeq", "2 INFO");
            h.set(
                "From",
                "<sip:34020000002000000001@34020000002000000001>;tag=p",
            );
            h.set(
                "To",
                "<sip:34020000001320000001@34020000002000000001>;tag=d",
            );
            sip_core::Incoming {
                message: SipMessage::Request(Request {
                    method: Method::Info,
                    uri: "sip:34020000001320000001@127.0.0.1".into(),
                    headers: h,
                    body: body.as_bytes().to_vec(),
                }),
                from: platform_addr,
            }
        };
        let mut prx = platform_tp.register("info-call-1");

        // PLAY + Scale=2.0 → 倍速 2.0,未暂停。
        let inc = make_info("PLAY MANSRTSP/1.0\r\nCSeq: 2\r\nScale: 2.0\r\n\r\n");
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert_eq!(control.speed(), 2.0);
        assert!(!control.is_paused());
        // 平台应收到 200。
        let ok = tokio::time::timeout(std::time::Duration::from_secs(2), prx.recv())
            .await
            .expect("超时")
            .expect("关闭");
        matches!(ok.message, SipMessage::Response(r) if r.status == 200);

        // PAUSE → 暂停。
        let inc = make_info("PAUSE MANSRTSP/1.0\r\nCSeq: 3\r\nPauseTime: now\r\n\r\n");
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert!(control.is_paused());

        // PLAY(无 Scale)→ 恢复,倍速回 1.0。
        let inc = make_info("PLAY MANSRTSP/1.0\r\nCSeq: 4\r\n\r\n");
        assert!(sim.answer_inbound(&device_tp, &inc).await.unwrap());
        assert!(!control.is_paused());
        assert_eq!(control.speed(), 1.0);
    }

    #[test]
    fn mobile_position单次查询_读取当前共享坐标() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        *sim.position.lock().unwrap() = (121.473_701, 31.230_416);
        let query = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>MobilePosition</CmdType><SN>17</SN><DeviceID>34020000001320000001</DeviceID></Query>",
        )
        .unwrap();
        let xml = sim.handle_query(&query).unwrap();
        assert!(xml.contains("<Response>"));
        assert!(xml.contains("<SN>17</SN>"));
        assert!(xml.contains("<Longitude>121.473701</Longitude>"));
        assert!(xml.contains("<Latitude>31.230416</Latitude>"));
    }

    #[tokio::test]
    async fn guarded与alarming独立_报警复位不撤防() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let guard = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>1</SN><DeviceID>d</DeviceID><GuardCmd>SetGuard</GuardCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&guard).await.unwrap();
        {
            let state = sim.control_state.lock().unwrap();
            assert!(state.guarded);
            assert!(!state.alarming);
        }

        sim.mark_alarm_active();
        let reset = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>d</DeviceID><AlarmCmd>ResetAlarm</AlarmCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&reset).await.unwrap();
        {
            let state = sim.control_state.lock().unwrap();
            assert!(state.guarded);
            assert!(!state.alarming);
        }

        sim.mark_alarm_active();
        let unknown = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>3</SN><DeviceID>d</DeviceID><AlarmCmd>unknown</AlarmCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&unknown).await.unwrap();
        let snapshot = sim.runtime_snapshot();
        assert!(snapshot.guarded);
        assert!(snapshot.alarming, "未知 AlarmCmd 不得伪造复位");
        assert_eq!(snapshot.last_change, "alarm_raised");

        let numeric_reset = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>4</SN><DeviceID>d</DeviceID><AlarmCmd>2</AlarmCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&numeric_reset).await.unwrap();
        let reset_guard = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>5</SN><DeviceID>d</DeviceID><GuardCmd>ResetGuard</GuardCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&reset_guard).await.unwrap();
        let snapshot = sim.runtime_snapshot();
        assert!(!snapshot.guarded);
        assert!(!snapshot.alarming);
    }

    #[tokio::test]
    async fn device_config应用后_config_download回读当前值() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let control = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>3</SN><DeviceID>d</DeviceID><BasicParam><Name>平台下发名称</Name><Expiration>7200</Expiration><HeartBeatInterval>15</HeartBeatInterval><HeartBeatCount>5</HeartBeatCount></BasicParam><PictureMask><Enabled>1</Enabled></PictureMask><AlarmReport><Enabled>0</Enabled></AlarmReport></Control>",
        )
        .unwrap();
        sim.handle_control(&control).await.unwrap();

        let query = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>ConfigDownload</CmdType><SN>4</SN><DeviceID>d</DeviceID><ConfigType>BasicParam/PictureMask/AlarmReport</ConfigType></Query>",
        )
        .unwrap();
        let xml = sim.handle_query(&query).unwrap();
        assert!(xml.contains("<Name>平台下发名称</Name>"));
        assert!(xml.contains("<Expiration>7200</Expiration>"));
        assert!(xml.contains("<HeartBeatInterval>15</HeartBeatInterval>"));
        assert!(xml.contains("<HeartBeatCount>5</HeartBeatCount>"));
        assert!(xml.contains("<On>1</On>"));
        assert!(xml.contains("<SumNum>0</SumNum>"));
        assert!(xml.contains("<AlarmReport><Enabled>0</Enabled></AlarmReport>"));

        let snapshot = sim.runtime_snapshot();
        assert_eq!(snapshot.name, "平台下发名称");
        assert_eq!(snapshot.expiration, 7200);
        assert_eq!(snapshot.heartbeat_interval, 15);
        assert_eq!(snapshot.heartbeat_count, 5);
        assert_eq!(snapshot.picture_mask_enabled, Some(1));
        assert_eq!(snapshot.alarm_report_enabled, Some(0));
        assert_eq!(snapshot.last_change, "device_config_applied");

        let partial = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>5</SN><DeviceID>d</DeviceID><BasicParam><Name>只改名称</Name></BasicParam></Control>",
        )
        .unwrap();
        sim.handle_control(&partial).await.unwrap();
        let snapshot = sim.runtime_snapshot();
        assert_eq!(snapshot.name, "只改名称");
        assert_eq!(snapshot.expiration, 7200, "未携带的字段必须保留");
        assert_eq!(snapshot.heartbeat_interval, 15);
        assert_eq!(snapshot.heartbeat_count, 5);

        let basic_only = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>ConfigDownload</CmdType><SN>6</SN><DeviceID>d</DeviceID><ConfigType>BasicParam</ConfigType></Query>",
        )
        .unwrap();
        let xml = sim.handle_query(&basic_only).unwrap();
        assert!(xml.contains("<BasicParam>"));
        assert!(!xml.contains("<PictureMask>"));
        assert!(!xml.contains("<AlarmReport>"));

        let fresh = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        assert_ne!(fresh.runtime_snapshot().name, "只改名称");
    }

    #[test]
    fn runtime_snapshot新实例含初始状态与基本配置() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        let snapshot = sim.runtime_snapshot();
        assert!(!snapshot.guarded);
        assert!(!snapshot.alarming);
        assert_eq!(
            (snapshot.longitude, snapshot.latitude),
            (116.397428, 39.909230)
        );
        assert_eq!(snapshot.expiration, sim.config().register_expires_secs);
        assert_eq!(
            snapshot.heartbeat_interval,
            sim.config().heartbeat_interval_secs as u32
        );
        assert_eq!(
            snapshot.heartbeat_count,
            sim.config().heartbeat_fail_threshold
        );
        assert_eq!(snapshot.last_change, "device_started");
    }

    #[test]
    fn record_info仅返回真实索引并保持分页总数() {
        use crate::recording::{RecordingEntry, RecordingKind, RecordingSource, RecordingStore};
        let root = std::env::temp_dir().join(format!(
            "uvp-record-info-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = Arc::new(RecordingStore::open(&root).unwrap());
        let channel = "34020000001320000001";
        for index in 0..6_u64 {
            let path = root.join(format!("record-{index}.mp4"));
            std::fs::write(&path, b"media").unwrap();
            store
                .add(RecordingEntry {
                    id: format!("record-{index}"),
                    channel_id: channel.into(),
                    start_time_ms: 1_000 + index,
                    end_time_ms: 2_000 + index,
                    start_time: "1970-01-01T00:00:01".into(),
                    end_time: "1970-01-01T00:00:02".into(),
                    source: RecordingSource::Local,
                    kind: RecordingKind::Manual,
                    path,
                    size_bytes: 5,
                    video_codec: None,
                    audio_codec: None,
                    audio_sample_rate_hz: None,
                })
                .unwrap();
        }
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.install_recording_store(store);
        let query = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>RecordInfo</CmdType><SN>88</SN><DeviceID>34020000001320000001</DeviceID><StartTime>1970-01-01T00:00:00</StartTime><EndTime>1970-01-01T00:00:10</EndTime><Type>manual</Type></Query>",
        )
        .unwrap();
        let packets = sim.handle_query_packets(&query).unwrap();
        assert_eq!(packets.len(), 2);
        assert!(packets[0].contains("<SN>88</SN>"));
        assert!(packets[0].contains("<SumNum>6</SumNum>"));
        assert!(packets[0].contains("Num=\"5\""));
        assert!(packets[1].contains("<SumNum>6</SumNum>"));
        assert!(packets[1].contains("Num=\"1\""));
        assert!(packets[0].contains("<FilePath>"));
        assert!(!packets.join("").contains("2026-07-03"));

        let empty = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>RecordInfo</CmdType><SN>89</SN><DeviceID>34020000001320000001</DeviceID><Type>alarm</Type></Query>",
        )
        .unwrap();
        let packets = sim.handle_query_packets(&empty).unwrap();
        assert_eq!(packets.len(), 1);
        assert!(packets[0].contains("<SumNum>0</SumNum>"));
        assert!(!packets[0].contains("<Item>"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn 控制快照_预置位姿态_看守位_扫描与存储() {
        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        sim.set_local_pose(12.5, 20.0, 2.0, 0.6, 0.4);

        let set = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>1</SN><DeviceID>d</DeviceID><PTZCmd>A50F0181010100</PTZCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&set).await.unwrap();
        let snapshot = sim.control_snapshot();
        let preset = snapshot
            .presets
            .iter()
            .find(|item| item.id == 1)
            .expect("预置位 1 应存在");
        assert_eq!((preset.pan, preset.tilt, preset.zoom), (12.5, 20.0, 2.0));

        let home = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>2</SN><DeviceID>d</DeviceID><HomePosition><Enabled>1</Enabled><ResetTime>15</ResetTime><PresetIndex>1</PresetIndex></HomePosition></Control>",
        )
        .unwrap();
        sim.handle_control(&home).await.unwrap();
        let snapshot = sim.control_snapshot();
        assert!(snapshot.home_enabled);
        assert_eq!(snapshot.home_preset_index, Some(1));
        assert_eq!(snapshot.home_reset_time, Some(15));

        let left = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>3</SN><DeviceID>d</DeviceID><PTZCmd>A50F0189010100</PTZCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&left).await.unwrap();
        let right = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>4</SN><DeviceID>d</DeviceID><PTZCmd>A50F0189010200</PTZCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&right).await.unwrap();
        let speed = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>5</SN><DeviceID>d</DeviceID><PTZCmd>A50F018A01FF10</PTZCmd></Control>",
        )
        .unwrap();
        sim.handle_control(&speed).await.unwrap();
        let snapshot = sim.control_snapshot();
        let scan = snapshot.scan.iter().find(|item| item.id == 1).expect("扫描组 1 应存在");
        assert_eq!(scan.left.map(|pose| pose.0), Some(12.5));
        assert_eq!(scan.right.map(|pose| pose.0), Some(12.5));
        assert_eq!(scan.speed, Some(511));

        let target = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>6</SN><DeviceID>d</DeviceID><TargetTrack><Mode>Manual</Mode><TargetArea><Length>1000</Length><Width>500</Width><MidPointX>500</MidPointX><MidPointY>250</MidPointY><LengthX>200</LengthX><LengthY>100</LengthY></TargetArea></TargetTrack></Control>",
        )
        .unwrap();
        sim.handle_control(&target).await.unwrap();
        let snapshot = sim.control_snapshot();
        assert_eq!(snapshot.target_mode.as_deref(), Some("Manual"));
        let target_box = snapshot.target_box.expect("手动跟踪应带框");
        assert!((target_box.0 - 0.4).abs() < 0.001);
        assert!((target_box.1 - 0.4).abs() < 0.001);
        assert!((target_box.2 - 0.2).abs() < 0.001);
        assert!((target_box.3 - 0.2).abs() < 0.001);
        let json = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(json["target_mode"], "Manual");
        assert!(json["target_box"].is_array());

        let query = gb28181_protocol::manscdp::Query::parse(
            "<Query><CmdType>SDCardStatus</CmdType><SN>6</SN><DeviceID>d</DeviceID></Query>",
        )
        .unwrap();
        let _ = sim.handle_query(&query).unwrap();
        let snapshot = sim.control_snapshot();
        assert!(snapshot.storage_queried);
        assert_eq!(snapshot.storage_cards.len(), 1);
        assert_eq!(snapshot.storage_cards[0].capacity_mb, 32768);

        let mask = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>7</SN><DeviceID>d</DeviceID><PictureMask><On>1</On><SumNum>2</SumNum><RegionList Num=\"2\"><Item><Seq>1</Seq><Point>20,30,50,60</Point></Item><Item><Seq>2</Seq><Point>100,100,200,200</Point></Item></RegionList></PictureMask></Control>",
        )
        .unwrap();
        sim.handle_control(&mask).await.unwrap();
        let snapshot = sim.control_snapshot();
        assert!(snapshot.mask_configured);
        assert!(snapshot.mask_on);
        assert_eq!(snapshot.mask_regions.len(), 2);
        assert_eq!(
            (
                snapshot.mask_regions[0].seq,
                snapshot.mask_regions[0].left,
                snapshot.mask_regions[0].top,
                snapshot.mask_regions[0].right,
                snapshot.mask_regions[0].bottom
            ),
            (1, 20, 30, 50, 60)
        );

        let drag = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceControl</CmdType><SN>8</SN><DeviceID>d</DeviceID><DragZoomIn><Length>1000</Length><Width>500</Width><MidPointX>500</MidPointX><MidPointY>250</MidPointY><LengthX>200</LengthX><LengthY>100</LengthY></DragZoomIn></Control>",
        )
        .unwrap();
        sim.handle_control(&drag).await.unwrap();
        let snapshot = sim.control_snapshot();
        let drag_box = snapshot.drag_zoom_box.expect("拉框应记录框");
        assert!((drag_box.0 - 0.4).abs() < 0.001);
        assert!((drag_box.1 - 0.4).abs() < 0.001);
        assert!((drag_box.2 - 0.2).abs() < 0.001);
        assert!((drag_box.3 - 0.2).abs() < 0.001);

        let osd = gb28181_protocol::manscdp::Control::parse(
            "<Control><CmdType>DeviceConfig</CmdType><SN>9</SN><DeviceID>d</DeviceID><OSDConfig><Length>1920</Length><Width>1080</Width><TimeX>12</TimeX><TimeY>8</TimeY><TimeEnable>1</TimeEnable><TextEnable>1</TextEnable><SumNum>1</SumNum><Item><Text>通道A</Text><X>20</X><Y>40</Y></Item></OSDConfig></Control>",
        )
        .unwrap();
        sim.handle_control(&osd).await.unwrap();
        let snapshot = sim.control_snapshot();
        assert!(snapshot.osd_configured);
        assert_eq!(snapshot.osd_length, 1920);
        assert_eq!(snapshot.osd_width, 1080);
        assert!(snapshot.osd_time_visible);
        assert!(snapshot.osd_text_visible);
        assert_eq!(snapshot.osd_items.len(), 1);
        assert_eq!(snapshot.osd_items[0].text, "通道A");

        let saved = sim.export_persisted_control_state();
        let restored = DeviceSimulator::new(test_cfg("127.0.0.1", 5060));
        restored.import_persisted_control_state(&saved);
        let restored_snapshot = restored.control_snapshot();
        assert!(restored_snapshot.presets.iter().any(|item| item.id == 1));
        assert_eq!(restored_snapshot.home_preset_index, Some(1));
        assert_eq!(restored_snapshot.home_reset_time, Some(15));
        assert!(restored_snapshot.scan.iter().any(|item| item.id == 1));
        assert_eq!(restored_snapshot.scan[0].speed, Some(511));
    }
}
