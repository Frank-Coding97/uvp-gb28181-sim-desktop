//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::{collections::HashSet, net::IpAddr};

use common::{DeviceEvent, DeviceId, DeviceObserver, Transport};
use gb28181_simulator::{
    ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator, RecordingEntry, RecordingKind,
    RecordingPhase, RecordingSource, RecordingState, RecordingStore,
};
use serde::Serialize;
use sip_core::SipTransport;
use stress_engine::{Metrics, Orchestrator};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle, Emitter, Manager,
}; // Tauri 2 emit/state 需要对应 trait
use tokio::sync::{broadcast, Mutex};

mod config_store;
mod preview_channel;
mod preview_manager;
use config_store::{
    BindMode, CatalogNodeConfig, ConfigStore, DesktopConfigV2, EffectiveDeviceConfig,
    StartDeviceInput,
};
use preview_channel::DesktopPreviewBus;
use preview_manager::{PreviewEventSink, PreviewForwardTarget, PreviewManager};

/// 应用全局状态（托管在 Tauri managed state）。
struct AppState {
    /// Rust 端配置真相源；路径在首次 Tauri 命令时由 app_data_dir 初始化。
    config_store: OnceLock<ConfigStore>,
    /// 应用数据根目录（配置 / 录像 / 升级状态共用）与它来自哪个候选。
    /// 解析一次即缓存：探测要真写文件，不该在每次启动设备时重复做。
    data_root: OnceLock<(std::path::PathBuf, String)>,
    /// 录像索引真相，设备停止后仍可查看历史文件。
    recording_store: OnceLock<Arc<RecordingStore>>,
    /// 配置文件读写串行化，避免并发保存与读取看到半次操作。
    config_io: Mutex<()>,
    /// Independent cancellation path: preparation holds the device startup lock.
    media_preparation: std::sync::Mutex<Option<Arc<AtomicBool>>>,
    shutting_down: AtomicBool,
    /// 当前压测句柄；任务完全退出前始终占用槽位。
    stress: Mutex<Option<StressHandle>>,
    /// 分配单调递增的压测 ID,防止旧任务事件污染新任务状态。
    next_stress_id: AtomicU64,
    /// 当前压测的实时指标；None 表示任务已完全退出。
    metrics: Mutex<Option<Arc<Metrics>>>,
    /// 最近一次压测的最终指标快照(停止后保留,供导出报告)。
    last_metrics: Mutex<Option<stress_engine::MetricsSnapshot>>,
    /// 最近一次启动使用的脱敏场景 TOML 与设备数（报告导出用）。
    last_scenario: Mutex<Option<(String, usize)>>,
    /// 单设备联调：当前设备实例 + 停止发射端 + 共享传输。
    device: Mutex<Option<DeviceHandle>>,
    /// 采集侧 MJPEG 旁路的最新帧总线。
    preview_bus: Arc<DesktopPreviewBus>,
    /// 应用级唯一预览 Channel 管理器；页面只 attach/detach。
    preview_manager: Arc<PreviewManager>,
}

/// 压测运行句柄。收到停止请求后仅标记 `stopping`，由后台任务退出时释放。
struct StressHandle {
    run_id: u64,
    stop_tx: broadcast::Sender<()>,
    stopping: bool,
    device_count: usize,
}

/// 单设备运行句柄。
struct DeviceHandle {
    sim: Arc<DeviceSimulator>,
    transport: Arc<SipTransport>,
    local_host: String,
    local_port: u16,
    stop_tx: tokio::sync::oneshot::Sender<()>,
    effective_config: EffectiveDeviceConfig,
}

/// Reserve startup before awaiting the device lock, and keep cancellation live
/// until REGISTER is committed. Dropping an unsuccessful startup releases it.
struct DeviceStartup<'a> {
    state: &'a AppState,
    cancel: Arc<AtomicBool>,
}

impl DeviceStartup<'_> {
    fn commit(&self, register: impl FnOnce()) -> Result<(), String> {
        let mut pending = self
            .state
            .media_preparation
            .lock()
            .map_err(|_| "媒体准备状态锁异常")?;
        if self.cancel.load(Ordering::Acquire) || self.state.shutting_down.load(Ordering::Acquire) {
            return Err("设备启动已取消或应用正在退出".into());
        }
        if !pending
            .as_ref()
            .is_some_and(|token| Arc::ptr_eq(token, &self.cancel))
        {
            return Err("设备启动状态已失效".into());
        }
        register();
        *pending = None;
        Ok(())
    }
}

impl Drop for DeviceStartup<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.state.media_preparation.lock() {
            if pending
                .as_ref()
                .is_some_and(|token| Arc::ptr_eq(token, &self.cancel))
            {
                *pending = None;
            }
        }
    }
}

impl AppState {
    fn begin_device_start(&self) -> Result<DeviceStartup<'_>, String> {
        let mut pending = self
            .media_preparation
            .lock()
            .map_err(|_| "媒体准备状态锁异常")?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("应用正在退出".into());
        }
        if pending.is_some() {
            return Err("设备正在启动，请等待或取消当前启动".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *pending = Some(cancel.clone());
        Ok(DeviceStartup {
            state: self,
            cancel,
        })
    }

    fn ensure_config_writable(&self, device_running: bool) -> Result<(), String> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err("应用正在退出，不能修改配置".into());
        }
        if device_running {
            return Err("设备运行中，不能修改配置；请先停止设备".into());
        }
        Ok(())
    }

    fn cancel_file_preparation(&self) -> Result<(), String> {
        let preparation = self
            .media_preparation
            .lock()
            .map_err(|_| "媒体准备状态锁异常")?;
        let cancel = preparation
            .as_ref()
            .ok_or("文件准备已结束，当前没有可取消的启动")?;
        cancel.store(true, Ordering::Release);
        Ok(())
    }

    fn new() -> Self {
        let preview_bus = Arc::new(DesktopPreviewBus::new());
        let preview_manager = Arc::new(PreviewManager::new(Arc::clone(&preview_bus)));
        AppState {
            config_store: OnceLock::new(),
            data_root: OnceLock::new(),
            recording_store: OnceLock::new(),
            config_io: Mutex::new(()),
            media_preparation: std::sync::Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            stress: Mutex::new(None),
            next_stress_id: AtomicU64::new(1),
            metrics: Mutex::new(None),
            last_metrics: Mutex::new(None),
            last_scenario: Mutex::new(None),
            device: Mutex::new(None),
            preview_bus,
            preview_manager,
        }
    }
}

fn app_recording_store(state: &AppState, app: &AppHandle) -> Result<Arc<RecordingStore>, String> {
    if let Some(store) = state.recording_store.get() {
        return Ok(Arc::clone(store));
    }
    // 与配置同一个根目录：这是「注册设备」失败的那一处，不能直接拼 app_data_dir()。
    let directory = app_data_root(state, app)?.join("recordings");
    let store = Arc::new(
        RecordingStore::open(&directory)
            .map_err(|error| format!("打开录像目录 {} 失败: {error}", directory.display()))?,
    );
    let _ = state.recording_store.set(store);
    Ok(Arc::clone(
        state
            .recording_store
            .get()
            .expect("RecordingStore 初始化后必须存在"),
    ))
}

/// 依次探测候选目录，返回第一个**真的能写入**的目录。
///
/// 直接信任 `app_data_dir()` 是不够的。2026-11-05 在装了企业 VDI/DLP 客户端的
/// 开发机上实测：`C:\Users\<user>\AppData` 下 `create_dir_all` 成功，但创建任何
/// 文件都被拒绝（os error 5），`%TEMP%` 同样如此，而工程所在的 D 盘可写；目录 ACL
/// 只有三条允许项、没有拒绝项，受控文件夹访问关闭、也没有 ASR 规则——是外部安全
/// 产品按进程拦截。这种情况下 `app_data_dir()` 本身不报错，错误会一路拖到第一次
/// 落盘，变成一句无法行动的“创建配置目录失败”。
///
/// 所以这里用真实写入探测，并把每个候选的失败原因汇总进最终错误，让下一次再遇到
/// 类似环境时能直接看出是哪一层挡住、还有哪些位置可用。
fn resolve_writable_dir(
    candidates: Vec<(String, std::path::PathBuf)>,
) -> Result<(std::path::PathBuf, String), String> {
    let mut failures = Vec::new();
    for (label, dir) in candidates {
        let probe = dir.join(".uvp-write-probe");
        let attempt = std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&probe, b"probe"))
            .and_then(|()| std::fs::remove_file(&probe));
        match attempt {
            Ok(()) => return Ok((dir, label)),
            Err(error) => failures.push(format!("{label} {} -> {error}", dir.display())),
        }
    }
    Err(format!("没有可写的应用数据目录：{}", failures.join("; ")))
}

/// 应用数据根目录：配置、录像、升级状态**三者共用**的一个真能写入的目录。
///
/// 之所以要共用：2026-11-05 在装了企业 VDI/DLP 的开发机上，`app_data_dir()` 建目录
/// 可能成功、写文件必被拒（os error 5）。当初只把**配置**换成了可写探测，录像与
/// 升级状态仍直接拼 `app_data_dir()`，于是「新增配置」好了、「注册设备」仍然失败，
/// 而且报出来的是 `网络错误: 拒绝访问。 (os error 5)`——一个文件错误被贴上了网络标签。
/// 三处必须走同一条解析，否则修一处漏两处。
fn app_data_root(state: &AppState, app: &AppHandle) -> Result<std::path::PathBuf, String> {
    if let Some((dir, _)) = state.data_root.get() {
        return Ok(dir.clone());
    }

    let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
    // 显式覆盖优先：受限环境里开发或部署时可以直接把数据目录挪到可写的位置。
    if let Some(explicit) = std::env::var_os("UVP_CONFIG_DIR") {
        candidates.push((
            "UVP_CONFIG_DIR".to_string(),
            std::path::PathBuf::from(explicit),
        ));
    }
    for (label, resolved) in [
        ("app_data_dir", app.path().app_data_dir()),
        ("app_local_data_dir", app.path().app_local_data_dir()),
    ] {
        if let Ok(dir) = resolved {
            candidates.push((label.to_string(), dir));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            candidates.push(("exe_dir/data".to_string(), parent.join("data")));
        }
    }

    let (directory, source) = resolve_writable_dir(candidates)?;
    tracing::info!(
        target: "uvp::config",
        "数据目录: {}（来源 {source}）",
        directory.display()
    );
    let _ = state.data_root.set((directory.clone(), source));
    Ok(directory)
}

fn app_config_store(state: &AppState, app: &AppHandle) -> Result<ConfigStore, String> {
    if let Some(store) = state.config_store.get() {
        return Ok(store.clone());
    }

    let directory = app_data_root(state, app)?;
    let store = ConfigStore::new(directory.join("desktop-config-v1.json"));
    let _ = state.config_store.set(store);
    Ok(state
        .config_store
        .get()
        .expect("ConfigStore 初始化后必须存在")
        .clone())
}

fn control_state_file(state: &AppState, app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(app_data_root(state, app)?.join("device-control-state-v1.json"))
}

fn load_control_state(
    sim: &gb28181_simulator::DeviceSimulator,
    state: &AppState,
    app: &AppHandle,
) {
    let Ok(path) = control_state_file(state, app) else {
        return;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    match serde_json::from_str::<gb28181_simulator::PersistedControlState>(&raw) {
        Ok(saved) => {
            sim.import_persisted_control_state(&saved);
            tracing::info!(target: "uvp::device", path = %path.display(), "已恢复设备控制状态");
        }
        Err(error) => {
            tracing::warn!(target: "uvp::device", %error, path = %path.display(), "设备控制状态文件损坏，忽略");
        }
    }
}

fn save_control_state(
    sim: &gb28181_simulator::DeviceSimulator,
    state: &AppState,
    app: &AppHandle,
) {
    let Ok(path) = control_state_file(state, app) else {
        return;
    };
    let saved = sim.export_persisted_control_state();
    match serde_json::to_vec_pretty(&saved) {
        Ok(bytes) => {
            if let Err(error) = std::fs::write(&path, bytes) {
                tracing::warn!(target: "uvp::device", %error, path = %path.display(), "设备控制状态写入失败");
            }
        }
        Err(error) => {
            tracing::warn!(target: "uvp::device", %error, "设备控制状态序列化失败");
        }
    }
}

/// 把设备事件映射成 `device_state` 字符串,通过 Tauri 事件推给前端(状态灯)。
struct StateEmitter {
    app: AppHandle,
}

fn camera_capture_guidance(message: &str) -> String {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("[camera_screen_locked]") {
        return "[camera_screen_locked] Mac 当前处于锁屏状态，系统不会向模拟器交付摄像头画面。请解锁 Mac 后回到首页重新注册设备。".into();
    }
    if normalized.contains("not authorized")
        || normalized.contains("permission denied")
        || normalized.contains("camera access denied")
        || normalized.contains("tcc")
    {
        return format!(
            "[camera_permission_denied] 摄像头权限未授权。请前往“系统设置 → 隐私与安全性 → 摄像头”，允许 UVP GB28181 Sim，然后回到首页重新注册设备。原始错误：{message}"
        );
    }
    if normalized.contains("device or resource busy")
        || normalized.contains("already in use")
        || normalized.contains("cannot use device")
    {
        return format!(
            "[camera_in_use] 摄像头正被其他采集会话占用，请先停止其他相机应用后重试。原始错误：{message}"
        );
    }
    message.to_string()
}

/// 语音广播/对讲下行的 PCM 输出。
///
/// 设备侧收流解码得到 8kHz 单声道 PCM,这里原样转成 `broadcast_audio` 事件;
/// 前端用 Web Audio 建一个 8kHz AudioContext 排程播放(不引入原生音频依赖)。
struct BroadcastAudioEmitter {
    app: AppHandle,
}

impl gb28181_simulator::BroadcastAudioSink for BroadcastAudioEmitter {
    fn on_pcm(&self, pcm: &[i16], sample_rate: u32) {
        let _ = self.app.emit(
            "broadcast_audio",
            serde_json::json!({ "sample_rate": sample_rate, "pcm": pcm }),
        );
    }
}

impl DeviceObserver for StateEmitter {
    fn on_event(&self, event: DeviceEvent) {
        let state = match event {
            DeviceEvent::RegisterAttempt => "Registering",
            DeviceEvent::RegisterSuccess => "Registered",
            DeviceEvent::RegisterFailure { message, .. } => {
                // 注册失败原先只推给前端、不落日志，排查时只能看到界面上那一句话。
                tracing::warn!(target: "uvp::device", "注册失败: {message}");
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = self.app.emit(
                    "device_error",
                    serde_json::json!({ "scope": "register", "message": message, "ts_ms": ts_ms }),
                );
                "Failed"
            }
            DeviceEvent::CaptureStarting => {
                let _ = self.app.emit("capture_state", "starting");
                return;
            }
            DeviceEvent::CaptureReady => {
                let _ = self.app.emit("capture_state", "ready");
                return;
            }
            DeviceEvent::CaptureStopped => {
                let _ = self.app.emit("capture_state", "stopped");
                return;
            }
            DeviceEvent::CaptureFailure(message) => {
                let message = camera_capture_guidance(&message);
                let _ = self.app.emit("capture_state", "error");
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = self.app.emit(
                    "device_error",
                    serde_json::json!({ "scope": "capture", "message": message, "ts_ms": ts_ms }),
                );
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
            DeviceEvent::PtzPresetCall { preset, name, pose } => {
                let _ = self.app.emit(
                    "ptz_preset",
                    serde_json::json!({
                        "id": preset,
                        "name": name,
                        "pose": pose.map(|(pan, tilt, zoom)| serde_json::json!({
                            "pan": pan, "tilt": tilt, "zoom": zoom,
                        })),
                    }),
                );
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
            // 语音广播(对讲下行)→ 前端状态指示 + 音频播放。
            DeviceEvent::BroadcastStarted {
                channel_id,
                source_id,
                transport,
                local_port,
            } => {
                let _ = self.app.emit(
                    "broadcast_state",
                    serde_json::json!({
                        "active": true,
                        "channel_id": channel_id,
                        "source_id": source_id,
                        "transport": transport,
                        "local_port": local_port,
                    }),
                );
                return;
            }
            DeviceEvent::BroadcastProgress { packets, bytes } => {
                let _ = self.app.emit(
                    "broadcast_progress",
                    serde_json::json!({ "packets": packets, "bytes": bytes }),
                );
                return;
            }
            DeviceEvent::BroadcastEnded {
                reason,
                packets,
                bytes,
            } => {
                let _ = self.app.emit(
                    "broadcast_state",
                    serde_json::json!({
                        "active": false,
                        "reason": reason,
                        "packets": packets,
                        "bytes": bytes,
                    }),
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
            // 运行时错误(如点播采集失败)→ 前端全局错误条。
            DeviceEvent::RuntimeError { scope, message } => {
                tracing::warn!(target: "uvp::device", "运行期错误[{scope}]: {message}");
                let ts_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let _ = self.app.emit(
                    "device_error",
                    serde_json::json!({ "scope": scope, "message": message, "ts_ms": ts_ms }),
                );
                return;
            }
        };
        let _ = self.app.emit("device_state", state);
    }
}

/// SIP 信令追踪观察者(FR-43):把每条收发报文结构化为 `sip_trace` 事件推给前端。
struct TraceEmitter {
    app: AppHandle,
}

fn format_sip_trace_log(direction: &str, peer: &str, raw: &str) -> String {
    format!("SIP {direction} {peer}\n{raw}")
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
        let direction = if matches!(trace.dir, sip_core::TraceDir::In) {
            "in"
        } else {
            "out"
        };
        let peer = trace.peer.to_string();
        tracing::info!(target: "sip_trace", "{}", format_sip_trace_log(direction, &peer, &raw));
        let entry = serde_json::json!({
            "ts_ms": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64).unwrap_or(0),
            "direction": direction,
            "method": method,
            "status": status,
            "cseq": cseq,
            "call_id": call_id,
            "peer": peer,
            "summary": summary,
            "raw": raw,
        });
        let _ = self.app.emit("sip_trace", entry);
    }
}

// ── IPC 命令 ─────────────────────────────────────────────

/// 引擎连通性自检（M0 已有，保留）。
#[tauri::command]
fn engine_version() -> String {
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
}

#[derive(Clone, Debug, Serialize)]
struct LocalIpOption {
    address: String,
    interface_name: String,
    address_family: &'static str,
    loopback: bool,
}

fn normalize_local_ip_options(addresses: Vec<(String, IpAddr)>) -> Vec<LocalIpOption> {
    let mut seen = HashSet::new();
    let mut options: Vec<_> = addresses
        .into_iter()
        .filter(|(_, address)| !address.is_unspecified() && !address.is_multicast())
        .filter(|(_, address)| seen.insert(*address))
        .map(|(interface_name, address)| LocalIpOption {
            address: address.to_string(),
            interface_name,
            address_family: if address.is_ipv4() { "IPv4" } else { "IPv6" },
            loopback: address.is_loopback(),
        })
        .collect();
    options.sort_by(|left, right| {
        left.loopback
            .cmp(&right.loopback)
            .then_with(|| (left.address_family == "IPv6").cmp(&(right.address_family == "IPv6")))
            .then_with(|| left.interface_name.cmp(&right.interface_name))
            .then_with(|| left.address.cmp(&right.address))
    });
    options
}

#[tauri::command]
fn list_local_ip_addresses() -> Result<Vec<LocalIpOption>, String> {
    let addresses = if_addrs::get_if_addrs()
        .map_err(|error| format!("读取本机网卡地址失败: {error}"))?
        .into_iter()
        .map(|interface| {
            let address = interface.ip();
            (interface.name, address)
        })
        .collect();
    Ok(normalize_local_ip_options(addresses))
}

#[tauri::command]
async fn get_desktop_config(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let store = app_config_store(&state, &app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.load())
        .await
        .map_err(|error| format!("读取配置任务异常: {error}"))?
}

#[tauri::command]
async fn save_desktop_config(
    config: DesktopConfigV2,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let device = state.device.lock().await;
    state.ensure_config_writable(device.is_some())?;
    let store = app_config_store(&state, &app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.save(&config))
        .await
        .map_err(|error| format!("保存配置任务异常: {error}"))?
}

#[tauri::command]
async fn save_media_config(
    media: media_rtp::profile::MediaProfile,
    expected_revision: u64,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let device = state.device.lock().await;
    state.ensure_config_writable(device.is_some())?;
    let store = app_config_store(&state, &app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.save_media(media, expected_revision))
        .await
        .map_err(|error| format!("保存媒体配置任务异常: {error}"))?
}

#[tauri::command]
async fn save_osd_config(
    osd: media_rtp::osd::OsdSettings,
    expected_revision: u64,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let device = state.device.lock().await;
    state.ensure_config_writable(device.is_some())?;
    let store = app_config_store(&state, &app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.save_osd(osd, expected_revision))
        .await
        .map_err(|error| format!("保存 OSD 配置任务异常: {error}"))?
}

/// OSD 能否烧进码流：FFmpeg 带 drawtext/drawbox，且有中文字体。
#[derive(serde::Serialize)]
struct OsdCapability {
    ffmpeg: bool,
    filters: bool,
    font: Option<String>,
}

#[tauri::command]
async fn get_osd_capability() -> Result<OsdCapability, String> {
    tokio::task::spawn_blocking(|| {
        let ffmpeg = media_rtp::ffmpeg_bin();
        OsdCapability {
            ffmpeg: ffmpeg.is_some(),
            filters: ffmpeg
                .as_deref()
                .is_some_and(media_rtp::osd::ffmpeg_supports_osd),
            font: media_rtp::osd::font_file().map(|path| path.display().to_string()),
        }
    })
    .await
    .map_err(|error| format!("检测 OSD 能力任务异常: {error}"))
}

#[tauri::command]
async fn reset_desktop_config(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let device = state.device.lock().await;
    state.ensure_config_writable(device.is_some())?;
    let store = app_config_store(&state, &app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.reset())
        .await
        .map_err(|error| format!("重置配置任务异常: {error}"))?
}

/// 校验场景 TOML 并返回摘要，不启动压测。
#[tauri::command]
async fn validate_scenario(toml: String) -> Result<ScenarioSummary, String> {
    let sc = scenario::LinearScenario::from_toml_str(&toml).map_err(|e| e.to_string())?;
    // 生成一台设备可触发所有语义校验,避免“语法正确、启动才失败”。
    Orchestrator::new(&sc, 1).map_err(|e| e.to_string())?;
    Ok(ScenarioSummary {
        name: sc.device_info.device_name.clone(),
        device_count: 0, // 由前端传 count 参数
        server_host: sc.server_host.clone(),
        server_port: sc.server_port,
    })
}

/// 报告中保留可复现场景,但基于解析后的结构体覆盖密码后再序列化，
/// 避免 quoted key、多行字符串等 TOML 语法绕过文本脱敏。
fn serialize_redacted_scenario(mut scenario: scenario::LinearScenario) -> Result<String, String> {
    scenario.password = "***".into();
    toml::to_string(&scenario).map_err(|e| format!("脱敏场景序列化失败: {e}"))
}

/// 启动压测。`toml` 为场景内容，`count` 为设备数。
/// 成功后每秒向前端发送带 `run_id` 的 `metrics_tick` 事件。
#[tauri::command]
async fn start_stress(
    toml: String,
    count: usize,
    position_interval: u64,
    alarm_interval: u64,
    state: tauri::State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<String, String> {
    if position_interval > 86_400 || alarm_interval > 86_400 {
        return Err("主动上报间隔不能超过 86400 秒".into());
    }

    let sc = scenario::LinearScenario::from_toml_str(&toml).map_err(|e| e.to_string())?;
    let redacted_scenario = serialize_redacted_scenario(sc.clone())?;
    let orch = Orchestrator::new(&sc, count).map_err(|e| e.to_string())?;

    let mut stress_guard = state.stress.lock().await;
    if stress_guard.is_some() {
        return Err("已有压测正在运行或停止中，请等待任务完全退出".into());
    }

    let run_id = state.next_stress_id.fetch_add(1, Ordering::Relaxed);
    let (tx, _) = broadcast::channel::<()>(1);
    // 必须在向前端暴露运行态前创建接收器，确保“启动后立即停止”不会丢信号。
    let run_stop = tx.subscribe();
    let tick_stop = tx.subscribe();
    *stress_guard = Some(StressHandle {
        run_id,
        stop_tx: tx.clone(),
        stopping: false,
        device_count: count,
    });

    let metrics = Arc::clone(&orch.metrics);
    *state.metrics.lock().await = Some(Arc::clone(&metrics));
    *state.last_metrics.lock().await = None;
    *state.last_scenario.lock().await = Some((redacted_scenario, count));

    // 先发布运行事件再启动后台任务，避免初始化瞬时失败时“最终事件”被旧启动事件覆盖。
    let _ = app.emit(
        "stress_state",
        serde_json::json!({
            "run_id": run_id,
            "running": true,
            "stopping": false,
            "device_count": count,
        }),
    );
    drop(stress_guard);

    // 每秒推送带运行 ID 的指标快照，前端据此隔离迟到事件。
    let app_for_tick = app.clone();
    let metrics_for_tick = Arc::clone(&metrics);
    tokio::spawn(async move {
        let mut tick_stop = tick_stop;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick_stop.recv() => break,
                _ = interval.tick() => {
                    let _ = app_for_tick.emit(
                        "metrics_tick",
                        serde_json::json!({
                            "run_id": run_id,
                            "metrics": metrics_for_tick.snapshot(),
                        }),
                    );
                }
            }
        }
    });

    // 压测任务结束后保存最终快照并回收槽位；停止请求不会提前释放运行权。
    let app_for_run = app.clone();
    let metrics_for_run = Arc::clone(&metrics);
    let tx_for_run = tx.clone();
    tokio::spawn(async move {
        let result = orch
            .run_with_stop_receiver(
                tx_for_run.clone(),
                run_stop,
                position_interval,
                alarm_interval,
            )
            .await;
        // 初始化失败时也要关闭指标推送任务。
        let _ = tx_for_run.send(());

        let state = app_for_run.state::<AppState>();
        let final_metrics = metrics_for_run.snapshot();
        let mut current = state.stress.lock().await;
        let owns_state = current
            .as_ref()
            .is_some_and(|handle| handle.run_id == run_id);
        if !owns_state {
            return;
        }
        let was_stopping = current.as_ref().is_some_and(|handle| handle.stopping);

        // 在仍持有本轮生命周期所有权时提交全部共享状态，最后才释放运行槽位。
        // last_metrics 先写、metrics 后清，确保报告能力不会出现瞬时空窗。
        *state.last_metrics.lock().await = Some(final_metrics.clone());
        state.metrics.lock().await.take();
        current.take();
        drop(current);

        let payload = match result {
            Ok(()) => serde_json::json!({
                "run_id": run_id,
                "running": false,
                "stopping": false,
                "reason": if was_stopping { "stopped" } else { "completed" },
                "device_count": count,
                "metrics": final_metrics,
            }),
            Err(error) => {
                tracing::error!(%error, run_id, "压测任务异常结束");
                serde_json::json!({
                    "run_id": run_id,
                    "running": false,
                    "stopping": false,
                    "reason": "failed",
                    "error": error.to_string(),
                    "device_count": count,
                    "metrics": final_metrics,
                })
            }
        };
        let _ = app_for_run.emit("stress_state", payload);
    });

    Ok(format!("已启动 {count} 台设备压测"))
}

/// 停止当前压测。只发送信号并进入 stopping 状态，运行槽位由后台任务退出时释放。
#[tauri::command]
async fn stop_stress(state: tauri::State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let mut stress_guard = state.stress.lock().await;
    match stress_guard.as_mut() {
        Some(handle) if handle.stopping => Ok("压测正在停止，请稍候".into()),
        Some(handle) => {
            handle.stopping = true;
            let _ = handle.stop_tx.send(());
            // 在持有生命周期锁时发布，保证后台最终事件一定排在 stopping 事件之后。
            let _ = app.emit(
                "stress_state",
                serde_json::json!({
                    "run_id": handle.run_id,
                    "running": true,
                    "stopping": true,
                    "reason": "stopping",
                    "device_count": handle.device_count,
                }),
            );
            Ok("压测停止信号已发送，正在等待设备任务退出".into())
        }
        None => Err("没有正在运行的压测".into()),
    }
}

/// 查询当前指标快照（供按需轮询；实时更新通过 metrics_tick 事件）。
#[tauri::command]
async fn get_metrics(state: tauri::State<'_, AppState>) -> Result<String, String> {
    match &*state.metrics.lock().await {
        Some(m) => serde_json::to_string(&m.snapshot()).map_err(|e| e.to_string()),
        None => Err("无正在运行的压测".into()),
    }
}

/// 查询压测状态，供 UI 页面切换或 IPC 返回后与引擎真实状态对账。
#[tauri::command]
async fn get_stress_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    // 生命周期变更也持有此锁；在锁内读取关联字段可得到同一运行代际的快照。
    let stress_guard = state.stress.lock().await;
    let (running, stopping, run_id, device_count) = match stress_guard.as_ref() {
        Some(handle) => (
            true,
            handle.stopping,
            Some(handle.run_id),
            handle.device_count,
        ),
        None => {
            let count = state
                .last_scenario
                .lock()
                .await
                .as_ref()
                .map(|(_, count)| *count)
                .unwrap_or(0);
            (false, false, None, count)
        }
    };
    let has_report =
        state.metrics.lock().await.is_some() || state.last_metrics.lock().await.is_some();
    drop(stress_guard);
    Ok(serde_json::json!({
        "running": running,
        "stopping": stopping,
        "run_id": run_id,
        "device_count": device_count,
        "has_report": has_report,
    }))
}

/// 查询单设备运行状态(UI 切页后对账用)。running=设备实例是否存在。
#[tauri::command]
async fn get_device_status(state: tauri::State<'_, AppState>) -> Result<serde_json::Value, String> {
    let device = state.device.lock().await;
    let (running, capture_state, effective_config) = match device.as_ref() {
        Some(handle) => {
            let ready = handle.sim.capture_ready().await;
            (
                true,
                if ready { "ready" } else { "starting" },
                Some(handle.effective_config.clone()),
            )
        }
        None => (false, "stopped", None),
    };
    Ok(serde_json::json!({
        "running": running,
        "capture_state": capture_state,
        "effective_config": effective_config,
    }))
}

#[derive(Debug, Serialize)]
struct DeviceRuntimeStateDto {
    guarded: bool,
    alarming: bool,
    longitude: f64,
    latitude: f64,
    name: String,
    expiration: u32,
    heartbeat_interval: u32,
    heartbeat_count: u32,
    video_record_plan_type: Option<u32>,
    alarm_record_duration: Option<u32>,
    picture_mask_enabled: Option<u32>,
    frame_mirror_mode: Option<u32>,
    alarm_report_enabled: Option<u32>,
    osd_time_show: Option<u32>,
    osd_show: Option<u32>,
    last_change: String,
    updated_at_ms: u64,
}

impl From<gb28181_simulator::DeviceRuntimeSnapshot> for DeviceRuntimeStateDto {
    fn from(snapshot: gb28181_simulator::DeviceRuntimeSnapshot) -> Self {
        Self {
            guarded: snapshot.guarded,
            alarming: snapshot.alarming,
            longitude: snapshot.longitude,
            latitude: snapshot.latitude,
            name: snapshot.name,
            expiration: snapshot.expiration,
            heartbeat_interval: snapshot.heartbeat_interval,
            heartbeat_count: snapshot.heartbeat_count,
            video_record_plan_type: snapshot.video_record_plan_type,
            alarm_record_duration: snapshot.alarm_record_duration,
            picture_mask_enabled: snapshot.picture_mask_enabled,
            frame_mirror_mode: snapshot.frame_mirror_mode,
            alarm_report_enabled: snapshot.alarm_report_enabled,
            osd_time_show: snapshot.osd_time_show,
            osd_show: snapshot.osd_show,
            last_change: snapshot.last_change,
            updated_at_ms: snapshot.updated_at_ms,
        }
    }
}

#[cfg(test)]
mod runtime_state_tests {
    use super::DeviceRuntimeStateDto;

    #[test]
    fn runtime_state_dto保留设备状态真相字段() {
        let dto = DeviceRuntimeStateDto::from(gb28181_simulator::DeviceRuntimeSnapshot {
            guarded: true,
            alarming: false,
            longitude: 116.397428,
            latitude: 39.909230,
            name: "桌面模拟器".into(),
            expiration: 7200,
            heartbeat_interval: 15,
            heartbeat_count: 5,
            video_record_plan_type: Some(2),
            alarm_record_duration: Some(30),
            picture_mask_enabled: Some(1),
            frame_mirror_mode: Some(0),
            alarm_report_enabled: Some(1),
            osd_time_show: Some(1),
            osd_show: Some(0),
            last_change: "device_config_applied".into(),
            updated_at_ms: 123,
        });
        let value = serde_json::to_value(dto).unwrap();
        assert_eq!(value["guarded"], true);
        assert_eq!(value["alarming"], false);
        assert_eq!(value["heartbeat_count"], 5);
        assert_eq!(value["last_change"], "device_config_applied");
    }
}

/// 查询当前设备实例的会话级运行时状态真相。
#[tauri::command]
async fn get_device_runtime_state(
    state: tauri::State<'_, AppState>,
) -> Result<DeviceRuntimeStateDto, String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动")?;
    Ok(handle.sim.runtime_snapshot().into())
}

/// 模拟器中心一次性只读快照：姿态、预置位姿态、看守位、巡航、扫描、存储卡等。
#[tauri::command]
async fn get_device_control_state(
    state: tauri::State<'_, AppState>,
) -> Result<gb28181_simulator::ControlSnapshot, String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动")?;
    Ok(handle.sim.control_snapshot())
}

/// 查询四类订阅的当前状态；页面刷新/重新进入时补一次基线。
#[tauri::command]
async fn get_device_subscriptions(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<gb28181_simulator::SubscriptionSnapshot>, String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动")?;
    Ok(handle.sim.subscription_snapshot().await)
}

/// 前端姿态积分结果回写；预置位设置/扫描边界以这份姿态为准。
#[tauri::command]
async fn report_ptz_pose(
    pan: f32,
    tilt: f32,
    zoom: f32,
    focus: f32,
    iris: f32,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动")?;
    handle.sim.set_local_pose(pan, tilt, zoom, focus, iris);
    Ok(())
}

/// 导出压测报告(FR-28):把场景参数 + 当前指标快照写成 JSON 文件,返回路径。
/// 文件落到系统临时目录下的 uvp-reports/report-<时间戳>.json。
#[tauri::command]
async fn export_report(state: tauri::State<'_, AppState>) -> Result<String, String> {
    // 与启动/完成提交共用生命周期锁，避免混合读取不同 run 的场景和指标。
    let _stress_guard = state.stress.lock().await;
    // 优先取运行中指标,否则取最近一次压测保留的快照。
    let snap = match &*state.metrics.lock().await {
        Some(m) => m.snapshot(),
        None => match &*state.last_metrics.lock().await {
            Some(s) => s.clone(),
            None => return Err("无压测数据可导出(尚未运行过压测)".into()),
        },
    };
    let scenario = state.last_scenario.lock().await.clone();

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let report = serde_json::json!({
        "generated_at_epoch_secs": ts,
        "scenario_toml_redacted": scenario.as_ref().map(|(t, _)| t),
        "device_count": scenario.as_ref().map(|(_, c)| c),
        "metrics": snap,
    });

    let dir = std::env::temp_dir().join("uvp-reports");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建报告目录失败: {e}"))?;
    let path = dir.join(format!("report-{ts}.json"));
    let content = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(&path, content).map_err(|e| format!("写报告失败: {e}"))?;

    Ok(path.to_string_lossy().to_string())
}

// ── 单设备联调命令 ────────────────────────────────────────

#[derive(Serialize)]
struct LiveScreenDto {
    display_id: u32,
    width: u32,
    height: u32,
    name: String,
    uri: String,
}

#[derive(Serialize)]
struct LiveAvDeviceDto {
    index: u32,
    name: String,
}

#[derive(Serialize)]
struct LiveCameraModeDto {
    width: u32,
    height: u32,
    min_fps: f64,
    max_fps: f64,
    format: String,
}

impl From<media_rtp::LiveCameraMode> for LiveCameraModeDto {
    fn from(mode: media_rtp::LiveCameraMode) -> Self {
        Self {
            width: mode.width,
            height: mode.height,
            min_fps: mode.min_fps,
            max_fps: mode.max_fps,
            format: mode.format,
        }
    }
}

#[derive(Serialize)]
struct LiveSourceCatalogDto {
    ffmpeg_available: bool,
    aac_available: bool,
    opus_available: bool,
    screens: Vec<LiveScreenDto>,
    cameras: Vec<LiveAvDeviceDto>,
    microphones: Vec<LiveAvDeviceDto>,
    screen_error: Option<String>,
    avfoundation_error: Option<String>,
}

impl From<media_rtp::LiveSourceCatalog> for LiveSourceCatalogDto {
    fn from(catalog: media_rtp::LiveSourceCatalog) -> Self {
        Self {
            ffmpeg_available: catalog.ffmpeg_available,
            aac_available: catalog.aac_available,
            opus_available: catalog.opus_available,
            screens: catalog
                .screens
                .into_iter()
                .map(|screen| LiveScreenDto {
                    display_id: screen.display_id,
                    width: screen.width,
                    height: screen.height,
                    name: screen.name,
                    uri: screen.uri,
                })
                .collect(),
            cameras: catalog
                .cameras
                .into_iter()
                .map(|device| LiveAvDeviceDto {
                    index: device.index,
                    name: device.name,
                })
                .collect(),
            microphones: catalog
                .microphones
                .into_iter()
                .map(|device| LiveAvDeviceDto {
                    index: device.index,
                    name: device.name,
                })
                .collect(),
            screen_error: catalog.screen_error,
            avfoundation_error: catalog.avfoundation_error,
        }
    }
}

#[derive(Serialize)]
struct LiveProbeDto {
    uri: String,
    video_ready: bool,
    audio_requested: bool,
    audio_ready: Option<bool>,
    message: String,
}

/// 枚举可用于规范实时源 URI 的摄像头、麦克风和显示器。
#[tauri::command]
async fn list_live_sources() -> Result<LiveSourceCatalogDto, String> {
    let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
        .await
        .map_err(|error| format!("实时源枚举任务异常: {error}"))?
        .map_err(|error| error.to_string())?;
    // 枚举依赖 FFmpeg 的 DirectShow/AVFoundation 后端。FFmpeg 找不到时 catalog 是空的
    // 且只有错误字段，界面上表现为“摄像头下拉框没有内容”。日志里留下判定依据，
    // 免得再靠猜。
    tracing::info!(
        ffmpeg = %catalog.ffmpeg_path.as_deref().unwrap_or("<未找到>"),
        cameras = catalog.cameras.len(),
        microphones = catalog.microphones.len(),
        screens = catalog.screens.len(),
        device_error = catalog.avfoundation_error.as_deref().unwrap_or("-"),
        "实时采集设备枚举完成"
    );
    Ok(LiveSourceCatalogDto::from(catalog))
}

/// 查询某台摄像头实际支持的分辨率与帧率。
///
/// 音视频设置页据此限制可选分辨率/帧率：设备做不到的档位不该出现在下拉框里，
/// 否则用户只能在启动采集时才吃到一个 DirectShow 错误。
#[tauri::command]
async fn list_camera_modes(camera_index: u32) -> Result<Vec<LiveCameraModeDto>, String> {
    let modes = tokio::task::spawn_blocking(move || media_rtp::list_camera_modes(camera_index))
        .await
        .map_err(|error| format!("摄像头能力查询任务异常: {error}"))?
        .map_err(|error| error.to_string())?;
    tracing::info!(camera_index, modes = modes.len(), "摄像头采集模式枚举完成");
    Ok(modes.into_iter().map(LiveCameraModeDto::from).collect())
}

/// 启动一次短时采集探测。请求音频时，用户应确保麦克风或系统正在产生声音。
#[tauri::command]
async fn probe_live_source(
    uri: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<LiveProbeDto, String> {
    let spec = media_rtp::LiveSourceSpec::parse(uri.trim()).map_err(|error| error.to_string())?;
    let canonical_uri = uri.trim().to_string();
    let is_screen = matches!(&spec, media_rtp::LiveSourceSpec::Screen { .. });
    let audio_requested = match spec {
        media_rtp::LiveSourceSpec::Camera { audio_index, .. } => audio_index.is_some(),
        media_rtp::LiveSourceSpec::Screen { audio, .. } => audio.is_enabled(),
    };
    let store = app_config_store(&state, &app)?;
    let profile = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || store.load())
            .await
            .map_err(|error| format!("读取采集配置失败: {error}"))??
            .media
    };
    tokio::task::spawn_blocking(move || {
        use media_rtp::VideoSource;

        let mut source = media_rtp::LiveSource::capture_with_profile(&canonical_uri, profile, None)
            .map_err(|error| error.to_string())?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut video_ready = false;
        let mut audio_ready = !audio_requested;
        while std::time::Instant::now() < deadline {
            video_ready |= source.next_frame().is_some();
            if audio_requested {
                audio_ready |= !source.next_audio().is_empty();
            }
            if video_ready && audio_ready {
                break;
            }
            if let Some(error) = source.take_error() {
                return Err(error);
            }
            if !source.is_live() {
                return Err("实时采集在探测完成前停止".to_string());
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let message = if video_ready && audio_ready {
            "视频与请求的音频采集正常".to_string()
        } else if video_ready && audio_requested {
            "视频采集正常，但未检测到音频；请播放系统声音或对麦克风说话后重试".to_string()
        } else if is_screen {
            "未检测到屏幕画面，请检查是否有其他程序占用桌面复制，或显示器是否已断开".to_string()
        } else {
            "未检测到视频帧，请检查设备占用和系统摄像头权限".to_string()
        };
        Ok(LiveProbeDto {
            uri: canonical_uri,
            video_ready,
            audio_requested,
            audio_ready: audio_requested.then_some(audio_ready),
            message,
        })
    })
    .await
    .map_err(|error| format!("实时采集探测任务异常: {error}"))?
}

struct TauriPreviewTarget(Channel<InvokeResponseBody>);

impl PreviewForwardTarget for TauriPreviewTarget {
    fn send(&self, bytes: Vec<u8>) -> Result<(), ()> {
        self.0.send(InvokeResponseBody::Raw(bytes)).map_err(|_| ())
    }
}

struct TauriPreviewEvents(AppHandle);

impl PreviewEventSink for TauriPreviewEvents {
    fn publish_status(&self, status: &media_rtp::PreviewStatus) {
        let _ = self.0.emit("preview_status", status);
        if status.phase == media_rtp::PreviewPhase::Unavailable {
            let _ = self.0.emit(
                "preview_error",
                status
                    .reason
                    .clone()
                    .unwrap_or_else(|| "预览 worker 当前不可用".into()),
            );
        }
    }
}

#[derive(Serialize)]
struct PreviewAttachDto {
    token: u64,
    status: media_rtp::PreviewStatus,
}

/// WebView2 can report document.visibilityState=visible while its native window is minimized.
#[tauri::command]
async fn is_preview_window_visible(window: tauri::WebviewWindow) -> Result<bool, String> {
    window
        .is_minimized()
        .map(|minimized| !minimized)
        .map_err(|error| error.to_string())
}

/// 页面挂载应用级唯一预览通道。重复调用会原子替换当前目标，不会重启媒体。
#[tauri::command]
async fn start_binary_preview(
    channel: Channel<InvokeResponseBody>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<PreviewAttachDto, String> {
    let target: Arc<dyn PreviewForwardTarget> = Arc::new(TauriPreviewTarget(channel));
    let events: Arc<dyn PreviewEventSink> = Arc::new(TauriPreviewEvents(app));
    let token = state.preview_manager.attach(target, events).await;
    Ok(PreviewAttachDto {
        token,
        status: state.preview_bus.latest_status(),
    })
}

#[tauri::command]
async fn stop_binary_preview(
    token: u64,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    Ok(if state.preview_manager.detach(token) {
        "预览页面已解除挂载"
    } else {
        "预览页面已过期，无需解除"
    }
    .into())
}

#[tauri::command]
async fn retry_binary_preview(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let guard = state.device.lock().await;
    let Some(device) = guard.as_ref() else {
        return Err("设备尚未启动".into());
    };
    if device.sim.retry_preview().await {
        Ok("正在重建本地预览，摄像头采集保持运行".into())
    } else {
        Err("当前媒体源没有可重试的摄像头预览".into())
    }
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

#[tauri::command]
fn cancel_media_preparation(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.cancel_file_preparation()
}

/// 启动一台设备:注册 + 心跳 + 入站应答,后台常驻。状态经 `device_state` 事件推送。
#[tauri::command]
async fn start_device(
    input: StartDeviceInput,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let startup = state.begin_device_start()?;
    let mut guard = state.device.lock().await;
    if state.shutting_down.load(Ordering::Acquire) {
        return Err("应用正在退出".into());
    }
    if guard.is_some() {
        return Err("已有设备在运行,请先停止".into());
    }

    let store = app_config_store(&state, &app)?;
    let persisted = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || store.load())
            .await
            .map_err(|error| format!("读取配置任务异常: {error}"))??
    };
    let mut resolved = persisted.resolve_start(input)?;
    // 信令传输由 profile.transport 决定:UDP 走共享 socket,TCP 走共享连接池。
    // 两者都在下面按模式创建,不再在此处拦截 TCP。
    if let Some(source) = resolved.video_source.take() {
        if source.starts_with("live:") {
            let spec = media_rtp::LiveSourceSpec::parse(&source)
                .map_err(|error| format!("实时媒体源配置无效: {error}"))?;
            let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
                .await
                .map_err(|error| format!("实时设备枚举任务异常: {error}"))?
                .map_err(|error| format!("实时设备枚举失败: {error}"))?;
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
                    return Err("当前内嵌 FFmpeg 不支持 libopus 编码，请改用 G.711A/G.711U".into());
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
                        .unwrap_or_else(|| format!("未找到摄像头索引 {video_index}")));
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
                        .unwrap_or_else(|| format!("未找到麦克风索引 {index}")));
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
                        .unwrap_or_else(|| format!("未找到麦克风索引 {index}")));
                }
                media_rtp::LiveSourceSpec::Screen { display_id, .. }
                    if display_id != 0
                        && !catalog
                            .screens
                            .iter()
                            .any(|screen| screen.display_id == display_id) =>
                {
                    return Err(catalog.screen_error.unwrap_or_else(|| {
                        format!("未找到显示器 {display_id}")
                    }));
                }
                _ => {}
            }
        }
        resolved.video_source = Some(source);
    }

    let device_id = DeviceId::new(resolved.device.device_id.clone()).map_err(|e| e.to_string())?;
    // 通道 ID:设备 ID 前 17 位 + 132(视频通道类型码)。
    let channel_id_str = format!("{}132", &resolved.device.device_id[..17]);
    let channel_id = DeviceId::new(channel_id_str).map_err(|e| e.to_string())?;

    let cfg = DeviceConfig {
        device_id: device_id.clone(),
        username: resolved.device.device_id.clone(),
        password: resolved.password.clone(),
        server_host: resolved.profile.server_host.clone(),
        server_port: resolved.profile.server_port,
        server_id: resolved.profile.server_id.clone(),
        server_domain: resolved.profile.server_domain.clone(),
        transport: resolved.profile.transport,
        audio_transport: resolved.profile.audio_transport,
        register_expires_secs: resolved.device.register_expires_secs,
        heartbeat_interval_secs: resolved.device.heartbeat_interval_secs,
        heartbeat_fail_threshold: resolved.device.heartbeat_fail_threshold,
        channels: vec![ChannelConfig {
            channel_id,
            name: resolved.device.channel_name.clone(),
            status: "ON".into(),
        }],
        device_info: DeviceInfo {
            device_name: resolved.device.device_name.clone(),
            manufacturer: resolved.device.manufacturer.clone(),
            model: resolved.device.model.clone(),
            firmware: resolved.device.firmware.clone(),
        },
        video_source: resolved.video_source.clone(),
        video_fps: resolved.media.video_fps,
        media_profile: Some(resolved.media.clone()),
        light_bitrate_kbps: None,
        gb_version: resolved.profile.gb_version,
        signaling_encoding: resolved.profile.signaling_encoding,
    };

    // Prepare the immutable output before REGISTER; INVITE only subscribes to it.
    let prepared_file = if let Some(path) = resolved
        .video_source
        .as_ref()
        .filter(|path| !path.trim().is_empty() && !path.starts_with("live:"))
    {
        let cancel = startup.cancel.clone();
        if state.shutting_down.load(Ordering::Acquire) {
            cancel.store(true, Ordering::Release);
        }
        let _ = app.emit(
            "media_preparation",
            serde_json::json!({"phase":"preparing", "message":"正在准备文件音视频"}),
        );
        let prepare_app = app.clone();
        let source_path = std::path::PathBuf::from(path);
        let profile = resolved.media.clone();
        let task_cancel = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            let progress = move |progress: media_rtp::file_profile::FilePreparationProgress| {
                use media_rtp::file_profile::FilePreparationPhase;
                let message = match progress.phase {
                    FilePreparationPhase::HashingSource => "正在检查源文件",
                    FilePreparationPhase::CheckingCache => "正在检查文件缓存",
                    FilePreparationPhase::Transcoding => "正在按所选参数转换音视频",
                    FilePreparationPhase::Extracting => "正在建立音视频索引",
                    FilePreparationPhase::Probing => "正在验证输出音视频",
                    FilePreparationPhase::Publishing => "正在保存准备结果",
                    FilePreparationPhase::Complete => "文件音视频准备完成",
                };
                let _ = prepare_app.emit(
                    "media_preparation",
                    serde_json::json!({"phase":"preparing", "message":message}),
                );
            };
            media_rtp::file_profile::prepare_file_profile(
                &source_path,
                &profile,
                &task_cancel,
                Some(&progress),
            )
        })
        .await;
        let result = result
            .map_err(|error| format!("文件准备任务异常: {error}"))
            .and_then(|result| result);
        if cancel.load(Ordering::Acquire) {
            let _ = app.emit(
                "media_preparation",
                serde_json::json!({"phase":"cancelled", "message":"已取消文件准备"}),
            );
            return Err("已取消文件准备".into());
        }
        match result {
            Ok(prepared) => {
                let _ = app.emit(
                    "media_preparation",
                    serde_json::json!({"phase":"ready", "message":"文件音视频准备完成"}),
                );
                Some(prepared)
            }
            Err(error) => {
                let _ = app.emit(
                    "media_preparation",
                    serde_json::json!({"phase":"failed", "message":error}),
                );
                return Err(format!("视频源准备失败: {error}"));
            }
        }
    } else {
        None
    };

    if state.shutting_down.load(Ordering::Acquire) {
        return Err("应用正在退出".into());
    }
    // 共享信令承载 + 本端地址发现。
    //
    // 两种传输都是"一套承载带全部虚拟设备":
    // - UDP:一个 socket,按 Call-ID/AOR 路由;
    // - TCP:一个连接池,设备按 ID 哈希固定分配到某条连接。
    // 因此 FD 占用与设备数量无关,这是万级压测能跑起来的前提。
    let local_host = match resolved.network.bind_mode {
        BindMode::Auto => discover_local_ip(&format!(
            "{}:{}",
            resolved.profile.server_host, resolved.profile.server_port
        )),
        BindMode::Specific => resolved.network.bind_address.clone(),
    };
    let transport = match resolved.profile.transport {
        Transport::Udp => {
            let bind_host = match resolved.network.bind_mode {
                BindMode::Auto => "0.0.0.0".to_string(),
                BindMode::Specific => resolved.network.bind_address.clone(),
            };
            SipTransport::bind_udp(&format!("{bind_host}:0")).await
        }
        Transport::Tcp => {
            let peer = format!(
                "{}:{}",
                resolved.profile.server_host, resolved.profile.server_port
            );
            let connections = resolved.profile.signaling_tcp_connections.max(1) as usize;
            SipTransport::connect_tcp(&peer, connections).await
        }
    }
    .map_err(|e| e.to_string())?;
    let local_port = transport.local_addr().map_err(|e| e.to_string())?.port();

    if resolved.network.sip_trace {
        transport.set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));
    }

    // 带状态观察者的设备实例。
    let observer: Arc<dyn DeviceObserver> = Arc::new(StateEmitter { app: app.clone() });
    let upgrade_state_dir = app_data_root(&state, &app)?.join("upgrade-state");
    let sim = Arc::new(DeviceSimulator::with_observer_and_upgrade_state_dir(
        cfg,
        observer,
        upgrade_state_dir,
    ));
    if let Some(prepared) = prepared_file {
        sim.install_prepared_file(prepared)?;
    }
    load_control_state(sim.as_ref(), &state, &app);
    sim.install_recording_store(app_recording_store(&state, &app)?);
    sim.set_preview_sink(Some(
        state.preview_bus.clone() as Arc<dyn media_rtp::PreviewSink>
    ));
    // 单设备实时采集时把 OSD 烧进码流；文件源在模拟器内部自动跳过。
    sim.set_osd_settings(Some(resolved.osd.clone()));
    // 语音广播/对讲下行的 PCM 输出:转成 Tauri 事件,由前端 Web Audio 播放。
    sim.set_broadcast_audio_sink(Arc::new(BroadcastAudioEmitter { app: app.clone() }));

    // 持久化目录树优先；首次使用时才回退到启动表单指定模板。
    // 平台注册后可能立即查询目录，必须在 run() 前准备好。
    if !resolved.catalog_tree.is_empty() {
        let nodes: Vec<_> = resolved
            .catalog_tree
            .iter()
            .map(CatalogNodeConfig::to_protocol_node)
            .collect::<Result<_, _>>()?;
        sim.set_catalog_tree(nodes)?;
    } else if !resolved.catalog_template.is_empty() {
        sim.load_catalog_template(&resolved.catalog_template);
    }

    let effective_config = EffectiveDeviceConfig {
        media: resolved.media.clone(),
        profile: resolved.profile.clone(),
        device: resolved.device.clone(),
        network: resolved.network.clone(),
        local_host: local_host.clone(),
        local_port,
        video_source: resolved.video_source.clone(),
        catalog_template: resolved.catalog_template.clone(),
        capabilities: Default::default(),
    };

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let sim_run = sim.clone();
    let tp_run = transport.clone();
    let host_run = local_host.clone();
    startup.commit(|| {
        tokio::spawn(async move {
            sim_run
                .run(tp_run, host_run, local_port, async move {
                    let _ = stop_rx.await;
                })
                .await;
        });

        *guard = Some(DeviceHandle {
            sim,
            transport,
            local_host,
            local_port,
            stop_tx,
            effective_config,
        });
    })?;
    Ok("设备已启动".into())
}

#[derive(Debug, Serialize)]
struct RecordingEntryDto {
    id: String,
    channel_id: String,
    start_time_ms: u64,
    end_time_ms: u64,
    start_time: String,
    end_time: String,
    source: RecordingSource,
    kind: RecordingKind,
    path: String,
    size_bytes: u64,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    audio_sample_rate_hz: Option<u32>,
}

impl From<RecordingEntry> for RecordingEntryDto {
    fn from(entry: RecordingEntry) -> Self {
        Self {
            id: entry.id,
            channel_id: entry.channel_id,
            start_time_ms: entry.start_time_ms,
            end_time_ms: entry.end_time_ms,
            start_time: entry.start_time,
            end_time: entry.end_time,
            source: entry.source,
            kind: entry.kind,
            path: entry.path.to_string_lossy().into_owned(),
            size_bytes: entry.size_bytes,
            video_codec: entry.video_codec,
            audio_codec: entry.audio_codec,
            audio_sample_rate_hz: entry.audio_sample_rate_hz,
        }
    }
}

#[tauri::command]
async fn get_recordings(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<Vec<RecordingEntryDto>, String> {
    Ok(app_recording_store(&state, &app)?
        .list()
        .into_iter()
        .map(Into::into)
        .collect())
}

#[tauri::command]
async fn get_recording_state(state: tauri::State<'_, AppState>) -> Result<RecordingState, String> {
    let device = state.device.lock().await;
    Ok(device
        .as_ref()
        .and_then(|handle| handle.sim.recording_service())
        .map(|service| service.state())
        .unwrap_or_default())
}

#[tauri::command]
async fn start_recording(state: tauri::State<'_, AppState>) -> Result<RecordingState, String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动，不能录像")?;
    handle
        .sim
        .start_recording(RecordingKind::Manual, RecordingSource::Local)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn stop_recording(
    state: tauri::State<'_, AppState>,
) -> Result<Option<RecordingEntryDto>, String> {
    let device = state.device.lock().await;
    let handle = device.as_ref().ok_or("设备未启动")?;
    handle
        .sim
        .stop_recording()
        .await
        .map(|entry| entry.map(Into::into))
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_recording(
    id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<bool, String> {
    if id.trim().is_empty() {
        return Err("录像 ID 不能为空".into());
    }
    let device = state.device.lock().await;
    if let Some(service) = device
        .as_ref()
        .and_then(|handle| handle.sim.recording_service())
    {
        let current = service.state();
        if current.active_id.as_deref() == Some(id.as_str())
            && !matches!(current.phase, RecordingPhase::Idle | RecordingPhase::Failed)
        {
            return Err("录像正在写入或收尾，不能删除".into());
        }
    }
    drop(device);
    app_recording_store(&state, &app)?
        .delete(&id)
        .map(|entry| entry.is_some())
        .map_err(|error| error.to_string())
}

/// 停止当前设备。
#[tauri::command]
async fn stop_device(state: tauri::State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    match guard.take() {
        Some(h) => {
            save_control_state(h.sim.as_ref(), &state, &app);
            let recording_result = match h.sim.recording_service() {
                Some(service) => service.stop().await.map_err(|error| error.to_string()),
                None => Ok(None),
            };
            h.sim.stop_shared_media().await;
            let _ = h.stop_tx.send(());
            let _ = app.emit("device_state", "Disconnected");
            recording_result?;
            Ok("设备已停止，活跃录像已收尾".into())
        }
        None => Err("没有正在运行的设备".into()),
    }
}

/// 主动上报一条报警。
#[tauri::command]
async fn fire_alarm(
    description: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            let desc = if description.is_empty() {
                "移动侦测报警".to_string()
            } else {
                description
            };
            let code = h
                .sim
                .report_alarm(&h.transport, &h.local_host, h.local_port, &desc)
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!("报警上报完成(平台响应 {code})"))
        }
        None => Err("设备未启动".into()),
    }
}

/// 主动上报一条 GPS 位置。
#[tauri::command]
async fn fire_position(
    longitude: f64,
    latitude: f64,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
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
                .await
                .map_err(|e| e.to_string())?;
            Ok(format!("位置上报完成(平台响应 {code})"))
        }
        None => Err("设备未启动".into()),
    }
}

/// 开关 SIP 信令追踪(FR-43)。开启后每条收发报文以 `sip_trace` 事件推给前端。
#[tauri::command]
async fn set_sip_trace(
    enabled: bool,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(h) => {
            if enabled {
                h.transport
                    .set_tracer(Some(Arc::new(TraceEmitter { app: app.clone() })));
                Ok("信令追踪已开启".into())
            } else {
                h.transport.set_tracer(None);
                Ok("信令追踪已关闭".into())
            }
        }
        None => Err("设备未启动".into()),
    }
}

// ── 数据传输对象 ──────────────────────────────────────────

/// 目录节点 DTO(前端多通道管理 ⇄ 引擎 CatalogNode)。
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ChannelNodeDto {
    id: String,
    /// 类型:Device/BusinessGroup/VirtualOrg/VideoChannel/AlarmChannel。
    node_type: String,
    name: String,
    parent_id: String,
    #[serde(default)]
    civil_code: Option<String>,
    #[serde(default)]
    business_group_id: Option<String>,
    #[serde(default = "default_on")]
    status: String,
}

fn default_on() -> String {
    "ON".into()
}

fn node_type_from_str(s: &str) -> Result<gb28181_protocol::id_codec::CatalogNodeType, String> {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    Ok(match s {
        "AdministrativeRegion" => AdministrativeRegion,
        "System" => System,
        "BusinessGroup" => BusinessGroup,
        "VirtualOrg" => VirtualOrg,
        "AlarmChannel" => AlarmChannel,
        "Device" => Device,
        "VideoChannel" => VideoChannel,
        other => return Err(format!("未知目录节点类型: {other}")),
    })
}

fn node_type_to_str(t: gb28181_protocol::id_codec::CatalogNodeType) -> &'static str {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    match t {
        AdministrativeRegion => "AdministrativeRegion",
        System => "System",
        Device => "Device",
        BusinessGroup => "BusinessGroup",
        VirtualOrg => "VirtualOrg",
        VideoChannel => "VideoChannel",
        AlarmChannel => "AlarmChannel",
    }
}

impl From<&gb28181_protocol::id_codec::CatalogNode> for ChannelNodeDto {
    fn from(n: &gb28181_protocol::id_codec::CatalogNode) -> Self {
        ChannelNodeDto {
            id: n.id.clone(),
            node_type: node_type_to_str(n.node_type).into(),
            name: n.name.clone(),
            parent_id: n.parent_id.clone(),
            civil_code: n.civil_code.clone(),
            business_group_id: n.business_group_id.clone(),
            status: n.status.clone(),
        }
    }
}

impl TryFrom<ChannelNodeDto> for gb28181_protocol::id_codec::CatalogNode {
    type Error = String;

    fn try_from(d: ChannelNodeDto) -> Result<Self, Self::Error> {
        let mut node = gb28181_protocol::id_codec::CatalogNode::new(
            d.id,
            node_type_from_str(&d.node_type)?,
            d.name,
            d.parent_id,
        );
        node.civil_code = d.civil_code;
        node.business_group_id = d.business_group_id;
        node.status = d.status;
        Ok(node)
    }
}

fn protocol_to_config(node: &gb28181_protocol::id_codec::CatalogNode) -> CatalogNodeConfig {
    CatalogNodeConfig {
        id: node.id.clone(),
        node_type: node_type_to_str(node.node_type).into(),
        name: node.name.clone(),
        parent_id: node.parent_id.clone(),
        civil_code: node.civil_code.clone(),
        business_group_id: node.business_group_id.clone(),
        status: node.status.clone(),
    }
}

async fn load_catalog_config(state: &AppState, app: &AppHandle) -> Result<DesktopConfigV2, String> {
    let store = app_config_store(state, app)?;
    let _io = state.config_io.lock().await;
    tokio::task::spawn_blocking(move || store.load())
        .await
        .map_err(|error| format!("读取目录配置任务异常: {error}"))?
}

fn catalog_nodes_from_config(
    config: &DesktopConfigV2,
) -> Result<Vec<gb28181_protocol::id_codec::CatalogNode>, String> {
    if !config.catalog_tree.is_empty() {
        return config
            .catalog_tree
            .iter()
            .map(CatalogNodeConfig::to_protocol_node)
            .collect();
    }
    let profile = config.active_profile().ok_or("活动平台档案不存在")?;
    Ok(gb28181_protocol::id_codec::catalog_template(
        "single",
        &config.device.device_id,
        &config.device.device_name,
        &profile.server_domain,
    ))
}

async fn save_catalog_config(
    state: &AppState,
    app: &AppHandle,
    nodes: &[gb28181_protocol::id_codec::CatalogNode],
) -> Result<(), String> {
    gb28181_protocol::id_codec::validate_catalog_tree(nodes)?;
    let store = app_config_store(state, app)?;
    let _io = state.config_io.lock().await;
    let nodes: Vec<_> = nodes.iter().map(protocol_to_config).collect();
    tokio::task::spawn_blocking(move || {
        let mut config = store.load()?;
        config.catalog_tree = nodes;
        store.save(&config).map(|_| ())
    })
    .await
    .map_err(|error| format!("保存目录配置任务异常: {error}"))?
}

async fn apply_runtime_catalog(
    state: &AppState,
    nodes: &[gb28181_protocol::id_codec::CatalogNode],
) -> Result<(), String> {
    let guard = state.device.lock().await;
    if let Some(handle) = guard.as_ref() {
        handle.sim.set_catalog_tree(nodes.to_vec())?;
        spawn_catalog_notify(handle);
    }
    Ok(())
}

/// 载入内置目录模板并持久化；设备运行中同步更新并触发增量通知。
#[tauri::command]
async fn load_catalog_template(
    template: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<Vec<ChannelNodeDto>, String> {
    let config = load_catalog_config(&state, &app).await?;
    let profile = config.active_profile().ok_or("活动平台档案不存在")?;
    let nodes = gb28181_protocol::id_codec::catalog_template(
        &template,
        &config.device.device_id,
        &config.device.device_name,
        &profile.server_domain,
    );
    gb28181_protocol::id_codec::validate_catalog_tree(&nodes)?;
    save_catalog_config(&state, &app, &nodes).await?;
    apply_runtime_catalog(&state, &nodes).await?;
    Ok(nodes.iter().map(Into::into).collect())
}

/// 获取持久化目录树；设备运行中优先返回当前实例快照。
#[tauri::command]
async fn get_catalog_tree(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<Vec<ChannelNodeDto>, String> {
    let guard = state.device.lock().await;
    if let Some(handle) = guard.as_ref() {
        let nodes = handle.sim.catalog_tree();
        if !nodes.is_empty() {
            return Ok(nodes.iter().map(Into::into).collect());
        }
    }
    drop(guard);
    let config = load_catalog_config(&state, &app).await?;
    let nodes = catalog_nodes_from_config(&config)?;
    Ok(nodes.iter().map(Into::into).collect())
}

/// 用完整目录树替换持久化真相；用于 JSON 导入和批量移动。
#[tauri::command]
async fn replace_catalog_tree(
    nodes: Vec<ChannelNodeDto>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<Vec<ChannelNodeDto>, String> {
    let nodes: Vec<gb28181_protocol::id_codec::CatalogNode> = nodes
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    gb28181_protocol::id_codec::validate_catalog_tree(&nodes)?;
    save_catalog_config(&state, &app, &nodes).await?;
    apply_runtime_catalog(&state, &nodes).await?;
    Ok(nodes.iter().map(Into::into).collect())
}

/// 新增/更新目录节点，先在完整候选树上校验，再持久化并通知。
#[tauri::command]
async fn upsert_channel(
    node: ChannelNodeDto,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let node: gb28181_protocol::id_codec::CatalogNode = node.try_into()?;
    let config = load_catalog_config(&state, &app).await?;
    let mut nodes = catalog_nodes_from_config(&config)?;
    let added = if let Some(existing) = nodes.iter_mut().find(|item| item.id == node.id) {
        *existing = node;
        false
    } else {
        nodes.push(node);
        true
    };
    gb28181_protocol::id_codec::validate_catalog_tree(&nodes)?;
    save_catalog_config(&state, &app, &nodes).await?;
    apply_runtime_catalog(&state, &nodes).await?;
    Ok(if added {
        "已新增目录节点".into()
    } else {
        "已更新目录节点".into()
    })
}

/// 删除目录节点及其全部后代。
#[tauri::command]
async fn remove_channel(
    id: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let config = load_catalog_config(&state, &app).await?;
    let nodes = catalog_nodes_from_config(&config)?;
    let target = nodes
        .iter()
        .find(|node| node.id == id)
        .ok_or("目录节点不存在")?;
    if target.id == target.parent_id {
        return Err("不能删除目录根节点".into());
    }
    let mut removed = std::collections::HashSet::from([id]);
    loop {
        let before = removed.len();
        for node in &nodes {
            if removed.contains(&node.parent_id) {
                removed.insert(node.id.clone());
            }
        }
        if removed.len() == before {
            break;
        }
    }
    let next: Vec<_> = nodes
        .into_iter()
        .filter(|node| !removed.contains(&node.id))
        .collect();
    gb28181_protocol::id_codec::validate_catalog_tree(&next)?;
    save_catalog_config(&state, &app, &next).await?;
    apply_runtime_catalog(&state, &next).await?;
    Ok(format!("已删除 {} 个目录节点", removed.len()))
}

#[derive(Serialize)]
struct CatalogActivityDto {
    target_id: String,
    result_count: usize,
    packet_count: usize,
    updated_at_ms: u64,
}

/// 回读最近一次平台 Catalog Query 活动。
#[tauri::command]
async fn get_catalog_activity(
    state: tauri::State<'_, AppState>,
) -> Result<Option<CatalogActivityDto>, String> {
    let guard = state.device.lock().await;
    Ok(guard.as_ref().and_then(|handle| {
        handle
            .sim
            .catalog_activity()
            .map(|activity| CatalogActivityDto {
                target_id: activity.target_id,
                result_count: activity.result_count,
                packet_count: activity.packet_count,
                updated_at_ms: activity.updated_at_ms,
            })
    }))
}

/// 后台推送目录增量 NOTIFY(fire-and-forget)。
///
/// NOTIFY 是通知平台的副作用,不该阻塞 IPC 返回:目录订阅存在但平台不及时回 200 时,
/// SIP 客户端事务会按 T1 退避重传,最坏阻塞约 4 秒。放后台 spawn 让 UI 立即拿到目录树。
fn spawn_catalog_notify(h: &DeviceHandle) {
    let sim = Arc::clone(&h.sim);
    let transport = Arc::clone(&h.transport);
    tokio::spawn(async move {
        if let Err(e) = sim.notify_catalog_changed(&transport).await {
            tracing::warn!(error = %e, "目录增量 NOTIFY 推送失败");
        }
    });
}

#[derive(serde::Serialize)]
struct ScenarioSummary {
    name: String,
    device_count: usize,
    server_host: String,
    server_port: u16,
}

// ── 系统信息:日志 ────────────────────────────────────────

/// 设置全局日志级别(error/warn/info/debug/trace,或 EnvFilter 语法)。返回生效后的级别。
#[tauri::command]
fn set_log_level(level: String) -> Result<String, String> {
    if common::logging::set_level(&level) {
        Ok(common::logging::current_level())
    } else {
        Err(format!("无效的日志级别: {level}"))
    }
}

/// 查询当前日志级别。
#[tauri::command]
fn get_log_level() -> String {
    common::logging::current_level()
}

/// 拉取最近缓冲的日志(前端打开日志页时补历史)。
#[tauri::command]
fn get_recent_logs(max: Option<usize>) -> Vec<common::logging::LogLine> {
    common::logging::recent(max.unwrap_or(500))
}

fn open_desktop_log(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

// ── 粘贴接入链接:一次性 token 兑换 SIP 接入参数 ──────────

/// 平台 `/api/gb28181/sip/qr/exchange` 的统一响应信封(`{code,message,data}`)。
#[derive(serde::Deserialize)]
struct QrExchangeEnvelope {
    code: i32,
    #[serde(default)]
    message: String,
    #[serde(default)]
    data: Option<QrProvisionPayload>,
}

/// 平台下发的 SIP 接入六元组。字段名与 Go 侧 `QRPayload` 的 json tag 一一对应。
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QrProvisionPayload {
    pub server_id: String,
    pub domain: String,
    pub ip: String,
    pub port: u16,
    pub transport: String,
    pub password: String,
}

/// 兑换失败的结构化原因。`code` 供前端分支判断,`message` 可直接展示给用户。
#[derive(serde::Serialize)]
pub struct QrProvisionError {
    pub code: &'static str,
    pub message: String,
}

impl QrProvisionError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 把 reqwest 的传输层错误归一成用户能处置的原因。
///
/// 证书失败必须单独分出来:内网绿色包普遍是自签 HTTPS,若只报「连不上平台」,
/// 用户会去查网络和端口 —— 2026-10-08 扫码接入就是这么把排查带偏的。
fn classify_exchange_transport_error(err: reqwest::Error) -> QrProvisionError {
    if err.is_timeout() {
        return QrProvisionError::new("unreachable", "连接平台超时,请确认地址可达、端口未被拦截");
    }
    let chain = error_chain(&err).to_lowercase();
    if chain.contains("certificate")
        || chain.contains("unknownissuer")
        || chain.contains("notvalidforname")
        || chain.contains("invalidpeer")
    {
        return QrProvisionError::new("cert", "平台证书不受信任(可能是自签名证书)");
    }
    if err.is_connect() {
        return QrProvisionError::new("unreachable", "无法连接平台,请确认地址与端口可达");
    }
    QrProvisionError::new("unreachable", format!("请求平台失败:{err}"))
}

/// 拼接错误链全文。reqwest 只在最外层给出 "error sending request",
/// 真正原因(证书/域名解析)藏在 source 里。
///
/// 用全限定调用而非 `err.source()`:`std::error::Error` 不在 prelude 里,
/// 方法调用取决于本文件是否恰好 import 了它,不该赌这个。
fn error_chain(err: &reqwest::Error) -> String {
    let mut text = err.to_string();
    let mut cursor = std::error::Error::source(err);
    while let Some(source) = cursor {
        text.push_str(" | ");
        text.push_str(&source.to_string());
        cursor = std::error::Error::source(source);
    }
    text
}

/// 用平台接入链接里的 token 兑换 SIP 接入参数。
///
/// 链接形态 `https://<平台>/gb28181/qr#t=<22 位 token>`。token 在 fragment 里,
/// 由前端解析后单独传入 —— 这里只负责发请求,不参与 URL 语义判断。
///
/// `allow_insecure` 仅在用户对自签证书显式确认后由前端置真。默认严格校验:
/// 放宽证书信任必须是一次可见的用户决定,不能是代码里的默认值。
#[tauri::command]
async fn exchange_sip_provision(
    base_url: String,
    token: String,
    allow_insecure: Option<bool>,
) -> Result<QrProvisionPayload, QrProvisionError> {
    let base = base_url.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(QrProvisionError::new(
            "rejected",
            "接入地址必须以 http:// 或 https:// 开头",
        ));
    }
    // 与平台 isWellFormedQRToken 对齐(22 位 base64url 无填充)。先挡一遍形态,
    // 避免明显错误输入去占用平台兑换端点的限流配额。
    if token.len() != 22
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(QrProvisionError::new("invalid", "链接中的接入码格式不正确"));
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .tls_danger_accept_invalid_certs(allow_insecure.unwrap_or(false))
        .build()
        .map_err(|e| QrProvisionError::new("server", format!("HTTP 客户端初始化失败:{e}")))?;

    let response = client
        .post(format!("{base}/api/gb28181/sip/qr/exchange"))
        // token 走 body 而非 query:query 会被平台访问日志落盘。
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
        .map_err(classify_exchange_transport_error)?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| QrProvisionError::new("unreachable", format!("读取平台响应失败:{e}")))?;

    // 失败响应同样是 {code,message} 信封;先按 HTTP 状态分流,
    // 再把平台侧原始文案带出去(平台已刻意合并「过期」与「已使用」)。
    let envelope: Option<QrExchangeEnvelope> = serde_json::from_str(&body).ok();
    let platform_message = envelope
        .as_ref()
        .map(|item| item.message.trim().to_string())
        .filter(|message| !message.is_empty());

    match status.as_u16() {
        200..=299 => {
            let envelope = envelope
                .ok_or_else(|| QrProvisionError::new("server", "平台响应不是预期的 JSON 结构"))?;
            if envelope.code != 0 {
                return Err(QrProvisionError::new(
                    "server",
                    platform_message
                        .unwrap_or_else(|| format!("平台返回业务错误码 {}", envelope.code)),
                ));
            }
            envelope
                .data
                .ok_or_else(|| QrProvisionError::new("server", "平台未返回接入信息"))
        }
        410 => Err(QrProvisionError::new(
            "expired",
            platform_message.unwrap_or_else(|| "接入码已失效".to_string()),
        )),
        429 => Err(QrProvisionError::new(
            "rate_limited",
            platform_message.unwrap_or_else(|| "尝试过于频繁".to_string()),
        )),
        400 | 422 => Err(QrProvisionError::new(
            "invalid",
            platform_message.unwrap_or_else(|| "链接内容不完整".to_string()),
        )),
        _ => Err(QrProvisionError::new(
            "server",
            platform_message.unwrap_or_else(|| format!("平台返回 HTTP {status}")),
        )),
    }
}

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .on_window_event(|window, event| {
            if matches!(
                event,
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::Focused(_)
            ) {
                if let Ok(minimized) = window.is_minimized() {
                    let _ = window.emit("preview_window_visibility", !minimized);
                }
            }
        })
        .setup(|app| {
            // 日志落盘。内存环形缓冲只够 UI 翻看，排查采集/预览这类"跑一次才复现"
            // 的问题时拿不出来——落到文件才能事后直接读。
            // 日志目录同样要走可写探测：受限环境里 AppData 能建目录但建不了文件，
            // 原来的 `create_dir_all` 成功掩盖了这一点，日志会悄无声息地缺失。
            let log_path = {
                let mut candidates: Vec<(String, std::path::PathBuf)> = Vec::new();
                if let Ok(dir) = app.path().app_log_dir() {
                    candidates.push(("app_log_dir".to_string(), dir));
                }
                if let Ok(exe) = std::env::current_exe() {
                    if let Some(parent) = exe.parent() {
                        candidates.push(("exe_dir/logs".to_string(), parent.join("logs")));
                    }
                }
                resolve_writable_dir(candidates)
                    .ok()
                    .map(|(dir, _)| dir.join("desktop.log"))
            };
            let file = log_path
                .as_ref()
                .and_then(|path| open_desktop_log(path).ok().map(std::sync::Mutex::new));
            let handle = app.handle().clone();
            common::logging::set_callback(move |line| {
                let _ = handle.emit("log_line", line);
                if let Some(file) = &file {
                    if let Ok(mut file) = file.lock() {
                        use std::io::Write;
                        let _ = writeln!(
                            file,
                            "{} {:5} {} {}",
                            line.ts_ms, line.level, line.target, line.message
                        );
                    }
                }
            });
            if let Some(path) = &log_path {
                tracing::info!(path = %path.display(), "日志文件已接续");
            } else {
                tracing::warn!("没有可写的日志目录，本次运行只保留内存日志");
            }

            // 开发期首屏自愈。
            //
            // 开发时窗口加载的是 devUrl（http://127.0.0.1:1420，已固定 IPv4，避免
            // localhost 在 Windows 上解析到 ::1 造成导航 ERR_ABORTED），由另一个进程
            // （npm run dev）提供。窗口可能早于 Vite 的模块图就绪就完成一次导航：
            // index.html 拿到了，但 /src/main.ts 及其依赖没执行，页面就停在
            // "无样式裸 DOM / 全白"。Tauri 不会重试导航，所以这里补一次。
            //
            // 关键约束：**只在页面确实没有进展时才重载**。
            // 2026-11-05 实测：调试器拉起时首个导航可能整体冻结 60s 以上，
            // 旧版本按固定间隔（3/4/6/8/10s）探活并 reload，5 次重载全部落在
            // 冻结窗口里排队，解冻后一起执行，反而中断了 22 个正常请求。
            // 现在改成页面内自驱：看 resource 计数，连续 30s 不增长才判定卡死，
            // 且最多重载 3 次。发布构建不编译这段代码。
            #[cfg(debug_assertions)]
            {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    // 多次注入是幂等的（页面内用 __uvpFirstPaintGuard 去重），
                    // 这样即使第一次注入落在 about:blank 上，导航后还能补上。
                    for delay_ms in [3_000u64, 7_000, 12_000] {
                        std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                        let Some(window) = handle.get_webview_window("main") else {
                            continue;
                        };
                        let _ = window.eval(
                            r#"(function () {
  try {
    if (window.__uvpFirstPaintGuard) return;
    window.__uvpFirstPaintGuard = true;
    var TICK = 5000, GRACE_TICKS = 3, STALL_TICKS = 6, MAX_RELOAD = 3;
    var ticks = 0, stalled = 0, lastRes = -1;
    var reloads = parseInt(sessionStorage.getItem('uvp.reload_guard') || '0', 10) || 0;
    var timer = setInterval(function () {
      ticks++;
      var app = document.getElementById('app');
      if (app && app.children.length > 0 && document.styleSheets.length > 0) {
        sessionStorage.removeItem('uvp.reload_guard');
        clearInterval(timer);
        return;
      }
      var res = performance.getEntriesByType('resource').length;
      if (res > lastRes) { lastRes = res; stalled = 0; } else { stalled++; }
      if (ticks < GRACE_TICKS || stalled < STALL_TICKS) return;
      if (reloads >= MAX_RELOAD) {
        console.warn('[uvp] 首屏连续 ' + MAX_RELOAD + ' 次未就绪，停止自动重载');
        clearInterval(timer);
        return;
      }
      reloads++;
      sessionStorage.setItem('uvp.reload_guard', String(reloads));
      console.warn('[uvp] 首屏 ' + (stalled * TICK / 1000) + 's 无新资源，自动重载第 ' + reloads + ' 次');
      location.reload();
    }, TICK);
  } catch (e) {}
})();"#,
                        );
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_version,
            list_local_ip_addresses,
            get_desktop_config,
            save_desktop_config,
            save_media_config,
            save_osd_config,
            get_osd_capability,
            reset_desktop_config,
            exchange_sip_provision,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
            export_report,
            list_live_sources,
            list_camera_modes,
            probe_live_source,
            is_preview_window_visible,
            start_binary_preview,
            stop_binary_preview,
            retry_binary_preview,
            start_device,
            cancel_media_preparation,
            stop_device,
            get_recordings,
            get_recording_state,
            start_recording,
            stop_recording,
            delete_recording,
            fire_alarm,
            fire_position,
            set_sip_trace,
            get_stress_status,
            get_device_status,
            get_device_runtime_state,
            get_device_control_state,
            get_device_subscriptions,
            report_ptz_pose,
            load_catalog_template,
            get_catalog_tree,
            replace_catalog_tree,
            upsert_channel,
            remove_channel,
            get_catalog_activity,
            set_log_level,
            get_log_level,
            get_recent_logs,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                let state = app_handle.state::<AppState>();
                if state.shutting_down.swap(true, Ordering::AcqRel) {
                    return;
                }
                // Keep the event loop alive while preparation/capture children are reaped.
                api.prevent_exit();
                let _ = state.cancel_file_preparation();
                let app = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let device = state.device.lock().await.take();
                    if let Some(handle) = device {
                        save_control_state(handle.sim.as_ref(), &state, &app);
                        if let Some(service) = handle.sim.recording_service() {
                            let _ = service.stop().await;
                        }
                        handle.sim.stop_shared_media().await;
                        let _ = handle.stop_tx.send(());
                    }
                    state.preview_manager.shutdown().await;
                    app.exit(0);
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn media_preparation_cancel_does_not_wait_for_device_lock() {
        let state = AppState::new();
        let _startup_guard = state.device.lock().await;
        let token = Arc::new(AtomicBool::new(false));
        *state.media_preparation.lock().unwrap() = Some(token.clone());
        state.cancel_file_preparation().unwrap();
        assert!(token.load(Ordering::Acquire));
        *state.media_preparation.lock().unwrap() = None;
        assert!(state.cancel_file_preparation().is_err());
    }

    #[test]
    fn media_start_cancel_remains_effective_until_registration_commit() {
        let state = AppState::new();
        let startup = state.begin_device_start().unwrap();
        assert!(state.begin_device_start().is_err());
        // Preparation has finished; cancellation must still prevent REGISTER.
        state.cancel_file_preparation().unwrap();
        let mut registered = false;
        assert!(startup.commit(|| registered = true).is_err());
        assert!(!registered);
        drop(startup);
        let retry = state.begin_device_start().unwrap();
        retry.commit(|| registered = true).unwrap();
        assert!(registered);
        assert!(state.cancel_file_preparation().is_err());
    }

    #[test]
    fn media_shutdown_rejects_config_writes_and_pending_registration() {
        let state = AppState::new();
        let startup = state.begin_device_start().unwrap();
        state.shutting_down.store(true, Ordering::Release);
        assert!(state.ensure_config_writable(false).is_err());
        assert!(startup.commit(|| panic!("must not register")).is_err());
        assert!(state.begin_device_start().is_err());
    }

    #[test]
    fn tcc拒绝映射为可执行系统设置提示() {
        let message = camera_capture_guidance("AVFoundation: not authorized to capture video");
        assert!(message.contains("系统设置 → 隐私与安全性 → 摄像头"));
        assert!(message.contains("[camera_permission_denied]"));
    }

    #[test]
    fn 锁屏错误映射为解锁后重试提示() {
        let message = camera_capture_guidance("[camera_screen_locked] console is locked");
        assert!(message.contains("解锁 Mac"));
        assert!(message.contains("[camera_screen_locked]"));
    }

    #[test]
    fn 普通媒体错误不伪装成权限错误() {
        let original = "preview FFmpeg produced no JPEG within 2000 ms";
        assert_eq!(camera_capture_guidance(original), original);
    }

    #[test]
    fn 本机ip去重过滤并优先展示非回环ipv4() {
        let options = normalize_local_ip_options(vec![
            ("lo0".into(), "127.0.0.1".parse().unwrap()),
            ("en0".into(), "192.168.1.8".parse().unwrap()),
            ("en1".into(), "192.168.1.8".parse().unwrap()),
            ("en0".into(), "fe80::1".parse().unwrap()),
            ("bad".into(), "0.0.0.0".parse().unwrap()),
        ]);

        assert_eq!(options.len(), 3);
        assert_eq!(options[0].address, "192.168.1.8");
        assert_eq!(options[0].interface_name, "en0");
        assert_eq!(options[1].address, "fe80::1");
        assert_eq!(options[2].address, "127.0.0.1");
    }

    #[test]
    fn 本机网卡扫描返回可解析地址() {
        let options = list_local_ip_addresses().expect("当前系统应允许读取网卡地址");
        assert!(!options.is_empty());
        assert!(options
            .iter()
            .all(|option| option.address.parse::<IpAddr>().is_ok()));
    }

    #[test]
    fn sip_trace日志保留完整authorization() {
        let raw = "REGISTER sip:test SIP/2.0\r\nAuthorization: Digest username=\"dev\", response=\"secret\"\r\nCall-ID: 1\r\n\r\n";
        let log = format_sip_trace_log("out", "127.0.0.1:5062", raw);
        assert!(log.contains("response=\"secret\""));
        assert!(log.contains("username=\"dev\""));
        assert!(log.contains("Call-ID: 1"));
    }

    #[test]
    fn 日志文件跨重启追加而不是截断() {
        let path = std::env::temp_dir().join(format!(
            "uvp-desktop-log-test-{}-{}.log",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"before\n").unwrap();
        {
            let mut file = open_desktop_log(&path).unwrap();
            use std::io::Write;
            writeln!(file, "after").unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before\nafter\n");
        std::fs::remove_file(path).unwrap();
    }
}
