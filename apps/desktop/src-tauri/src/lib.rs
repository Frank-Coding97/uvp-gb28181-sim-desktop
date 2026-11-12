//! Tauri 桌面端后端库。
//!
//! 注册 IPC 命令。
//! 第一阶段内嵌 stress-engine；后续可拆为独立进程。

use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, OnceLock,
};
use std::{collections::HashSet, net::IpAddr, time::Duration};

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
mod device_metadata;
mod log_export;
mod preview_channel;
mod preview_manager;
mod report;
use config_store::{
    BindMode, CatalogNodeConfig, ConfigStore, DesktopConfigV2, EffectiveDeviceConfig,
    RecordPlanSettings, StartDeviceInput, TimeSyncSettings,
};
use preview_channel::DesktopPreviewBus;
use preview_manager::{
    PreviewEventSink, PreviewForwardTarget, PreviewManager, PreviewMediaSource, PreviewSubscription,
};

/// 应用全局状态（托管在 Tauri managed state）。
struct AppState {
    /// Rust 端配置真相源；路径在首次 Tauri 命令时由 app_data_dir 初始化。
    config_store: OnceLock<ConfigStore>,
    /// 应用数据根目录（配置 / 录像 / 升级状态共用）与它来自哪个候选。
    /// 解析一次即缓存：探测要真写文件，不该在每次启动设备时重复做。
    data_root: OnceLock<(std::path::PathBuf, String)>,
    /// 录像索引真相，设备停止后仍可查看历史文件。
    recording_store: OnceLock<Arc<RecordingStore>>,
    /// 本地抓拍图库（预览帧旁路落盘）。
    snapshot_store: OnceLock<Arc<media_rtp::snapshot::SnapshotStore>>,
    /// 报警历史（最近 50 条，成功与失败都记）。
    alarm_history: Mutex<std::collections::VecDeque<AlarmRecord>>,
    /// 报警记录单调递增 ID，避免同一毫秒的多条记录排序不稳定。
    next_alarm_id: AtomicU64,
    /// 配置文件读写串行化，避免并发保存与读取看到半次操作。
    config_io: Mutex<()>,
    /// Independent cancellation path: preparation holds the device startup lock.
    media_preparation: std::sync::Mutex<Option<Arc<AtomicBool>>>,
    /// 设备已注册、但文件媒体仍在后台预热时的取消令牌。
    ///
    /// 与 `media_preparation` 分开：后者只覆盖"设备启动请求"这段短生命周期，
    /// 一旦 REGISTER 提交就会被清空；而预热可能还要跑几分钟，必须有自己的槽位，
    /// 停止设备/退出应用时才能把它一起掐掉。
    media_task: std::sync::Mutex<Option<Arc<AtomicBool>>>,
    shutting_down: AtomicBool,
    /// 当前压测句柄；任务完全退出前始终占用槽位。
    stress: Mutex<Option<StressHandle>>,
    /// 分配单调递增的压测 ID,防止旧任务事件污染新任务状态。
    next_stress_id: AtomicU64,
    /// 当前压测的实时指标；None 表示任务已完全退出。
    metrics: Mutex<Option<Arc<Metrics>>>,
    /// 最近一次压测的完整记录（场景 + 秒级采样 + 最终指标），供导出报告。
    /// 停止后保留，直到下一次压测启动才清空。
    last_run: Mutex<Option<report::RunRecord>>,
    /// 正在运行的这次压测记录；每秒追加采样点。
    /// 与 `last_run` 分开存放：运行时导出报告读这里，结束后由任务冻结进 `last_run`。
    live_run: Mutex<Option<Arc<std::sync::Mutex<report::RunRecord>>>>,
    /// 单设备联调：当前设备实例 + 停止发射端 + 共享传输。
    device: Mutex<Option<DeviceHandle>>,
    /// 采集侧 MJPEG 旁路的最新帧总线。
    preview_bus: Arc<DesktopPreviewBus>,
    /// 应用级唯一预览 Channel 管理器；页面只 attach/detach。
    preview_manager: Arc<PreviewManager>,
    /// NTP 周期刷新任务（设备运行期才启动）。
    ntp: Mutex<NtpRuntime>,
    /// 运行期录像计划。与配置文件同源，但调度任务每 10s 要读一次，
    /// 走内存共享而不是磁盘读，避免把 IO 抖动带进起停判断。
    record_plan: Arc<tokio::sync::RwLock<RecordPlanSettings>>,
    /// 定时录像调度任务（设备运行期才启动）。
    plan_scheduler: Mutex<NtpRuntime>,
}

/// NTP 刷新任务的运行态。
///
/// `running` 用 `Arc<AtomicBool>` 而不是普通 bool：任务自己退出（例如取消后
/// 跑完最后一轮）也要能把状态置回 false，否则界面会一直显示"刷新中"。
struct NtpRuntime {
    cancel: Option<Arc<AtomicBool>>,
    running: Arc<AtomicBool>,
}

impl NtpRuntime {
    fn new() -> Self {
        Self {
            cancel: None,
            running: Arc::new(AtomicBool::new(false)),
        }
    }
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

/// 报警历史上限。50 条够覆盖一次联调会话，又不会让页面滚动失去意义。
const ALARM_HISTORY_LIMIT: usize = 50;

/// 一条报警上报记录（成功与失败都记）。
#[derive(Debug, Clone, Serialize)]
struct AlarmRecord {
    id: u64,
    device_id: String,
    priority: u8,
    method: u8,
    alarm_type: Option<u32>,
    description: Option<String>,
    longitude: Option<f64>,
    latitude: Option<f64>,
    /// 报文里实际写入的报警时间（可能因"N 分钟前"而早于 `captured_at_ms`）。
    alarm_time: String,
    /// 落库时刻。
    captured_at_ms: i64,
    /// 是否走了平台 Event:Alarm 订阅对话（否则是独立 MESSAGE）。
    via_notify: bool,
    /// 平台响应码；失败时为 None。
    status: Option<u16>,
    /// 失败原因；成功时为 None。
    error: Option<String>,
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

/// 后台媒体预热槽位的 RAII 占位。
///
/// 只有显式 `release()` 才把令牌交给后台任务；任何提前返回（transport 绑定失败、
/// 应用退出…）都会在 Drop 里释放，避免"设备没起来但槽位一直被占"。
struct MediaTaskSlot<'a> {
    state: &'a AppState,
    token: Arc<AtomicBool>,
    armed: bool,
}

impl<'a> MediaTaskSlot<'a> {
    fn new(state: &'a AppState) -> Result<Self, String> {
        let token = state.begin_media_task()?;
        Ok(Self {
            state,
            token,
            armed: true,
        })
    }

    fn release(mut self) -> Arc<AtomicBool> {
        self.armed = false;
        Arc::clone(&self.token)
    }
}

impl Drop for MediaTaskSlot<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.state.finish_media_task(&self.token);
        }
    }
}

/// 媒体准备阶段的用户可读文案。
fn media_preparation_message(
    phase: media_rtp::file_profile::FilePreparationPhase,
) -> &'static str {
    use media_rtp::file_profile::FilePreparationPhase;
    match phase {
        FilePreparationPhase::HashingSource => "正在校验源文件",
        FilePreparationPhase::CheckingCache => "正在检查文件缓存",
        FilePreparationPhase::Transcoding => "正在按所选参数转换音视频",
        FilePreparationPhase::Extracting => "正在建立音视频索引",
        FilePreparationPhase::Probing => "正在验证输出音视频",
        FilePreparationPhase::Publishing => "正在保存准备结果",
        FilePreparationPhase::Complete => "文件音视频准备完成",
    }
}

/// 上报媒体准备进度。
///
/// `completed`/`total` 同单位（字节或微秒），前端据此画进度条并估算剩余时间；
/// 拿不到总量时只显示阶段文案，不编造百分比。
fn emit_media_progress(
    app: &AppHandle,
    progress: media_rtp::file_profile::FilePreparationProgress,
) {
    let mut payload = serde_json::json!({
        "phase": "preparing",
        "message": media_preparation_message(progress.phase),
        "completed": progress.completed,
        "total": progress.total,
    });
    if let (Some(done), Some(total)) = (progress.completed, progress.total) {
        if total > 0 {
            payload["percent"] =
                serde_json::json!((done as f64 * 100.0 / total as f64).clamp(0.0, 100.0));
        }
    }
    let _ = app.emit("media_preparation", payload);
}

/// 后台任务里没有 `State` 借用，只能从 `AppHandle` 反查状态来释放预热槽位。
fn finish_media_task_via(app: &AppHandle, token: &Arc<AtomicBool>) {
    let state = app.state::<AppState>();
    state.finish_media_task(token);
}

impl AppState {
    /// 启动（或重启）NTP 周期刷新任务。
    ///
    /// 每次重启都换一个新的 `running` 标志：旧任务退出时只会把自己那份置 false，
    /// 不会把新任务的状态误标成"未运行"。
    async fn restart_ntp_refresh(&self, settings: TimeSyncSettings) {
        let mut runtime = self.ntp.lock().await;
        if let Some(cancel) = runtime.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let running = Arc::new(AtomicBool::new(true));
        runtime.cancel = Some(Arc::clone(&cancel));
        runtime.running = Arc::clone(&running);
        drop(runtime);
        tokio::spawn(ntp_refresh_loop(settings, cancel, running));
    }

    async fn stop_ntp_refresh(&self) {
        let mut runtime = self.ntp.lock().await;
        if let Some(cancel) = runtime.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        runtime.running.store(false, Ordering::Release);
    }

    async fn ntp_running(&self) -> bool {
        self.ntp.lock().await.running.load(Ordering::Acquire)
    }

    /// 启动定时录像调度任务（重复启动会先取消旧任务）。
    async fn start_plan_scheduler(&self, sim: Arc<DeviceSimulator>) {
        let mut runtime = self.plan_scheduler.lock().await;
        if let Some(cancel) = runtime.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        // 每次启动换新的运行标志：旧任务退出时不能把新任务标成"未运行"。
        let running = Arc::new(AtomicBool::new(true));
        runtime.cancel = Some(Arc::clone(&cancel));
        runtime.running = Arc::clone(&running);
        let plan = Arc::clone(&self.record_plan);
        tokio::spawn(record_plan_loop(sim, plan, cancel, running));
    }

    async fn stop_plan_scheduler(&self) {
        let mut runtime = self.plan_scheduler.lock().await;
        if let Some(cancel) = runtime.cancel.take() {
            cancel.store(true, Ordering::Release);
        }
        runtime.running.store(false, Ordering::Release);
    }

    async fn plan_scheduler_running(&self) -> bool {
        self.plan_scheduler
            .lock()
            .await
            .running
            .load(Ordering::Acquire)
    }

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
        // 先掐后台预热：设备已经注册之后，用户点「取消准备」针对的就是它。
        {
            let task = self
                .media_task
                .lock()
                .map_err(|_| "媒体预热状态锁异常")?;
            if let Some(cancel) = task.as_ref() {
                cancel.store(true, Ordering::Release);
                return Ok(());
            }
        }
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

    /// 占用后台媒体预热槽位。
    ///
    /// 已有任务在跑时直接拒绝：并发预热同一素材既浪费 CPU，也会让"当前进度"
    /// 出现两条互不相关的流。调用方应先让用户停止设备（会取消旧任务）。
    fn begin_media_task(&self) -> Result<Arc<AtomicBool>, String> {
        let mut slot = self.media_task.lock().map_err(|_| "媒体预热状态锁异常")?;
        if slot.is_some() {
            return Err("媒体仍在准备中，请等待完成或先停止设备".into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *slot = Some(Arc::clone(&cancel));
        Ok(cancel)
    }

    /// 释放槽位。只清自己那一个令牌，避免旧任务退出时误删新任务的占位。
    fn finish_media_task(&self, token: &Arc<AtomicBool>) {
        if let Ok(mut slot) = self.media_task.lock() {
            if slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, token))
            {
                *slot = None;
            }
        }
    }

    /// 停止设备/退出应用时取消后台预热。没有任务时是无操作。
    fn cancel_media_task(&self) {
        if let Ok(slot) = self.media_task.lock() {
            if let Some(token) = slot.as_ref() {
                token.store(true, Ordering::Release);
            }
        }
    }

    fn media_task_running(&self) -> bool {
        self.media_task
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    fn new() -> Self {
        let preview_bus = Arc::new(DesktopPreviewBus::new());
        let preview_manager = Arc::new(PreviewManager::new(Arc::clone(&preview_bus)));
        AppState {
            config_store: OnceLock::new(),
            data_root: OnceLock::new(),
            recording_store: OnceLock::new(),
            snapshot_store: OnceLock::new(),
            alarm_history: Mutex::new(std::collections::VecDeque::new()),
            next_alarm_id: AtomicU64::new(1),
            config_io: Mutex::new(()),
            media_preparation: std::sync::Mutex::new(None),
            media_task: std::sync::Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            stress: Mutex::new(None),
            next_stress_id: AtomicU64::new(1),
            metrics: Mutex::new(None),
            last_run: Mutex::new(None),
            live_run: Mutex::new(None),
            device: Mutex::new(None),
            preview_bus,
            preview_manager,
            ntp: Mutex::new(NtpRuntime::new()),
            record_plan: Arc::new(tokio::sync::RwLock::new(RecordPlanSettings::default())),
            plan_scheduler: Mutex::new(NtpRuntime::new()),
        }
    }
}

/// NTP 周期刷新。首次查询立即执行，之后按 `refresh_interval_secs` 循环。
///
/// 失败不终止循环：NTP 服务器通常是内网地址，偶发超时不该让校时永久停摆；
/// 每次失败都会回退到 SIP Date 并把原因写进 [`common::clock`]，界面可查。
/// 计划是否真的会生效：开关打开且至少有一段时间。
///
/// 只开开关不画时段等于不生效；这种情况按"没有定时计划"处理，避免平台侧看到
/// 一个永远不会触发的定时计划类型。
fn plan_effective(plan: &RecordPlanSettings) -> bool {
    plan.enabled && plan.has_segments()
}

/// 定时录像调度：每 10 秒判断"此刻是否落在计划时段内"，据此起停录像。
///
/// 10 秒而不是 1 秒：计划粒度是 30 分钟格（可精确到秒），10 秒的判定延迟远小于
/// 误差容忍，又不会让一台设备在后台每秒抢一次锁。
async fn record_plan_loop(
    sim: Arc<DeviceSimulator>,
    plan: Arc<tokio::sync::RwLock<RecordPlanSettings>>,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
) {
    // 只有本任务会写这个标记：用来区分「计划起的录像」和「用户手动起的录像」。
    // 时段结束时不能把用户手动录的那段一起停掉。
    let mut plan_owned = false;
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let settings = plan.read().await.clone();
        let (weekday, secs) = record_plan::weekday_and_secs(common::clock::synced_epoch_secs());
        let in_window = settings.enabled && settings.week.should_record(weekday, secs);
        let phase = sim.recording_service().map(|service| service.state().phase);
        match phase {
            Some(RecordingPhase::Idle) if in_window => {
                if !plan_owned {
                    match sim
                        .start_recording(RecordingKind::Time, RecordingSource::Local)
                        .await
                    {
                        Ok(state) => {
                            plan_owned = true;
                            tracing::info!(
                                target: "uvp::record_plan",
                                weekday,
                                secs,
                                active_id = state.active_id.as_deref().unwrap_or(""),
                                "进入计划时段，开始定时录像"
                            );
                        }
                        Err(error) => {
                            // 起不来通常是媒体未就绪；下一轮再试，不把任务打死。
                            tracing::warn!(target: "uvp::record_plan", %error, "定时录像启动失败，等待下一轮重试");
                        }
                    }
                }
            }
            Some(RecordingPhase::Recording) if !in_window && plan_owned => {
                match sim.stop_recording().await {
                    Ok(entry) => {
                        plan_owned = false;
                        tracing::info!(
                            target: "uvp::record_plan",
                            weekday,
                            secs,
                            entry = entry.as_ref().map(|item| item.id.as_str()).unwrap_or(""),
                            "计划时段结束，已停止定时录像"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(target: "uvp::record_plan", %error, "停止定时录像失败");
                    }
                }
            }
            // 录像被外部停掉（用户手动停止/失败收尾）后，计划拥有权随之释放。
            Some(RecordingPhase::Idle) => plan_owned = false,
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    running.store(false, Ordering::Release);
}

async fn ntp_refresh_loop(
    settings: TimeSyncSettings,
    cancel: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
) {
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let server = settings.ntp_server.clone();
        let port = settings.ntp_port;
        let outcome = tokio::task::spawn_blocking(move || {
            common::ntp::query(&server, port, Duration::from_millis(1_500))
        })
        .await;
        match outcome {
            Ok(Ok(sample)) => {
                common::clock::apply_ntp(
                    sample.offset_secs(),
                    &sample.server,
                    sample.round_trip_ms,
                );
                tracing::info!(
                    server = %sample.server,
                    offset_secs = sample.offset_secs(),
                    rtt_ms = sample.round_trip_ms,
                    stratum = sample.stratum,
                    "NTP 校时完成"
                );
            }
            Ok(Err(error)) => {
                common::clock::ntp_failed(&error);
                tracing::warn!(error = %error, "NTP 校时失败，已回退 SIP Date");
            }
            Err(error) => {
                common::clock::ntp_failed(&format!("NTP 查询任务异常: {error}"));
                tracing::warn!(error = %error, "NTP 查询任务异常");
            }
        }

        let interval = Duration::from_secs(settings.refresh_interval_secs.max(1));
        let deadline = tokio::time::Instant::now() + interval;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if cancel.load(Ordering::Acquire) {
                break;
            }
        }
    }
    running.store(false, Ordering::Release);
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

fn app_snapshot_store(
    state: &AppState,
    app: &AppHandle,
) -> Result<Arc<media_rtp::snapshot::SnapshotStore>, String> {
    if let Some(store) = state.snapshot_store.get() {
        return Ok(Arc::clone(store));
    }
    let directory = app_data_root(state, app)?.join("snapshots");
    let store = Arc::new(
        media_rtp::snapshot::SnapshotStore::open(&directory)
            .map_err(|error| format!("打开抓拍目录 {} 失败: {error}", directory.display()))?,
    );
    let _ = state.snapshot_store.set(store);
    Ok(Arc::clone(
        state
            .snapshot_store
            .get()
            .expect("SnapshotStore 初始化后必须存在"),
    ))
}

/// 抓拍当前采集画面。
///
/// 取共享媒体缓存的最近一个可独立解码的关键帧转 JPEG,与平台点播同源;
/// 不要求本地预览开启,也不会打断推流。设备未采集时明确报错。
#[tauri::command]
async fn take_snapshot(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<media_rtp::snapshot::SnapshotMeta, String> {
    let sim = state
        .device
        .lock()
        .await
        .as_ref()
        .map(|device| device.sim.clone())
        .ok_or_else(|| "设备尚未启动，无法抓拍".to_string())?;
    let jpeg = sim
        .current_keyframe_jpeg()
        .await
        .map_err(|error| error.to_string())?;
    let store = app_snapshot_store(&state, &app)?;
    let captured_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    tokio::task::spawn_blocking(move || store.save(&jpeg, captured_at_ms))
        .await
        .map_err(|error| format!("抓拍任务异常: {error}"))?
}

#[tauri::command]
async fn list_snapshots(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    limit: Option<usize>,
) -> Result<Vec<media_rtp::snapshot::SnapshotMeta>, String> {
    let store = app_snapshot_store(&state, &app)?;
    let limit = limit.unwrap_or(60).clamp(1, 500);
    tokio::task::spawn_blocking(move || store.list(limit))
        .await
        .map_err(|error| format!("读取抓拍列表任务异常: {error}"))?
}

/// 读取抓拍并转成 data URL。
///
/// 不走自定义协议的原因：数据根目录可能回落到 `exe_dir/data`，而 assetProtocol
/// 的 scope 只能写死 `$APPDATA`；用 data URL 就不依赖实际落盘路径。
#[tauri::command]
async fn read_snapshot(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<String, String> {
    use base64::Engine as _;
    let store = app_snapshot_store(&state, &app)?;
    let bytes = tokio::task::spawn_blocking(move || store.read(&id))
        .await
        .map_err(|error| format!("读取抓拍任务异常: {error}"))??;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[tauri::command]
async fn delete_snapshot(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<(), String> {
    let store = app_snapshot_store(&state, &app)?;
    tokio::task::spawn_blocking(move || store.delete(&id))
        .await
        .map_err(|error| format!("删除抓拍任务异常: {error}"))?
}

#[tauri::command]
async fn export_snapshot(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    id: String,
    target_path: String,
) -> Result<String, String> {
    let store = app_snapshot_store(&state, &app)?;
    let target = std::path::PathBuf::from(target_path);
    tokio::task::spawn_blocking(move || store.export_copy(&id, &target))
        .await
        .map_err(|error| format!("导出抓拍任务异常: {error}"))??;
    Ok("抓拍已另存".into())
}

/// 导出当前页签日志（敏感字段已脱敏）。
#[tauri::command]
async fn export_logs(
    tab: String,
    blocks: Vec<String>,
    target_path: String,
) -> Result<log_export::LogExportResult, String> {
    if blocks.is_empty() {
        return Err("当前页签没有可导出的内容".into());
    }
    if blocks.len() > 20_000 {
        return Err("单次导出条目过多，请先筛选后再导出".into());
    }
    let label = match tab.as_str() {
        "sip" => "SIP 报文",
        "system" => "系统日志",
        "commands" => "平台命令",
        other => return Err(format!("未知日志页签: {other}")),
    };
    let target = std::path::PathBuf::from(target_path);
    let exported_at = common::clock::epoch_to_iso8601(common::clock::local_epoch_ms() / 1_000);
    tokio::task::spawn_blocking(move || {
        log_export::write_export(label, &exported_at, &blocks, &target)
    })
    .await
    .map_err(|error| format!("导出日志任务异常: {error}"))?
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

/// 内置演示视频（`resources/builtin-camera.mp4`）在运行期的绝对路径。
///
/// 发布包通过 `bundle.resources` 把它放到资源目录；`tauri dev` 下资源目录可能指到
/// `target/debug`，未必已经复制，所以再回退到源码树里的同一份文件。找不到返回
/// `None`，由调用方退回「未配置媒体源」的旧行为——内置素材不能变成启动硬依赖。
fn resolve_builtin_video(app: &AppHandle) -> Option<std::path::PathBuf> {
    const FILE_NAME: &str = "builtin-camera.mp4";
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = app.path().resource_dir() {
        candidates.push(dir.join("resources").join(FILE_NAME));
        candidates.push(dir.join(FILE_NAME));
    }
    candidates.push(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(FILE_NAME),
    );
    candidates.into_iter().find(|path| path.is_file())
}

/// 首页默认媒体源：内置演示视频的绝对路径（未打包素材时返回 `null`）。
#[tauri::command]
fn builtin_video_source(app: AppHandle) -> Option<String> {
    resolve_builtin_video(&app).map(|path| path.to_string_lossy().into_owned())
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

fn load_control_state(sim: &gb28181_simulator::DeviceSimulator, state: &AppState, app: &AppHandle) {
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

fn save_control_state(sim: &gb28181_simulator::DeviceSimulator, state: &AppState, app: &AppHandle) {
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

/// 校时页要的全部信息:生效偏移、来源、SIP 回退值与 NTP 配置。
#[derive(Debug, Serialize)]
struct ClockStateDto {
    /// 当前生效偏移/来源/凭证明细。
    clock: common::clock::ClockSnapshot,
    /// 持久化的 NTP 配置。
    time_sync: TimeSyncSettings,
    /// NTP 周期刷新任务是否在跑(设备未运行或未启用时为 false)。
    ntp_running: bool,
    /// 本机当前时间(毫秒),由前端做本地时区格式化。
    local_epoch_ms: i64,
    /// 校时后的设备对外时间(毫秒)= 本地 + 偏移。
    platform_epoch_ms: i64,
    /// 读取时的配置版本，保存 NTP 配置时原样带回，避免覆盖并发修改。
    config_revision: u64,
}

/// 一次手动 NTP 校时的结果。
#[derive(Debug, Serialize)]
struct NtpProbeDto {
    offset_secs: i64,
    round_trip_ms: i64,
    stratum: u8,
    server: String,
    clock: common::clock::ClockSnapshot,
}

/// 读取当前校时状态与 NTP 配置。
#[tauri::command]
async fn get_clock_state(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<ClockStateDto, String> {
    let store = app_config_store(&state, &app)?;
    let (time_sync, config_revision) = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || {
            store
                .load()
                .map(|config| (config.time_sync, config.config_revision))
        })
        .await
        .map_err(|error| format!("读取配置任务异常: {error}"))??
    };
    let clock = common::clock::snapshot();
    let local_epoch_ms = common::clock::local_epoch_ms();
    Ok(ClockStateDto {
        platform_epoch_ms: local_epoch_ms + clock.offset_secs.saturating_mul(1_000),
        clock,
        ntp_running: state.ntp_running().await,
        time_sync,
        local_epoch_ms,
        config_revision,
    })
}

/// 保存 NTP 配置。
///
/// 与其它配置不同,这里**不要求设备已停止**:校时是运行态能力,内网 NTP 地址经常
/// 要边跑边试。保存后在跑的设备会立刻按新配置重启刷新任务,不需要重新注册。
#[tauri::command]
async fn save_time_sync(
    time_sync: TimeSyncSettings,
    expected_revision: u64,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let store = app_config_store(&state, &app)?;
    let saved = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || store.save_time_sync(time_sync, expected_revision))
            .await
            .map_err(|error| format!("保存校时配置任务异常: {error}"))??
    };
    let device_running = state.device.lock().await.is_some();
    if !saved.time_sync.ntp_enabled {
        state.stop_ntp_refresh().await;
        common::clock::clear_ntp();
    } else if device_running {
        state.restart_ntp_refresh(saved.time_sync.clone()).await;
    }
    Ok(saved)
}

/// 手动触发一次 NTP 校时(不依赖设备是否在运行)。
#[tauri::command]
async fn sync_ntp_now(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<NtpProbeDto, String> {
    let store = app_config_store(&state, &app)?;
    let settings = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || store.load().map(|config| config.time_sync))
            .await
            .map_err(|error| format!("读取配置任务异常: {error}"))??
    };
    let server = settings.ntp_server.clone();
    let port = settings.ntp_port;
    let sample = tokio::task::spawn_blocking(move || {
        common::ntp::query(&server, port, Duration::from_millis(1_500))
    })
    .await
    .map_err(|error| format!("NTP 查询任务异常: {error}"))?
    .map_err(|error| {
        common::clock::ntp_failed(&error);
        error
    })?;
    common::clock::apply_ntp(sample.offset_secs(), &sample.server, sample.round_trip_ms);
    Ok(NtpProbeDto {
        offset_secs: sample.offset_secs(),
        round_trip_ms: sample.round_trip_ms,
        stratum: sample.stratum,
        server: sample.server,
        clock: common::clock::snapshot(),
    })
}

/// 录像计划运行态（界面用来显示"此刻是否在计划时段内"）。
#[derive(Debug, Serialize)]
struct RecordPlanStateDto {
    plan: RecordPlanSettings,
    config_revision: u64,
    scheduler_running: bool,
    /// 今天周几（0=周一）。
    weekday: usize,
    weekday_label: String,
    /// 当前一天内秒偏移。
    secs_of_day: u32,
    /// 此刻是否落在计划时段内（已计入总开关）。
    in_window: bool,
    /// 当前录像状态；设备未启动时为 None。
    recording: Option<RecordingState>,
}

#[tauri::command]
async fn get_record_plan(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<RecordPlanStateDto, String> {
    let store = app_config_store(&state, &app)?;
    let (plan, config_revision) = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || {
            store
                .load()
                .map(|config| (config.record_plan, config.config_revision))
        })
        .await
        .map_err(|error| format!("读取配置任务异常: {error}"))??
    };
    let (weekday, secs_of_day) =
        record_plan::weekday_and_secs(common::clock::synced_epoch_secs());
    let in_window = plan.enabled && plan.week.should_record(weekday, secs_of_day);
    // 运行态从设备句柄取：配置文件说"计划开着"不代表调度任务真的在跑。
    let (recording, scheduler_running) = {
        let guard = state.device.lock().await;
        (
            guard
                .as_ref()
                .and_then(|handle| handle.sim.recording_service())
                .map(|service| service.state()),
            state.plan_scheduler_running().await,
        )
    };
    Ok(RecordPlanStateDto {
        plan,
        config_revision,
        scheduler_running,
        weekday,
        weekday_label: record_plan::weekday_label(weekday).to_string(),
        secs_of_day,
        in_window,
        recording,
    })
}

#[tauri::command]
async fn save_record_plan(
    plan: RecordPlanSettings,
    expected_revision: u64,
    state: tauri::State<'_, AppState>,
    app: AppHandle,
) -> Result<DesktopConfigV2, String> {
    let store = app_config_store(&state, &app)?;
    let saved = {
        let _io = state.config_io.lock().await;
        tokio::task::spawn_blocking(move || store.save_record_plan(plan, expected_revision))
            .await
            .map_err(|error| format!("保存录像计划任务异常: {error}"))??
    };
    // 调度任务每轮读共享状态，保存后立即生效，不用等设备重启。
    *state.record_plan.write().await = saved.record_plan.clone();
    let effective = plan_effective(&saved.record_plan);
    let guard = state.device.lock().await;
    if let Some(handle) = guard.as_ref() {
        handle.sim.set_record_plan_enabled(effective);
    }
    Ok(saved)
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

/// 把场景结构体翻译成报告里的中文参数表。
///
/// 密码一律不写。这里给的是「人话」而不是字段名:报告是给客户看的,
/// `active_ratio=0.3` 这种要读代码才知道含义的写法必须翻译掉。
fn scenario_report_rows(sc: &scenario::LinearScenario) -> Vec<(String, String)> {
    let media = match sc.media_profile {
        scenario::MediaProfile::A => "A 档 · 仅信令，不推流",
        scenario::MediaProfile::B => "B 档 · 轻量伪流",
        scenario::MediaProfile::C => "C 档 · 真实 H.264 推流",
    };
    let ramp = if sc.ramp_per_second == 0 {
        "一次性全部拉起".to_string()
    } else {
        format!("{} 台 / 秒", sc.ramp_per_second)
    };
    vec![
        ("设备名称".into(), sc.device_info.device_name.clone()),
        ("起始设备 ID".into(), sc.base_device_id.clone()),
        (
            "接入平台".into(),
            format!("{}:{}", sc.server_host, sc.server_port),
        ),
        ("信令传输".into(), sc.transport.to_string()),
        (
            "平台 ID / 域".into(),
            if sc.server_id.trim().is_empty() {
                // 旧场景 TOML 不写 server_id,目标按域解析;报告里不能显示成「 / 域」。
                format!("{}（按域解析）", sc.server_domain)
            } else {
                format!("{} / {}", sc.server_id, sc.server_domain)
            },
        ),
        (
            "心跳间隔".into(),
            format!("{} 秒", sc.heartbeat_interval_secs),
        ),
        (
            "每台通道数".into(),
            sc.channels_per_device.to_string(),
        ),
        ("媒体档位".into(), media.to_string()),
        (
            "视频帧率 / 码率".into(),
            format!("{} fps / {} kbps", sc.video_fps, sc.bitrate_kbps),
        ),
        (
            "真实推流占比".into(),
            format!("{:.0}%", sc.active_ratio * 100.0),
        ),
        ("爬坡速率".into(), ramp),
        (
            "厂商 / 型号 / 固件".into(),
            format!(
                "{} / {} / {}",
                sc.device_info.manufacturer, sc.device_info.model, sc.device_info.firmware
            ),
        ),
        ("注册密码".into(), "已隐藏（报告不保存密码）".into()),
    ]
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

    // 报告建档：启动即创建，运行中每秒追加采样点，停下时冻结进 last_run。
    let record = Arc::new(std::sync::Mutex::new(report::RunRecord {
        started_at_ms: common::clock::local_epoch_ms(),
        finished_at_ms: 0,
        device_count: count,
        ramp_per_second: sc.ramp_per_second,
        target: format!("{}:{} · {}", sc.server_host, sc.server_port, sc.transport),
        gb_version: if sc.gb_version.is_2022() {
            "GB/T 28181-2022"
        } else {
            "GB/T 28181-2016"
        }
        .into(),
        scenario_rows: scenario_report_rows(&sc),
        scenario_toml_redacted: redacted_scenario,
        samples: Vec::new(),
        metrics: stress_engine::MetricsSnapshot::default(),
    }));

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
    *state.live_run.lock().await = Some(Arc::clone(&record));
    *state.last_run.lock().await = None;

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
    // 同一份快照同时写入报告记录，保证「界面看到的曲线」和「报告里的曲线」同源。
    let app_for_tick = app.clone();
    let metrics_for_tick = Arc::clone(&metrics);
    let record_for_tick = Arc::clone(&record);
    tokio::spawn(async move {
        let mut tick_stop = tick_stop;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = tick_stop.recv() => break,
                _ = interval.tick() => {
                    let snapshot = metrics_for_tick.snapshot();
                    if let Ok(mut rec) = record_for_tick.lock() {
                        let elapsed = ((common::clock::local_epoch_ms() - rec.started_at_ms).max(0)
                            / 1_000) as u64;
                        let point = report::Sample {
                            offset_secs: elapsed,
                            snapshot: snapshot.clone(),
                        };
                        rec.samples.push(point.clone());
                        // 超上限按 2 抽 1，末点必须留下：长时间压测不能让采样把内存吃穿。
                        if rec.samples.len() > report::MAX_SAMPLES {
                            let mut i = 0usize;
                            rec.samples.retain(|_| {
                                let keep = i % 2 == 0;
                                i += 1;
                                keep
                            });
                            rec.samples.push(point);
                        }
                        rec.metrics = snapshot.clone();
                    }
                    let _ = app_for_tick.emit(
                        "metrics_tick",
                        serde_json::json!({
                            "run_id": run_id,
                            "metrics": snapshot,
                        }),
                    );
                }
            }
        }
    });

    // 压测任务结束后保存最终快照并回收槽位；停止请求不会提前释放运行权。
    let app_for_run = app.clone();
    let metrics_for_run = Arc::clone(&metrics);
    let record_for_run = Arc::clone(&record);
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
        // last_run 先写、live_run 后清，确保报告能力不会出现瞬时空窗。
        let frozen = record_for_run
            .lock()
            .map(|mut rec| {
                rec.finished_at_ms = common::clock::local_epoch_ms();
                rec.metrics = final_metrics.clone();
                rec.clone()
            })
            .ok();
        *state.last_run.lock().await = frozen;
        *state.live_run.lock().await = None;
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
                .last_run
                .lock()
                .await
                .as_ref()
                .map(|run| run.device_count)
                .unwrap_or(0);
            (false, false, None, count)
        }
    };
    let has_report =
        state.metrics.lock().await.is_some() || state.last_run.lock().await.is_some();
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
        // 设备已注册但文件媒体还在后台预热：页面据此显示"上线中/准备中"，
        // 而不是把 capture_state=starting 误读成采集卡住。
        "media_preparing": state.media_task_running(),
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

/// 导出压测报告(FR-28)。
///
/// 主产物是**一份自包含 HTML 报表**(图表内联,浏览器直接打开,可打印成 PDF);
/// 同目录再落一份同名 `.json` 作为机器可读的原始数据,方便二次处理与归档比对。
///
/// `target_path` 由前端「另存为」对话框给出;为空时落到系统临时目录,
/// 便于没有对话框的场景(或自动化调用)也能出报告。
#[tauri::command]
async fn export_report(
    state: tauri::State<'_, AppState>,
    target_path: Option<String>,
) -> Result<String, String> {
    // 与启动/完成提交共用生命周期锁，避免混合读取不同 run 的场景和指标。
    let _stress_guard = state.stress.lock().await;
    // 优先取运行中的记录,否则取最近一次压测冻结下来的记录。
    let live = state.live_run.lock().await.clone();
    let record = match live {
        Some(shared) => shared
            .lock()
            .map(|rec| rec.clone())
            .map_err(|_| "压测记录读取失败".to_string())?,
        None => state
            .last_run
            .lock()
            .await
            .clone()
            .ok_or("无压测数据可导出（尚未运行过压测）")?,
    };

    let html = report::render_html(&record);
    let json = report::render_json(&record);

    // 目标路径:调用方给就用;否则落到临时目录的 uvp-reports/。
    let target = match target_path {
        Some(path) if !path.trim().is_empty() => {
            let mut path = std::path::PathBuf::from(path);
            // 对话框没补扩展名时补上,避免出现无后缀文件。
            if path.extension().is_none() {
                path.set_extension("html");
            }
            path
        }
        _ => {
            let dir = std::env::temp_dir().join("uvp-reports");
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建报告目录失败: {e}"))?;
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            dir.join(format!("report-{ts}.html"))
        }
    };

    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| format!("创建报告目录失败: {e}"))?;
        }
    }

    // 先写 HTML,再写 JSON 附属文件;两者共用同一个文件名主干。
    std::fs::write(&target, html).map_err(|e| format!("写报告失败: {e}"))?;
    let json_path = target.with_extension("json");
    if let Err(error) = std::fs::write(&json_path, json) {
        // 附属文件失败不该让用户以为主报告也没出来。
        tracing::warn!(%error, path = %json_path.display(), "压测报告附属 JSON 写入失败");
    }

    Ok(target.to_string_lossy().to_string())
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

/// 本机 H.264 / H.265 实际使用的视频编码后端(硬件优先,首次调用会试编探测约 1~3 秒)。
#[tauri::command]
async fn get_encoder_status() -> Result<Vec<media_rtp::encoder::SelectedEncoder>, String> {
    tokio::task::spawn_blocking(|| {
        let ffmpeg =
            media_rtp::ffmpeg_bin().ok_or_else(|| "未找到 FFmpeg，无法探测编码器".to_string())?;
        Ok(media_rtp::encoder::status(&ffmpeg))
    })
    .await
    .map_err(|error| format!("编码器探测任务异常: {error}"))?
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

/// 预览订阅入口:从当前运行的设备取共享媒体。
struct DevicePreviewMedia(AppHandle);

impl PreviewMediaSource for DevicePreviewMedia {
    fn subscribe(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<PreviewSubscription>> + Send + '_>>
    {
        Box::pin(async move {
            let state = self.0.state::<AppState>();
            let sim = state.device.lock().await.as_ref()?.sim.clone();
            let fps = sim.config().video_fps;
            let source = sim.subscribe_shared_media().await?;
            Some(PreviewSubscription { source, fps })
        })
    }
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
    if state.preview_manager.restart() {
        Ok("正在重建本地预览，采集与推流保持运行".into())
    } else {
        Err("当前没有打开的本地预览".into())
    }
}

/// 前端不支持当前编码的 WebCodecs 解码时切到 JPEG 回退(或切回)。
#[tauri::command]
async fn set_preview_fallback(
    fallback: bool,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state.preview_manager.set_fallback(fallback);
    Ok(())
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
    // 设备句柄建好后再把 sim 交给调度任务（handle 会拿走所有权）。
    let mut sim_for_plan: Option<Arc<DeviceSimulator>> = None;
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
    // 没有显式媒体源时用内置演示视频：装好即可注册推流，不必先在本机准备素材。
    // 显式配置（含 live: 摄像头/屏幕）永远优先，用户清空来源仍沿用此默认。
    if resolved.video_source.is_none() {
        resolved.video_source =
            resolve_builtin_video(&app).map(|path| path.to_string_lossy().into_owned());
    }
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
                    return Err(catalog
                        .screen_error
                        .unwrap_or_else(|| format!("未找到显示器 {display_id}")));
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
            // 硬件版本只在 GB-2022 下有意义：2016 平台不认识这个字段。
            // 空串视同未填写，避免报文里出现空节点。
            hardware_version: (resolved.profile.gb_version == common::GbVersion::V2022
                && !resolved.device.hardware_version.trim().is_empty())
            .then(|| resolved.device.hardware_version.clone()),
            // 位置/类型只认真值表内的 id；未知值退化为 None，不把脏数据拼进 MANSCDP XML。
            installation_address: device_metadata::location_label(&resolved.device.location)
                .map(String::from),
            device_type: device_metadata::camera_type_label(&resolved.device.camera_type)
                .map(String::from),
        },
        video_source: resolved.video_source.clone(),
        video_fps: resolved.media.video_fps,
        media_profile: Some(resolved.media.clone()),
        light_bitrate_kbps: None,
        gb_version: resolved.profile.gb_version,
        // 出站字符集必须与国标版本相容:切到 2016 后仍存着 GB18030 时兜底成 GB2312,
        // 否则会带着 2016 平台不认的声明名出站(现场表现是"收到了但解析不出")。
        signaling_encoding: resolved
            .profile
            .signaling_encoding
            .effective(resolved.profile.gb_version),
    };

    // 文件源只记下待预热的素材：REGISTER 只需要 SIP 身份（设备 ID/密码/平台地址），
    // 与有没有转码产物无关。以前把整片预热串在注册前面，长素材会让"设备上线"等好
    // 几分钟，转码失败甚至直接导致注册不上——两个独立的失败域被绑在了一起。
    // 现在注册先行，产物由下方后台任务补齐；点播早于产物就绪时 INVITE 回 480。
    let pending_file: Option<(std::path::PathBuf, media_rtp::profile::MediaProfile)> = resolved
        .video_source
        .as_ref()
        .filter(|path| !path.trim().is_empty() && !path.starts_with("live:"))
        .map(|path| (std::path::PathBuf::from(path), resolved.media.clone()));
    // 预热槽位提前占：占不到说明上一次预热还没退干净。此时设备尚未启动，
    // 直接失败比"设备起来了却没人预热"干净；guard 在提前返回时会自动释放。
    let media_slot = match pending_file {
        Some(_) => Some(MediaTaskSlot::new(state.inner())?),
        None => None,
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
    // 文件媒体的预热产物由下方后台任务装入；注册不需要它。
    load_control_state(sim.as_ref(), &state, &app);
    sim.install_recording_store(app_recording_store(&state, &app)?);
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
    let sim_for_media = Arc::clone(&sim);
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

        // 调度任务要长期持有 sim，而 handle 会拿走所有权，故先克隆一份。
        sim_for_plan = Some(Arc::clone(&sim));
        *guard = Some(DeviceHandle {
            sim,
            transport,
            local_host,
            local_port,
            stop_tx,
            effective_config,
        });
    })?;
    if persisted.time_sync.ntp_enabled {
        state.restart_ntp_refresh(persisted.time_sync.clone()).await;
    }
    // 计划以配置文件为准：设备启动时把持久化计划灌进共享状态，避免调度任务
    // 读到上一个设备实例留下的旧值。
    *state.record_plan.write().await = persisted.record_plan.clone();
    if let Some(sim) = sim_for_plan {
        // 先对齐 ConfigDownload 回读，再启动调度：平台在任何时刻查到到的都应是
        // 与设备实际行为一致的计划类型。
        sim.set_record_plan_enabled(plan_effective(&persisted.record_plan));
        state.start_plan_scheduler(sim).await;
    }

    // 文件媒体的预热放到这里：REGISTER 已经提交，平台侧马上能看到设备上线，
    // 预热期间点播收到 480（handle_invite 的既有无就绪分支），就绪后自动可播。
    let started_message = if let (Some((source_path, profile)), Some(slot)) = (pending_file, media_slot)
    {
        let cancel = slot.release();
        let prepare_app = app.clone();
        let prepare_sim = sim_for_media;
        tokio::spawn(async move {
            let _ = prepare_app.emit(
                "media_preparation",
                serde_json::json!({
                    "phase": "preparing",
                    "message": "设备已注册，正在后台准备文件音视频",
                }),
            );
            let task_cancel = Arc::clone(&cancel);
            let progress_app = prepare_app.clone();
            let joined = tokio::task::spawn_blocking(move || {
                let progress =
                    move |progress: media_rtp::file_profile::FilePreparationProgress| {
                        emit_media_progress(&progress_app, progress);
                    };
                // Windows 上 tokio 阻塞线程栈默认只有 2MB，文件准备链路的栈开销随参数变化，
                // 这里显式给 8MB 栈兜底，避免再次触发 STATUS_STACK_OVERFLOW(0xc00000fd) 闪退。
                std::thread::Builder::new()
                    .name("media-prepare".to_string())
                    .stack_size(8 * 1024 * 1024)
                    .spawn(move || {
                        media_rtp::file_profile::prepare_file_profile(
                            &source_path,
                            &profile,
                            &task_cancel,
                            Some(&progress),
                        )
                    })
                    .map_err(|error| format!("创建文件准备线程失败: {error}"))
                    .and_then(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|_| Err("文件准备线程异常退出".to_string()))
                    })
            })
            .await;
            let result = joined
                .map_err(|error| format!("文件准备任务异常: {error}"))
                .and_then(|result| result);

            // 设备可能已被停止或重新启动：取消后不再安装产物，避免给已下线的
            // 实例补媒体、或在错误的 profile 上启动采集。
            if cancel.load(Ordering::Acquire) {
                let _ = prepare_app.emit(
                    "media_preparation",
                    serde_json::json!({"phase":"cancelled", "message":"已取消文件准备"}),
                );
                finish_media_task_via(&prepare_app, &cancel);
                return;
            }
            match result {
                Ok(prepared) => match prepare_sim
                    .install_prepared_file_and_start(prepared, &cancel)
                    .await
                {
                    Ok(()) => {
                        let _ = prepare_app.emit(
                            "media_preparation",
                            serde_json::json!({"phase":"ready", "message":"文件音视频准备完成，可以点播"}),
                        );
                    }
                    Err(error) => {
                        // 收尾阶段被停止设备打断时，不要显示成"准备失败"——
                        // 那是用户主动行为，不是故障。
                        let phase = if cancel.load(Ordering::Acquire) {
                            "cancelled"
                        } else {
                            "failed"
                        };
                        let _ = prepare_app.emit(
                            "media_preparation",
                            serde_json::json!({"phase": phase, "message": error}),
                        );
                    }
                },
                Err(error) => {
                    let _ = prepare_app.emit(
                        "media_preparation",
                        serde_json::json!({"phase":"failed", "message":error}),
                    );
                }
            }
            finish_media_task_via(&prepare_app, &cancel);
        });
        "设备已启动，正在后台准备文件音视频"
    } else {
        "设备已启动"
    };
    Ok(started_message.into())
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

/// 取某段录像的首帧缩略图，返回可直接放进 `<img src>` 的 data URL。
///
/// 抽帧走外部 ffmpeg，抽到的 JPEG 缓存在数据目录下按 ID 命名：同一段录像只抽一次。
/// 找不到 ffmpeg / 文件损坏时返回错误，前端退化为占位图 —— 缩略图是增强项，
/// 不该因为它不可用就让整个录像列表报错。
#[tauri::command]
async fn get_recording_thumbnail(
    state: tauri::State<'_, AppState>,
    app: AppHandle,
    id: String,
) -> Result<String, String> {
    use base64::Engine as _;
    if !media_rtp::thumbnail::is_valid_id(&id) {
        return Err("录像 ID 只允许字母、数字、- 与 _".into());
    }
    let store = app_recording_store(&state, &app)?;
    let entry = store
        .list()
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| format!("录像不存在: {id}"))?;
    let source = std::path::PathBuf::from(&entry.path);
    let cache_dir = app_data_root(&state, &app)?.join(media_rtp::thumbnail::THUMB_DIR);
    let cache = media_rtp::thumbnail::cache_path(&cache_dir, &id)
        .ok_or_else(|| "缩略图路径非法".to_string())?;

    // 缓存比源文件新才复用：录像被覆盖/重写时要重新抽，不能拿旧图糊弄。
    let reusable = std::fs::metadata(&cache)
        .and_then(|cached| {
            std::fs::metadata(&source)
                .and_then(|origin| Ok(cached.modified()? >= origin.modified()?))
        })
        .unwrap_or(false);
    if !reusable {
        let duration_secs = (entry.end_time_ms.saturating_sub(entry.start_time_ms)) as f64 / 1000.0;
        let seek = media_rtp::thumbnail::seek_secs_for(duration_secs);
        let ffmpeg = media_rtp::ffmpeg_bin()
            .ok_or_else(|| "未找到 ffmpeg，无法生成录像缩略图".to_string())?;
        let input = source.clone();
        let output = cache.clone();
        tokio::task::spawn_blocking(move || {
            media_rtp::thumbnail::extract(&input, &output, seek, &ffmpeg)
        })
        .await
        .map_err(|error| format!("缩略图任务异常: {error}"))??;
    }

    let bytes = std::fs::read(&cache)
        .map_err(|error| format!("读取缩略图 {} 失败: {error}", cache.display()))?;
    Ok(format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
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
    // 先掐掉后台预热：设备都停了还继续转码只是白烧 CPU，且产物不会被任何人使用。
    state.cancel_media_task();
    // 设备停止后不再有"设备时间"的维护者，先停 NTP 周期任务；偏移保留，
    // 这样平台侧看到的最后一次时间不会突然跳回本机时钟。
    state.stop_ntp_refresh().await;
    // 先取消调度，再收尾录像：否则任务可能在收尾之后又把录像起起来。
    state.stop_plan_scheduler().await;
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

/// 主动上报一条报警（首页快捷入口，等价于默认视频报警单）。
#[tauri::command]
async fn fire_alarm(
    description: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let desc = if description.is_empty() {
        "移动侦测报警".to_string()
    } else {
        description
    };
    let record = dispatch_alarm(&state, gb28181_protocol::alarm::AlarmReport::video(desc)).await?;
    Ok(format!(
        "报警上报完成(平台响应 {})",
        record.status.unwrap_or_default()
    ))
}

/// 按报警单上报（报警单管理页）。返回落库后的历史记录。
#[tauri::command]
async fn report_alarm(
    report: gb28181_protocol::alarm::AlarmReport,
    state: tauri::State<'_, AppState>,
) -> Result<AlarmRecord, String> {
    dispatch_alarm(&state, report).await
}

/// 预览报警 XML：与实际发送共用同一份组装逻辑，SN 固定为 1。
#[tauri::command]
async fn preview_alarm_xml(
    report: gb28181_protocol::alarm::AlarmReport,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let device_id = {
        let guard = state.device.lock().await;
        match &*guard {
            Some(handle) => handle.sim.config().device_id.as_str().to_string(),
            // 设备未启动时用国标示例 ID，让用户先把报文看清楚再去注册。
            None => "34020000001320000001".to_string(),
        }
    };
    let notify = report
        .to_notify(device_id, 1)
        .map_err(|error| error.to_string())?;
    notify.to_xml().map_err(|error| error.to_string())
}

#[tauri::command]
async fn list_alarm_history(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<AlarmRecord>, String> {
    let history = state
        .alarm_history
        .lock()
        .await
        .iter()
        .cloned()
        .collect();
    Ok(history)
}

#[tauri::command]
async fn clear_alarm_history(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.alarm_history.lock().await.clear();
    Ok(())
}

/// 本机复位报警（只清设备本地状态，不向平台发报文）。
#[tauri::command]
async fn reset_alarm(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let guard = state.device.lock().await;
    match &*guard {
        Some(handle) => {
            handle.sim.reset_alarm();
            Ok("已复位本机报警状态".into())
        }
        None => Err("设备未启动".into()),
    }
}

/// 报警上报的统一出口：成功/失败都写进历史，再决定是否向上抛错。
///
/// 失败也记录，是因为"平台没收到报警"是现场最常见的排查起点 —— 只把错误弹在
/// toast 里，用户一刷新就再也找不到当时那条报了什么。
async fn dispatch_alarm(
    state: &AppState,
    report: gb28181_protocol::alarm::AlarmReport,
) -> Result<AlarmRecord, String> {
    report.validate().map_err(|error| error.to_string())?;
    let guard = state.device.lock().await;
    let Some(handle) = guard.as_ref() else {
        return Err("设备未启动".into());
    };
    let via_notify = handle.sim.has_alarm_subscription().await;
    let device_id = handle.sim.config().device_id.as_str().to_string();
    let captured_at_ms = common::clock::local_epoch_ms();
    // 记录里存的是"发出去的那个时间戳"，而不是落库时间，二者可能差 N 分钟。
    let alarm_time = gb28181_protocol::alarm::alarm_time(report.minutes_ago);
    let outcome = handle
        .sim
        .report_alarm_report(
            &handle.transport,
            &handle.local_host,
            handle.local_port,
            &report,
        )
        .await
        .map_err(|error| error.to_string());

    let mut history = state.alarm_history.lock().await;
    let id = state.next_alarm_id.fetch_add(1, Ordering::Relaxed);
    let record = AlarmRecord {
        id,
        device_id,
        priority: report.priority,
        method: report.method,
        alarm_type: report.alarm_type,
        description: report.description.clone(),
        longitude: report.longitude,
        latitude: report.latitude,
        alarm_time,
        captured_at_ms,
        via_notify,
        status: outcome.as_ref().ok().copied(),
        error: outcome.as_ref().err().cloned(),
    };
    history.push_front(record.clone());
    history.truncate(ALARM_HISTORY_LIMIT);
    drop(history);

    match outcome {
        Ok(_) => Ok(record),
        Err(error) => Err(error),
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
        &config.device.channel_name,
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
        &config.device.channel_name,
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

/// 预览当前编辑中的目录树对应的 Catalog NOTIFY XML(移动端「预览 NOTIFY」同款)。
///
/// 纯函数:不落盘、不触碰运行态、不发 SIP 报文,SN 固定为 1。
/// 与实际出站共用 `CatalogNotify::to_xml` 组装逻辑;实际发送还会按 UDP 预算分片,
/// 预览固定输出单包全量,便于核对字段与层级。
#[tauri::command]
async fn preview_catalog_xml(
    nodes: Vec<ChannelNodeDto>,
    device_id: String,
) -> Result<String, String> {
    use gb28181_protocol::manscdp::{CatalogNotify, CatalogNotifyItem};
    use gb28181_protocol::id_codec::CatalogNode;

    let protocol_nodes: Vec<CatalogNode> = nodes
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<_, _>>()?;
    gb28181_protocol::id_codec::validate_catalog_tree(&protocol_nodes)?;
    let items: Vec<CatalogNotifyItem> = protocol_nodes
        .iter()
        .map(|node| CatalogNotifyItem {
            device_id: node.id.clone(),
            name: node.name.clone(),
            civil_code: node.civil_code.clone(),
            parental: Some(node.node_type.parental()),
            parent_id: Some(node.parent_id.clone()),
            business_group_id: node.business_group_id.clone(),
            event: "ON".into(),
            status: node.status.clone(),
        })
        .collect();
    CatalogNotify::new(device_id, 1, items)
        .to_xml()
        .map_err(|error| error.to_string())
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

// ── 内置媒体运行时（ffmpeg / ffprobe）────────────────────

/// 进程级缓存：启动期解析一次，避免每个 IPC 调用都起子进程探版本。
static MEDIA_RUNTIME: OnceLock<MediaRuntimeStatus> = OnceLock::new();

/// 媒体工具的运行期状态。
#[derive(Debug, Clone, Serialize)]
pub struct MediaToolStatus {
    /// 解析到的绝对路径（或 PATH 中的命令名）；未解析到时为 `None`。
    pub path: Option<String>,
    /// `-version` 首行，便于在日志/自检面板里核对实际版本。
    pub version: Option<String>,
    /// `bundled`（安装包内嵌）/ `environment`（显式环境变量）/ `path`（系统 PATH）/ `missing`。
    pub source: String,
}

impl MediaToolStatus {
    fn missing() -> Self {
        Self {
            path: None,
            version: None,
            source: "missing".into(),
        }
    }
}

/// 媒体工具状态汇总，供启动自检与前端提示使用。
#[derive(Debug, Clone, Serialize)]
pub struct MediaRuntimeStatus {
    pub ffmpeg: MediaToolStatus,
    pub ffprobe: MediaToolStatus,
    /// 主干媒体能力是否可用（缺 ffmpeg 即不可用）。
    pub ready: bool,
    /// 录像音轨校验是否可用（缺 ffprobe 时录像仍可录，只是不做音轨核验）。
    pub verification_available: bool,
}

fn media_tool_names(tool: &str) -> Vec<String> {
    if cfg!(target_os = "windows") {
        vec![format!("{tool}.exe"), tool.to_string()]
    } else {
        vec![tool.to_string(), format!("{tool}.exe")]
    }
}

/// 内嵌媒体工具的候选路径（按发布包布局枚举，不做跨目录猜测）。
///
/// 关键事实：Tauri 打包**保留相对路径**（`tauri_utils::resources::resource_relpath`），
/// 因此 `bundle.resources` 里的 `runtime/ffmpeg/*` 会落在 `resources/runtime/ffmpeg/`
/// 下，而不是 `resources/` 根。历史上只按「exe 同级 / resources 同级」查找，Windows
/// 安装包明明内嵌了 ffmpeg 却找不到，录像收尾因此报「未找到 ffprobe」。
fn media_tool_candidates(tool: &str) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // macOS .app：可执行文件在 Contents/MacOS，资源在 Contents/Resources。
            dirs.push(parent.to_path_buf());
            dirs.push(parent.join("../Resources"));
            // Tauri 资源目录（dev 下即 target/debug，发布下为 <install>/resources）。
            dirs.push(parent.join("resources"));
            dirs.push(parent.join("../resources"));
            // Windows 打包脚本的内嵌布局。
            dirs.push(parent.join("resources").join("runtime").join("ffmpeg"));
            dirs.push(parent.join("runtime").join("ffmpeg"));
        }
    }
    let names = media_tool_names(tool);
    let mut candidates = Vec::new();
    for dir in dirs {
        for name in &names {
            candidates.push(dir.join(name));
        }
    }
    candidates
}

fn probe_media_tool(path: &std::path::Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let output = common::hidden_command(path).arg("-version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let first_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    (!first_line.is_empty()).then_some(first_line)
}

/// 解析媒体工具：显式环境变量 → 内嵌资源 → media-rtp 的 PATH/常见安装位置探测。
///
/// 命中内嵌资源时把绝对路径写回环境变量，让 media-rtp 只认注入值，不再自行猜测
/// 目录与扩展名——`ffprobe` 在 Windows 上因缺 `.exe` 找不到、进而整段录像被判失败的
/// 事故，根因就是猜。
fn resolve_media_tool(tool: &str, env_key: &str) -> MediaToolStatus {
    if let Some(configured) = std::env::var_os(env_key).map(std::path::PathBuf::from) {
        if let Some(version) = probe_media_tool(&configured) {
            return MediaToolStatus {
                path: Some(configured.to_string_lossy().into_owned()),
                version: Some(version),
                source: "environment".into(),
            };
        }
    }
    for candidate in media_tool_candidates(tool) {
        if let Some(version) = probe_media_tool(&candidate) {
            std::env::set_var(env_key, &candidate);
            return MediaToolStatus {
                path: Some(candidate.to_string_lossy().into_owned()),
                version: Some(version),
                source: "bundled".into(),
            };
        }
    }
    // 兜底：开发机把 ffmpeg 装在 PATH 或 WinGet/Chocolatey 目录里。
    let fallback = if tool == "ffprobe" {
        media_rtp::ffprobe_bin()
    } else {
        media_rtp::ffmpeg_bin()
    };
    match fallback {
        Some(path) => {
            let version = probe_media_tool(std::path::Path::new(&path));
            MediaToolStatus {
                path: Some(path),
                version,
                source: "path".into(),
            }
        }
        None => MediaToolStatus::missing(),
    }
}

/// 启动期解析并注入媒体工具路径；返回自检结果。
///
/// **必须在任何媒体调用之前执行**：media-rtp 侧只读环境变量，晚于首次调用注入就无效。
/// 结果缓存在进程内，重复调用只读缓存，避免每次都起子进程跑 `-version`。
fn install_media_tool_env() -> MediaRuntimeStatus {
    if let Some(status) = MEDIA_RUNTIME.get() {
        return status.clone();
    }
    let ffmpeg = resolve_media_tool("ffmpeg", "UVP_FFMPEG_PATH");
    let ffprobe = resolve_media_tool("ffprobe", "UVP_FFPROBE_PATH");
    let status = MediaRuntimeStatus {
        ready: ffmpeg.path.is_some(),
        verification_available: ffprobe.path.is_some(),
        ffmpeg,
        ffprobe,
    };
    match (&status.ffmpeg.path, &status.ffprobe.path) {
        (Some(ffmpeg), Some(ffprobe)) => tracing::info!(
            ffmpeg = %ffmpeg,
            ffprobe = %ffprobe,
            "媒体运行时已就绪"
        ),
        (Some(ffmpeg), None) => tracing::warn!(
            ffmpeg = %ffmpeg,
            "未找到 ffprobe：录像仍可进行，但跳过音轨编码/采样率校验"
        ),
        (None, _) => tracing::error!(
            "未找到 ffmpeg：文件源转封装、摄像头/屏幕采集、快照与录像收尾均不可用"
        ),
    }
    MEDIA_RUNTIME.get_or_init(|| status).clone()
}

/// 媒体运行时自检：前端在首屏据此提示，而不是等用户点录像才报错。
#[tauri::command]
fn media_runtime_status() -> MediaRuntimeStatus {
    install_media_tool_env()
}

// ── 应用入口 ──────────────────────────────────────────────

/// 初始化日志、注册命令、启动 Tauri 事件循环。
pub fn run() {
    common::logging::init();
    // 先于任何媒体调用注入内置 ffmpeg/ffprobe 的绝对路径。
    install_media_tool_env();
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
            // 本地预览按需订阅当前设备的共享媒体总线。
            app.state::<AppState>()
                .preview_manager
                .set_media_source(Arc::new(DevicePreviewMedia(app.handle().clone())));
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
            builtin_video_source,
            media_runtime_status,
            list_local_ip_addresses,
            get_desktop_config,
            save_desktop_config,
            save_media_config,
            save_osd_config,
            get_osd_capability,
            get_clock_state,
            save_time_sync,
            sync_ntp_now,
            get_record_plan,
            save_record_plan,
            report_alarm,
            preview_alarm_xml,
            list_alarm_history,
            clear_alarm_history,
            reset_alarm,
            take_snapshot,
            list_snapshots,
            read_snapshot,
            delete_snapshot,
            export_snapshot,
            export_logs,
            reset_desktop_config,
            exchange_sip_provision,
            validate_scenario,
            start_stress,
            stop_stress,
            get_metrics,
            export_report,
            list_live_sources,
            get_encoder_status,
            list_camera_modes,
            probe_live_source,
            is_preview_window_visible,
            start_binary_preview,
            stop_binary_preview,
            retry_binary_preview,
            set_preview_fallback,
            start_device,
            cancel_media_preparation,
            stop_device,
            get_recordings,
            get_recording_thumbnail,
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
            preview_catalog_xml,
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

    #[test]
    fn 报告参数行含速率并且在缺_server_id_时不显示空斜杠() {
        let mut sc = scenario::LinearScenario::from_toml_str(
            r#"
base_device_id = "34020000001320000001"
password = "secret"
server_host = "10.0.0.9"
server_port = 5060
server_domain = "3402000000"
ramp_per_second = 30
active_ratio = 0.5

[device_info]
device_name = "压测"
manufacturer = "UVP"
model = "M"
firmware = "1"
"#,
        )
        .expect("场景应可解析");
        let rows = scenario_report_rows(&sc);
        assert!(rows.iter().any(|(k, v)| k == "爬坡速率" && v == "30 台 / 秒"));
        assert!(rows.iter().any(|(k, v)| k == "真实推流占比" && v == "50%"));
        let id_row = rows.iter().find(|(k, _)| k == "平台 ID / 域").unwrap();
        assert_eq!(id_row.1, "3402000000（按域解析）", "缺 server_id 时不能出现空斜杠");

        sc.server_id = "34020000002000000001".into();
        let rows = scenario_report_rows(&sc);
        let id_row = rows.iter().find(|(k, _)| k == "平台 ID / 域").unwrap();
        assert_eq!(id_row.1, "34020000002000000001 / 3402000000");
    }

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

    /// 设备注册完成后（启动令牌已释放），「取消准备」必须仍然有效——
    /// 这才是用户真正会点的时刻：平台已经看到设备上线，但文件还在后台转码。
    #[test]
    fn 注册后取消准备仍然生效且不会并发预热() {
        let state = AppState::new();
        let token = state.begin_media_task().expect("首次占用预热槽位应成功");
        assert!(state.media_task_running());
        // 同一时刻只允许一个预热任务：并发转同一素材既浪费 CPU，进度也会串台。
        assert!(state.begin_media_task().is_err());

        state
            .cancel_file_preparation()
            .expect("启动令牌已释放时，取消必须落到后台预热上");
        assert!(token.load(Ordering::Acquire), "取消应置位预热令牌");

        state.finish_media_task(&token);
        assert!(!state.media_task_running(), "任务退出后槽位必须清空");
        // 槽位空且没有启动请求时，重复取消应明确报错而不是静默成功。
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
