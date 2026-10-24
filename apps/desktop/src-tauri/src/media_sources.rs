use ipc_error::{sanitize_public_message, IpcError, IpcResult};

use serde::Serialize;
use tauri::{AppHandle, Manager}; // Tauri 2 emit/state 需要对应 trait

use super::*;

#[derive(Serialize)]
pub(crate) struct LiveScreenDto {
    display_id: u32,
    width: u32,
    height: u32,
    name: String,
    uri: String,
}

#[derive(Serialize)]
pub(crate) struct LiveAvDeviceDto {
    index: u32,
    name: String,
}

#[derive(Serialize)]
pub(crate) struct LiveSourceCatalogDto {
    ffmpeg_available: bool,
    aac_available: bool,
    opus_available: bool,
    screens: Vec<LiveScreenDto>,
    cameras: Vec<LiveAvDeviceDto>,
    microphones: Vec<LiveAvDeviceDto>,
    screen_error: Option<String>,
    avfoundation_error: Option<String>,
}

/// 系统内容选择器(SCContentSharingPicker)的返回值。
///
/// 前端把它拼进 `live:screen|window|app:<目标>` URI:选中的是**目标身份**
/// (显示器 id / 窗口号 / bundle id),不是一次性的过滤器对象 —— 这样同一次选择
/// 才能跨会话复用(每次平台 INVITE 都会重建采集管道)。
#[derive(Serialize)]
pub(crate) struct ShareTargetDto {
    /// `display` / `window` / `application`。
    kind: String,
    display_id: Option<u32>,
    window_id: Option<u32>,
    bundle_id: Option<String>,
    /// 界面展示用标签(窗口标题 / 应用名 / 显示器编号)。
    label: String,
}

impl From<media_rtp::PickedShareTarget> for ShareTargetDto {
    fn from(picked: media_rtp::PickedShareTarget) -> Self {
        use media_rtp::ScreenCaptureTarget as Target;
        let (kind, display_id, window_id, bundle_id) = match &picked.target {
            Target::Display(id) => ("display", Some(*id), None, None),
            Target::Window(id) => ("window", None, Some(*id), None),
            Target::Application(bundle) => ("application", None, None, Some(bundle.clone())),
        };
        Self {
            kind: kind.into(),
            display_id,
            window_id,
            bundle_id,
            label: picked.label,
        }
    }
}

pub(crate) fn sanitize_catalog_error(message: String) -> String {
    if message.to_ascii_lowercase().contains("ffmpeg") {
        "FFmpeg 设备枚举失败（详细输出已隐藏，请查看系统日志）".to_string()
    } else {
        sanitize_public_message(&message)
    }
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
            screen_error: catalog.screen_error.map(sanitize_catalog_error),
            avfoundation_error: catalog.avfoundation_error.map(sanitize_catalog_error),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct LiveProbeDto {
    uri: String,
    video_ready: bool,
    audio_requested: bool,
    audio_ready: Option<bool>,
    message: String,
}

/// 枚举可用于规范实时源 URI 的摄像头、麦克风和显示器。
#[tauri::command]
pub(crate) async fn list_live_sources() -> IpcResult<LiveSourceCatalogDto> {
    let catalog = tokio::task::spawn_blocking(media_rtp::list_live_sources)
        .await
        .map_err(|error| IpcError::internal(format!("实时源枚举任务异常: {error}")))??;
    Ok(LiveSourceCatalogDto::from(catalog))
}

/// 启动一次短时采集探测。请求音频时，用户应确保麦克风或系统正在产生声音。
#[tauri::command]
pub(crate) async fn probe_live_source(uri: String) -> IpcResult<LiveProbeDto> {
    let spec = media_rtp::LiveSourceSpec::parse(uri.trim())?;
    let canonical_uri = uri.trim().to_string();
    let audio_requested = match spec {
        media_rtp::LiveSourceSpec::Camera { audio_index, .. } => audio_index.is_some(),
        media_rtp::LiveSourceSpec::Screen { audio, .. } => audio.is_enabled(),
    };
    let result = tokio::task::spawn_blocking(move || -> Result<LiveProbeDto, String> {
        use media_rtp::VideoSource;

        let mut source = media_rtp::LiveSource::capture(&canonical_uri, 25)
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
    .map_err(|error| IpcError::internal(format!("实时采集探测任务异常: {error}")))?;
    Ok(result?)
}

/// 弹出 macOS 系统内容选择器(`SCContentSharingPicker`),让用户按
/// 「显示器 / 窗口 / 应用」挑选要推流的画面。
///
/// 为什么必须走系统选择器:平台侧要的是"某一块画面",而"哪一块"只有用户知道;
/// 应用自己列窗口既拿不到跨应用的稳定身份,也不符合 macOS 的授权习惯。
/// 选择器只在主线程呈现,等待结果放在阻塞线程池 —— 主线程被占住会让回调
/// (走主队列)永远回不来。
///
/// 用户取消时返回 `None`(不是错误)。
#[tauri::command]
pub(crate) async fn pick_share_target(app: AppHandle) -> IpcResult<Option<ShareTargetDto>> {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        // 呈现动作本身是非阻塞的:把句柄送回异步上下文后再等结果。
        let _ = ready_tx.send(media_rtp::show_share_picker());
    })
    .map_err(|error| IpcError::internal(format!("无法在主线程呈现系统选择器: {error}")))?;

    let handle = ready_rx
        .await
        .map_err(|_| IpcError::internal("系统选择器未能呈现(主线程已退出)"))?
        .map_err(|error| IpcError::new("media.picker_failed", error.to_string()))?;

    let picked = tokio::task::spawn_blocking(move || handle.wait(media_rtp::SHARE_PICKER_TIMEOUT))
        .await
        .map_err(|error| IpcError::internal(format!("系统选择器等待任务异常: {error}")))?
        .map_err(|error| IpcError::new("media.picker_failed", error.to_string()))?;

    Ok(picked.map(ShareTargetDto::from))
}

/// 弹出系统文件对话框让用户选一个视频源文件,并**同时**授权它的 asset 预览。
///
/// 对话框由 Rust 侧弹出而不是前端 —— 这样"哪个路径被授权"完全由用户的真实选择决定,
/// webview 无法凭空捏造一个路径来换取读取能力(见 `media_access` 模块文档)。
/// 用户取消时返回 `None`。
#[tauri::command]
pub(crate) async fn pick_video_source(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;

    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择视频源文件")
        .add_filter("视频", media_access::MEDIA_EXTENSIONS)
        .pick_file(move |picked| {
            // 对话框回调在别的线程,用 oneshot 把结果搬回当前 async 上下文。
            let _ = tx.send(picked);
        });
    let picked = rx
        .await
        .map_err(|_| IpcError::new("media.dialog_failed", "文件选择对话框异常关闭"))?;
    let Some(file) = picked else {
        return Ok(None); // 用户取消,不是错误
    };
    let path = file.into_path().map_err(|error| {
        IpcError::new(
            "media.path_invalid",
            format!("无法识别所选文件路径: {error}"),
        )
    })?;

    let canonical = state
        .media_grants
        .grant(&path)
        .map_err(|message| IpcError::new("media.access", message))?;
    app.asset_protocol_scope()
        .allow_file(&canonical)
        .map_err(|error| {
            IpcError::new("media.preview_denied", format!("放行媒体预览失败: {error}"))
        })?;
    Ok(Some(canonical.to_string_lossy().into_owned()))
}

/// 为**已授权过**的媒体文件重新放行 asset 预览(应用重启后恢复上次选的文件)。
///
/// asset scope 的放行只在本次进程内有效,而用户上次选的文件路径存在前端 localStorage 里,
/// 重启后需要再放行一次。这里只接受 `media_grants` 里已有的条目 ——
/// 那个集合唯一的写入口是上面的系统文件对话框,所以 webview 传任何别的路径都会被拒。
#[tauri::command]
pub(crate) fn allow_media_preview(
    path: String,
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> IpcResult<()> {
    let p = std::path::PathBuf::from(&path);
    if !state.media_grants.is_granted(&p) {
        return Err(IpcError::new(
            "media.preview_denied",
            "该文件尚未经你选择授权,请点「选择文件」重新选取",
        )
        .with_retryable(false)
        .with_action("请点「选择文件」重新选择该媒体文件"));
    }
    let canonical = p
        .canonicalize()
        .map_err(|error| IpcError::new("media.access", format!("文件不可访问: {error}")))?;
    app.asset_protocol_scope()
        .allow_file(&canonical)
        .map_err(|error| {
            IpcError::new("media.preview_denied", format!("放行媒体预览失败: {error}"))
        })?;
    Ok(())
}

// ── 应用入口 ──────────────────────────────────────────────
