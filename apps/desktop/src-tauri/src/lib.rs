//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

use std::sync::Arc;

use common::{DeviceEvent, DeviceId, DeviceObserver, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use serde::Deserialize;
use sip_core::UdpTransport;
use tauri::ipc::Channel;
use tauri::{AppHandle, Emitter}; // Tauri 2 emit 需要 Emitter trait
use tokio::sync::Mutex;

/// 应用全局状态(托管在 Tauri managed state)。
struct AppState {
    /// 单设备:当前设备实例 + 停止发射端 + 共享传输。
    device: Mutex<Option<DeviceHandle>>,
}

/// 单设备运行句柄。
///
/// `transport` / `local_host` / `local_port` 在 v2 首波清场后暂无 IPC 调用者(报警/位置/
/// trace 开关已删),但 v2 后续要拆媒体 IPC + 对讲 + 主动通知重接,仍会用到,先保留。
#[allow(dead_code)]
struct DeviceHandle {
    sim: Arc<DeviceSimulator>,
    transport: Arc<UdpTransport>,
    local_host: String,
    local_port: u16,
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// 后台任务是否已退出(正常结束或 panic)。
    ///
    /// 后台任务拿不到 Tauri managed state,无法自己清理容器,故用共享标志对账:
    /// 任务退出时置 true,命令侧据此把容器复位。不这样做的话,注册失败后
    /// `device` 容器永远是 `Some`,`start_device` 的 `is_some()` 会拦死重试。
    exited: Arc<std::sync::atomic::AtomicBool>,
    /// 预览转发任务句柄(broadcast::Receiver → Tauri Channel)。
    /// 前端订阅时 spawn,取消或设备停止时 abort。
    preview_task: Option<tokio::task::JoinHandle<()>>,
}

impl AppState {
    fn new() -> Self {
        AppState {
            device: Mutex::new(None),
        }
    }
}

/// 发送 `device_state` 事件。payload 为 `{ state, error }` 结构化对象:
/// 失败原因必须随状态一起送到前端,否则界面只能显示"注册失败"却说不出为什么。
fn emit_device_state(app: &AppHandle, state: &str, error: Option<String>) {
    let _ = app.emit(
        "device_state",
        serde_json::json!({ "state": state, "error": error }),
    );
}

/// 把设备事件映射成 `device_state` 事件,推给前端(状态灯 + 失败原因)。
struct StateEmitter {
    app: AppHandle,
}

impl DeviceObserver for StateEmitter {
    fn on_event(&self, event: DeviceEvent) {
        let state = match event {
            DeviceEvent::RegisterAttempt => "Registering",
            DeviceEvent::RegisterSuccess => "Registered",
            // 注册失败:带上原因分类,前端常驻展示。
            //
            // 注意 FailureKind 只有超时/被拒/其它三档,拿不到平台返回的原始
            // 401 reason-phrase —— 要更细的原因得改 common::DeviceEvent 的
            // 契约(设备层目前也没把它传出来),不在本期范围。
            DeviceEvent::RegisterFailure(kind) => {
                let reason = match kind {
                    common::FailureKind::Timeout => "注册超时:平台无响应(检查 IP/端口/防火墙)",
                    common::FailureKind::Rejected => "平台拒绝注册(检查服务器 ID/域/密码)",
                    common::FailureKind::Other => "注册失败:网络或其它错误",
                };
                emit_device_state(&self.app, "Failed", Some(reason.into()));
                return;
            }
            DeviceEvent::StreamStart => "InCall",
            DeviceEvent::StreamStop => "Registered",
            // 心跳事件不改变状态灯。
            DeviceEvent::HeartbeatOk | DeviceEvent::HeartbeatFail => return,
            // 云台控制:单独推 ptz_action 事件给前端做云台动画。
            DeviceEvent::Ptz {
                up,
                down,
                left,
                right,
                zoom_in,
                zoom_out,
                pan_speed,
                tilt_speed,
                zoom_speed,
            } => {
                let _ = self.app.emit(
                    "ptz_action",
                    serde_json::json!({
                        "up": up, "down": down, "left": left, "right": right,
                        "zoom_in": zoom_in, "zoom_out": zoom_out,
                        "pan_speed": pan_speed, "tilt_speed": tilt_speed, "zoom_speed": zoom_speed,
                    }),
                );
                return;
            }
            DeviceEvent::PtzPresetCall { preset } => {
                let _ = self.app.emit("ptz_preset", preset);
                return;
            }
            // 平台下发 OSD 配置命令,设备已应用:推给前端展示。
            DeviceEvent::OsdConfig {
                time_show,
                osd_show,
            } => {
                let _ = self.app.emit(
                    "osd_config",
                    serde_json::json!({ "time_show": time_show, "osd_show": osd_show }),
                );
                return;
            }
            // 平台命令语义事件 → 前端"平台命令时间线"。
            DeviceEvent::PlatformCommand { kind, summary } => {
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = self.app.emit(
                    "platform_command",
                    serde_json::json!({ "kind": kind, "summary": summary, "ts_ms": ts_ms }),
                );
                return;
            }
            // 订阅状态变化 → 前端"活跃订阅面板"。
            DeviceEvent::SubscriptionChanged {
                kind,
                active,
                notify_count,
            } => {
                let _ = self.app.emit(
                    "subscription_state",
                    serde_json::json!({ "kind": kind, "active": active, "notify_count": notify_count }),
                );
                return;
            }
            // 长任务进度 → 前端进度条。
            DeviceEvent::Progress {
                kind,
                current,
                total,
                percent,
            } => {
                let _ = self.app.emit(
                    "task_progress",
                    serde_json::json!({ "kind": kind, "current": current, "total": total, "percent": percent }),
                );
                return;
            }
        };
        emit_device_state(&self.app, state, None);
    }
}

/// SIP 信令追踪观察者(FR-43):把每条收发报文结构化为 `sip_trace` 事件推给前端。
struct TraceEmitter {
    app: AppHandle,
}

impl sip_core::TraceObserver for TraceEmitter {
    fn on_trace(&self, trace: sip_core::SipTrace<'_>) {
        let (method, status, cseq, call_id, summary) = match trace.message {
            sip_core::SipMessage::Request(r) => (
                r.method.as_str().to_string(),
                None,
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("{} {}", r.method.as_str(), r.uri),
            ),
            sip_core::SipMessage::Response(r) => (
                "SIP/2.0".to_string(),
                Some(r.status),
                r.headers.cseq().map(str::to_string),
                r.headers.call_id().map(str::to_string),
                format!("SIP/2.0 {} {}", r.status, r.reason),
            ),
        };
        // 完整报文文本(供前端展开查看详情);body 可能是 GB18030,按 lossy 解码展示。
        let raw = {
            let bytes = match trace.message {
                sip_core::SipMessage::Request(r) => r.to_bytes(),
                sip_core::SipMessage::Response(r) => r.to_bytes(),
            };
            // GB18030 兼容 ASCII 头;中文 body 用 GB18030 解码更可读。
            let (text, _, _) = encoding_rs::GB18030.decode(&bytes);
            text.into_owned()
        };
        let entry = serde_json::json!({
            "ts_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            "direction": if matches!(trace.dir, sip_core::TraceDir::In) { "in" } else { "out" },
            "method": method,
            "status": status,
            "cseq": cseq,
            "call_id": call_id,
            "peer": trace.peer.to_string(),
            "summary": summary,
            "raw": raw,
        });
        let _ = self.app.emit("sip_trace", entry);
    }
}

// ── IPC 命令 ─────────────────────────────────────────────

/// 查询单设备运行状态(UI 切页后对账用)。
///
/// running 反映**后台任务是否真的还活着**,不只看容器是否 `Some`——
/// 否则设备失败退出后这里会一直报 running。
#[tauri::command]
async fn get_device_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut guard = state.device.lock().await;
    let running = !reap_if_exited(&mut guard);
    Ok(serde_json::json!({ "running": running }))
}

// ── 单设备联调命令 ────────────────────────────────────────

/// 前端传入的单设备配置。
#[derive(Deserialize)]
struct DeviceCfg {
    server_host: String,
    server_port: u16,
    server_domain: String,
    /// 平台 SIP ID(20 位)。留空回退用 `server_domain`,兼容旧前端调用。
    #[serde(default)]
    server_id: String,
    device_id: String,
    password: String,
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
}

/// 通过"向平台发起 UDP connect"发现本机对外 IP(不真正发包)。
fn discover_local_ip(server: &str) -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect(server)?;
            Ok(s.local_addr()?.ip().to_string())
        })
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 启动一台设备:注册 + 心跳 + 入站应答,后台常驻。状态经 `device_state` 事件推送。
#[tauri::command]
async fn start_device(
    config: DeviceCfg,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    // 先回收已退出的旧设备,注册失败后无需手工「注销」即可直接重试。
    if !reap_if_exited(&mut guard) {
        return Err("已有设备在运行,请先停止".into());
    }

    let device_id = DeviceId::new(config.device_id.clone()).map_err(|e| e.to_string())?;
    // 通道 ID:设备 ID 前 17 位 + 132(视频通道类型码)。
    // 切片用已校验的 device_id(DeviceId::new 保证 20 位数字),不依赖未校验的入参。
    let channel_id_str = format!("{}132", &device_id.as_str()[..17]);
    let channel_id = DeviceId::new(channel_id_str).map_err(|e| e.to_string())?;
    let gb_version = if config.gb_version == "2016" {
        GbVersion::V2016
    } else {
        GbVersion::V2022
    };
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
        password: config.password.clone(),
        server_host: config.server_host.clone(),
        server_port: config.server_port,
        server_domain: config.server_domain.clone(),
        server_id: config.server_id.clone(),
        transport,
        heartbeat_interval_secs: 60,
        channels: vec![ChannelConfig {
            channel_id,
            name: channel_name,
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: "UVP-Sim-Desktop".into(),
            manufacturer: "UVP".into(),
            model: "Desktop-Sim".into(),
            firmware: "0.1.0".into(),
        },
        video_source: config.video_source.clone(),
        video_fps: 25,
        light_bitrate_kbps: None,
        gb_version,
        signaling_encoding: common::SignalingEncoding::from_str_lenient(&config.signaling_encoding),
    };

    // 预热视频源(仅文件源):容器(MP4 等)转封装可能耗时数秒,若留到 INVITE 时同步做会
    // 阻塞 200 OK 与首包推流,导致平台收流超时。这里在设备上线前先转好、缓存,INVITE 时
    // 命中缓存瞬时加载。ffmpeg 是阻塞调用,放 spawn_blocking。
    // `live:*` 是运行时采集(摄像头/屏幕),没有可预热的文件,跳过。
    if let Some(ref vs) = config.video_source {
        let trimmed = vs.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("live:") {
            let vs = vs.clone();
            let prepared =
                tokio::task::spawn_blocking(move || gb28181_simulator::prepare_video_source(&vs))
                    .await
                    .map_err(|e| e.to_string())?;
            if let Err(e) = prepared {
                return Err(format!("视频源准备失败:{e}"));
            }
        }
    }

    // 共享 UDP 传输 + 本端地址发现。
    let udp = UdpTransport::bind("0.0.0.0:0")
        .await
        .map_err(|e| e.to_string())?;
    let local_port = udp.local_addr().map_err(|e| e.to_string())?.port();
    let local_host = discover_local_ip(&format!("{}:{}", config.server_host, config.server_port));

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
    // 退出标志:后台任务结束(含 panic)时置位,供命令侧对账清理容器。
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exited_run = exited.clone();
    let app_run = app.clone();
    tokio::spawn(async move {
        // JoinHandle 包一层:run() 内部 panic 时也要落到退出分支,
        // 否则容器永远 Some,用户改对参数也无法重试。
        let joined = tokio::spawn(async move {
            sim_run
                .run(tp_run, host_run, local_port, async move {
                    let _ = stop_rx.await;
                })
                .await;
        })
        .await;
        exited_run.store(true, std::sync::atomic::Ordering::SeqCst);
        // panic 要告知前端;正常退出不覆盖已有状态(可能是注册失败原因)。
        if joined.is_err() {
            emit_device_state(&app_run, "Failed", Some("设备任务异常退出(panic)".into()));
        }
    });

    *guard = Some(DeviceHandle {
        sim,
        transport: udp,
        local_host,
        local_port,
        stop_tx,
        exited,
        preview_task: None,
    });
    Ok("设备已启动".into())
}

/// 若后台任务已退出,清空设备容器并返回 true(表示当前无活设备)。
///
/// 所有读写 `device` 容器的命令都先走这一步对账,否则失败退出的设备会把
/// 容器永久占住:`get_device_status` 报假 running、`start_device` 拒绝重试。
fn reap_if_exited(guard: &mut Option<DeviceHandle>) -> bool {
    let dead = guard
        .as_ref()
        .is_some_and(|h| h.exited.load(std::sync::atomic::Ordering::SeqCst));
    if dead {
        *guard = None;
    }
    guard.is_none()
}

/// 停止当前设备。
#[tauri::command]
async fn stop_device(state: tauri::State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    match guard.take() {
        Some(mut h) => {
            if let Some(handle) = h.preview_task.take() {
                handle.abort();
            }
            let _ = h.stop_tx.send(());
            emit_device_state(&app, "Disconnected", None);
            Ok("设备已停止".into())
        }
        None => Err("没有正在运行的设备".into()),
    }
}

// ── 摄像头设备枚举 ──────────────────────────────────────

/// 摄像头设备条目(前端"摄像头"选择器用)。
#[derive(serde::Serialize)]
struct CameraDevice {
    /// 传给 `video_source` 的输入值(不含 `live:` 前缀,前端拼)。
    /// macOS 是 avfoundation 序号(如 "0");Windows 是 dshow 设备名;Linux 是 `/dev/videoN`。
    id: String,
    /// 展示名(设备型号/描述)。
    name: String,
}

/// 枚举本机摄像头设备。
///
/// 用 `ffmpeg -list_devices` 探测:macOS avfoundation / Windows dshow;Linux 直接扫 `/dev/video*`。
/// 无 ffmpeg 或探测失败时返回空列表(前端提示用户手填输入)。
#[tauri::command]
fn list_cameras() -> Vec<CameraDevice> {
    let Some(ffmpeg) = gb28181_simulator::ffmpeg_bin() else {
        return Vec::new();
    };
    if cfg!(target_os = "macos") {
        list_cameras_avfoundation(&ffmpeg)
    } else if cfg!(target_os = "windows") {
        list_cameras_dshow(&ffmpeg)
    } else {
        list_cameras_v4l2()
    }
}

/// macOS avfoundation:`ffmpeg -f avfoundation -list_devices true -i ""`
/// stderr 输出形如:
/// ```text
/// [AVFoundation indev @ 0x...] AVFoundation video devices:
/// [AVFoundation indev @ 0x...] [0] FaceTime HD Camera
/// [AVFoundation indev @ 0x...] [1] Capture screen 0
/// [AVFoundation indev @ 0x...] AVFoundation audio devices:
/// ```
/// 只取 video 段的行(遇到 audio 段头停止),且跳过屏幕采集条目(仅摄像头)。
fn list_cameras_avfoundation(ffmpeg: &str) -> Vec<CameraDevice> {
    let out = std::process::Command::new(ffmpeg)
        .args(["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut cams = Vec::new();
    let mut in_video = false;
    for line in stderr.lines() {
        // 定位视频设备段。
        if line.contains("AVFoundation video devices:") {
            in_video = true;
            continue;
        }
        if line.contains("AVFoundation audio devices:") {
            break; // 遇到音频段,视频段结束
        }
        if !in_video {
            continue;
        }
        // 解析 `[N] 名字`(前缀日志和方括号索引之间可能有空格)。
        let Some(idx_open) = line.rfind('[') else {
            continue;
        };
        let Some(idx_close_rel) = line[idx_open..].find(']') else {
            continue;
        };
        let idx = line[idx_open + 1..idx_open + idx_close_rel].to_string();
        // 只接受纯数字索引(排除 "AVFoundation indev @ 0x..." 那种前缀日志)。
        if !idx.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let name = line[idx_open + idx_close_rel + 1..].trim().to_string();
        if name.is_empty() {
            continue;
        }
        // 跳过屏幕采集条目(用户只想选摄像头)。
        if name.starts_with("Capture screen") {
            continue;
        }
        cams.push(CameraDevice { id: idx, name });
    }
    cams
}

/// Windows dshow:`ffmpeg -f dshow -list_devices true -i dummy`。
/// stderr 里每个视频设备为 `"<设备名>" (video)`;id 就是设备名本身(dshow 靠名字选)。
fn list_cameras_dshow(ffmpeg: &str) -> Vec<CameraDevice> {
    let out = std::process::Command::new(ffmpeg)
        .args(["-f", "dshow", "-list_devices", "true", "-i", "dummy"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut cams = Vec::new();
    for line in stderr.lines() {
        // 形如: [dshow @ 0x...]  "USB Camera" (video)
        if !line.contains("(video)") {
            continue;
        }
        // 取最后一对引号中间的内容。
        let Some(start) = line.find('"') else { continue };
        let rest = &line[start + 1..];
        let Some(end) = rest.find('"') else { continue };
        let name = &rest[..end];
        cams.push(CameraDevice {
            id: format!("video={name}"), // dshow 输入格式:`video=名字`
            name: name.to_string(),
        });
    }
    cams
}

/// Linux v4l2:扫 `/dev/video*`,读 `/sys/class/video4linux/<name>/name` 拿设备名。
fn list_cameras_v4l2() -> Vec<CameraDevice> {
    let mut cams = Vec::new();
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return cams;
    };
    let mut devs: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("video") && n[5..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    devs.sort();
    for dev in devs {
        let name = std::fs::read_to_string(format!("/sys/class/video4linux/{dev}/name"))
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| dev.clone());
        cams.push(CameraDevice {
            id: format!("/dev/{dev}"),
            name,
        });
    }
    cams
}

// ── 预览:H.264 帧旁路 → Tauri Channel ──────────────────

/// 推给前端的一帧 H.264 数据。
#[derive(Clone, serde::Serialize)]
struct PreviewFrame {
    /// H.264 Annex B(起始码 + NAL)。前端喂给 WebCodecs VideoDecoder。
    data: Vec<u8>,
    /// 是否关键帧(含 SPS/PPS/IDR)。前端首帧必须等到关键帧才能 configure。
    key: bool,
    /// 帧序号,前端可用来判断是否丢帧。
    seq: u32,
}

/// 订阅推流预览:每帧 H.264 数据通过 Channel 送前端。
///
/// 前端建一个 `Channel<PreviewFrame>` 传进来,后端从 CameraStream 拿 broadcast::Receiver,
/// spawn 一个 tokio 任务把帧转发到 Channel。任务会持有 Channel 与 Receiver 直到设备停止
/// 或取消订阅。每次调用都会替换掉旧订阅任务(前端页面重挂时自动重建)。
#[tauri::command]
async fn subscribe_preview(
    channel: Channel<PreviewFrame>,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    let h = guard.as_mut().ok_or("设备未启动")?;
    // 已有订阅任务:先停,再重开(避免多个任务并行给同一 channel 塞帧)。
    if let Some(handle) = h.preview_task.take() {
        handle.abort();
    }
    let cam = h.sim.camera_stream().ok_or("摄像头流未启动(非 live 源?)")?;
    let mut rx = cam.subscribe();

    // 后台任务:阻塞地收 broadcast,转发到前端 Channel。
    // Channel.send 是同步非阻塞;broadcast::recv 是 async。前端断开时 Channel.send 返回 Err
    // (但 Tauri Channel 目前不区分,失败就静默;任务靠 abort() 或 broadcast Close 结束)。
    let seq = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let handle = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    let n = seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let _ = channel.send(PreviewFrame {
                        data: frame.data,
                        key: frame.key_frame,
                        seq: n,
                    });
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // 消费太慢丢了老帧,继续拿新的。
                    continue;
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break, // 上游关了
            }
        }
    });
    h.preview_task = Some(handle);
    Ok("预览已订阅".into())
}

/// 取消预览订阅(停止转发任务)。
#[tauri::command]
async fn unsubscribe_preview(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    if let Some(h) = guard.as_mut() {
        if let Some(handle) = h.preview_task.take() {
            handle.abort();
        }
    }
    Ok("预览已取消".into())
}

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            start_device,
            stop_device,
            get_device_status,
            list_cameras,
            subscribe_preview,
            unsubscribe_preview,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}
