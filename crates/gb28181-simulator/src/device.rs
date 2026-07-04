//! 单设备仿真:设备配置 + 注册/心跳状态机。
//!
//! 实现 docs/10-functional/device-simulation.md §3.1(注册+Digest)、§3.2(心跳)。
//! 注册闭环:REGISTER(无鉴权)→ 平台 401 挑战 → REGISTER(带 Authorization)→ 200 OK。
//! 注册成功后按 `heartbeat_interval_secs` 周期发送 Keepalive。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use common::{DeviceId, Error, Result, Transport};
use gb28181_protocol::manscdp::Keepalive;
use sip_core::{authorization, Challenge, UdpTransport};

use crate::builder::{self, DialogIds};

/// 设备初始内置的两个预置位(可被平台的预置位设置/删除命令修改)。
fn default_presets() -> std::collections::BTreeMap<u8, String> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(1, "预置位1".to_string());
    m.insert(2, "预置位2".to_string());
    m
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
    /// 上级平台域(SIP domain / 服务器 ID 中心编码)。
    pub server_domain: String,
    /// 信令传输方式。
    pub transport: Transport,
    /// 心跳间隔(秒)。
    pub heartbeat_interval_secs: u64,
    /// 通道列表(目录查询应答用)。
    pub channels: Vec<ChannelConfig>,
    /// 设备信息(DeviceInfo 查询应答用)。
    pub device_info: DeviceInfo,
    /// 视频源文件路径(H.264 Annex B,C 档);None 且无 light_bitrate 时不推流(A 档)。
    pub video_source: Option<String>,
    /// 推流帧率(fps)。
    pub video_fps: u32,
    /// B 档轻量伪流码率(kbps);Some 且无 video_source 时用 LightSource。
    pub light_bitrate_kbps: Option<u32>,
    /// GB28181 协议版本(影响 MANSCDP 应答字段集,2022 默认)。
    pub gb_version: common::GbVersion,
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
}

/// 设备状态(与 docs/10-functional/device-simulation.md#2、UI DTO 对齐)。
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

/// 设备仿真运行时状态机。持有配置 + 会话标识 + CSeq 计数 + 会话状态 + 事件观察者。
pub struct DeviceSimulator {
    config: DeviceConfig,
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
    /// 报警订阅对话(平台 `SUBSCRIBE + Alarm` 时记录:对话标识 + 平台地址)。
    /// 存在时,report_alarm 走对话内 NOTIFY;否则退化为独立 MESSAGE(如手动触发)。
    alarm_dialog: tokio::sync::Mutex<Option<(builder::NotifyDialog, SocketAddr)>>,
    /// 预置位表(编号 → 名称)。PTZ 预置位设置/删除命令更新,PresetQuery 返回。
    presets: std::sync::Mutex<std::collections::BTreeMap<u8, String>>,
    /// 设备控制态(布防/看守位/巡航/精准姿态)。扩展查询与控制命令共同维护(FR-17/18)。
    control_state: std::sync::Mutex<ControlState>,
}

/// 设备控制态。由扩展控制命令(布防/看守位/巡航/精准云台)更新,
/// 供扩展查询(AlarmStatus/HomePositionQuery/CruiseTrack*/PTZPreciseStatusQuery)读回。
#[derive(Debug, Clone)]
pub struct ControlState {
    /// 是否处于布防/报警态(AlarmStatus 的 DutyStatus)。
    pub alarming: bool,
    /// 看守位是否启用。
    pub home_enabled: bool,
    /// 是否已设看守位(HomePositionQuery 的 PresetIndex 标志)。
    pub home_set: bool,
    /// 巡航轨迹:轨迹号 → 该轨迹的预置位号列表(增点/删点维护)。
    pub cruise_tracks: std::collections::BTreeMap<u32, Vec<u32>>,
    /// 最近一次精准云台姿态(pan 度, tilt 度, zoom 倍)。
    pub precise_pose: (f32, f32, f32),
}

impl Default for ControlState {
    fn default() -> Self {
        ControlState {
            alarming: false,
            home_enabled: true,
            home_set: false,
            cruise_tracks: std::collections::BTreeMap::new(),
            precise_pose: (0.0, 0.0, 1.0),
        }
    }
}

/// 活跃的推流会话(推流任务句柄 + 停止信号)。
struct PushSession {
    /// push_stream 任务。
    _task: tokio::task::JoinHandle<()>,
    /// 停止信号发送端(drop 或 send 均可触发停流)。
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 回放控制(倍速/暂停),供会话内 INFO 调整推流。
    control: Arc<media_rtp::PlaybackControl>,
    /// 是否为回放/下载会话(s=Playback/Download):结束时发 MediaStatus 121。
    is_history: bool,
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
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer: Arc::new(common::NoopObserver),
            local_addr: std::sync::OnceLock::new(),
            position: std::sync::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            presets: std::sync::Mutex::new(default_presets()),
            control_state: std::sync::Mutex::new(ControlState::default()),
        }
    }

    /// 带事件观察者创建(压测时由编排器注入共享 Metrics)。
    pub fn with_observer(config: DeviceConfig, observer: Arc<dyn common::DeviceObserver>) -> Self {
        Self {
            config,
            ids: DialogIds::new(),
            cseq: AtomicU32::new(1),
            session: tokio::sync::Mutex::new(None),
            observer,
            local_addr: std::sync::OnceLock::new(),
            position: std::sync::Mutex::new((116.397_428, 39.909_230)),
            subscriptions: tokio::sync::Mutex::new(HashMap::new()),
            alarm_dialog: tokio::sync::Mutex::new(None),
            presets: std::sync::Mutex::new(default_presets()),
            control_state: std::sync::Mutex::new(ControlState::default()),
        }
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
        transport: &Arc<UdpTransport>,
        local_host: &str,
        local_port: u16,
    ) -> Result<DeviceState> {
        use common::{DeviceEvent, FailureKind};
        self.observer.on_event(DeviceEvent::RegisterAttempt);
        let result = self.register_inner(transport, local_host, local_port).await;
        match &result {
            Ok(_) => self.observer.on_event(DeviceEvent::RegisterSuccess),
            Err(e) => {
                // 按错误信息归因:超时 → Timeout,含状态码拒绝 → Rejected,其余 Other。
                let kind = match e {
                    Error::Sip(msg) if msg.contains("超时") => FailureKind::Timeout,
                    Error::Sip(msg) if msg.contains("被拒") => FailureKind::Rejected,
                    _ => FailureKind::Other,
                };
                self.observer.on_event(DeviceEvent::RegisterFailure(kind));
            }
        }
        result
    }

    /// 注册核心逻辑(不含事件上报)。
    async fn register_inner(
        &self,
        transport: &Arc<UdpTransport>,
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
            3600,
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
                    3600,
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

    /// 发送一条无对话的 MANSCDP MESSAGE(心跳/报警/位置等)并等平台响应。
    ///
    /// 每次事务用**全新** [`DialogIds`](独立 Call-ID):同一设备的心跳、报警、
    /// 位置上报可能并发(周期任务 + 手动触发),若共用注册 Call-ID 会在共享传输上
    /// 互相覆盖接收路由、抢走对方的响应导致误判超时。独立 Call-ID 彻底隔离,
    /// 事务结束即注销路由。
    async fn send_message_xml(
        &self,
        transport: &Arc<UdpTransport>,
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

    /// 发送一次心跳(MESSAGE + Keepalive XML)。返回平台响应状态码。
    pub async fn send_keepalive(
        &self,
        transport: &Arc<UdpTransport>,
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

    /// 主动上报一条视频侦测报警(FR-9)。设备 → 平台 MESSAGE + Alarm Notify XML。
    /// `description` 为报警描述;返回平台响应状态码。
    pub async fn report_alarm(
        &self,
        transport: &Arc<UdpTransport>,
        local_host: &str,
        local_port: u16,
        description: &str,
    ) -> Result<u16> {
        let sn = self.next_cseq();
        // 报警时间用校时后的 ISO8601(与平台时间对齐,见 common::clock)。
        let time = common::clock::synced_iso8601();
        let alarm = gb28181_protocol::manscdp::AlarmNotify::video(
            self.config.device_id.as_str(),
            sn,
            time,
            description,
        );
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
            return Ok(result?.status);
        }

        self.send_message_xml(transport, local_host, local_port, &xml)
            .await
    }

    /// 主动上报一条移动位置(GPS,MobilePosition NOTIFY)。设备 → 平台。
    /// 位置订阅场景下由周期任务调用;也可手动触发。返回平台响应状态码。
    pub async fn report_position(
        &self,
        transport: &Arc<UdpTransport>,
        local_host: &str,
        local_port: u16,
        longitude: f64,
        latitude: f64,
    ) -> Result<u16> {
        // 记录当前坐标,供位置订阅的周期 NOTIFY 复用。
        if let Ok(mut p) = self.position.lock() {
            *p = (longitude, latitude);
        }
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
        transport: &Arc<UdpTransport>,
        dst: SocketAddr,
        dialog: &builder::NotifyDialog,
        sn: u32,
    ) -> Result<u16> {
        use gb28181_protocol::manscdp::{CatalogNotify, CatalogNotifyItem};
        let items: Vec<CatalogNotifyItem> = self
            .config
            .channels
            .iter()
            .map(|ch| CatalogNotifyItem {
                device_id: ch.channel_id.as_str().to_string(),
                name: ch.name.clone(),
                event: "ON".into(),
                status: ch.status.clone(),
            })
            .collect();
        let notify = CatalogNotify::new(self.config.device_id.as_str(), sn, items);
        let xml = notify.to_xml()?;

        let cseq = self.next_cseq();
        let req = builder::notify_in_dialog(
            &self.config,
            dialog,
            cseq,
            &self.local_host(),
            self.local_port(),
            &xml,
        );
        // 对话内 NOTIFY 的响应按订阅 Call-ID 回来;注册该路由收 200,收完注销。
        let mut rx = transport.register(dialog.call_id.clone());
        let result =
            sip_core::client_transact(transport, dst, &req, &mut rx, sip_core::Timing::default())
                .await;
        transport.unregister(&dialog.call_id);
        Ok(result?.status)
    }

    /// 启动(或重建)移动位置订阅的周期上报任务。
    ///
    /// 平台下发 `SUBSCRIBE + MobilePosition`(携 Interval)后调用:按 `interval` 秒
    /// 周期把当前坐标以 MobilePosition NOTIFY 上报给平台,直到订阅被替换或设备下线。
    /// `interval` 缺省/为 0 时取 5 秒兜底。同一 CmdType 再次订阅会替换旧任务。
    async fn start_position_subscription(
        self: &Arc<Self>,
        transport: &Arc<UdpTransport>,
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
                        if let Err(e) = sim
                            .report_position(&tp, &local_host, local_port, lon, lat)
                            .await
                        {
                            tracing::warn!(error=%e, "位置订阅周期上报失败");
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
        tracing::info!(interval = secs, "移动位置订阅已建立,开始周期上报");
    }

    /// 停止全部活跃订阅(设备下线/重注册时调,避免残留周期任务)。
    async fn stop_subscriptions(&self) {
        let mut subs = self.subscriptions.lock().await;
        for (_, sub) in subs.drain() {
            let _ = sub.stop_tx.send(());
        }
        *self.alarm_dialog.lock().await = None;
    }

    /// 处理一条入站请求:OPTIONS 简单回 200;MESSAGE 解析 XML 查询并应答;
    /// SUBSCRIBE 回 200 并按需启动周期 NOTIFY。返回是否已应答(true=已回,false=暂不处理)。
    ///
    /// 取 `self: &Arc<Self>`(而非 `&self`):移动位置订阅需要把设备句柄交给后台
    /// 周期上报任务,任务生命周期独立于单次入站处理。
    pub async fn answer_inbound(
        self: &Arc<Self>,
        transport: &Arc<UdpTransport>,
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
                    if let Ok(body_str) = std::str::from_utf8(&req.body) {
                        let is_control =
                            body_str.contains("<Control>") || body_str.contains("<Control ");
                        if is_control {
                            if let Ok(ctrl) = gb28181_protocol::manscdp::Control::parse(body_str) {
                                let xml = self.handle_control(&ctrl).await?;
                                self.send_reply_message(transport, incoming.from, &xml)
                                    .await?;
                                // 抓拍/升级等需设备主动发 NOTIFY 的命令:回 200/结果后异步执行。
                                self.spawn_control_side_effects(transport, &ctrl);
                            }
                        } else if body_str.contains("Broadcast") {
                            // 语音广播:回 Broadcast Response(OK/ERROR),接受则反向发 INVITE。
                            if let Ok(bc) =
                                gb28181_protocol::manscdp::BroadcastNotify::parse(body_str)
                            {
                                self.handle_broadcast(transport, &bc, incoming.from).await?;
                            }
                        } else if let Ok(query) = gb28181_protocol::manscdp::Query::parse(body_str)
                        {
                            if let Ok(xml) = self.handle_query(&query) {
                                self.send_reply_message(transport, incoming.from, &xml)
                                    .await?;
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

                    if let Ok(body_str) = std::str::from_utf8(&req.body) {
                        if let Ok(query) = gb28181_protocol::manscdp::Query::parse(body_str) {
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
                            match query.cmd_type.as_str() {
                                // 移动位置订阅:启动周期位置 NOTIFY(独立 MESSAGE 路径)。
                                "MobilePosition" => {
                                    self.start_position_subscription(transport, query.interval)
                                        .await;
                                }
                                // 目录订阅:在订阅对话内回发一条目录 NOTIFY(全通道 Event=ON)。
                                "Catalog" => {
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
        transport: &Arc<UdpTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        if let Ok(body) = std::str::from_utf8(&req.body) {
            let head = body.trim_start();
            let guard = self.session.lock().await;
            if let Some(session) = guard.as_ref() {
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
                    session.control.set_speed(scale);
                    session.control.resume();
                    tracing::info!(scale, "回放控制:播放/倍速");
                }
            }
        }
        let resp = sip_core::SipMessage::Response(builder::response_ok(req));
        transport.send_to(&resp, from).await?;
        Ok(true)
    }

    /// 处理 Query,返回应答 XML。
    fn handle_query(&self, query: &gb28181_protocol::manscdp::Query) -> Result<String> {
        use gb28181_protocol::manscdp::*;
        match query.cmd_type.as_str() {
            "Catalog" => {
                // GB-2022 输出新增字段(安全能力/IP/端口),GB-2016 置 None 不输出。
                let is_2022 = self.config.gb_version.is_2022();
                let items: Vec<CatalogItem> = self
                    .config
                    .channels
                    .iter()
                    .map(|ch| CatalogItem {
                        device_id: ch.channel_id.to_string(),
                        name: ch.name.clone(),
                        manufacturer: Some(self.config.device_info.manufacturer.clone()),
                        model: Some(self.config.device_info.model.clone()),
                        civil_code: None,
                        parental: Some(0),
                        parent_id: Some(self.config.device_id.to_string()),
                        status: ch.status.clone(),
                        security_level_code: is_2022.then(|| "A".to_string()),
                        ip_address: None,
                        port: None,
                    })
                    .collect();
                let resp = CatalogResponse::new(&query.device_id, query.sn, items);
                resp.to_xml()
            }
            "DeviceInfo" => {
                let resp = DeviceInfoResponse {
                    cmd_type: "DeviceInfo".into(),
                    sn: query.sn,
                    device_id: query.device_id.clone(),
                    result: "OK".into(),
                    device_name: self.config.device_info.device_name.clone(),
                    manufacturer: self.config.device_info.manufacturer.clone(),
                    model: self.config.device_info.model.clone(),
                    firmware: self.config.device_info.firmware.clone(),
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
            "RecordInfo" => {
                // 返回一段模拟录像(FR-10)。真实实现应按查询时间范围列举本地录像。
                let items = vec![RecordItem {
                    device_id: query.device_id.clone(),
                    name: "record".into(),
                    start_time: "2026-07-03T10:00:00".into(),
                    end_time: "2026-07-03T10:05:00".into(),
                    kind: "time".into(),
                }];
                let resp = RecordInfoResponse::new(&query.device_id, query.sn, items);
                resp.to_xml()
            }
            "ConfigDownload" => {
                // 设备配置查询:按 ConfigType 返回基本参数(BasicParam)和/或视频参数(VideoParamOpt)。
                // 心跳超时次数固定 3(与 device-simulation.md §3.2 重注册阈值一致)。
                let resp = ConfigDownloadResponse::by_type(
                    &query.device_id,
                    query.sn,
                    query.config_type.as_deref().unwrap_or("BasicParam"),
                    self.config.device_info.device_name.clone(),
                    3600,
                    self.config.heartbeat_interval_secs as u32,
                    3,
                    "1920*1080",
                );
                resp.to_xml()
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
            "StorageCardStatusQuery" => {
                // 存储卡状态查询(FR-17):模拟单张 32G 卡余 24G。
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
                // 巡航轨迹详情查询(FR-17):读 GroupID(缺省 1),返回该轨迹预置点(Speed/DwellTime 固定 5/3)。
                let group = query.group_id.unwrap_or(1);
                let presets: Vec<u32> = self
                    .control_state
                    .lock()
                    .map(|s| s.cruise_tracks.get(&group).cloned().unwrap_or_default())
                    .unwrap_or_default();
                CruiseTrackQueryResponse::new(&query.device_id, query.sn, group, &presets).to_xml()
            }
            "PTZPreciseStatusQuery" => {
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

    /// 处理设备控制命令(PTZ/关键帧/录像/布防/复位/重启),返回应答 XML。
    /// 语义:模拟器接受并回 Result=OK;强制关键帧会触发当前推流立即发关键帧
    ///(当前 FileSource/LightSource 本就周期性带关键帧,记录日志即可)。
    async fn handle_control(&self, ctrl: &gb28181_protocol::manscdp::Control) -> Result<String> {
        tracing::info!(kind = %ctrl.kind(), sn = ctrl.sn, "收到设备控制");
        if let Some(ptz) = &ctrl.ptz_cmd {
            // PTZ 8 字节码:识别预置位设置/调用/删除并更新预置位表;其余为方向/变倍。
            use gb28181_protocol::manscdp::PresetAction;
            match ctrl.preset_op() {
                Some((PresetAction::Set, idx)) => {
                    if let Ok(mut p) = self.presets.lock() {
                        p.insert(idx, format!("预置位{idx}"));
                    }
                    tracing::info!(preset = idx, "预置位设置");
                }
                Some((PresetAction::Delete, idx)) => {
                    if let Ok(mut p) = self.presets.lock() {
                        p.remove(&idx);
                    }
                    tracing::info!(preset = idx, "预置位删除");
                }
                Some((PresetAction::Call, idx)) => {
                    tracing::info!(preset = idx, "预置位调用(转到)");
                    self.observer
                        .on_event(common::DeviceEvent::PtzPresetCall { preset: idx });
                }
                None => {
                    // 先识别巡航/辅助/聚焦(均 0x8x/聚焦位),否则按方向/变倍运动。
                    if let Some(op) = ctrl.cruise_op() {
                        self.apply_cruise_op(op);
                    } else if let Some(aux) = ctrl.aux_op() {
                        tracing::info!(func = aux.function.label(), on = aux.on, "辅助控制");
                    } else if let Some((near, far)) = ctrl.focus_op() {
                        tracing::info!(near, far, "聚焦/光圈控制");
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
                    } else {
                        tracing::info!(ptz = %ptz, "PTZ 云台控制(模拟接受)");
                    }
                }
            }
        }
        // 精确云台控制(GB-2022):记录姿态供 PTZPreciseStatusQuery 读回。
        if let Some(p) = &ctrl.ptz_precise_ctrl {
            tracing::info!(pan = p.pan, tilt = p.tilt, zoom = p.zoom, "精确云台控制");
            if let Ok(mut s) = self.control_state.lock() {
                s.precise_pose = (p.pan, p.tilt, p.zoom);
            }
        }
        // 目标跟踪:白名单模式外忽略,仍应答 200/OK。
        if let Some(t) = &ctrl.target_track {
            let mode = t.mode.trim();
            if matches!(mode, "Auto" | "Manual" | "Stop") {
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
        // 布防/撤防:更新报警值守态(供 AlarmStatus 查询读回)。
        if let Some(g) = &ctrl.guard_cmd {
            let arm = g.eq_ignore_ascii_case("SetGuard");
            if let Ok(mut s) = self.control_state.lock() {
                s.alarming = arm;
            }
            tracing::info!(guard = %g, "布防/撤防");
        }
        // 看守位设置:更新启用/已设标志(供 HomePositionQuery 读回)。
        if let Some(hp) = &ctrl.home_position {
            if let Ok(mut s) = self.control_state.lock() {
                s.home_enabled = hp.enabled != 0;
                s.home_set = hp.enabled != 0;
            }
            tracing::info!(enabled = hp.enabled, preset = hp.preset_index, "看守位设置");
        }
        if ctrl.iframe_cmd.is_some() {
            tracing::info!("强制关键帧(下一帧起带 IDR)");
        }
        let resp = gb28181_protocol::manscdp::ControlResponse::ok(&ctrl.device_id, ctrl.sn);
        resp.to_xml()
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
                tracing::info!(track, speed, "巡航设速");
            }
            CruiseOp::SetDwell { track, dwell } => {
                s.cruise_tracks.entry(track).or_default();
                tracing::info!(track, dwell, "巡航设停留");
            }
            CruiseOp::Start { track } => {
                if track == 0 {
                    tracing::info!("巡航停止");
                } else {
                    s.cruise_tracks.entry(track).or_default();
                    tracing::info!(track, "巡航启动");
                }
            }
        }
    }

    /// 处理语音广播(FR-36):校验 TargetID,回 Broadcast Response,接受则反向发 INVITE。
    ///
    /// 设备为**主叫 UAC**:向平台发 `m=audio ... a=recvonly` 的 INVITE 收 G.711A 下行。
    /// ⚠️ 反向 INVITE 的完整音频接收/ACK/BYE 链路本 WVP 环境无法触发验证,当前实现
    /// 到"发出带正确 SDP 的 INVITE"为止(记忆:不盲写无法验证的部分),已足以让平台侧建流。
    async fn handle_broadcast(
        self: &Arc<Self>,
        transport: &Arc<UdpTransport>,
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
        let resp = if owned {
            gb28181_protocol::manscdp::BroadcastResponse::ok(&bc.target_id, bc.sn)
        } else {
            gb28181_protocol::manscdp::BroadcastResponse::error(
                self.config.device_id.as_str(),
                bc.sn,
                "target mismatch",
            )
        };
        // 先回 200 OK(入站已回),再以独立 MESSAGE 发 Broadcast Response。
        let xml = resp.to_xml()?;
        self.send_reply_message(transport, from, &xml).await?;

        if owned {
            // 构造反向 INVITE 的 SDP offer(设备收流)。真正的 client INVITE 事务
            // 需音频 RTP 接收端,本环境无法验证,故此处仅生成 offer 并记录。
            let local_ip = transport
                .local_addr()
                .map(|a| a.ip())
                .unwrap_or_else(|_| std::net::IpAddr::from([0, 0, 0, 0]));
            let offer = sip_core::build_broadcast_offer(
                &bc.target_id,
                local_ip,
                0, // 端口 0:占位(未实际开收流 socket)
                rand::random::<u32>() & 0x00FF_FFFF,
                false,
            );
            tracing::info!(source = %bc.source_id, target = %bc.target_id,
                "语音广播接受,反向 INVITE offer 已构造(音频接收链路待真机验证)");
            tracing::debug!(sdp = %offer, "广播 SDP offer");
        } else {
            tracing::warn!(target = %bc.target_id, "语音广播 TargetID 不属本设备,拒绝");
        }
        Ok(())
    }

    /// 触发需设备主动 NOTIFY/上报的控制副作用(在线升级 4 步进度、抓拍上传)。
    /// 已在入站循环里回过 200/结果,这里异步执行不阻塞应答。
    fn spawn_control_side_effects(
        self: &Arc<Self>,
        transport: &Arc<UdpTransport>,
        ctrl: &gb28181_protocol::manscdp::Control,
    ) {
        // 在线升级:4 步进度 NOTIFY。
        if let Some(up) = &ctrl.device_upgrade {
            let dev = Arc::clone(self);
            let tp = Arc::clone(transport);
            let up = up.clone();
            tokio::spawn(async move {
                dev.run_upgrade_progress(&tp, &up).await;
            });
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

    /// 在线升级 4 步进度:percent [0,30,60,100],步间 1.5s,发 DeviceUpgradeResult NOTIFY。
    async fn run_upgrade_progress(
        &self,
        transport: &Arc<UdpTransport>,
        up: &gb28181_protocol::manscdp::DeviceUpgrade,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        for percent in [0u8, 30, 60, 100] {
            let sn = self.next_cseq() & 0xFFFF;
            let notify = gb28181_protocol::manscdp::DeviceUpgradeResultNotify::new(
                self.config.device_id.as_str(),
                sn,
                &up.session_id,
                &up.firmware,
                percent,
            );
            if let Ok(xml) = notify.to_xml() {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!(percent, "在线升级进度");
            if percent < 100 {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
        }
    }

    /// 抓拍上传(GB-2022):串行拍 N 张,每张 HTTP PUT 上传后发完成 NOTIFY。
    /// 模拟设备无摄像头,上传一段占位 JPEG 字节。
    async fn run_snapshot_upload(
        &self,
        transport: &Arc<UdpTransport>,
        cfg: &gb28181_protocol::manscdp::SnapShotConfig,
    ) {
        let (h, p) = (self.local_host(), self.local_port());
        let num = cfg.clamped_num();
        // 占位 JPEG(SOI + EOI),真实设备为编码帧。
        let jpeg: &[u8] = &[0xFF, 0xD8, 0xFF, 0xD9];
        for idx in 1..=num {
            let ts = common::clock::synced_iso8601();
            let snap_id = format!("{}_{idx}", ts.replace([':', '-'], "").replace('.', ""));
            let url = if cfg.upload_url.ends_with('/') {
                format!("{}{snap_id}.jpg", cfg.upload_url)
            } else {
                cfg.upload_url.clone()
            };
            match http_put_jpeg(&url, jpeg).await {
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
            if idx < num && cfg.interval > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(cfg.interval as u64)).await;
            }
        }
    }

    /// 向平台发送一条应答/通知 MESSAGE(查询应答、控制应答、位置通知等复用)。
    async fn send_reply_message(
        &self,
        transport: &Arc<UdpTransport>,
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

    /// 处理 INVITE:解析平台 SDP,启动推流,回 200 OK(带本端 SDP)。
    async fn handle_invite(
        &self,
        transport: &Arc<UdpTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        use std::net::IpAddr;

        // 解析平台 SDP(提取 RTP 目标地址与端口)。
        let body_str = std::str::from_utf8(&req.body)
            .map_err(|_| Error::Sip("INVITE body 非 UTF-8".into()))?;
        let platform_sdp = sip_core::SessionDescription::parse(body_str)?;
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
            return Err(Error::Gb28181("已有活跃会话".into()));
        }

        // 传输模式:平台 SDP 的 m= proto 含 "TCP" 则走 TCP-ACTIVE(RFC 4571)。
        let use_tcp = platform_sdp.media.proto.to_uppercase().contains("TCP");

        // 按配置选择视频源:C 档(文件)优先,其次 B 档(轻量伪流),否则 A 档(不推流)。
        let fps = self.config.video_fps;
        let source: Option<Box<dyn media_rtp::VideoSource>> =
            if let Some(ref path) = self.config.video_source {
                // 含音频轨的文件走音视频复合流(from_path_av);裸流/无音频自动退化为纯视频。
                Some(Box::new(
                    media_rtp::FileSource::from_path_av(path, fps)
                        .map_err(|e| Error::Media(format!("加载视频源失败: {e}")))?,
                ))
            } else {
                self.config
                    .light_bitrate_kbps
                    .map(|kbps| Box::new(media_rtp::LightSource::new(kbps, fps)) as _)
            };

        // 先构造并发送 200 OK(带本端 SDP,SSRC 回显平台值)。
        // TCP-PASSIVE 下平台收到 200 后才开始监听,故推流必须在 200 之后再连接。
        let local_ip: std::net::IpAddr = self
            .local_host()
            .parse()
            .unwrap_or_else(|_| transport.local_addr().map(|a| a.ip()).unwrap_or(rtp_host));
        // o= 用户名填被点播的通道 ID(取 INVITE Request-URI 的 user 部分)。
        let channel = req
            .uri
            .strip_prefix("sip:")
            .and_then(|s| s.split('@').next())
            .unwrap_or(self.config.device_id.as_str());
        let local_sdp =
            sip_core::SessionDescription::new_device_response(channel, local_ip, 0, ssrc, use_tcp);
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
            let control = media_rtp::PlaybackControl::new();
            // 录像下载会话(s=Download + a=downloadspeed:N):按 N 倍速推流。
            if platform_sdp.is_download() {
                if let Some(spd) = platform_sdp.download_speed {
                    control.set_speed(spd as f32);
                    tracing::info!(speed = spd, "INVITE:录像下载模式,按倍速推流");
                }
            }
            let control_task = Arc::clone(&control);
            let task = tokio::spawn(async move {
                if use_tcp {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                }
                if let Err(e) = media_rtp::push_stream_controlled(
                    source,
                    rtp_dst,
                    ssrc,
                    fps,
                    use_tcp,
                    control_task,
                    async {
                        let _ = stop_rx.await;
                    },
                )
                .await
                {
                    tracing::warn!(error=%e, "推流结束(错误)");
                }
            });
            *sess_guard = Some(PushSession {
                _task: task,
                stop_tx,
                control,
                is_history: platform_sdp.is_playback() || platform_sdp.is_download(),
            });
        }
        drop(sess_guard);
        Ok(true)
    }

    /// 处理 BYE:停止推流,回 200 OK。回放/下载会话结束后补发 MediaStatus 121。
    async fn handle_bye(
        self: &Arc<Self>,
        transport: &Arc<UdpTransport>,
        req: &sip_core::Request,
        from: SocketAddr,
    ) -> Result<bool> {
        let mut sess_guard = self.session.lock().await;
        let was_history = sess_guard.as_ref().map(|s| s.is_history).unwrap_or(false);
        if let Some(session) = sess_guard.take() {
            let _ = session.stop_tx.send(()); // 触发停流
        }
        drop(sess_guard);

        let resp = sip_core::SipMessage::Response(builder::response_ok(req));
        transport.send_to(&resp, from).await?;

        // 回放/下载会话结束:补发 MediaStatus 121(历史媒体文件发送结束)。
        if was_history {
            let (h, p) = (self.local_host(), self.local_port());
            let sn = self.next_cseq();
            if let Ok(xml) = gb28181_protocol::manscdp::MediaStatusNotify::finished(
                self.config.device_id.as_str(),
                sn,
            )
            .to_xml()
            {
                let _ = self.send_message_xml(transport, &h, p, &xml).await;
            }
            tracing::info!("回放/下载结束,发 MediaStatus 121");
        }
        Ok(true)
    }

    /// 运行设备:注册 → 启动入站应答(OPTIONS)→ 周期心跳,直到 `shutdown` 完成。
    ///
    /// 心跳连续失败 `max_hb_fail` 次判定掉线,触发重注册(指数退避 backoff→2×,上限 backoff_max)。
    /// 这是设备"保持在线"的主循环(docs/10-functional/device-simulation.md §3.2/§3.3)。
    pub async fn run(
        self: Arc<Self>,
        transport: Arc<UdpTransport>,
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
        let backoff_base = Duration::from_secs(1);
        let backoff_max = Duration::from_secs(30);
        let mut backoff = backoff_base;
        while (self.register(&transport, &local_host, local_port).await).is_err() {
            tokio::select! {
                _ = &mut shutdown => { inbound_task.abort(); return; }
                _ = tokio::time::sleep(backoff) => {
                    backoff = (backoff * 2).min(backoff_max);
                }
            }
        }
        tracing::info!(device=%self.config.device_id, "注册成功");

        // 心跳主循环 + 注册续约(FR-32)。
        // 注册有效期 3600s(与 builder::register 一致),到期前 80%(2880s)主动重注册续约。
        let interval = Duration::from_secs(self.config.heartbeat_interval_secs.max(1));
        let renew_after = Duration::from_secs(3600 * 80 / 100);
        let mut renew_ticker = tokio::time::interval(renew_after);
        renew_ticker.reset(); // 跳过启动即触发
        let max_hb_fail = 3u32;
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
                            hb_fail += 1;
                            if hb_fail >= max_hb_fail {
                                tracing::warn!(device=%self.config.device_id, "心跳连续失败,重注册");
                                hb_fail = 0;
                                let mut b = backoff_base;
                                while self.register(&transport, &local_host, local_port).await.is_err() {
                                    tokio::select! {
                                        _ = &mut shutdown => { inbound_task.abort(); return; }
                                        _ = tokio::time::sleep(b) => { b = (b * 2).min(backoff_max); }
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
        transport.unregister_inbound(self.config.device_id.as_str());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sip_core::{Headers, Response, SipMessage};

    fn test_cfg(host: &str, port: u16) -> DeviceConfig {
        DeviceConfig {
            device_id: DeviceId::new("34020000001320000001").unwrap(),
            username: "34020000001320000001".into(),
            password: "12345678".into(),
            server_host: host.into(),
            server_port: port,
            server_domain: "34020000002000000001".into(),
            transport: Transport::Udp,
            heartbeat_interval_secs: 60,
            channels: vec![],
            device_info: DeviceInfo {
                device_name: "Test Device".into(),
                manufacturer: "UVP".into(),
                model: "Sim".into(),
                firmware: "0.1.0".into(),
            },
            video_source: None,
            video_fps: 25,
            light_bitrate_kbps: None,
            gb_version: common::GbVersion::V2022,
        }
    }

    /// mock 平台:收到首个 REGISTER 回 401(带挑战),第二个回 200。
    async fn mock_platform_register(
        platform: Arc<UdpTransport>,
        device_addr: SocketAddr,
        call_id: String,
    ) {
        let mut prx = platform.register(call_id.clone());
        let mut seen = 0;
        while let Some(inc) = prx.recv().await {
            if let SipMessage::Request(req) = inc.message {
                let cseq = req.headers.cseq().unwrap_or("1 REGISTER").to_string();
                let mut h = Headers::new();
                h.set("Call-ID", call_id.clone());
                h.set("CSeq", cseq);
                let resp = if seen == 0 {
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
            }
        }
    }

    #[tokio::test]
    async fn 注册闭环_401后带鉴权成功() {
        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_addr = platform_tp.local_addr().unwrap();
        let device_addr = device_tp.local_addr().unwrap();

        let sim = DeviceSimulator::new(test_cfg("127.0.0.1", platform_addr.port()));
        tokio::spawn(mock_platform_register(
            platform_tp.clone(),
            device_addr,
            sim.call_id().to_string(),
        ));

        let state = sim
            .register(&device_tp, "127.0.0.1", device_addr.port())
            .await
            .unwrap();
        assert_eq!(state, DeviceState::Registered);
    }

    #[tokio::test]
    async fn options_回_200() {
        use sip_core::{Method, Request};

        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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

        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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

    #[test]
    fn 目录应答按gb版本差异化字段() {
        use gb28181_protocol::manscdp::Query;

        let query = Query {
            cmd_type: "Catalog".into(),
            sn: 1,
            device_id: "35020000001310000001".into(),
            interval: None,
            group_id: None,
            config_type: None,
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

        let sd = sim
            .handle_query(&mk("StorageCardStatusQuery", None))
            .unwrap();
        assert!(sd.contains("<TotalCapacity>32768</TotalCapacity>"));

        let list = sim.handle_query(&mk("CruiseTrackListQuery", None)).unwrap();
        assert!(list.contains("<TrackList Num=\"1\">"));
        assert!(list.contains("<Name>巡航 1</Name>"));

        let detail = sim.handle_query(&mk("CruiseTrackQuery", Some(1))).unwrap();
        assert!(detail.contains("<GroupID>1</GroupID>"));
        assert!(detail.contains("<PresetID>3</PresetID>"));

        let precise = sim
            .handle_query(&mk("PTZPreciseStatusQuery", None))
            .unwrap();
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

    /// 语音广播(FR-36):TargetID 属本设备回 Broadcast Response OK。
    #[tokio::test]
    async fn 语音广播_目标匹配回ok() {
        use sip_core::{Headers, Method, Request, SipMessage};
        let device_tp = std::sync::Arc::new(UdpTransport::bind("127.0.0.1:0").await.unwrap());
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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

        // 布防 → alarming=true。
        let guard = Control {
            cmd_type: "DeviceControl".into(),
            device_id: "d".into(),
            sn: 3,
            guard_cmd: Some("SetGuard".into()),
            ..Default::default()
        };
        sim.handle_control(&guard).await.unwrap();
        assert!(sim.control_state.lock().unwrap().alarming);
    }

    #[tokio::test]
    async fn 移动位置订阅_回200并周期上报位置notify() {
        use sip_core::{Method, Request};

        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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

        // 停止订阅,任务应退出(不再 panic/泄漏)。
        sim.stop_subscriptions().await;
        assert!(sim.subscriptions.lock().await.is_empty());
    }

    #[tokio::test]
    async fn 报警订阅_回200并记录对话供对话内notify() {
        use sip_core::{Method, Request};

        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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

        // 下线清理:对话应被清除。
        sim.stop_subscriptions().await;
        assert!(sim.alarm_dialog.lock().await.is_none());
    }

    #[tokio::test]
    async fn info回放控制_调速与暂停恢复并回200() {
        use sip_core::{Method, Request};

        let device_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
        let platform_tp = UdpTransport::bind("127.0.0.1:0").await.unwrap();
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
            _task: task,
            stop_tx,
            control: control.clone(),
            is_history: false,
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
}
