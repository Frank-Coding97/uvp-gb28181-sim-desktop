//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use base64::Engine;
use common::{DeviceEvent, DeviceId, DeviceObserver, GbVersion, Transport};
use gb28181_simulator::{ChannelConfig, DeviceConfig, DeviceInfo, DeviceSimulator};
use serde::{Deserialize, Serialize};
use sip_core::UdpTransport;
use stress_engine::{Metrics, Orchestrator};
use tauri::{
    ipc::{Channel, InvokeResponseBody},
    AppHandle, Emitter, Manager,
}; // Tauri 2 emit/state 需要对应 trait
use tokio::sync::{broadcast, Mutex};

#[allow(dead_code)]
mod preview_ffmpeg;
mod preview_store;
use preview_store::{PreviewEnvelope, PreviewFrameStore};

/// 应用全局状态（托管在 Tauri managed state）。
struct AppState {
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
    /// 本地预览解码任务；输入来自注册期共享采集总线，不会再打开第二路摄像头。
    preview: Mutex<Option<PreviewHandle>>,
    binary_preview: Mutex<Option<BinaryPreviewHandle>>,
    preview_store: Arc<PreviewFrameStore>,
    preview_ack_session: Arc<AtomicU64>,
    preview_ack_sequence: Arc<AtomicU64>,
    preview_bus: Arc<DesktopPreviewBus>,
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
    transport: Arc<UdpTransport>,
    local_host: String,
    local_port: u16,
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

struct PreviewHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    child: Arc<std::sync::Mutex<Option<Child>>>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

struct BinaryPreviewHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

const PREVIEW_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

struct PreviewFramePayload {
    data: String,
    captured_at_ms: u64,
    sequence: u64,
}

/// 推流器到桌面预览的有界同帧总线。慢消费者只丢帧，不阻塞 RTP。
struct DesktopPreviewBus {
    /// 预览是实时画面，不应回放积压帧；watch 通道只保留最新访问单元。
    subscribers:
        std::sync::Mutex<Vec<tokio::sync::watch::Sender<Option<media_rtp::PreviewPacket>>>>,
    latest_config_keyframe: std::sync::Mutex<Option<media_rtp::PreviewPacket>>,
    store: Arc<PreviewFrameStore>,
}

impl DesktopPreviewBus {
    #[allow(dead_code)] // 单元测试直接构造独立总线；生产由 AppState 注入共享 store。
    fn new() -> Self {
        let store = Arc::new(PreviewFrameStore::new());
        Self::with_store(store)
    }

    fn with_store(store: Arc<PreviewFrameStore>) -> Self {
        Self {
            subscribers: std::sync::Mutex::new(Vec::new()),
            latest_config_keyframe: std::sync::Mutex::new(None),
            store,
        }
    }

    fn subscribe(&self) -> tokio::sync::watch::Receiver<Option<media_rtp::PreviewPacket>> {
        let initial = self.latest_config_keyframe.lock().unwrap().clone();
        let (tx, rx) = tokio::sync::watch::channel(initial);
        self.subscribers.lock().unwrap().push(tx);
        rx
    }
}

impl media_rtp::PreviewSink for DesktopPreviewBus {
    fn publish(&self, packet: media_rtp::PreviewPacket) {
        self.store.publish(packet.clone());
        if media_rtp::is_config_keyframe(&packet) {
            *self.latest_config_keyframe.lock().unwrap() = Some(packet.clone());
        }
        let mut subscribers = self.subscribers.lock().unwrap();
        subscribers.retain(|tx| tx.send(Some(packet.clone())).is_ok());
    }

    fn stopped(&self) {
        // 关闭当前订阅发送端，让预览线程退出并让前端收到 stopped。
        self.subscribers.lock().unwrap().clear();
        self.latest_config_keyframe.lock().unwrap().take();
        self.store.clear();
    }
}

impl AppState {
    fn new() -> Self {
        let preview_store = Arc::new(PreviewFrameStore::new());
        AppState {
            stress: Mutex::new(None),
            next_stress_id: AtomicU64::new(1),
            metrics: Mutex::new(None),
            last_metrics: Mutex::new(None),
            last_scenario: Mutex::new(None),
            device: Mutex::new(None),
            preview: Mutex::new(None),
            binary_preview: Mutex::new(None),
            preview_store: Arc::clone(&preview_store),
            preview_ack_session: Arc::new(AtomicU64::new(0)),
            preview_ack_sequence: Arc::new(AtomicU64::new(0)),
            preview_bus: Arc::new(DesktopPreviewBus::with_store(preview_store)),
        }
    }
}

/// 把设备事件映射成 `device_state` 字符串,通过 Tauri 事件推给前端(状态灯)。
struct StateEmitter {
    app: AppHandle,
}

impl DeviceObserver for StateEmitter {
    fn on_event(&self, event: DeviceEvent) {
        let state = match event {
            DeviceEvent::RegisterAttempt => "Registering",
            DeviceEvent::RegisterSuccess => "Registered",
            DeviceEvent::RegisterFailure(_) => "Failed",
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
            DeviceEvent::PtzPresetCall { preset, name } => {
                let _ = self.app.emit(
                    "ptz_preset",
                    serde_json::json!({ "id": preset, "name": name }),
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

/// 引擎连通性自检（M0 已有，保留）。
#[tauri::command]
fn engine_version() -> String {
    format!("stress-engine v{} 就绪", env!("CARGO_PKG_VERSION"))
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
    let (running, capture_state) = match device.as_ref() {
        Some(handle) => {
            let ready = handle.sim.capture_ready().await;
            (true, if ready { "ready" } else { "starting" })
        }
        None => (false, "stopped"),
    };
    Ok(serde_json::json!({ "running": running, "capture_state": capture_state }))
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
    tokio::task::spawn_blocking(media_rtp::list_live_sources)
        .await
        .map_err(|error| format!("实时源枚举任务异常: {error}"))?
        .map(LiveSourceCatalogDto::from)
        .map_err(|error| error.to_string())
}

/// 启动一次短时采集探测。请求音频时，用户应确保麦克风或系统正在产生声音。
#[tauri::command]
async fn probe_live_source(uri: String) -> Result<LiveProbeDto, String> {
    let spec = media_rtp::LiveSourceSpec::parse(uri.trim()).map_err(|error| error.to_string())?;
    let canonical_uri = uri.trim().to_string();
    let audio_requested = match spec {
        media_rtp::LiveSourceSpec::Camera { audio_index, .. } => audio_index.is_some(),
        media_rtp::LiveSourceSpec::Screen { audio, .. } => audio.is_enabled(),
    };
    tokio::task::spawn_blocking(move || {
        use media_rtp::VideoSource;

        let mut source = media_rtp::LiveSource::capture(&canonical_uri, 30)
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
        } else {
            "未检测到视频帧，请检查设备占用和 macOS 隐私权限".to_string()
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

/// 订阅推流器的同帧预览。source 参数保留用于兼容旧前端，新实现不再重新采集。
#[tauri::command]
async fn start_preview(
    _source: String,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let mut preview_guard = state.preview.lock().await;
    if preview_guard.is_some() {
        let finished = preview_guard.as_ref().is_some_and(|handle| {
            handle.done.load(Ordering::Relaxed)
                || handle
                    .child
                    .lock()
                    .ok()
                    .and_then(|mut child| {
                        child
                            .as_mut()
                            .map(|process| process.try_wait().ok().flatten().is_some())
                    })
                    .unwrap_or(false)
        });
        if finished {
            *preview_guard = None;
        } else {
            return Err("已有预览正在运行，请先停止当前预览".into());
        }
    }
    let rx = state.preview_bus.subscribe();
    let child_slot = Arc::new(std::sync::Mutex::new(None));
    let child_slot_reader = Arc::clone(&child_slot);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done_reader = Arc::clone(&done);
    let app_reader = app.clone();
    std::thread::spawn(move || {
        let mut rx = rx;
        let _ = app_reader.emit("preview_state", "starting");
        let first_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let first = loop {
            if stop_reader.load(Ordering::Relaxed) {
                done_reader.store(true, Ordering::Relaxed);
                return;
            }
            if std::time::Instant::now() >= first_deadline {
                let _ = app_reader.emit(
                    "preview_error",
                    "预览等待视频参数集超时：采集源没有产生可解码关键帧",
                );
                let _ = app_reader.emit("preview_state", "stopped");
                done_reader.store(true, Ordering::Relaxed);
                return;
            }
            if rx.has_changed().is_err() {
                let _ = app_reader.emit("preview_state", "stopped");
                done_reader.store(true, Ordering::Relaxed);
                return;
            }
            match rx.borrow_and_update().clone() {
                Some(packet) if media_rtp::is_config_keyframe(&packet) => break packet,
                Some(_) | None => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
        };
        let ffmpeg = match media_rtp::ffmpeg_bin() {
            Some(path) => path,
            None => {
                let _ = app_reader.emit("preview_error", "未找到内置 FFmpeg，无法解码同帧预览");
                let _ = app_reader.emit("preview_state", "stopped");
                done_reader.store(true, Ordering::Relaxed);
                return;
            }
        };
        let last_captured = Arc::new(AtomicU64::new(first.captured_at_ms));
        let last_captured_reader = Arc::clone(&last_captured);
        let input_format = if packet_is_h265(&first) {
            "hevc"
        } else {
            "h264"
        };
        // 裸 H.264/HEVC 管道通常没有有效 PTS；仅设置 +genpts 仍可能让 MJPEG
        // 编码器收到重复时间戳并停止输出。先按源帧率用帧序号生成 PTS，再降到预览帧率。
        let source_fps = first.fps.max(1);
        let source_fps_text = source_fps.to_string();
        let preview_filter = if source_fps <= 30 {
            format!("setpts=N/({source_fps}*TB),scale=640:-2")
        } else {
            format!("setpts=N/({source_fps}*TB),fps=30,scale=640:-2")
        };
        let mut command = Command::new(ffmpeg);
        command.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "warning",
            "-fflags",
            "nobuffer+genpts",
            "-flags",
            "low_delay",
            "-avioflags",
            "direct",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-f",
            input_format,
            "-framerate",
            &source_fps_text,
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            &preview_filter,
            "-fps_mode",
            "passthrough",
            "-q:v",
            "7",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "-flush_packets",
            "1",
            "pipe:1",
        ]);
        let stderr = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let stderr_reader = Arc::clone(&stderr);
        let mut child = match command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(error) => {
                let _ = app_reader.emit(
                    "preview_error",
                    format!("启动内置 FFmpeg 解码失败：{error}"),
                );
                done_reader.store(true, Ordering::Relaxed);
                return;
            }
        };
        let mut stdin = child.stdin.take().expect("preview ffmpeg stdin");
        let mut stdout = child.stdout.take().expect("preview ffmpeg stdout");
        if let Some(mut child_stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut chunk = [0_u8; 4096];
                while let Ok(n) = child_stderr.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut output) = stderr_reader.lock() {
                        output.extend_from_slice(&chunk[..n]);
                        if output.len() > 16 * 1024 {
                            let drain = output.len() - 16 * 1024;
                            output.drain(..drain);
                        }
                    }
                }
            });
        }
        if let Ok(mut slot) = child_slot_reader.lock() {
            *slot = Some(child);
        }
        let stop_frames = Arc::clone(&stop_reader);
        let latest_frame = Arc::new(std::sync::Mutex::new(None::<PreviewFramePayload>));
        let latest_frame_writer = Arc::clone(&latest_frame);
        let frame_reader_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let frame_reader_done_reader = Arc::clone(&frame_reader_done);
        let preview_frames = Arc::new(AtomicU64::new(0));
        let preview_frames_reader = Arc::clone(&preview_frames);
        let frame_event_stop = Arc::clone(&stop_reader);
        let frame_event_slot = Arc::clone(&latest_frame);
        let frame_event_done = Arc::clone(&frame_reader_done);
        let frame_event_app = app_reader.clone();
        std::thread::spawn(move || loop {
            let payload = frame_event_slot
                .lock()
                .ok()
                .and_then(|mut slot| slot.take());
            if let Some(payload) = payload {
                let _ = frame_event_app.emit(
                    "preview_frame",
                    serde_json::json!({
                        "data": payload.data,
                        "captured_at_ms": payload.captured_at_ms,
                        "sequence": payload.sequence,
                    }),
                );
                let _ = frame_event_app.emit("preview_state", "playing");
                continue;
            }
            if frame_event_stop.load(Ordering::Relaxed) || frame_event_done.load(Ordering::Acquire)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        });
        std::thread::spawn(move || {
            let mut buf = Vec::with_capacity(256 * 1024);
            let mut chunk = [0_u8; 32 * 1024];
            loop {
                if stop_frames.load(Ordering::Relaxed) {
                    break;
                }
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&chunk[..n]);
                        while let Some(start) = buf.windows(2).position(|w| w == [0xff, 0xd8]) {
                            let Some(end_rel) =
                                buf[start + 2..].windows(2).position(|w| w == [0xff, 0xd9])
                            else {
                                if start > 0 {
                                    buf.drain(..start);
                                }
                                break;
                            };
                            let end = start + 2 + end_rel + 2;
                            let frame = buf[start..end].to_vec();
                            buf.drain(..end);
                            let encoded = base64::engine::general_purpose::STANDARD.encode(frame);
                            let sequence =
                                preview_frames_reader.fetch_add(1, Ordering::Relaxed) + 1;
                            if let Ok(mut slot) = latest_frame_writer.lock() {
                                *slot = Some(PreviewFramePayload {
                                    data: encoded,
                                    captured_at_ms: last_captured_reader.load(Ordering::Relaxed),
                                    sequence,
                                });
                            }
                        }
                    }
                }
            }
            frame_reader_done_reader.store(true, Ordering::Release);
        });
        if stdin.write_all(&first.data).is_err() {
            done_reader.store(true, Ordering::Relaxed);
            return;
        }
        let mut input_frames = 1_u64;
        let pts_step = u64::from(media_rtp::CLOCK_HZ / source_fps);
        let mut last_input_pts = first.pts_90k;
        let mut needs_keyframe = false;
        let mut last_output_check = std::time::Instant::now();
        loop {
            if stop_reader.load(Ordering::Relaxed) {
                break;
            }
            match rx.has_changed() {
                Ok(true) => {
                    let Some(packet) = rx.borrow_and_update().clone() else {
                        continue;
                    };
                    // watch 通道只保留最新帧，消费者忙时可能跨过参考帧；跳过普通帧，
                    // 直到下一个带 SPS/PPS/IDR 的访问单元，避免 FFmpeg 长时间花屏或停解码。
                    if packet.pts_90k > last_input_pts.saturating_add(pts_step) {
                        needs_keyframe = true;
                    }
                    last_input_pts = packet.pts_90k;
                    let is_config_keyframe = media_rtp::is_config_keyframe(&packet);
                    if needs_keyframe && !is_config_keyframe {
                        continue;
                    }
                    if is_config_keyframe {
                        needs_keyframe = false;
                    }
                    last_captured.store(packet.captured_at_ms, Ordering::Relaxed);
                    if stdin.write_all(&packet.data).is_err() {
                        break;
                    }
                    input_frames += 1;
                }
                Ok(false) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => break,
            }
            if last_output_check.elapsed() >= std::time::Duration::from_secs(5) {
                if input_frames >= 25 && preview_frames.load(Ordering::Relaxed) <= 1 {
                    let detail = stderr
                        .lock()
                        .ok()
                        .map(|output| String::from_utf8_lossy(&output).trim().to_string())
                        .unwrap_or_default();
                    let message = if detail.is_empty() {
                        format!("预览解码未持续输出画面（已输入 {input_frames} 帧）")
                    } else {
                        format!("预览解码未持续输出画面：{detail}")
                    };
                    let _ = app_reader.emit("preview_error", message);
                    break;
                }
                last_output_check = std::time::Instant::now();
            }
        }
        drop(stdin);
        if let Ok(mut slot) = child_slot_reader.lock() {
            if let Some(mut child) = slot.take() {
                let _ = child.kill();
            }
        }
        let _ = app_reader.emit("preview_state", "stopped");
        done_reader.store(true, Ordering::Relaxed);
    });
    *preview_guard = Some(PreviewHandle {
        stop,
        child: child_slot,
        done,
    });
    let _ = app.emit("preview_state", "starting");
    Ok("已订阅同帧预览，等待平台点播".into())
}

fn packet_is_h265(packet: &media_rtp::PreviewPacket) -> bool {
    matches!(packet.codec, media_rtp::VideoCodec::H265)
}

/// 预览编码访问单元的二进制传输通道。
/// 每次最多发送一个未 ACK 的 envelope，避免 Channel 队列无限增长。
#[tauri::command]
async fn start_binary_preview(
    channel: Channel<InvokeResponseBody>,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let mut guard = state.binary_preview.lock().await;
    if guard
        .as_ref()
        .is_some_and(|handle| !handle.done.load(Ordering::Acquire))
    {
        return Err("已有二进制预览正在运行".into());
    }
    if guard.is_some() {
        // A stalled sender has already stopped its worker; release the old
        // handle before accepting a new session.
        *guard = None;
    }
    let mut rx = state.preview_bus.subscribe();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_reader = Arc::clone(&stop);
    let done_reader = Arc::clone(&done);
    let ack_session = Arc::clone(&state.preview_ack_session);
    let ack_sequence = Arc::clone(&state.preview_ack_sequence);
    let app_reader = app.clone();
    let store = Arc::clone(&state.preview_store);
    std::thread::spawn(move || {
        let mut pending = rx.borrow().clone();
        let _ = app_reader.emit(
            "preview_transport",
            serde_json::json!({
                "state": "starting",
                "transport": "binary-channel",
            }),
        );
        loop {
            let packet = if let Some(packet) = pending.take() {
                packet
            } else {
                if stop_reader.load(Ordering::Acquire) || rx.has_changed().is_err() {
                    break;
                }
                let Some(packet) = rx.borrow_and_update().clone() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                packet
            };
            let Some(packet) =
                store.latest_after(packet.session_id, packet.sequence.saturating_sub(1))
            else {
                continue;
            };
            let envelope = PreviewEnvelope::from_packet(packet.clone()).encode();
            ack_session.store(0, Ordering::Release);
            ack_sequence.store(0, Ordering::Release);
            if channel.send(InvokeResponseBody::Raw(envelope)).is_err() {
                let _ = app_reader.emit(
                    "preview_transport",
                    serde_json::json!({
                        "state": "send_error",
                        "session_id": packet.session_id,
                        "sequence": packet.sequence,
                    }),
                );
                break;
            }
            let deadline = std::time::Instant::now() + PREVIEW_ACK_TIMEOUT;
            loop {
                if stop_reader.load(Ordering::Acquire) {
                    break;
                }
                if ack_session.load(Ordering::Acquire) == packet.session_id
                    && ack_sequence.load(Ordering::Acquire) == packet.sequence
                {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = app_reader.emit(
                        "preview_transport",
                        serde_json::json!({
                            "state": "stalled",
                            "session_id": packet.session_id,
                            "sequence": packet.sequence,
                            "timeout_ms": PREVIEW_ACK_TIMEOUT.as_millis(),
                        }),
                    );
                    stop_reader.store(true, Ordering::Release);
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        let _ = app_reader.emit(
            "preview_transport",
            serde_json::json!({
                "state": "stopped",
                "transport": "binary-channel",
            }),
        );
        done_reader.store(true, Ordering::Release);
    });
    *guard = Some(BinaryPreviewHandle { stop, done });
    Ok("二进制预览通道已启动".into())
}

#[tauri::command]
fn ack_preview_frame(
    session_id: u64,
    sequence: u64,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state
        .preview_ack_session
        .store(session_id, Ordering::Release);
    state
        .preview_ack_sequence
        .store(sequence, Ordering::Release);
    Ok(())
}

#[tauri::command]
async fn stop_binary_preview(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let mut guard = state.binary_preview.lock().await;
    if let Some(handle) = guard.take() {
        handle.stop.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(600);
        while !handle.done.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        state.preview_ack_session.store(0, Ordering::Release);
        state.preview_ack_sequence.store(0, Ordering::Release);
        let _ = app.emit(
            "preview_transport",
            serde_json::json!({ "state": "stopped" }),
        );
    }
    Ok("二进制预览已停止".into())
}

#[tauri::command]
async fn stop_preview(state: tauri::State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let mut guard = state.preview.lock().await;
    if let Some(handle) = guard.take() {
        handle.stop.store(true, Ordering::Relaxed);
        if let Ok(mut child) = handle.child.lock() {
            if let Some(mut process) = child.take() {
                let _ = process.kill();
            }
        }
        let _ = app.emit("preview_state", "stopped");
    }
    Ok("预览已停止".into())
}

/// 前端传入的单设备配置。
#[derive(Deserialize)]
struct DeviceCfg {
    server_host: String,
    server_port: u16,
    server_domain: String,
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
    mut config: DeviceCfg,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    if guard.is_some() {
        return Err("已有设备在运行,请先停止".into());
    }

    if config.transport.eq_ignore_ascii_case("TCP") {
        return Err("当前 SIP 信令仅支持 UDP；TCP 传输尚未实现,请改用 UDP".into());
    }
    if config.server_port == 0
        || format!("{}:{}", config.server_host, config.server_port)
            .parse::<std::net::SocketAddr>()
            .is_err()
    {
        return Err("平台地址必须是有效的 IP:端口（当前不支持域名）".into());
    }
    if config.password.is_empty() {
        return Err("SIP 认证密码不能为空".into());
    }
    if let Some(source) = config.video_source.take() {
        let source = source.trim().to_string();
        if source.is_empty() {
            config.video_source = None;
        } else {
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
                        return Err(catalog.avfoundation_error.unwrap_or_else(|| {
                            format!("未找到 AVFoundation 摄像头索引 {video_index}")
                        }));
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
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}")));
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
                            .unwrap_or_else(|| format!("未找到 AVFoundation 麦克风索引 {index}")));
                    }
                    media_rtp::LiveSourceSpec::Screen { display_id, .. }
                        if display_id != 0
                            && !catalog
                                .screens
                                .iter()
                                .any(|screen| screen.display_id == display_id) =>
                    {
                        return Err(catalog.screen_error.unwrap_or_else(|| {
                            format!("未找到 ScreenCaptureKit 显示器 {display_id}")
                        }));
                    }
                    _ => {}
                }
            }
            config.video_source = Some(source);
        }
    }

    let device_id = DeviceId::new(config.device_id.clone()).map_err(|e| e.to_string())?;
    // 通道 ID:设备 ID 前 17 位 + 132(视频通道类型码)。
    let channel_id_str = format!("{}132", &config.device_id[..17]);
    let channel_id = DeviceId::new(channel_id_str).map_err(|e| e.to_string())?;
    let gb_version = if config.gb_version == "2016" {
        GbVersion::V2016
    } else {
        GbVersion::V2022
    };
    let transport = Transport::Udp;
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
        video_fps: 30,
        light_bitrate_kbps: None,
        gb_version,
        signaling_encoding: common::SignalingEncoding::from_str_lenient(&config.signaling_encoding),
    };

    // 预热视频源:容器(MP4 等)转封装可能耗时数秒,若留到 INVITE 时同步做会阻塞
    // 200 OK 与首包推流,导致平台收流超时。这里在设备上线前先转好、缓存,
    // INVITE 时命中缓存瞬时加载。ffmpeg 是阻塞调用,放 spawn_blocking。
    // 实时采集(live:)无文件可预热,跳过;仅对文件源(C 档容器)做转封装预热。
    if let Some(ref vs) = config.video_source {
        if !vs.trim().is_empty() && !vs.trim().starts_with("live:") {
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
    sim.set_preview_sink(Some(
        state.preview_bus.clone() as Arc<dyn media_rtp::PreviewSink>
    ));

    // 若指定了目录模板,在注册前载入(平台注册后会立即同步目录,需在这之前准备好)。
    if !config.catalog_template.is_empty() {
        sim.load_catalog_template(&config.catalog_template);
    }

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let sim_run = sim.clone();
    let tp_run = udp.clone();
    let host_run = local_host.clone();
    tokio::spawn(async move {
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
    });
    Ok("设备已启动".into())
}

/// 停止当前设备。
#[tauri::command]
async fn stop_device(state: tauri::State<'_, AppState>, app: AppHandle) -> Result<String, String> {
    let mut guard = state.device.lock().await;
    match guard.take() {
        Some(h) => {
            h.sim.stop_shared_media().await;
            if let Some(preview) = state.preview.lock().await.take() {
                preview.stop.store(true, Ordering::Relaxed);
                if let Ok(mut child) = preview.child.lock() {
                    if let Some(mut process) = child.take() {
                        let _ = process.kill();
                    }
                }
                let _ = app.emit("preview_state", "stopped");
            }
            let _ = h.stop_tx.send(());
            let _ = app.emit("device_state", "Disconnected");
            Ok("设备已停止".into())
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
    #[serde(default = "default_on")]
    status: String,
}

fn default_on() -> String {
    "ON".into()
}

fn node_type_from_str(s: &str) -> gb28181_protocol::id_codec::CatalogNodeType {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    match s {
        "BusinessGroup" => BusinessGroup,
        "VirtualOrg" => VirtualOrg,
        "AlarmChannel" => AlarmChannel,
        "Device" => Device,
        _ => VideoChannel,
    }
}

fn node_type_to_str(t: gb28181_protocol::id_codec::CatalogNodeType) -> &'static str {
    use gb28181_protocol::id_codec::CatalogNodeType::*;
    match t {
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
            status: n.status.clone(),
        }
    }
}

impl From<ChannelNodeDto> for gb28181_protocol::id_codec::CatalogNode {
    fn from(d: ChannelNodeDto) -> Self {
        let mut node = gb28181_protocol::id_codec::CatalogNode::new(
            d.id,
            node_type_from_str(&d.node_type),
            d.name,
            d.parent_id,
        );
        node.civil_code = d.civil_code;
        node.status = d.status;
        node
    }
}

/// 载入内置目录模板(single/nvr-8ch/civil-3x2/large-16ch),返回加载后的节点列表(FR-34)。
#[tauri::command]
async fn load_catalog_template(
    template: String,
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ChannelNodeDto>, String> {
    let guard = state.device.lock().await;
    let h = guard.as_ref().ok_or("设备未启动,请先在单设备页启动设备")?;
    h.sim.load_catalog_template(&template);
    // 增量 NOTIFY 推送是副作用,后台发送不阻塞 UI 返回(平台不及时回 200 时事务会退避重传数秒)。
    spawn_catalog_notify(h);
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 获取当前目录树节点(FR-34)。
#[tauri::command]
async fn get_catalog_tree(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ChannelNodeDto>, String> {
    let guard = state.device.lock().await;
    let h = guard.as_ref().ok_or("设备未启动")?;
    Ok(h.sim.catalog_tree().iter().map(Into::into).collect())
}

/// 新增/更新一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
async fn upsert_channel(
    node: ChannelNodeDto,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let guard = state.device.lock().await;
    let h = guard.as_ref().ok_or("设备未启动")?;
    let added = h.sim.upsert_channel(node.into());
    spawn_catalog_notify(h);
    Ok(if added {
        "已新增通道".into()
    } else {
        "已更新通道".into()
    })
}

/// 删除一个目录通道节点,触发增量 NOTIFY(FR-34)。
#[tauri::command]
async fn remove_channel(id: String, state: tauri::State<'_, AppState>) -> Result<String, String> {
    let guard = state.device.lock().await;
    let h = guard.as_ref().ok_or("设备未启动")?;
    if h.sim.remove_channel(&id) {
        spawn_catalog_notify(h);
        Ok("已删除通道".into())
    } else {
        Err("通道不存在".into())
    }
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

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            // 注册实时日志回调:每条日志发 `log_line` 事件给前端日志页。
            let handle = app.handle().clone();
            common::logging::set_callback(move |line| {
                let _ = handle.emit("log_line", line);
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_version,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
            export_report,
            list_live_sources,
            probe_live_source,
            start_preview,
            stop_preview,
            start_binary_preview,
            ack_preview_frame,
            stop_binary_preview,
            start_device,
            stop_device,
            fire_alarm,
            fire_position,
            set_sip_trace,
            get_stress_status,
            get_device_status,
            load_catalog_template,
            get_catalog_tree,
            upsert_channel,
            remove_channel,
            set_log_level,
            get_log_level,
            get_recent_logs,
        ])
        .run(tauri::generate_context!())
        .expect("Tauri 应用启动失败");
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_rtp::PreviewSink;

    fn packet(captured_at_ms: u64) -> media_rtp::PreviewPacket {
        media_rtp::PreviewPacket {
            data: vec![0, 0, 0, 1, 0x65, captured_at_ms as u8],
            key_frame: false,
            codec: media_rtp::VideoCodec::H264,
            session_id: 1,
            sequence: captured_at_ms,
            fps: 25,
            pts_90k: captured_at_ms,
            captured_at_ms,
        }
    }

    #[test]
    fn 预览总线只保留最新帧() {
        let bus = DesktopPreviewBus::new();
        let mut receiver = bus.subscribe();
        bus.publish(packet(1));
        bus.publish(packet(2));

        assert!(receiver.has_changed().expect("预览订阅仍应有效"));
        assert_eq!(
            receiver
                .borrow_and_update()
                .as_ref()
                .map(|frame| frame.captured_at_ms),
            Some(2)
        );
    }

    #[test]
    fn 延迟订阅从最近参数关键帧启动() {
        let bus = DesktopPreviewBus::new();
        let config = media_rtp::PreviewPacket {
            data: vec![0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3],
            key_frame: true,
            codec: media_rtp::VideoCodec::H264,
            session_id: 1,
            sequence: 1,
            fps: 25,
            pts_90k: 3,
            captured_at_ms: 3,
        };
        bus.publish(config.clone());

        let receiver = bus.subscribe();
        assert_eq!(
            receiver.borrow().as_ref().map(|frame| frame.captured_at_ms),
            Some(3)
        );
    }
}
