//! Tauri 桌面端后端库。
//!
//! 按 docs/20-architecture/api-contract.md 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

mod artifacts;
mod diagnostics;
mod ipc_error;
mod media_access;
mod reports_db;
mod secrets;
mod series;

mod catalog_channels;
mod device_console;
mod media_sources;
mod stress;
mod system;
mod trace;
pub(crate) use catalog_channels::*;
pub(crate) use device_console::*;
pub(crate) use media_sources::*;
pub(crate) use stress::*;
pub(crate) use system::*;
pub(crate) use trace::*;

use ipc_error::IpcError;

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};

use common::{DeviceEvent, DeviceObserver};
use gb28181_simulator::DeviceSimulator;
use sip_core::SipTransport;
use stress_engine::Metrics;
use tauri::{AppHandle, Emitter, Manager}; // Tauri 2 emit/state 需要对应 trait
use tokio::sync::{broadcast, Mutex};

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
    /// 最近一次压测的时序指标 JSONL 文件路径。
    ///
    /// 之前时序只活在前端图表内存里,且只保留 60 个点 —— 跑 10 分钟只剩最后一分钟,
    /// 报告里连时序都没有。现在引擎每秒追加一行,导出报告时一并打包。
    last_series: Mutex<Option<std::path::PathBuf>>,
    /// 最近一次(含进行中)压测的 run_id:导出报告后回填历史库的报告路径用。
    last_run_id: AtomicU64,
    /// 单设备联调：当前设备实例 + 停止发射端 + 共享传输。
    device: Mutex<Option<DeviceHandle>>,
    /// 用户经**系统文件对话框**授权预览的媒体文件集合(见 media_access 模块文档)。
    media_grants: media_access::MediaGrants,
}

/// 压测运行句柄。收到停止请求后仅标记 `stopping`，由后台任务退出时释放。
struct StressHandle {
    run_id: u64,
    stop_tx: broadcast::Sender<()>,
    stopping: bool,
    stopping_flag: Arc<AtomicBool>,
    worker_abort: tokio::task::AbortHandle,
    completion: tokio::sync::watch::Receiver<bool>,
    device_count: usize,
    /// 当前 run 的 `.inprogress` 时序；retention 必须始终保留。
    series_path: Option<std::path::PathBuf>,
}

/// 单设备运行句柄。
struct DeviceHandle {
    sim: Arc<DeviceSimulator>,
    transport: Arc<dyn SipTransport>,
    local_host: String,
    local_port: u16,
    stop_tx: tokio::sync::oneshot::Sender<()>,
    /// `sim.run()` 的任务句柄。
    ///
    /// 之前是 fire-and-forget:`stop_tx.send(())` 之后立刻返回"设备已停止",
    /// 而真正的下线序列(停订阅 → 拆媒体链路发 BYE → REGISTER Expires=0)还在跑。
    /// 关窗口时进程被直接结束,平台要等心跳超时才把设备置离线,
    /// ffmpeg 子进程也来不及被 `Drop` 清掉,留下孤儿进程。
    run_task: tokio::task::JoinHandle<()>,
}

/// 退出清理是否已经开始。
///
/// `AppHandle::exit()` 会再触发一次 `ExitRequested`;没有这个闸的话
/// "拦下退出 → 清理 → 调 exit → 又被拦下"会变成死循环,应用永远关不掉。
static CLEANUP_STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 优雅停止的等待上限。
///
/// 下线序列里最慢的一步是"REGISTER Expires=0"这次 SIP 事务:平台不响应时
/// 客户端事务会重传到 Timer B(约 32 秒)。用户关窗口不该等那么久 ——
/// 5 秒足够覆盖平台正常响应的情形,超时就放弃并如实记一条日志。
const GRACEFUL_STOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// 等待 Tokio 任务优雅退出；超时后必须 abort + await。
///
/// 仅把 `JoinHandle` 移进 `timeout` 再丢弃会 detach 任务，不会取消。设备运行任务持有
/// 推流 future，detach 会阻断 `LiveSource::drop`，从而让 FFmpeg 变成孤儿进程。
async fn await_or_abort_task(what: &str, task: &mut tokio::task::JoinHandle<()>) -> bool {
    match tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, &mut *task).await {
        Ok(Ok(())) => true,
        Ok(Err(error)) if error.is_cancelled() => true,
        Ok(Err(error)) => {
            tracing::warn!(what, %error, "后台任务异常结束");
            true
        }
        Err(_) => {
            tracing::warn!(
                what,
                secs = GRACEFUL_STOP_TIMEOUT.as_secs(),
                "后台任务未在期限内退出,正在强制取消"
            );
            task.abort();
            match tokio::time::timeout(std::time::Duration::from_secs(1), &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) if error.is_cancelled() => {}
                Ok(Err(error)) => tracing::warn!(what, %error, "强制取消后的任务异常"),
                Err(_) => tracing::error!(what, "强制取消后任务仍未收敛"),
            }
            false
        }
    }
}

impl AppState {
    /// `app_data_dir` 为 None 时媒体授权退化为仅本次运行有效(拿不到数据目录的极端情形)。
    fn new(app_data_dir: Option<std::path::PathBuf>) -> Self {
        AppState {
            stress: Mutex::new(None),
            next_stress_id: AtomicU64::new(1),
            last_run_id: AtomicU64::new(0),
            metrics: Mutex::new(None),
            last_metrics: Mutex::new(None),
            last_scenario: Mutex::new(None),
            last_series: Mutex::new(None),
            device: Mutex::new(None),
            media_grants: media_access::MediaGrants::load(app_data_dir),
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
            DeviceEvent::RegisterAttempt { .. } => "Registering",
            DeviceEvent::RegisterSuccess { .. } => "Registered",
            DeviceEvent::RegisterFailure { .. } => "Failed",
            DeviceEvent::StreamStart => "InCall",
            DeviceEvent::StreamStop => "Registered",
            // 心跳与在线翻转不改变状态灯(状态灯由注册/推流事件驱动)。
            DeviceEvent::HeartbeatOk { .. }
            | DeviceEvent::HeartbeatFail
            | DeviceEvent::OnlineChanged { .. } => return,
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
                let error = IpcError::from(message);
                let _ = self.app.emit(
                    "device_error",
                    serde_json::json!({ "scope": scope, "error": error, "ts_ms": ts_ms }),
                );
                return;
            }
        };
        let _ = self.app.emit("device_state", state);
    }
}

/// 应用退出前的清理:先设备、再压测。
///
/// 顺序有讲究:设备下线要发 SIP 注销,而那需要传输还活着;
/// 压测停止会连带释放它自己的分片传输。
///
/// **ffmpeg 采集子进程不需要单独处理**:所有权链是
/// `PushSession.task` → 推流 future → `Box<dyn VideoSource>` → `LiveSource::drop`,
/// 而 `release_media_on_shutdown` 现在会 join 推流任务(见 gb28181-simulator 的
/// `await_task`)。等到这里返回时子进程已被各自的 `Drop` 杀掉。
/// 只有等待超时那条路径可能留下孤儿 —— 那种情况下面会明确告警。
async fn shutdown_all(state: &AppState) {
    let stopped_device = stop_device_graceful(state).await;
    let stopped_stress = stop_stress_graceful(state).await;
    tracing::info!(stopped_device, stopped_stress, "退出前清理完成");
}

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    // 注意:日志在 `.setup()` 里初始化,不在这里。
    // 落盘目录取自 `app_data_dir()`,而那个路径要有 AppHandle 才能解析 ——
    // 在这里自己按平台规则拼一遍会和 Tauri 的实现产生第二个真相来源。
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 日志必须最先初始化,否则 setup 里后续的日志会漏掉落盘。
            //
            // 之前只写标准输出:桌面应用从 Finder / 开始菜单启动时标准输出无处可去,
            // 日志只活在 2000 条内存环里,进程一崩就没有任何现场。
            let data_dir = app.path().app_data_dir().ok();
            match data_dir.as_ref().map(|d| d.join("logs")) {
                Some(dir) => match common::logging::init_with_file(dir) {
                    Some(dir) => tracing::info!(log_dir = %dir.display(), "日志已落盘"),
                    None => {
                        common::logging::init();
                        tracing::warn!("日志目录不可用,本次仅输出到控制台");
                    }
                },
                None => {
                    common::logging::init();
                    tracing::warn!("无法定位应用数据目录,本次仅输出到控制台");
                }
            }

            // 状态在 setup 里注册而不是 builder 上:媒体预览授权要落到应用数据目录,
            // 而那个目录只有拿到 AppHandle 之后才能解析出来。setup 早于窗口加载,
            // 任何命令跑起来时状态都已就位。
            app.manage(AppState::new(data_dir.clone()));
            let artifact_dir = data_dir
                .unwrap_or_else(std::env::temp_dir)
                .join("uvp-reports");
            tauri::async_runtime::spawn_blocking(move || {
                let health = cleanup_artifacts(&artifact_dir, std::collections::HashSet::new());
                if !health.healthy {
                    tracing::warn!(?health, "应用启动时产物清理未完全健康");
                }
            });
            tracing::info!(
                media_grants = app.state::<AppState>().media_grants.len(),
                work_dir = %media_rtp::work_dir().display(),
                "已载入媒体预览授权记录"
            );
            // 实时日志推送:**批量聚合**后再过 IPC。
            //
            // 之前是每条日志一个 `log_line` 事件。压测时 10k 设备的 info 日志轻易到
            // 每秒数千条,每条都要 JSON 序列化 + 跨进程送进 webview——前端裁到 3000 条
            // 是在代价已经付完之后。现在每 100ms 合并成一个 `log_batch` 事件,
            // 上限 10 个事件/秒,并统计因队列满被丢弃的条数。
            let handle = app.handle().clone();
            let (log_tx, mut log_rx) = tokio::sync::mpsc::channel::<common::logging::LogLine>(4096);
            let dropped = Arc::new(AtomicU64::new(0));
            let dropped_producer = Arc::clone(&dropped);
            common::logging::set_callback(move |line| {
                if log_tx.try_send(line.clone()).is_err() {
                    dropped_producer.fetch_add(1, Ordering::Relaxed);
                }
            });
            tauri::async_runtime::spawn(async move {
                const FLUSH_MS: u64 = 100;
                const MAX_PER_BATCH: usize = 500;
                let mut ticker = tokio::time::interval(std::time::Duration::from_millis(FLUSH_MS));
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut batch: Vec<common::logging::LogLine> = Vec::with_capacity(MAX_PER_BATCH);
                loop {
                    ticker.tick().await;
                    batch.clear();
                    while batch.len() < MAX_PER_BATCH {
                        match log_rx.try_recv() {
                            Ok(line) => batch.push(line),
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            // 发送端随回调常驻,不会真的断开;断了就退出任务。
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => return,
                        }
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    let dropped_now = dropped.swap(0, Ordering::Relaxed);
                    let _ = handle.emit(
                        "log_batch",
                        serde_json::json!({ "lines": &batch, "dropped": dropped_now }),
                    );
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            engine_version,
            get_inbound_security_capabilities,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
            export_report,
            list_live_sources,
            probe_live_source,
            start_device,
            stop_device,
            fire_alarm,
            fire_position,
            get_broadcast_status,
            get_media_telemetry,
            hangup_broadcast,
            set_sip_trace,
            get_stress_status,
            list_stress_runs,
            get_stress_run_series,
            get_device_status,
            load_catalog_template,
            get_catalog_tree,
            upsert_channel,
            remove_channel,
            set_log_level,
            get_log_level,
            get_recent_logs,
            allow_media_preview,
            pick_share_target,
            pick_video_source,
            export_diagnostics,
            secrets::save_platform_secret,
            secrets::delete_platform_secret,
            secrets::platform_secret_status,
        ])
        .build(tauri::generate_context!())
        .expect("Tauri 应用启动失败")
        .run(|app, event| {
            // 退出前收敛设备与压测。
            //
            // 之前没有这个钩子,关窗口等于直接结束进程,后果有三个:
            // 1. 不发注销 REGISTER,平台要等心跳超时(默认几分钟)才把设备置离线,
            //    下一轮联调看到的是一台"在线但不响应"的幽灵设备;
            // 2. 压测的分片 socket 被硬关,平台侧留下上万个半死连接;
            // 3. ffmpeg 采集子进程来不及被 Drop 清掉,变成孤儿进程继续占摄像头。
            if let tauri::RunEvent::ExitRequested { api, .. } = &event {
                // 只拦第一次。`AppHandle::exit()` 会再发一次 ExitRequested,
                // 无条件 prevent_exit 会让应用永远退不掉。
                if CLEANUP_STARTED.swap(true, Ordering::SeqCst) {
                    return;
                }
                api.prevent_exit();
                let handle = app.clone();
                tauri::async_runtime::spawn(async move {
                    shutdown_all(&handle.state::<AppState>()).await;
                    handle.exit(0);
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 实时源部分错误不会透传_ffmpeg_输出() {
        let safe = sanitize_catalog_error(
            "FFmpeg 未返回可解析设备: /Users/alice/private.mov secret".into(),
        );
        assert!(safe.contains("详细输出已隐藏"));
        assert!(!safe.contains("private.mov"));
        assert!(!safe.contains("secret"));
    }

    #[test]
    fn 入站安全能力不谎报双向_digest() {
        let capabilities = get_inbound_security_capabilities();
        assert!(capabilities.source_ip_filter);
        assert!(capabilities.from_platform_id);
        assert!(capabilities.digest_client_registration);
        assert!(!capabilities.digest_server_validation);
        assert!(capabilities.literal_platform_ip_only);
    }

    #[test]
    fn sip追踪里的鉴权头被脱敏() {
        // SIP 追踪面板会把整条报文原文推给前端,用户还能复制导出。
        // Digest 的 response= 是可离线爆破的口令哈希,而同一条报文里
        // username/realm/nonce/uri/method 全是已知明文 —— 等于连盐一起交出去。
        let raw = "REGISTER sip:3402@example SIP/2.0\r\n\
                   Via: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK1\r\n\
                   Authorization: Digest username=\"34020000001320000001\", \
                   realm=\"3402000000\", nonce=\"abc123\", uri=\"sip:3402@example\", \
                   response=\"6629fae49393a05397450978507c4ef1\"\r\n\
                   CSeq: 2 REGISTER\r\n\
                   Content-Length: 0\r\n\r\n";
        let out = redact_sip_credentials(raw);
        assert!(
            !out.contains("6629fae49393a05397450978507c4ef1"),
            "摘要不能出现在推给前端的报文里:\n{out}"
        );
        assert!(!out.contains("nonce=\"abc123\""));
        assert!(
            out.contains(&format!("Authorization: {REDACTED_MARK}")),
            "应保留头域名与「带了鉴权」这个事实,便于排障:\n{out}"
        );
        // 其它头与起始行必须原样保留。
        assert!(out.starts_with("REGISTER sip:3402@example SIP/2.0\r\n"));
        assert!(out.contains("Via: SIP/2.0/UDP 10.0.0.2:5060;branch=z9hG4bK1\r\n"));
        assert!(out.contains("CSeq: 2 REGISTER\r\n"));
        assert!(out.ends_with("\r\n\r\n"), "头部结束的空行要保留");
    }

    #[test]
    fn 脱敏覆盖全部鉴权头且大小写不敏感() {
        for name in [
            "Authorization",
            "authorization",
            "PROXY-AUTHORIZATION",
            "WWW-Authenticate",
            "Proxy-Authenticate",
            "Authentication-Info",
        ] {
            let raw = format!("SIP/2.0 401 Unauthorized\r\n{name}: Digest secret-here\r\n\r\n");
            let out = redact_sip_credentials(&raw);
            assert!(!out.contains("secret-here"), "{name} 未脱敏: {out}");
            assert!(out.contains(REDACTED_MARK));
        }
    }

    #[test]
    fn 脱敏不动正文() {
        // MANSCDP 正文里没有凭据(目录项的 Password 我们从不输出),
        // 而正文还要给用户看,不能被误伤。
        let raw = "MESSAGE sip:x SIP/2.0\r\n\
                   Authorization: Digest response=\"deadbeef\"\r\n\
                   Content-Length: 60\r\n\r\n\
                   <?xml version=\"1.0\"?><Notify>Authorization: 不该被改</Notify>";
        let out = redact_sip_credentials(raw);
        assert!(!out.contains("deadbeef"), "头部仍要脱敏");
        assert!(
            out.contains("<Notify>Authorization: 不该被改</Notify>"),
            "空行之后是正文,不该被当成头部处理:\n{out}"
        );
    }

    #[tokio::test]
    async fn stress_completion区分完成值和sender异常关闭() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).expect("发布完成");
        assert!(wait_stress_completion(&mut rx).await);

        let (tx, mut rx) = tokio::sync::watch::channel(false);
        drop(tx);
        assert!(
            !wait_stress_completion(&mut rx).await,
            "sender 未写 true 就关闭不得误判完成"
        );
    }

    #[tokio::test]
    async fn stress_completion不早于guard销毁被观察到() {
        let dir =
            std::env::temp_dir().join(format!("uvp-completion-seq-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");
        let (completion_tx, mut rx) = tokio::sync::watch::channel(false);
        let writer = series::SeriesWriter::new(&dir, 999).expect("创建 writer");
        let inprogress_path = writer.inprogress_path().to_path_buf();
        assert!(artifacts::is_registered_active(&inprogress_path));

        // 模拟 supervisor 尾部：先释放 writer guard，再发送 completion=true
        drop(writer);
        assert!(
            !artifacts::is_registered_active(&inprogress_path),
            "guard 销毁后注销活跃注册"
        );
        completion_tx.send(true).expect("发送 completion");

        assert!(wait_stress_completion(&mut rx).await);
        // 观察到 completion 时活跃注册必须已经消除，不得产生竞争
        assert!(!artifacts::is_registered_active(&inprogress_path));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn 重复启动被拒绝时不残留空inprogress() {
        let app_state = AppState::new(None);
        let (tx, _) = tokio::sync::broadcast::channel(1);
        let (_, rx) = tokio::sync::watch::channel(false);
        let dir =
            std::env::temp_dir().join(format!("uvp-no-orphan-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");

        // 模拟前序压测正在运行
        {
            let mut guard = app_state.stress.lock().await;
            *guard = Some(StressHandle {
                run_id: 1,
                stop_tx: tx,
                stopping: false,
                stopping_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                worker_abort: tokio::spawn(async {}).abort_handle(),
                completion: rx,
                device_count: 1,
                series_path: None,
            });
        }

        // 槽位被占用时直接返回拒绝，在此分支前不得触碰 SeriesWriter::new
        let guard = app_state.stress.lock().await;
        assert!(guard.is_some(), "已有任务运行");
        drop(guard);

        let inprogress_count = std::fs::read_dir(&dir)
            .expect("读取目录")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".inprogress"))
            .count();
        assert_eq!(
            inprogress_count, 0,
            "拒绝启动路径下绝不创建任何 .inprogress 文件"
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn schema_v1报告边界保持裸metrics数组() {
        let dir =
            std::env::temp_dir().join(format!("uvp-report-v1-{}", artifacts::unique_suffix()));
        artifacts::create_secure_dir(&dir).expect("建目录");
        let path = dir.join("series-run1-1-1.jsonl");
        std::fs::write(
            &path,
            b"{\"kind\":\"sample\",\"seq\":1,\"ts_ms\":123,\"metrics\":{\"devices_online\":7}}\n{\"kind\":\"run_end\",\"seq\":2,\"sample_count\":1,\"outcome\":\"completed\",\"complete\":true,\"degraded\":false}\n",
        )
        .expect("写时序");
        let series = series::read_series(&path);
        assert!(series.complete, "reader 应先用 envelope 校验完整性");
        let report = build_report_v1(ReportV1Input {
            policy: artifacts::RetentionPolicy::default(),
            retention_health: artifacts::CleanupHealth::default(),
            environment: diagnostics::Environment::collect(),
            scenario: None,
            metrics: Metrics::default().snapshot(),
            series,
            series_file: Some(path.to_string_lossy().into_owned()),
            generated_at_epoch_secs: 1,
        });

        assert_eq!(report["schema_version"], 1);
        let samples = report["series"].as_array().expect("series 数组");
        assert_eq!(samples, &[serde_json::json!({"devices_online": 7})]);
        for field in ["kind", "seq", "ts_ms", "metrics"] {
            assert!(
                samples[0].get(field).is_none(),
                "不得泄漏 envelope 字段 {field}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn 脱敏对畸形输入不panic() {
        assert_eq!(redact_sip_credentials(""), "");
        // 没有空行(只有头部)。
        let out = redact_sip_credentials("Authorization: x\r\n");
        assert!(out.contains(REDACTED_MARK));
        // 没有冒号的行原样保留。
        assert_eq!(redact_sip_credentials("garbage\r\n"), "garbage\r\n");
        // LF-only 行尾也要能处理(某些平台不严格用 CRLF)。
        let out = redact_sip_credentials("Authorization: x\n\nbody");
        assert!(out.contains(REDACTED_MARK) && out.ends_with("body"));
    }
}
