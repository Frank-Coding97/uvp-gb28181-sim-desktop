use ipc_error::{IpcError, IpcResult};

use std::sync::Arc;

use common::{DeviceId, DeviceObserver, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use serde::Deserialize;
use sip_core::{SipTransport, TcpTransport, UdpTransport};
use tauri::{AppHandle, Emitter}; // Tauri 2 emit/state 需要对应 trait

use super::*;

/// 查询单设备运行状态(UI 切页后对账用)。running=设备实例是否存在。
#[tauri::command]
pub(crate) async fn get_device_status(
    state: tauri::State<'_, AppState>,
) -> IpcResult<serde_json::Value> {
    let running = state.device.lock().await.is_some();
    Ok(serde_json::json!({ "running": running }))
}

/// 前端传入的单设备配置。
///
/// 注意:**没有 password 字段**。前端只给 `profile_id`,密码由 Rust 侧从 OS 钥匙串取,
/// 避免明文经 IPC 往返、也避免落 localStorage。
#[derive(Deserialize)]
pub(crate) struct DeviceCfg {
    /// 平台档案标识,用于从钥匙串取 SIP 密码。
    profile_id: String,
    server_host: String,
    server_port: u16,
    server_domain: String,
    #[serde(default)]
    inbound_identity_mode: common::InboundIdentityMode,
    device_id: String,
    #[serde(default)]
    transport: String, // "UDP" / "TCP"
    #[serde(default)]
    gb_version: String, // "2016" / "2022"
    #[serde(default)]
    channel_name: String,
    #[serde(default)]
    video_source: Option<String>,
    /// 目录模板(single/nvr-8ch/civil-3x2/large-16ch);空则用默认单通道。
    #[serde(default)]
    catalog_template: String,
    /// 信令字符集编码(GB18030/UTF-8);空则默认 GB18030。
    #[serde(default)]
    signaling_encoding: String,
    /// 心跳间隔(秒);缺省 60。
    #[serde(default)]
    heartbeat_interval: Option<u64>,
    /// 注册有效期(秒);缺省 3600。平台可授予更短时长,续约以平台值为准。
    #[serde(default)]
    expires: Option<u32>,
    /// 设备厂商 / 型号 / 固件版本(DeviceInfo 应答字段)。之前前端填了但后端写死。
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    firmware: Option<String>,
    /// 输出帧率(1–120);缺省 25。对文件源与实时采集都生效。
    #[serde(default)]
    video_fps: Option<u32>,
}

/// 通过"向平台发起 UDP connect"发现本机对外 IP(不真正发包)。
pub(crate) fn discover_local_ip(server: &str) -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 去掉空白后仍非空则返回,否则 None(用于"前端留空则用默认值")。
pub(crate) fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// 启动一台设备:注册 + 心跳 + 入站应答,后台常驻。状态经 `device_state` 事件推送。
#[tauri::command]
pub(crate) async fn start_device(
    mut config: DeviceCfg,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let mut guard = state.device.lock().await;
    if guard.is_some() {
        return Err(
            IpcError::new("device.already_running", "已有设备在运行,请先停止")
                .with_retryable(false)
                .with_action("请先停止当前设备实例"),
        );
    }

    if config.server_port == 0 {
        return Err("平台端口必须在 1–65535 之间".into());
    }
    let platform_addr = format!("{}:{}", config.server_host, config.server_port)
        .parse::<std::net::SocketAddr>()
        .map_err(|_| "平台地址必须是有效的 IP:端口（当前不支持域名）".to_string())?;
    // 密码从钥匙串取:未配置时给出明确的引导而不是「密码不能为空」。
    let password = secrets::get(&config.profile_id)?;
    if let Some(source) = config.video_source.take() {
        let source = source.trim().to_string();
        if source.is_empty() {
            config.video_source = None;
        } else {
            if source.starts_with("live:") {
                let spec = media_rtp::LiveSourceSpec::parse(&source)?;
                let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
                    .await
                    .map_err(|error| {
                        IpcError::internal(format!("实时设备枚举任务异常: {error}"))
                    })??;
                let requested_audio_codec = match &spec {
                    media_rtp::LiveSourceSpec::Camera {
                        audio_index: Some(_),
                        audio_codec,
                        ..
                    } => Some(*audio_codec),
                    media_rtp::LiveSourceSpec::Screen {
                        audio, audio_codec, ..
                    } if audio.is_enabled() => Some(*audio_codec),
                    _ => None,
                };
                match requested_audio_codec {
                    Some(media_rtp::LiveAudioCodec::Aac) if !catalog.aac_available => {
                        return Err("当前内嵌 FFmpeg 不支持 AAC 编码，请改用 G.711A/G.711U".into());
                    }
                    Some(media_rtp::LiveAudioCodec::Opus) if !catalog.opus_available => {
                        return Err(
                            "当前内嵌 FFmpeg 不支持 libopus 编码，请改用 G.711A/G.711U".into()
                        );
                    }
                    _ => {}
                }
                match spec {
                    media_rtp::LiveSourceSpec::Camera { video_index, .. }
                        if !catalog
                            .cameras
                            .iter()
                            .any(|camera| camera.index == video_index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| {
                                format!("未找到 AVFoundation 摄像头索引 {video_index}")
                            })
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Camera {
                        audio_index: Some(index),
                        ..
                    } if !catalog
                        .microphones
                        .iter()
                        .any(|microphone| microphone.index == index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}"))
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Screen {
                        audio: media_rtp::LiveAudioSource::Microphone(index),
                        ..
                    } if !catalog
                        .microphones
                        .iter()
                        .any(|microphone| microphone.index == index) =>
                    {
                        return Err(catalog
                            .avfoundation_error
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}"))
                            .into());
                    }
                    media_rtp::LiveSourceSpec::Screen {
                        target: media_rtp::ScreenCaptureTarget::Display(display_id),
                        ..
                    } if display_id != 0
                        && !catalog
                            .screens
                            .iter()
                            .any(|screen| screen.display_id == display_id) =>
                    {
                        return Err(catalog
                            .screen_error
                            .unwrap_or_else(|| {
                                format!("未找到 ScreenCaptureKit 显示器 {display_id}")
                            })
                            .into());
                    }
                    _ => {}
                }
            }
            config.video_source = Some(source);
        }
    }

    // 输出帧率:前端可选 15/25/30/60,这里仍按 1–120 兜底校验(IPC 入参不可信)。
    let video_fps = config.video_fps.unwrap_or(25);
    if !(1..=120).contains(&video_fps) {
        return Err("输出帧率必须在 1–120 之间".into());
    }

    let device_id = DeviceId::new(config.device_id.clone())?;
    // 通道 ID:设备 ID 前 17 位 + 132(视频通道类型码)。
    let channel_id_str = format!("{}132", &config.device_id[..17]);
    let channel_id = DeviceId::new(channel_id_str)?;
    let gb_version = if config.gb_version == "2016" {
        GbVersion::V2016
    } else {
        GbVersion::V2022
    };
    // 传输由用户选择;Via 里的标识由传输对象自己给出,不会与实际发送通道不一致。
    let transport = if config.transport.eq_ignore_ascii_case("TCP") {
        Transport::Tcp
    } else {
        Transport::Udp
    };
    let channel_name = if config.channel_name.is_empty() {
        "Camera-1".to_string()
    } else {
        config.channel_name.clone()
    };

    let cfg = DeviceConfig {
        device_id: device_id.clone(),
        username: config.device_id.clone(),
        password: common::Secret::new(password),
        server_host: config.server_host.clone(),
        server_port: config.server_port,
        server_domain: config.server_domain.clone(),
        inbound_identity_mode: config.inbound_identity_mode,
        transport,
        heartbeat_interval_secs: config.heartbeat_interval.unwrap_or(60).max(1),
        register_expires_secs: config.expires.unwrap_or(3600).max(1),
        // §9.1.1 c):注册重试间隔不应短于 60s(单设备联调遵守标准)。
        register_retry_max_secs: gb28181_simulator::DEFAULT_REGISTER_RETRY_MAX_SECS,
        channels: vec![ChannelConfig {
            channel_id,
            name: channel_name,
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: if config.channel_name.is_empty() {
                "UVP-Sim-Desktop".to_string()
            } else {
                config.channel_name.clone()
            },
            manufacturer: non_empty(config.manufacturer.as_deref())
                .unwrap_or("UVP")
                .into(),
            model: non_empty(config.model.as_deref())
                .unwrap_or("Desktop-Sim")
                .into(),
            firmware: non_empty(config.firmware.as_deref())
                .unwrap_or(env!("CARGO_PKG_VERSION"))
                .into(),
        },
        video_source: config.video_source.clone(),
        video_fps,
        light_bitrate_kbps: None,
        gb_version,
        signaling_encoding: common::SignalingEncoding::from_str_lenient(&config.signaling_encoding),
    };

    // 预热视频源:容器(MP4 等)归一化转码可能耗时数秒,若留到 INVITE 时同步做会阻塞
    // 200 OK 与首包推流,导致平台收流超时。这里在设备上线前先转好、缓存,
    // INVITE 时命中缓存瞬时加载。ffmpeg 是阻塞调用,放 spawn_blocking。
    // 实时采集(live:)无文件可预热,跳过;仅对文件源(C 档容器)做归一化预热。
    // 帧率必须与设备配置一致:归一化产物按帧率缓存,预热错帧率等于白转。
    if let Some(ref vs) = config.video_source {
        if !vs.trim().is_empty() && !vs.trim().starts_with("live:") {
            let vs = vs.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                gb28181_simulator::prepare_video_source_at(&vs, video_fps)
            })
            .await
            .map_err(|error| IpcError::internal(format!("视频源准备任务异常: {error}")))?;
            prepared?;
        }
    }

    // 共享传输 + 本端地址发现。可信来源在接收/accept 循环启动前原子安装，
    // 避免 TCP bind 后再设白名单时，竞态窗口中的恶意连接被长期保留。
    let trusted_ips = vec![platform_addr.ip()];
    let udp: Arc<dyn SipTransport> = match transport {
        Transport::Udp => UdpTransport::bind_with_trusted_ips("0.0.0.0:0", trusted_ips).await?,
        Transport::Tcp => TcpTransport::bind_with_trusted_ips("0.0.0.0:0", trusted_ips).await?,
    };
    let local_port = udp.local_addr()?.port();
    let local_host = discover_local_ip(&platform_addr.to_string());

    // 单设备联调默认开启 SIP 信令追踪(FR-43),把收发报文推给前端。
    udp.set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));

    // 带状态观察者的设备实例。
    let observer: Arc<dyn DeviceObserver> = Arc::new(StateEmitter { app: app.clone() });
    let sim = Arc::new(DeviceSimulator::with_observer(cfg, observer));

    // 若指定了目录模板,在注册前载入(平台注册后会立即同步目录,需在这之前准备好)。
    if !config.catalog_template.is_empty() {
        sim.load_catalog_template(&config.catalog_template);
    }

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let sim_run = sim.clone();
    let tp_run = udp.clone();
    let host_run = local_host.clone();
    let run_task = tokio::spawn(async move {
        sim_run
            .run(tp_run, host_run, local_port, async move {
                let _ = stop_rx.await;
            })
            .await;
    });

    *guard = Some(DeviceHandle {
        sim,
        transport: udp,
        local_host,
        local_port,
        stop_tx,
        run_task,
    });
    Ok("设备已启动".into())
}

/// 停设备并**等到下线序列真的走完**(带超时)。返回是否停了一台设备。
///
/// 与 `stop_device` 命令的区别:命令要立刻返回让界面响应,这里要等 ——
/// 应用退出时必须先让平台收到注销,否则平台侧那台设备会挂着直到心跳超时。
pub(crate) async fn stop_device_graceful(state: &AppState) -> bool {
    let handle = state.device.lock().await.take();
    let Some(handle) = handle else {
        return false;
    };
    let DeviceHandle {
        transport,
        stop_tx,
        mut run_task,
        ..
    } = handle;
    let _ = stop_tx.send(());
    if await_or_abort_task("设备下线", &mut run_task).await {
        tracing::info!("设备下线序列已完成");
    }
    // 即使任务被 abort，future drop 会先释放媒体源；随后关传输释放 socket。
    transport.shutdown();
    true
}

/// 停止当前设备。
#[tauri::command]
pub(crate) async fn stop_device(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> IpcResult<String> {
    let mut guard = state.device.lock().await;
    match guard.take() {
        Some(h) => {
            let DeviceHandle {
                transport,
                stop_tx,
                mut run_task,
                ..
            } = h;
            let _ = stop_tx.send(());
            let _ = app.emit("device_state", "Disconnected");
            // 界面要立刻响应,所以在后台等下线；超时必须 abort+await，不能 detach
            // 仍持有 LiveSource/FFmpeg 的任务。
            tokio::spawn(async move {
                await_or_abort_task("设备停止", &mut run_task).await;
                transport.shutdown();
            });
            Ok("设备已停止".into())
        }
        None => Err(IpcError::device_not_running("没有正在运行的设备")),
    }
}

/// 主动上报一条报警（支持指定国标 13 种视频报警类型与优先级）。
#[tauri::command]
pub(crate) async fn fire_alarm(
    description: Option<String>,
    alarm_type: Option<u32>,
    priority: Option<u8>,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            let desc = description
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "移动侦测报警".to_string());
            let vtype = match alarm_type.unwrap_or(2) {
                1 => gb28181_protocol::manscdp::VideoAlarmType::Manual,
                2 => gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
                3 => gb28181_protocol::manscdp::VideoAlarmType::ObjectLeft,
                4 => gb28181_protocol::manscdp::VideoAlarmType::ObjectRemoved,
                5 => gb28181_protocol::manscdp::VideoAlarmType::TripwireCrossing,
                6 => gb28181_protocol::manscdp::VideoAlarmType::Intrusion,
                7 => gb28181_protocol::manscdp::VideoAlarmType::WrongWay,
                8 => gb28181_protocol::manscdp::VideoAlarmType::Loitering,
                9 => gb28181_protocol::manscdp::VideoAlarmType::FlowStatistics,
                10 => gb28181_protocol::manscdp::VideoAlarmType::DensityDetection,
                11 => gb28181_protocol::manscdp::VideoAlarmType::VideoAbnormal,
                12 => gb28181_protocol::manscdp::VideoAlarmType::FastMoving,
                13 => gb28181_protocol::manscdp::VideoAlarmType::Tampering,
                _ => gb28181_protocol::manscdp::VideoAlarmType::MotionDetection,
            };
            let code = h
                .sim
                .report_alarm_full(
                    &h.transport,
                    &h.local_host,
                    h.local_port,
                    &desc,
                    vtype,
                    priority,
                )
                .await?;
            if code == 0 {
                Ok("报警已触发，但因不满足平台订阅筛选条件已被本端过滤".to_string())
            } else {
                Ok(format!("报警上报完成(平台响应 {code})"))
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

/// 当前语音广播会话状态(§9.12.1)。无会话时返回 null。
///
/// 走轮询而不是事件流:收流统计每 20ms 就变一次,推事件等于给 webview 灌洪水;
/// 会话的开始/结束本身有 `platform_command` 事件,轮询只补统计与电平。
#[tauri::command]
pub(crate) async fn get_broadcast_status(
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<gb28181_simulator::BroadcastSnapshot>> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => Ok(h.sim.broadcast_snapshot().await),
        // 设备没启动不是错误:界面就该显示"空闲"。
        None => Ok(None),
    }
}

/// 当前推流会话的媒体遥测(发送统计 + 平台回的 RTCP 接收报告)。
///
/// 无推流会话、或会话刚建立发送通道还没就绪时返回 null。
/// 同样走轮询:RTCP RR 每 5 秒才来一条,但发送包数是每帧在变。
#[tauri::command]
pub(crate) async fn get_media_telemetry(
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<gb28181_simulator::MediaTelemetrySnapshot>> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => Ok(h.sim.media_telemetry().await),
        None => Ok(None),
    }
}

/// 设备侧主动挂断语音广播(按附录 K a) 发会话内 BYE)。
#[tauri::command]
pub(crate) async fn hangup_broadcast(state: tauri::State<'_, AppState>) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            if h.sim.hangup_broadcast(&h.transport).await {
                Ok("已挂断语音广播并通知平台".into())
            } else {
                Ok("当前没有进行中的语音广播".into())
            }
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}

/// 主动上报一条 GPS 位置。
#[tauri::command]
pub(crate) async fn fire_position(
    longitude: f64,
    latitude: f64,
    state: tauri::State<'_, AppState>,
) -> IpcResult<String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            let code = h
                .sim
                .report_position(
                    &h.transport,
                    &h.local_host,
                    h.local_port,
                    longitude,
                    latitude,
                )
                .await?;
            Ok(format!("位置上报完成(平台响应 {code})"))
        }
        None => Err(IpcError::device_not_running("设备未启动")),
    }
}
