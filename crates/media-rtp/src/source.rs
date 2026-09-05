//! 视频源抽象(可插拔)。
//!
//! 对应压测三档媒体强度(docs/10-functional/stress-testing.md#2):
//! - `NoneSource`:空媒体(不产帧)—— A 档
//! - `LightSource`:合成低码率伪包 —— B 档(测 RTP 通道/带宽,内容不要求可解码)
//! - `FileSource`:H.264 文件循环 —— C 档 / 单设备联调

use common::{Error, Result};

#[path = "timing.rs"]
mod timing;
pub use timing::{
    ps_timestamp, rtp_timestamp, LegacyMediaClock, MediaEvent, SampleClock, TimedAudioAu,
    TimedVideoAu, MEDIA_CLOCK_HZ, PS_TIMESTAMP_MASK, RTP_TIMESTAMP_MASK,
};

/// 判断字节流是否以 Annex B 起始码开头(`00 00 01` 或 `00 00 00 01`)。
fn starts_with_annexb(b: &[u8]) -> bool {
    b.starts_with(&[0, 0, 1]) || b.starts_with(&[0, 0, 0, 1])
}

/// 把视频源解析为可直接切帧的 **Annex B 裸流文件路径**。
///
/// 裸流(.h264/.h265)原样返回;容器(MP4/FLV/MKV/MOV)经 ffmpeg 转封装并缓存后返回。
/// **应在设备启动时预调用一次**(预热缓存):容器转封装可能耗时数秒,若放到 INVITE
/// 处理里同步执行,会阻塞 200 OK 与首包推流,导致平台收流超时。预热后 INVITE 时
/// [`FileSource::from_path`] 命中缓存瞬时返回。
pub fn prepare_video_source(path: &str) -> Result<std::path::PathBuf> {
    let head = {
        use std::io::Read;
        let mut f = std::fs::File::open(path).map_err(Error::Io)?;
        let mut buf = [0u8; 8];
        let n = f.read(&mut buf).map_err(Error::Io)?;
        buf[..n].to_vec()
    };
    if starts_with_annexb(&head) {
        Ok(std::path::PathBuf::from(path))
    } else {
        ensure_annexb(path)
    }
}

/// 定位 ffmpeg 可执行文件。
///
/// **关键**:发布包必须优先使用应用内置的 FFmpeg，不能要求用户安装系统依赖。
/// 开发阶段才回退到 `UVP_FFMPEG_PATH`、PATH 和常见 Homebrew 路径，方便本地调试。
/// macOS 的 `.app`、Windows 安装目录和 Linux AppImage/普通安装目录都按可执行文件
/// 旁的 `ffmpeg`/`ffmpeg.exe` 及 `resources/` 目录探测。
pub fn ffmpeg_bin() -> Option<String> {
    use std::path::PathBuf;
    use std::process::Command;
    let probe = |bin: &str| {
        Command::new(bin)
            .arg("-version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    let mut bundled = Vec::<PathBuf>::new();
    if let Ok(path) = std::env::var("UVP_FFMPEG_PATH") {
        bundled.push(PathBuf::from(path));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let names: &[&str] = if cfg!(target_os = "windows") {
                &["ffmpeg.exe", "ffmpeg"]
            } else {
                &["ffmpeg", "ffmpeg.exe"]
            };
            for name in names {
                bundled.push(dir.join(name));
                bundled.push(dir.join("resources").join(name));
                bundled.push(dir.join("../resources").join(name));
                bundled.push(dir.join("../Resources").join(name));
            }
        }
    }
    for path in bundled {
        let s = path.to_string_lossy().into_owned();
        if path.is_file() && probe(&s) {
            return Some(s);
        }
    }
    let path_names: &[&str] = if cfg!(target_os = "windows") {
        &["ffmpeg.exe", "ffmpeg"]
    } else {
        &["ffmpeg", "ffmpeg.exe"]
    };
    for name in path_names {
        if probe(name) {
            return Some((*name).into());
        }
    }
    // 开发环境常见绝对路径兜底；发布包不会依赖这些路径。
    for p in [
        "/opt/homebrew/bin/ffmpeg", // Apple Silicon Homebrew
        "/usr/local/bin/ffmpeg",    // Intel Homebrew
        "/opt/local/bin/ffmpeg",    // MacPorts
        "/usr/bin/ffmpeg",          // Linux 系统包
    ] {
        if std::path::Path::new(p).exists() && probe(p) {
            return Some(p.to_string());
        }
    }
    None
}

/// 把容器视频文件(MP4/FLV/MKV/MOV 等)转封装为 H.264 Annex B 裸流,返回裸流文件路径。
///
/// 用内置或开发环境解析到的 `ffmpeg`:优先 `-c:v copy -bsf:v h264_mp4toannexb`(无损转封装,快);
/// 失败则回退重编码(`libx264 -preset ultrafast`,兼容任意输入)。结果按"源路径 + 修改时间"
/// 缓存到临时目录,同一文件只转一次(压测多设备共用同一文件时只转一次)。
///
/// 发布包缺少 FFmpeg 时应在打包阶段失败；运行时仍返回明确错误，便于开发环境诊断。
fn ensure_annexb(src: &str) -> Result<std::path::PathBuf> {
    use std::process::Command;

    // 缓存键:源路径 + 修改时间(mtime),避免源文件更新后用旧缓存。
    let meta = std::fs::metadata(src).map_err(Error::Io)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(src, mtime), &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    let out = std::env::temp_dir().join(format!("uvp-annexb-{key:016x}.h264"));
    if out.exists() {
        return Ok(out);
    }

    // 检查 ffmpeg 是否可用(PATH + 常见绝对路径,兼容 GUI 启动无 Homebrew PATH)。
    let ffmpeg = ffmpeg_bin().ok_or_else(|| {
        Error::Media(format!(
            "视频源是容器格式({src}),需要 ffmpeg 转封装,但未找到 ffmpeg;\
             请安装 ffmpeg(brew install ffmpeg),或直接提供 .h264/.h265 Annex B 裸流"
        ))
    })?;

    // ① 无损转封装(copy + h264_mp4toannexb)。
    let copy_ok = Command::new(&ffmpeg)
        .args([
            "-y",
            "-i",
            src,
            "-c:v",
            "copy",
            "-bsf:v",
            "h264_mp4toannexb",
        ])
        .arg(&out)
        .output()
        .map(|o| o.status.success() && out.exists())
        .unwrap_or(false);
    if copy_ok {
        return Ok(out);
    }

    // ② 回退:重编码为 H.264 Annex B(兼容任意编码/容器)。
    // HEVC 容器使用不同的 Annex-B bitstream filter；先尝试无损保留
    // H.265，避免历史录像被静默重编码成 H.264 后时间轴/编码声明失真。
    let hevc_copy_ok = Command::new(&ffmpeg)
        .args([
            "-y",
            "-i",
            src,
            "-c:v",
            "copy",
            "-bsf:v",
            "hevc_mp4toannexb",
        ])
        .arg(&out)
        .output()
        .map(|o| o.status.success() && out.exists())
        .unwrap_or(false);
    if hevc_copy_ok {
        return Ok(out);
    }

    let _ = std::fs::remove_file(&out);
    let enc = Command::new(&ffmpeg)
        .args([
            "-y",
            "-i",
            src,
            "-an",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-f",
            "h264",
        ])
        .arg(&out)
        .output()
        .map_err(|e| Error::Media(format!("ffmpeg 执行失败: {e}")))?;
    if enc.status.success() && out.exists() {
        Ok(out)
    } else {
        Err(Error::Media(format!(
            "ffmpeg 转封装/重编码失败({src}):{}",
            String::from_utf8_lossy(&enc.stderr)
                .lines()
                .last()
                .unwrap_or("")
        )))
    }
}

/// 从视频文件抽音频轨为 G.711A(PCMA)8kHz 单声道裸流,返回字节;无音频轨返回空 Vec。
///
/// 用系统 ffmpeg(`-vn -c:a pcm_alaw -ar 8000 -ac 1 -f alaw`),结果按源路径+mtime 缓存。
/// ffmpeg 缺失或无音频轨时返回空(调用方据此退化为纯视频)。
fn extract_g711a(src: &str) -> Result<Vec<u8>> {
    use std::process::Command;

    let meta = std::fs::metadata(src).map_err(Error::Io)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(src, mtime, "g711a"), &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    let out = std::env::temp_dir().join(format!("uvp-g711a-{key:016x}.alaw"));
    if out.exists() {
        return std::fs::read(&out).map_err(Error::Io);
    }

    let Some(ffmpeg) = ffmpeg_bin() else {
        return Ok(Vec::new()); // 无 ffmpeg:静默退化为纯视频。
    };

    let res = Command::new(&ffmpeg)
        .args([
            "-y", "-i", src, "-vn", "-c:a", "pcm_alaw", "-ar", "8000", "-ac", "1", "-f", "alaw",
        ])
        .arg(&out)
        .output()
        .map_err(|e| Error::Media(format!("ffmpeg 抽音频失败: {e}")))?;
    // 无音频轨时 ffmpeg 会失败或产空文件 —— 都按"无音频"处理,不报错。
    if res.status.success() && out.exists() {
        std::fs::read(&out).map_err(Error::Io)
    } else {
        let _ = std::fs::remove_file(&out);
        Ok(Vec::new())
    }
}

/// 一帧编码数据(H.264 Annex B 访问单元,含起始码)。
#[derive(Debug, Clone)]
pub struct Frame {
    /// 帧字节(Annex B)。
    pub data: Vec<u8>,
    /// 是否关键帧(含 SPS/PPS/IDR)。
    pub key_frame: bool,
}

/// 视频源:按需产出下一帧。实现方负责循环/结束语义。
pub trait VideoSource: Send {
    /// 取下一帧;返回 `None` 表示无更多帧(有限源)。循环源永不返回 None。
    fn next_frame(&mut self) -> Option<Frame>;

    /// 取与当前视频帧同时间窗的完整编码音频访问单元。
    /// 不同编码的帧大小和时长不同；实现方不得按固定字节数拆分 AAC/Opus。
    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        Vec::new()
    }

    /// 当前源是否已经提供带真实 PTS/时长的媒体事件。
    ///
    /// 默认值为 `false`，旧实现仍通过 [`next_frame`] / [`next_audio`] 读取；
    /// 共享媒体和推流边界会用 [`LegacyMediaClock`] 只补一次时间戳。新的
    /// 摄像头、屏幕和文件适配器应覆盖此方法并实现 [`next_media_event`]。
    fn supports_timed_events(&self) -> bool {
        false
    }

    /// 定时事件是否已经由源按其 PTS 做过墙钟调度。
    ///
    /// 默认值为 `false`，推流器会为有限源提供回放等待；如果文件读取器
    /// 自己已经等待 PTS，则覆盖为 `true`，避免同一时间轴被等待两次。
    fn timed_events_are_paced(&self) -> bool {
        false
    }

    /// 读取下一个独立媒体事件。
    ///
    /// 默认实现只把旧视频入口包装成一个未定时事件。旧调用者继续使用
    /// [`next_frame`] / [`next_audio`] 不受影响；新的来源覆盖该方法后可以
    /// 任意顺序交付视频、音频和时间轴 discontinuity。
    fn next_media_event(&mut self) -> Option<MediaEvent> {
        let frame = self.next_frame()?;
        Some(MediaEvent::Video(TimedVideoAu::new(
            frame.data,
            self.codec(),
            frame.key_frame,
            0,
            0,
        )))
    }

    /// 源的默认音频采样率，仅用于旧入口的时间戳适配。
    /// 带时间戳的新事件必须使用 [`TimedAudioAu::sample_rate_hz`] 自身的值。
    fn audio_sample_rate_hz(&self) -> u32 {
        self.audio_codec().default_sample_rate_hz()
    }

    /// 本源是否带音频轨(决定 PS 是否声明音频 ES)。
    fn has_audio(&self) -> bool {
        false
    }

    /// 本源音频编码(决定 PSM stream_type)。默认 G.711A。
    fn audio_codec(&self) -> crate::ps::AudioCodec {
        crate::ps::AudioCodec::G711A
    }

    /// 视频编码(决定 PSM stream_type)。默认 H.264;H.265 源覆盖。
    fn codec(&self) -> crate::ps::VideoCodec {
        crate::ps::VideoCodec::H264
    }

    /// 拖动(seek)到流的千分比位置(0..=1000)。回放 Range 定位用(§9.8)。
    /// 默认忽略(不支持定位的源如空媒体/循环灯);FileSource 覆盖为跳转帧游标。
    fn seek(&mut self, _permille: u32) {}

    /// 是否为实时采集源(持续产帧,无"结束"概念)。
    ///
    /// **关键**:pusher 用 `next_frame()` 返回 `None` 表示"有限源已耗尽"→停流。
    /// 但实时源(摄像头/屏幕)刚启动时,后台采集线程可能还没攒够一整帧,
    /// 此时 `next_frame()` 也返回 `None`——若按有限源语义直接 break,会导致
    /// "第一拍就停流、平台收流超时、又无报错"。故实时源覆盖此方法返回 true,
    /// pusher 遇 `None` 时跳过当前拍继续等待,而非终止会话。
    fn is_live(&self) -> bool {
        false
    }

    /// 取出实时采集后端的终止错误。默认源没有异步后端错误。
    /// pusher 每拍检查一次；返回错误后结束媒体会话并让上层更新状态。
    fn take_error(&mut self) -> Option<String> {
        None
    }

    /// 可选的本地预览控制面；只重建解码 worker，不重启物理采集。
    fn preview_control(&self) -> Option<std::sync::Arc<crate::preview_worker::PreviewControl>> {
        None
    }
}

/// 注册期唯一采集源的帧总线。预览和平台点播各自订阅，任何慢消费者都不会阻塞采集。
pub struct SharedMedia {
    events: tokio::sync::broadcast::Sender<MediaEvent>,
    /// 最近一帧含参数集和关键图像的访问单元。
    /// 新订阅者必须从它开始，否则延迟加入的 FFmpeg/RTP 解码器只有 IDR、没有 SPS/PPS。
    /// 这里只缓存视频，不缓存该关键帧产生时的旧音频。
    latest_config_keyframe: std::sync::Mutex<Option<TimedVideoAu>>,
    stop: std::sync::atomic::AtomicBool,
    alive: std::sync::atomic::AtomicBool,
    has_audio: bool,
    audio_codec: crate::ps::AudioCodec,
    video_codec: crate::ps::VideoCodec,
    error: std::sync::Mutex<Option<String>>,
    frames_seen: std::sync::atomic::AtomicU64,
    seek_permille_plus1: std::sync::atomic::AtomicU32,
    preview_control: Option<std::sync::Arc<crate::preview_worker::PreviewControl>>,
}

/// 从一个源启动唯一采集线程。实时源在设备注册期间持续运行，直到调用 [`SharedMedia::stop`]。
pub fn start_shared_media(
    mut source: Box<dyn VideoSource>,
    fps: u32,
) -> std::sync::Arc<SharedMedia> {
    let fps = fps.max(1);
    let preview_control = source.preview_control();
    // 音频事件独立于视频事件，容量按事件而不是视频帧估算。
    let (events, _) = tokio::sync::broadcast::channel((fps * 8).max(64) as usize);
    let timed_source = source.supports_timed_events();
    let media = std::sync::Arc::new(SharedMedia {
        events,
        latest_config_keyframe: std::sync::Mutex::new(None),
        stop: std::sync::atomic::AtomicBool::new(false),
        alive: std::sync::atomic::AtomicBool::new(true),
        has_audio: source.has_audio(),
        audio_codec: source.audio_codec(),
        video_codec: source.codec(),
        error: std::sync::Mutex::new(None),
        frames_seen: std::sync::atomic::AtomicU64::new(0),
        seek_permille_plus1: std::sync::atomic::AtomicU32::new(0),
        preview_control,
    });
    let producer = std::sync::Arc::clone(&media);
    std::thread::spawn(move || {
        let interval = std::time::Duration::from_micros(1_000_000 / u64::from(fps));
        let mut legacy_clock =
            LegacyMediaClock::new(fps, source.audio_codec(), source.audio_sample_rate_hz());
        while !producer.stop.load(std::sync::atomic::Ordering::Acquire) {
            let tick = std::time::Instant::now();
            let seek = producer
                .seek_permille_plus1
                .swap(0, std::sync::atomic::Ordering::AcqRel);
            if seek > 0 {
                source.seek(seek - 1);
            }
            if let Some(error) = source.take_error() {
                *producer.error.lock().unwrap() = Some(error);
                break;
            }
            let mut emitted = false;
            if timed_source {
                if let Some(event) = source.next_media_event() {
                    emitted = true;
                    publish_media_event(&producer, event);
                } else if !source.is_live() {
                    if let Some(error) = source.take_error() {
                        *producer.error.lock().unwrap() = Some(error);
                    }
                    break;
                }
            } else if let Some(frame) = source.next_frame() {
                // 旧入口一次取一帧，再取该视频窗内的音频包；时间戳只在这里补一次。
                let video = legacy_clock.video(frame.data, source.codec(), frame.key_frame);
                publish_media_event(&producer, MediaEvent::Video(video));
                for packet in source.next_audio() {
                    publish_media_event(
                        &producer,
                        MediaEvent::Audio(legacy_clock.audio(packet, None)),
                    );
                }
                emitted = true;
            } else if !source.is_live() {
                if let Some(error) = source.take_error() {
                    *producer.error.lock().unwrap() = Some(error);
                }
                break;
            }
            if timed_source {
                // 编码源本身负责输出节奏；总线不再按 fps 二次节流。
                if !emitted {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            } else if let Some(remaining) = interval.checked_sub(tick.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
        // LiveSource::drop 会等待并回收它持有的 camera/preview children。
        // alive 只能在这个析构完成后变 false，后续采集才能安全启动。
        drop(source);
        producer
            .alive
            .store(false, std::sync::atomic::Ordering::Release);
    });
    media
}

fn publish_media_event(producer: &std::sync::Arc<SharedMedia>, event: MediaEvent) {
    if let MediaEvent::Video(video) = &event {
        producer
            .frames_seen
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        if video.key_frame && crate::pusher::contains_codec_config(&video.data, video.codec) {
            *producer.latest_config_keyframe.lock().unwrap() = Some(video.clone());
        }
    }
    let _ = producer.events.send(event);
}

impl SharedMedia {
    /// 为一路 RTP 推流建立独立消费者；发生积压时从下一关键帧恢复。
    pub fn subscribe(self: &std::sync::Arc<Self>) -> SharedVideoSource {
        SharedVideoSource {
            media: std::sync::Arc::clone(self),
            events: self.events.subscribe(),
            pending_event: None,
            pending_audio: Vec::new(),
            bootstrap: self.latest_config_keyframe.lock().unwrap().clone(),
            need_key_frame: true,
        }
    }

    /// 设备注销时停止唯一采集线程并释放底层摄像头/屏幕句柄。
    pub fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
    }

    /// 请求停止并等待底层源完成析构；返回 false 表示超过调用方给定上限。
    pub async fn stop_and_wait(&self, timeout: std::time::Duration) -> bool {
        self.stop();
        let deadline = tokio::time::Instant::now() + timeout;
        while self.is_alive() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        !self.is_alive()
    }

    /// 保持注册和主采集不动，只允许预览 supervisor 新建 preview generation。
    pub fn retry_preview(&self) -> bool {
        let Some(control) = &self.preview_control else {
            return false;
        };
        control.retry();
        true
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Acquire)
    }

    /// 采集线程至少产出过一帧，表示源已经真正启动，而不是仅创建了后台线程。
    pub fn has_frames(&self) -> bool {
        self.frames_seen.load(std::sync::atomic::Ordering::Acquire) > 0
    }

    /// 返回采集线程的异步错误（如果有）。
    pub fn capture_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }

    /// 返回最近一帧可独立解码的关键帧与编码，供录像启动和当前帧抓拍共用。
    pub fn latest_config_frame(&self) -> Option<(Frame, crate::ps::VideoCodec)> {
        self.latest_config_keyframe
            .lock()
            .ok()
            .and_then(|video| video.clone())
            .map(|video| {
                (
                    Frame {
                        data: video.data,
                        key_frame: video.key_frame,
                    },
                    video.codec,
                )
            })
    }

    fn seek(&self, permille: u32) {
        self.seek_permille_plus1
            .store(permille.min(1000) + 1, std::sync::atomic::Ordering::Release);
    }
}

/// 把可独立解码的 Annex B 关键帧转为真实 JPEG。
pub fn frame_to_jpeg(frame: &Frame, codec: crate::ps::VideoCodec) -> Result<Vec<u8>> {
    if !frame.key_frame || !crate::pusher::contains_codec_config(&frame.data, codec) {
        return Err(Error::Media("当前帧不含完整编码参数集".into()));
    }
    let ffmpeg = ffmpeg_bin().ok_or_else(|| Error::Media("未找到 ffmpeg，无法生成 JPEG".into()))?;
    let nonce = now_ms().wrapping_add(u64::from(rand::random::<u32>()));
    let (suffix, input_format) = if codec == crate::ps::VideoCodec::H265 {
        ("h265", "hevc")
    } else {
        ("h264", "h264")
    };
    let input = std::env::temp_dir().join(format!("uvp-snapshot-{nonce}.{suffix}"));
    let output = std::env::temp_dir().join(format!("uvp-snapshot-{nonce}.jpg"));
    std::fs::write(&input, &frame.data)?;
    let result = std::process::Command::new(ffmpeg)
        .args(["-y", "-f", input_format, "-i"])
        .arg(&input)
        .args(["-frames:v", "1", "-f", "image2"])
        .arg(&output)
        .output()
        .map_err(|error| Error::Media(format!("ffmpeg 抓拍启动失败: {error}")));
    let jpeg = match result {
        Ok(result) if result.status.success() => std::fs::read(&output).map_err(Error::Io),
        Ok(result) => Err(Error::Media(format!(
            "ffmpeg 抓拍失败: {}",
            String::from_utf8_lossy(&result.stderr)
                .lines()
                .last()
                .unwrap_or("未知错误")
        ))),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(input);
    let _ = std::fs::remove_file(output);
    let jpeg = jpeg?;
    if jpeg.len() <= 4 || !jpeg.starts_with(&[0xFF, 0xD8]) || !jpeg.ends_with(&[0xFF, 0xD9]) {
        return Err(Error::Media("ffmpeg 未生成有效 JPEG".into()));
    }
    Ok(jpeg)
}

impl Drop for SharedMedia {
    fn drop(&mut self) {
        self.stop();
    }
}

/// [`SharedMedia`] 的单路 RTP 读取端。
pub struct SharedVideoSource {
    media: std::sync::Arc<SharedMedia>,
    events: tokio::sync::broadcast::Receiver<MediaEvent>,
    pending_event: Option<MediaEvent>,
    pending_audio: Vec<TimedAudioAu>,
    bootstrap: Option<TimedVideoAu>,
    need_key_frame: bool,
}

impl VideoSource for SharedVideoSource {
    fn supports_timed_events(&self) -> bool {
        true
    }

    fn timed_events_are_paced(&self) -> bool {
        // SharedMedia 的生产端已经按其来源的节奏发布事件；订阅端不能再次
        // 用同一批 PTS 做墙钟等待。实时共享源即使不等待，也由 pusher 的
        // `is_live` 分支直接按到达发送。
        true
    }

    fn next_media_event(&mut self) -> Option<MediaEvent> {
        loop {
            let event = if let Some(event) = self.pending_event.take() {
                Some(event)
            } else if let Some(video) = self.bootstrap.take() {
                // 启动补发只带视频参数集；不能把该关键帧产生时的旧音频一并补发。
                Some(MediaEvent::Video(video))
            } else {
                match self.events.try_recv() {
                    Ok(event) => Some(event),
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                        self.need_key_frame = true;
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                    | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => None,
                }
            }?;
            match event {
                MediaEvent::Video(video) => {
                    if self.need_key_frame
                        && (!video.key_frame
                            || !crate::pusher::contains_codec_config(&video.data, video.codec))
                    {
                        continue;
                    }
                    self.need_key_frame = false;
                    return Some(MediaEvent::Video(video));
                }
                MediaEvent::Audio(audio) => {
                    // 在首个完整视频关键帧之前，音频事件没有可用的解码起点。
                    if self.need_key_frame {
                        continue;
                    }
                    return Some(MediaEvent::Audio(audio));
                }
                discontinuity @ MediaEvent::Discontinuity { .. } => {
                    self.need_key_frame = true;
                    return Some(discontinuity);
                }
            }
        }
    }

    fn next_frame(&mut self) -> Option<Frame> {
        loop {
            match self.next_media_event()? {
                MediaEvent::Video(video) => {
                    return Some(Frame {
                        data: video.data,
                        key_frame: video.key_frame,
                    });
                }
                MediaEvent::Audio(audio) => self.pending_audio.push(audio),
                MediaEvent::Discontinuity { .. } => self.need_key_frame = true,
            }
        }
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = self
            .pending_audio
            .drain(..)
            .map(|audio| audio.data)
            .collect();
        while let Some(event) = self.next_media_event() {
            match event {
                MediaEvent::Audio(audio) => out.push(audio.data),
                other => {
                    self.pending_event = Some(other);
                    break;
                }
            }
        }
        out
    }

    fn has_audio(&self) -> bool {
        self.media.has_audio
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.media.audio_codec
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.media.video_codec
    }

    fn seek(&mut self, permille: u32) {
        self.media.seek(permille);
        self.bootstrap = None;
        self.need_key_frame = true;
    }

    fn is_live(&self) -> bool {
        self.media.alive.load(std::sync::atomic::Ordering::Acquire)
    }

    fn take_error(&mut self) -> Option<String> {
        self.media
            .error
            .lock()
            .ok()
            .and_then(|mut error| error.take())
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// 空媒体源:永不产帧(A 档,只维持信令)。
pub struct NoneSource;

impl VideoSource for NoneSource {
    fn next_frame(&mut self) -> Option<Frame> {
        None
    }
}

/// 轻量伪包源(B 档):按目标码率合成固定大小伪 NAL 帧,无限循环。
///
/// 用途:压测平台 RTP 端口/会话/带宽调度,不追求可解码内容。
/// 每帧字节数 = bitrate_kbps*1000/8/fps(下限 64);约每秒一个关键帧标记。
pub struct LightSource {
    frame_bytes: usize,
    keyframe_interval: u32,
    counter: u32,
}

impl LightSource {
    /// 按目标码率(kbps)与帧率构造。
    pub fn new(bitrate_kbps: u32, fps: u32) -> Self {
        let fps = fps.max(1);
        let frame_bytes = ((bitrate_kbps as usize * 1000) / 8 / fps as usize).max(64);
        LightSource {
            frame_bytes,
            keyframe_interval: fps,
            counter: 0,
        }
    }
}

impl VideoSource for LightSource {
    fn next_frame(&mut self) -> Option<Frame> {
        let key_frame = self.counter % self.keyframe_interval == 0;
        self.counter = self.counter.wrapping_add(1);
        // 伪 NAL:Annex B 起始码 + NAL 头(IDR type5 / slice type1)+ 填充占位。
        let nal_type: u8 = if key_frame { 0x65 } else { 0x61 };
        let mut data = Vec::with_capacity(self.frame_bytes + 5);
        data.extend_from_slice(&[0, 0, 0, 1, nal_type]);
        data.resize(self.frame_bytes + 5, 0xAB);
        Some(Frame { data, key_frame })
    }
}

/// G.711A 每 20ms 分包的字节数(8kHz × 0.02s × 1B)。
const G711_PACKET_BYTES: usize = 160;

/// H.264 Annex B 文件循环源。加载时按起始码切成访问单元,循环产出。
/// 若源含音频轨,同时持 G.711A 音频分包,与视频同步循环产出(音视频复合流)。
pub struct FileSource {
    frames: Vec<Frame>,
    cursor: usize,
    /// 视频编码(从 NAL 类型探测:H.264 或 H.265)。
    codec: crate::ps::VideoCodec,
    /// G.711A 音频分包(每包 20ms/160B);空表示无音频轨。
    audio: Vec<Vec<u8>>,
    audio_cursor: usize,
    /// 每个视频帧对应几个音频包(按帧率折算:25fps→40ms/帧→2 包)。
    audio_per_frame: usize,
    /// true=循环直播文件，false=有限历史文件。
    looping: bool,
    /// 历史容器文件按源自身 PTS 建立的事件序列；普通裸流/旧入口保持 None。
    historical_events: Option<Vec<MediaEvent>>,
    historical_event_cursor: usize,
    historical_pending_event: Option<MediaEvent>,
    historical_pending_audio: Vec<TimedAudioAu>,
}

impl FileSource {
    /// 从视频文件加载并切帧(纯视频,不抽音频)。
    ///
    /// 直接支持 H.264/H.265 **Annex B 裸流**;容器(MP4/FLV/MKV/MOV)自动经 ffmpeg
    /// 转封装为 Annex B(见 [`ensure_annexb`])。要音视频复合流用 [`from_path_av`](Self::from_path_av)。
    pub fn from_path(path: &str) -> Result<Self> {
        let annexb = prepare_video_source(path)?;
        let bytes = std::fs::read(&annexb).map_err(Error::Io)?;
        Self::from_bytes(&bytes)
    }

    /// 从一个或多个历史文件创建有限播放源，末帧后返回 `None`。
    pub fn from_paths_once(paths: &[std::path::PathBuf]) -> Result<Self> {
        if paths.is_empty() {
            return Err(Error::Media("历史录像文件列表为空".into()));
        }
        let mut source = Self::from_paths_once_legacy(paths)?;
        // 裸 Annex-B 文件没有可验证的源时基，保留原有调用兼容；容器历史必须
        // 由 ffprobe 提供自身 PTS，不能用当前设备 FPS 猜测播放时长。
        if paths.iter().any(|path| looks_like_annexb(path)) {
            return Ok(source);
        }

        let mut all_events = Vec::new();
        let mut segment_offset_90k = 0_u64;
        let mut expected_frames = 0_usize;
        for path in paths {
            let path_str = path
                .to_str()
                .ok_or_else(|| Error::Media("历史录像路径非 UTF-8".into()))?;
            let annexb = prepare_video_source(path_str)?;
            let bytes = std::fs::read(&annexb).map_err(Error::Io)?;
            let nals = split_annex_b(&bytes);
            let codec = detect_codec(&nals);
            let frames = group_into_frames(nals, codec);
            expected_frames = expected_frames.saturating_add(frames.len());
            let segment = probe_historical_segment(path_str)?;
            if segment.codec != codec {
                return Err(Error::Media(format!(
                    "历史视频编码转换不一致: 源 {:?}, Annex-B {:?}",
                    segment.codec, codec
                )));
            }
            if segment.video.len() != frames.len() {
                return Err(Error::Media(format!(
                    "历史视频 PTS 帧数与 Annex-B 帧数不一致: {} != {}",
                    segment.video.len(),
                    frames.len()
                )));
            }
            let mut segment_events = build_historical_events(&segment, frames)?;
            let segment_duration = segment_events
                .iter()
                .map(|event| event.pts_90k().saturating_add(event_duration_90k(event)))
                .max()
                .unwrap_or(0);
            for event in &mut segment_events {
                add_event_offset(event, segment_offset_90k)?;
            }
            segment_offset_90k = segment_offset_90k.saturating_add(segment_duration);
            all_events.extend(segment_events);
        }
        if source.frames.len() != expected_frames {
            return Err(Error::Media(format!(
                "历史视频合并后帧数不一致: {} != {}",
                source.frames.len(),
                expected_frames
            )));
        }
        all_events
            .sort_by_key(|event| (event.pts_90k(), matches!(event, MediaEvent::Audio(_)) as u8));
        source.historical_events = Some(all_events);
        source.historical_event_cursor = 0;
        source.historical_pending_event = None;
        source.historical_pending_audio.clear();
        Ok(source)
    }

    fn from_paths_once_legacy(paths: &[std::path::PathBuf]) -> Result<Self> {
        let mut bytes = Vec::new();
        for path in paths {
            let path = path
                .to_str()
                .ok_or_else(|| Error::Media("历史录像路径非 UTF-8".into()))?;
            let annexb = prepare_video_source(path)?;
            bytes.extend_from_slice(&std::fs::read(annexb)?);
        }
        let mut source = Self::from_bytes(&bytes)?;
        source.looping = false;
        Ok(source)
    }

    /// 从内存 Annex B 创建有限源，主要用于回放契约测试。
    pub fn from_bytes_once(bytes: &[u8]) -> Result<Self> {
        let mut source = Self::from_bytes(bytes)?;
        source.looping = false;
        Ok(source)
    }

    /// 从视频文件加载视频 + 音频(音视频复合流)。`fps` 用于折算每帧音频包数。
    ///
    /// 视频同 [`from_path`](Self::from_path);另经 ffmpeg 抽音频轨为 G.711A 8kHz 单声道裸流
    /// (`-c:a pcm_alaw -ar 8000 -ac 1 -f alaw`),按 160B/20ms 分包。源无音频轨时音频为空,
    /// 退化为纯视频。裸流(.h264)无音频。
    pub fn from_path_av(path: &str, fps: u32) -> Result<Self> {
        let mut s = Self::from_path(path)?;
        // 裸流无容器音频;仅对容器源尝试抽音频。
        if let Ok(alaw) = extract_g711a(path) {
            if !alaw.is_empty() {
                s.audio = alaw.chunks(G711_PACKET_BYTES).map(|c| c.to_vec()).collect();
                // 每帧时长 = 1/fps 秒;每音频包 20ms;每帧音频包数 = (1000/fps)/20。
                let fps = fps.max(1);
                s.audio_per_frame = ((1000 / fps) / 20).max(1) as usize;
            }
        }
        Ok(s)
    }

    /// 从内存中的 Annex B 字节切帧(纯视频)。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let nals = split_annex_b(bytes);
        if nals.is_empty() {
            return Err(Error::Media("H.264 流为空或无起始码".into()));
        }
        let codec = detect_codec(&nals);
        let frames = group_into_frames(nals, codec);
        Ok(FileSource {
            frames,
            cursor: 0,
            codec,
            audio: Vec::new(),
            audio_cursor: 0,
            audio_per_frame: 0,
            looping: true,
            historical_events: None,
            historical_event_cursor: 0,
            historical_pending_event: None,
            historical_pending_audio: Vec::new(),
        })
    }

    /// 帧总数。
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// 是否无帧。
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

#[derive(Debug, Clone)]
struct HistoricalVideoTiming {
    pts_90k: i128,
    duration_90k: u64,
    key_frame: bool,
}

#[derive(Debug, Clone)]
struct HistoricalAudioTiming {
    pts_90k: i128,
    duration_90k: u64,
    sample_rate_hz: u32,
    sample_count: u32,
    codec: crate::ps::AudioCodec,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
struct HistoricalSegment {
    codec: crate::ps::VideoCodec,
    video: Vec<HistoricalVideoTiming>,
    audio: Vec<HistoricalAudioTiming>,
}

fn looks_like_annexb(path: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0_u8; 8];
    let count = file.read(&mut head).unwrap_or(0);
    starts_with_annexb(&head[..count])
}

fn history_ffprobe_bin() -> Option<String> {
    use std::path::PathBuf;
    use std::process::Command;
    let mut candidates = Vec::<PathBuf>::new();
    if let Ok(path) = std::env::var("FFPROBE_BIN") {
        candidates.push(PathBuf::from(path));
    }
    if let Some(ffmpeg) = ffmpeg_bin() {
        if let Some(parent) = std::path::Path::new(&ffmpeg).parent() {
            candidates.push(parent.join(if cfg!(target_os = "windows") {
                "ffprobe.exe"
            } else {
                "ffprobe"
            }));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            for name in if cfg!(target_os = "windows") {
                ["ffprobe.exe", "ffprobe"]
            } else {
                ["ffprobe", "ffprobe.exe"]
            } {
                candidates.push(parent.join(name));
                candidates.push(parent.join("resources").join(name));
                candidates.push(parent.join("../Resources").join(name));
            }
        }
    }
    candidates.extend([
        PathBuf::from("ffprobe"),
        PathBuf::from("/opt/homebrew/bin/ffprobe"),
        PathBuf::from("/usr/local/bin/ffprobe"),
        PathBuf::from("/opt/local/bin/ffprobe"),
        PathBuf::from("/usr/bin/ffprobe"),
    ]);
    candidates.into_iter().find_map(|candidate| {
        let executable = candidate.to_string_lossy().into_owned();
        Command::new(&executable)
            .arg("-version")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|_| executable)
    })
}

fn run_history_ffprobe(path: &str, args: &[&str]) -> Result<serde_json::Value> {
    use std::process::Command;
    let ffprobe = history_ffprobe_bin()
        .ok_or_else(|| Error::Media(format!("历史录像需要 ffprobe 读取源时间戳: {path}")))?;
    let output = Command::new(ffprobe)
        .args(["-v", "error"])
        .args(args)
        .arg(path)
        .output()
        .map_err(|error| Error::Media(format!("启动 ffprobe 失败: {error}")))?;
    if !output.status.success() {
        return Err(Error::Media(format!(
            "ffprobe 历史录像失败({path}): {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("未知错误")
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::Media(format!("解析历史录像 ffprobe JSON 失败: {error}")))
}

fn json_i128(value: &serde_json::Value, key: &str) -> Option<i128> {
    let value = value.get(key)?;
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
        .or_else(|| value.as_str()?.parse::<i128>().ok())
}

fn json_u32(value: &serde_json::Value, key: &str) -> Option<u32> {
    let value = value.get(key)?;
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .or_else(|| value.as_str()?.parse::<u32>().ok())
}

fn json_bool(value: &serde_json::Value, key: &str) -> bool {
    json_i128(value, key).unwrap_or(0) != 0
}

fn json_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn parse_rational(value: &str) -> Option<(u64, u64)> {
    let (numerator, denominator) = value.split_once('/')?;
    let numerator = numerator.parse::<u64>().ok()?;
    let denominator = denominator.parse::<u64>().ok()?;
    (numerator > 0 && denominator > 0).then_some((numerator, denominator))
}

fn stream_time_base(stream: &serde_json::Value) -> Result<(u64, u64)> {
    let value = json_string(stream, "time_base")
        .ok_or_else(|| Error::Media("历史录像流缺少 time_base".into()))?;
    parse_rational(value).ok_or_else(|| Error::Media(format!("历史录像 time_base 无效: {value}")))
}

fn rescale_timestamp(value: i128, numerator: u64, denominator: u64) -> Result<i128> {
    let scaled = value
        .checked_mul(i128::from(MEDIA_CLOCK_HZ))
        .and_then(|value| value.checked_mul(i128::from(numerator)))
        .ok_or_else(|| Error::Media("历史录像时间戳超出内部范围".into()))?;
    let denominator = i128::from(denominator);
    let mut result = scaled / denominator;
    let remainder = scaled % denominator;
    if remainder.abs() * 2 >= denominator {
        result += if scaled.is_negative() { -1 } else { 1 };
    }
    Ok(result)
}

fn rescale_samples(
    value: i128,
    numerator: u64,
    denominator: u64,
    sample_rate_hz: u32,
) -> Result<u32> {
    let scaled = value
        .checked_mul(i128::from(numerator))
        .and_then(|value| value.checked_mul(i128::from(sample_rate_hz)))
        .ok_or_else(|| Error::Media("历史录像音频样本数超出内部范围".into()))?;
    let denominator = i128::from(denominator);
    let sample_count = (scaled / denominator).max(0);
    u32::try_from(sample_count).map_err(|_| Error::Media("历史录像音频样本数超出 u32 范围".into()))
}

fn value_array<'a>(value: &'a serde_json::Value, key: &str) -> &'a [serde_json::Value] {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn probe_historical_segment(path: &str) -> Result<HistoricalSegment> {
    let video_json = run_history_ffprobe(
        path,
        &[
            "-select_streams",
            "v:0",
            "-show_streams",
            "-show_frames",
            "-show_entries",
            "stream=codec_name,codec_type,has_b_frames,time_base,duration_ts,r_frame_rate:frame=key_frame,best_effort_timestamp,pkt_pts,pkt_dts,pkt_duration,pict_type",
            "-of",
            "json",
        ],
    )?;
    let stream = value_array(&video_json, "streams")
        .first()
        .ok_or_else(|| Error::Media(format!("历史录像没有视频流: {path}")))?;
    let codec = match json_string(stream, "codec_name") {
        Some("h264") => crate::ps::VideoCodec::H264,
        Some("hevc") | Some("h265") => crate::ps::VideoCodec::H265,
        Some(codec) => return Err(Error::Media(format!("历史录像视频编码不支持: {codec}"))),
        None => return Err(Error::Media("历史录像视频流缺少 codec_name".into())),
    };
    if json_i128(stream, "has_b_frames").unwrap_or(0) > 0 {
        return Err(Error::Media(
            "历史录像包含 B 帧，当前历史时间轴不支持按显示顺序回放".into(),
        ));
    }
    let (time_base_num, time_base_den) = stream_time_base(stream)?;
    let frame_values = value_array(&video_json, "frames");
    if frame_values.is_empty() {
        return Err(Error::Media(format!("历史录像没有可读取的视频帧: {path}")));
    }
    let frame_rate = json_string(stream, "r_frame_rate").and_then(parse_rational);
    let fallback_duration = frame_rate.and_then(|(numerator, denominator)| {
        rescale_timestamp(i128::from(denominator), 1, numerator)
            .ok()
            .and_then(|duration| u64::try_from(duration).ok())
    });
    let stream_duration = json_i128(stream, "duration_ts")
        .map(|duration| rescale_timestamp(duration, time_base_num, time_base_den))
        .transpose()?;
    let mut raw_pts = Vec::with_capacity(frame_values.len());
    let mut key_frames = Vec::with_capacity(frame_values.len());
    let mut raw_durations = Vec::with_capacity(frame_values.len());
    for frame in frame_values {
        if json_string(frame, "pict_type") == Some("B") {
            return Err(Error::Media(
                "历史录像包含 B 帧，当前历史时间轴不支持按显示顺序回放".into(),
            ));
        }
        let pts = json_i128(frame, "best_effort_timestamp")
            .or_else(|| json_i128(frame, "pkt_pts"))
            .or_else(|| json_i128(frame, "pkt_dts"))
            .ok_or_else(|| Error::Media("历史录像视频帧缺少 PTS".into()))?;
        raw_pts.push(pts);
        key_frames.push(json_bool(frame, "key_frame"));
        raw_durations.push(json_i128(frame, "pkt_duration"));
    }
    let mut video = Vec::with_capacity(raw_pts.len());
    let mut previous_pts = None;
    for index in 0..raw_pts.len() {
        if let Some(previous) = previous_pts {
            if raw_pts[index] <= previous {
                return Err(Error::Media(
                    "历史录像视频 PTS 非递增，可能包含未支持的 B 帧".into(),
                ));
            }
        }
        previous_pts = Some(raw_pts[index]);
        let duration_90k = raw_durations[index]
            .filter(|duration| *duration > 0)
            .map(|duration| rescale_timestamp(duration, time_base_num, time_base_den))
            .transpose()?
            .and_then(|duration| u64::try_from(duration).ok())
            .filter(|duration| *duration > 0)
            .or_else(|| {
                raw_pts
                    .get(index + 1)
                    .map(|next| next - raw_pts[index])
                    .and_then(|duration| {
                        rescale_timestamp(duration, time_base_num, time_base_den)
                            .ok()
                            .and_then(|duration| u64::try_from(duration).ok())
                    })
            })
            .or_else(|| {
                let duration = stream_duration?;
                let pts = rescale_timestamp(raw_pts[index], time_base_num, time_base_den).ok()?;
                u64::try_from(duration - pts).ok()
            })
            .or(fallback_duration)
            .filter(|duration| *duration > 0)
            .ok_or_else(|| Error::Media("历史录像视频帧缺少有效时长".into()))?;
        let pts_90k = rescale_timestamp(raw_pts[index], time_base_num, time_base_den)?;
        video.push(HistoricalVideoTiming {
            pts_90k,
            duration_90k,
            key_frame: key_frames[index],
        });
    }
    let audio = probe_historical_audio(path)?;
    Ok(HistoricalSegment {
        codec,
        video,
        audio,
    })
}

fn probe_historical_audio(path: &str) -> Result<Vec<HistoricalAudioTiming>> {
    let audio_json = run_history_ffprobe(
        path,
        &[
            "-select_streams",
            "a:0",
            "-show_streams",
            "-show_packets",
            "-show_entries",
            "stream=codec_name,codec_type,sample_rate,time_base:packet=pts,dts,duration,size",
            "-of",
            "json",
        ],
    )?;
    let Some(stream) = value_array(&audio_json, "streams").first() else {
        return Ok(Vec::new());
    };
    let codec = match json_string(stream, "codec_name") {
        Some("aac") => crate::ps::AudioCodec::Aac,
        Some("pcm_alaw") | Some("alaw") => crate::ps::AudioCodec::G711A,
        Some("pcm_mulaw") | Some("mulaw") => crate::ps::AudioCodec::G711U,
        Some("opus") => crate::ps::AudioCodec::Opus,
        Some(codec) => return Err(Error::Media(format!("历史录像音频编码不支持: {codec}"))),
        None => return Err(Error::Media("历史录像音频流缺少 codec_name".into())),
    };
    let sample_rate_hz = json_u32(stream, "sample_rate")
        .ok_or_else(|| Error::Media("历史录像音频流缺少 sample_rate".into()))?;
    if sample_rate_hz == 0 {
        return Err(Error::Media("历史录像音频采样率无效".into()));
    }
    let (time_base_num, time_base_den) = stream_time_base(stream)?;
    let packet_values = value_array(&audio_json, "packets");
    if packet_values.is_empty() {
        return Err(Error::Media("历史录像音频流没有可读取的音频包".into()));
    }
    let mut packets = Vec::with_capacity(packet_values.len());
    for packet in packet_values {
        let pts = json_i128(packet, "pts")
            .or_else(|| json_i128(packet, "dts"))
            .ok_or_else(|| Error::Media("历史录像音频包缺少 PTS".into()))?;
        let size = json_u32(packet, "size")
            .ok_or_else(|| Error::Media("历史录像音频包缺少 size".into()))?;
        let duration_units = json_i128(packet, "duration");
        let sample_count = duration_units
            .filter(|duration| *duration > 0)
            .map(|duration| rescale_samples(duration, time_base_num, time_base_den, sample_rate_hz))
            .transpose()?
            .filter(|count| *count > 0)
            .unwrap_or_else(|| crate::ps::AudioCodec::default_sample_count(codec));
        let duration_90k = duration_units
            .filter(|duration| *duration > 0)
            .map(|duration| rescale_timestamp(duration, time_base_num, time_base_den))
            .transpose()?
            .and_then(|duration| u64::try_from(duration).ok())
            .filter(|duration| *duration > 0)
            .unwrap_or_else(|| {
                crate::ps::AudioCodec::duration_90k_for_samples(codec, sample_rate_hz, sample_count)
            });
        packets.push((
            pts,
            duration_90k,
            sample_rate_hz,
            sample_count,
            usize::try_from(size).map_err(|_| Error::Media("历史录像音频包过大".into()))?,
        ));
    }
    let payloads = extract_historical_audio(path, codec, &packets)?;
    if payloads.len() != packets.len() {
        return Err(Error::Media(format!(
            "历史录像音频包数与抽取结果不一致: {} != {}",
            packets.len(),
            payloads.len()
        )));
    }
    Ok(packets
        .into_iter()
        .zip(payloads)
        .map(
            |((pts, duration_90k, sample_rate_hz, sample_count, _), data)| {
                Ok(HistoricalAudioTiming {
                    pts_90k: rescale_timestamp(pts, time_base_num, time_base_den)?,
                    duration_90k,
                    sample_rate_hz,
                    sample_count,
                    codec,
                    data,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?)
}

fn extract_historical_audio(
    path: &str,
    codec: crate::ps::AudioCodec,
    packets: &[(i128, u64, u32, u32, usize)],
) -> Result<Vec<Vec<u8>>> {
    use std::process::Command;
    let ffmpeg = ffmpeg_bin()
        .ok_or_else(|| Error::Media(format!("历史录像包含音频，需要 ffmpeg 抽取编码包: {path}")))?;
    let nonce = now_ms().wrapping_add(rand::random::<u32>() as u64);
    let (format, suffix) = match codec {
        crate::ps::AudioCodec::Aac => ("adts", "aac"),
        crate::ps::AudioCodec::G711A => ("alaw", "alaw"),
        crate::ps::AudioCodec::G711U => ("mulaw", "mulaw"),
        crate::ps::AudioCodec::Opus => ("data", "opus"),
    };
    let output_path = std::env::temp_dir().join(format!("uvp-history-audio-{nonce}.{suffix}"));
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(path)
        .args([
            "-map", "0:a:0", "-vn", "-sn", "-dn", "-c:a", "copy", "-f", format,
        ])
        .arg(&output_path)
        .output()
        .map_err(|error| Error::Media(format!("启动 ffmpeg 抽取历史音频失败: {error}")))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&output_path);
        return Err(Error::Media(format!(
            "ffmpeg 抽取历史音频失败({path}): {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("未知错误")
        )));
    }
    let bytes = std::fs::read(&output_path).map_err(Error::Io)?;
    let _ = std::fs::remove_file(&output_path);
    match codec {
        crate::ps::AudioCodec::Aac => split_adts_frames(&bytes),
        crate::ps::AudioCodec::G711A
        | crate::ps::AudioCodec::G711U
        | crate::ps::AudioCodec::Opus => split_sized_packets(&bytes, packets),
    }
}

fn split_sized_packets(
    bytes: &[u8],
    packets: &[(i128, u64, u32, u32, usize)],
) -> Result<Vec<Vec<u8>>> {
    let expected = packets.iter().try_fold(0_usize, |total, packet| {
        total
            .checked_add(packet.4)
            .ok_or_else(|| Error::Media("历史录像音频总大小超出内部范围".into()))
    })?;
    if bytes.len() != expected {
        return Err(Error::Media(format!(
            "历史录像音频抽取大小与 packet size 不一致: {} != {}",
            bytes.len(),
            expected
        )));
    }
    let mut offset = 0_usize;
    Ok(packets
        .iter()
        .map(|packet| {
            let end = offset + packet.4;
            let data = bytes[offset..end].to_vec();
            offset = end;
            data
        })
        .collect::<Vec<_>>())
}

fn split_adts_frames(bytes: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut offset = 0_usize;
    let mut frames = Vec::new();
    while offset < bytes.len() {
        if bytes.len().saturating_sub(offset) < 7
            || bytes[offset] != 0xff
            || (bytes[offset + 1] & 0xf6) != 0xf0
        {
            return Err(Error::Media("历史录像 AAC 抽取结果不是有效 ADTS".into()));
        }
        let header_len = if bytes[offset + 1] & 1 == 0 { 9 } else { 7 };
        let frame_len = (usize::from(bytes[offset + 3] & 0x03) << 11)
            | (usize::from(bytes[offset + 4]) << 3)
            | usize::from(bytes[offset + 5] >> 5);
        if frame_len < header_len || offset + frame_len > bytes.len() {
            return Err(Error::Media("历史录像 AAC ADTS 帧长度无效".into()));
        }
        frames.push(bytes[offset..offset + frame_len].to_vec());
        offset += frame_len;
    }
    Ok(frames)
}

fn build_historical_events(
    segment: &HistoricalSegment,
    frames: Vec<Frame>,
) -> Result<Vec<MediaEvent>> {
    let first_video_pts = segment
        .video
        .first()
        .map(|video| video.pts_90k)
        .ok_or_else(|| Error::Media("历史录像没有视频时间戳".into()))?;
    let first_audio_pts = segment.audio.first().map(|audio| audio.pts_90k);
    let segment_base = first_audio_pts
        .map(|audio| audio.min(first_video_pts))
        .unwrap_or(first_video_pts);
    let mut events = Vec::with_capacity(segment.video.len() + segment.audio.len());
    for (frame, timing) in frames.into_iter().zip(&segment.video) {
        let pts_90k = u64::try_from(timing.pts_90k - segment_base)
            .map_err(|_| Error::Media("历史录像视频 PTS 归一化失败".into()))?;
        events.push(MediaEvent::Video(TimedVideoAu::new(
            frame.data,
            segment.codec,
            frame.key_frame || timing.key_frame,
            pts_90k,
            timing.duration_90k,
        )));
    }
    for audio in &segment.audio {
        let pts_90k = u64::try_from(audio.pts_90k - segment_base)
            .map_err(|_| Error::Media("历史录像音频 PTS 归一化失败".into()))?;
        events.push(MediaEvent::Audio(TimedAudioAu::with_duration(
            audio.data.clone(),
            audio.codec,
            audio.sample_rate_hz,
            audio.sample_count,
            pts_90k,
            audio.duration_90k,
        )));
    }
    events.sort_by_key(|event| (event.pts_90k(), matches!(event, MediaEvent::Audio(_)) as u8));
    Ok(events)
}

fn event_duration_90k(event: &MediaEvent) -> u64 {
    match event {
        MediaEvent::Video(video) => video.duration_90k,
        MediaEvent::Audio(audio) => audio.duration_90k,
        MediaEvent::Discontinuity { .. } => 0,
    }
}

fn add_event_offset(event: &mut MediaEvent, offset_90k: u64) -> Result<()> {
    let pts = event
        .pts_90k()
        .checked_add(offset_90k)
        .ok_or_else(|| Error::Media("历史录像多片段时间轴超出内部范围".into()))?;
    match event {
        MediaEvent::Video(video) => video.pts_90k = pts,
        MediaEvent::Audio(audio) => audio.pts_90k = pts,
        MediaEvent::Discontinuity { pts_90k } => *pts_90k = pts,
    }
    Ok(())
}

impl VideoSource for FileSource {
    fn next_frame(&mut self) -> Option<Frame> {
        if self.historical_events.is_some() {
            loop {
                match self.next_historical_event()? {
                    MediaEvent::Video(video) => {
                        return Some(Frame {
                            data: video.data,
                            key_frame: video.key_frame,
                        });
                    }
                    MediaEvent::Audio(audio) => self.historical_pending_audio.push(audio),
                    MediaEvent::Discontinuity { .. } => {}
                }
            }
        }
        if self.frames.is_empty() {
            return None;
        }
        if !self.looping && self.cursor >= self.frames.len() {
            return None;
        }
        let f = self.frames[self.cursor].clone();
        self.cursor += 1;
        if self.looping {
            self.cursor %= self.frames.len();
        }
        Some(f)
    }

    fn has_audio(&self) -> bool {
        self.historical_events
            .as_ref()
            .map(|events| {
                events
                    .iter()
                    .any(|event| matches!(event, MediaEvent::Audio(_)))
            })
            .unwrap_or(!self.audio.is_empty())
    }

    fn seek(&mut self, permille: u32) {
        if self.frames.is_empty() {
            return;
        }
        if self.historical_events.is_some() {
            self.seek_historical(permille);
            return;
        }
        // 千分比 → 帧游标;从最近的关键帧起播,避免解码花屏。
        let target = (self.frames.len() as u64 * permille.min(1000) as u64 / 1000) as usize;
        let target = target.min(self.frames.len() - 1);
        // 向前找最近关键帧(找不到就用 target)。
        let start = (0..=target)
            .rev()
            .find(|&i| self.frames[i].key_frame)
            .unwrap_or(target);
        self.cursor = start;
        // 音频游标按比例同步。
        if !self.audio.is_empty() {
            self.audio_cursor = (self.audio.len() as u64 * permille.min(1000) as u64 / 1000)
                as usize
                % self.audio.len();
        }
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        if self.historical_events.is_some() {
            let mut out: Vec<Vec<u8>> = self
                .historical_pending_audio
                .drain(..)
                .map(|audio| audio.data)
                .collect();
            while let Some(event) = self.next_historical_event() {
                match event {
                    MediaEvent::Audio(audio) => out.push(audio.data),
                    other => {
                        self.historical_pending_event = Some(other);
                        break;
                    }
                }
            }
            return out;
        }
        if self.audio.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.audio_per_frame);
        for _ in 0..self.audio_per_frame {
            out.push(self.audio[self.audio_cursor].clone());
            self.audio_cursor = (self.audio_cursor + 1) % self.audio.len(); // 循环
        }
        out
    }

    fn supports_timed_events(&self) -> bool {
        self.historical_events.is_some()
    }

    fn audio_sample_rate_hz(&self) -> u32 {
        self.historical_events
            .as_ref()
            .and_then(|events| {
                events.iter().find_map(|event| match event {
                    MediaEvent::Audio(audio) => Some(audio.sample_rate_hz),
                    _ => None,
                })
            })
            .unwrap_or_else(|| self.audio_codec().default_sample_rate_hz())
    }

    fn audio_codec(&self) -> crate::ps::AudioCodec {
        self.historical_events
            .as_ref()
            .and_then(|events| {
                events.iter().find_map(|event| match event {
                    MediaEvent::Audio(audio) => Some(audio.codec),
                    _ => None,
                })
            })
            .unwrap_or(crate::ps::AudioCodec::G711A)
    }

    fn next_media_event(&mut self) -> Option<MediaEvent> {
        if self.historical_events.is_some() {
            return self.next_historical_event();
        }
        let frame = self.next_frame()?;
        Some(MediaEvent::Video(TimedVideoAu::new(
            frame.data,
            self.codec,
            frame.key_frame,
            0,
            0,
        )))
    }

    fn codec(&self) -> crate::ps::VideoCodec {
        self.codec
    }
}

impl FileSource {
    fn next_historical_event(&mut self) -> Option<MediaEvent> {
        if let Some(event) = self.historical_pending_event.take() {
            return Some(event);
        }
        let events = self.historical_events.as_ref()?;
        let event = events.get(self.historical_event_cursor)?.clone();
        self.historical_event_cursor += 1;
        Some(event)
    }

    fn seek_historical(&mut self, permille: u32) {
        let Some(events) = self.historical_events.as_ref() else {
            return;
        };
        let duration = events
            .iter()
            .map(|event| event.pts_90k().saturating_add(event_duration_90k(event)))
            .max()
            .unwrap_or(0);
        let target = duration.saturating_mul(u64::from(permille.min(1000))) / 1000;
        let candidate = events
            .iter()
            .enumerate()
            .find(|(_, event)| matches!(event, MediaEvent::Video(video) if video.pts_90k >= target))
            .map(|(index, _)| index)
            .or_else(|| {
                events
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, event)| matches!(event, MediaEvent::Video(_)))
                    .map(|(index, _)| index)
            })
            .unwrap_or(events.len());
        self.historical_event_cursor = if candidate == events.len() {
            events.len()
        } else {
            (0..=candidate)
                .rev()
                .find(|&index| {
                    matches!(events[index], MediaEvent::Video(ref video) if video.key_frame)
                })
                .unwrap_or(candidate)
        };
        self.historical_pending_event = None;
        self.historical_pending_audio.clear();
    }
}

/// 从 NAL 流探测视频编码。H.265 存在 VPS(nal_type=32),H.264 无此类型;
/// 以此区分。NAL 头首字节 = bytes[3](起始码后),H.265 type=(b>>1)&0x3F。
fn detect_codec(nals: &[Nal<'_>]) -> crate::ps::VideoCodec {
    // H.264 的非 IDR slice 头(例如 0x41)右移一位后也等于 H.265 VPS
    // 类型 32；先用 H.264 的 VCL/参数集类型排除这个重叠，否则普通 H.264
    // 码流会被误判为 H.265，进而无法正确切帧。
    if nals.iter().any(|nal| matches!(nal.nal_type, 1 | 5 | 7 | 8)) {
        return crate::ps::VideoCodec::H264;
    }
    for nal in nals {
        if let Some(&b) = nal.bytes.get(3) {
            let h265_type = (b >> 1) & 0x3F;
            // VPS(32)/SPS(33)/PPS(34) 为 H.265 参数集;VPS 在 H.264 中不存在,最可靠。
            if h265_type == 32 {
                return crate::ps::VideoCodec::H265;
            }
        }
    }
    crate::ps::VideoCodec::H264
}

/// 一个带起始码位置的 NAL 单元(含起始码)。
struct Nal<'a> {
    bytes: &'a [u8],
    nal_type: u8,
}

/// 按 Annex B 起始码(00 00 01 或 00 00 00 01)切分 NAL,保留起始码。
fn split_annex_b(data: &[u8]) -> Vec<Nal<'_>> {
    let mut positions = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            positions.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut nals = Vec::new();
    for idx in 0..positions.len() {
        let start = positions[idx];
        let end = if idx + 1 < positions.len() {
            positions[idx + 1]
        } else {
            data.len()
        };
        let bytes = &data[start..end];
        // 起始码后第一个字节是 NAL 头,type = 低 5 位。
        let nal_type = bytes.get(3).map(|b| b & 0x1f).unwrap_or(0);
        nals.push(Nal { bytes, nal_type });
    }
    nals
}

/// 把 NAL 聚成访问单元(帧)。遇到新的 VCL slice 作为帧起点。
///
/// H.264:type=低5位,VCL=1/5,关键帧含 5(IDR)/7(SPS)/8(PPS)。
/// H.265:type=(b>>1)&0x3F,VCL=0..=31,关键帧含 IDR/CRA(16-21)或 VPS/SPS/PPS(32/33/34)。
fn group_into_frames(nals: Vec<Nal<'_>>, codec: crate::ps::VideoCodec) -> Vec<Frame> {
    let is_h265 = codec == crate::ps::VideoCodec::H265;
    let mut frames = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut cur_key = false;
    let mut has_vcl = false;

    for nal in nals {
        // 按编码取 NAL 类型。
        let t = if is_h265 {
            nal.bytes.get(3).map(|b| (b >> 1) & 0x3F).unwrap_or(0)
        } else {
            nal.nal_type
        };
        let is_vcl = if is_h265 { t <= 31 } else { t == 1 || t == 5 };
        let is_key = if is_h265 {
            (16..=21).contains(&t) || (32..=34).contains(&t)
        } else {
            t == 5 || t == 7 || t == 8
        };
        // 已有一个含 VCL 的帧,又来新 VCL → 切帧。
        if is_vcl && has_vcl {
            frames.push(Frame {
                data: std::mem::take(&mut cur),
                key_frame: cur_key,
            });
            cur_key = false;
            has_vcl = false;
        }
        if is_key {
            cur_key = true;
        }
        if is_vcl {
            has_vcl = true;
        }
        cur.extend_from_slice(nal.bytes);
    }
    if !cur.is_empty() {
        frames.push(Frame {
            data: cur,
            key_frame: cur_key,
        });
    }
    frames
}

/// 实时采集视频源:用 ffmpeg avfoundation(macOS)/dshow(Windows)/v4l2(Linux)
/// 采集摄像头/屏幕,实时编码为 H.264 Annex B,边采边推(把电脑当真实 IPC)。
///
/// ffmpeg 进程 stdout 输出 Annex B 裸流,后台线程按起始码切帧塞入队列,
/// `next_frame()` 从队列取;队列空(采集慢于消费)时返回上一关键帧维持画面。
#[path = "source_live.rs"]
mod live;
pub use live::{
    list_live_sources, LiveAudioCodec, LiveAudioSource, LiveAvDevice, LiveScreenDevice, LiveSource,
    LiveSourceCatalog, LiveSourceSpec, LiveVideoCodec, LiveVideoProfile,
};

// Superseded implementation retained disabled so the surrounding user changes remain reviewable
// while canonical live URIs use the implementation in source_live.rs.
#[cfg(any())]
mod obsolete_live {
    use super::*;

    /// 实时采集视频源后端模式。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum LiveBackend {
        /// 方案 B:Native 原生模式(内置 openh264 硬件/软编码 + 原生 xcap 屏幕抓取/动态渲染器,零外部 CLI 依赖)
        Native,
        /// 方案 A:FFmpeg 命令行子进程模式(依赖系统路径包含 ffmpeg 可执行文件)
        FfmpegCmd,
    }

    /// 实时采集视频源。
    ///
    /// 方案 B 默认激活:使用内置 `openh264` 编码器 + `xcap` 原生屏幕/摄像头捕获,
    /// 无需在系统安装 ffmpeg 命令行工具。
    ///
    /// 方案 A 完整保留在 [`LiveSource::capture_ffmpeg`]:可通过 `live:ffmpeg:...` 或
    /// 环境变量 `UVP_LIVE_BACKEND=ffmpeg` 随时切换回方案 A 对比效果。
    pub struct LiveSource {
        /// 帧队列接收端(后台采集线程 → 消费)。
        rx: std::sync::mpsc::Receiver<Frame>,
        /// 本地帧缓冲(FIFO,保持解码顺序;堆积时快进到最近关键帧)。
        buf: std::collections::VecDeque<Frame>,
        /// 目标帧率(用于计算缓冲上界和 last_key 重复上限)。
        fps: u32,
        /// 方案 A 时使用的 ffmpeg 子进程(drop 时 kill)。
        child: Option<std::process::Child>,
        /// 方案 B 后台采集/编码线程停止标志。
        stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
        /// 最近一个关键帧(队列空时短暂兜底,避免瞬时花屏)。
        last_key: Option<Frame>,
        /// 连续队列空的拍数计数。超过阈值后停止返回 last_key,改返回 None。
        consecutive_empty: u32,
    }

    impl LiveSource {
        /// 采集指定输入设备。默认采用方案 B(Native openh264 + 原生捕获)。
        /// 若 `input` 包含 `ffmpeg` 或指定环境变量 `UVP_LIVE_BACKEND=ffmpeg`,则走方案 A。
        pub fn capture(input: &str, fps: u32) -> Result<Self> {
            let use_ffmpeg_a = input.contains("ffmpeg")
                || std::env::var("UVP_LIVE_BACKEND")
                    .map(|v| v.eq_ignore_ascii_case("ffmpeg"))
                    .unwrap_or(false);

            if use_ffmpeg_a {
                tracing::info!(input, "【方案 A 激活】使用外置 FFmpeg 子进程实时采集");
                Self::capture_ffmpeg(input, fps)
            } else {
                tracing::info!(
                    input,
                    "【方案 B 激活】使用内置 Native (openh264 + 原生抓屏) 实时采集"
                );
                Self::capture_native(input, fps)
            }
        }

        /// 方案 B:Native 原生模式(openh264 编码 + 原生屏幕抓取/动态视效)。
        /// 完全零外部依赖,免安装 ffmpeg 二进制文件。
        pub fn capture_native(input: &str, fps: u32) -> Result<Self> {
            use openh264::encoder::Encoder;
            use openh264::formats::{RgbSliceU8, YUVBuffer};
            use std::sync::atomic::{AtomicBool, Ordering};

            let width = 1280usize;
            let height = 720usize;
            let fps = fps.max(1);

            let mut encoder = Encoder::new()
                .map_err(|e| Error::Media(format!("方案 B 初始化 openh264 编码器失败: {e}")))?;

            let (tx, rx) = std::sync::mpsc::channel::<Frame>();
            let stop_flag = std::sync::Arc::new(AtomicBool::new(false));
            let sf = stop_flag.clone();

            // 尝试解析屏幕设备序号
            let monitor_idx: usize = input
                .trim_start_matches("live:")
                .trim_start_matches("native:")
                .parse()
                .unwrap_or(0);

            std::thread::spawn(move || {
                let interval = std::time::Duration::from_millis(1000 / fps as u64);
                let mut rgb_buf = vec![0u8; width * height * 3];
                let mut frame_seq: u64 = 0;

                while !sf.load(Ordering::Relaxed) {
                    let start_time = std::time::Instant::now();
                    frame_seq += 1;

                    // 尝试用 xcap 抓取原生屏幕图像
                    let captured = xcap::Monitor::all().ok().and_then(|monitors| {
                        let m = monitors.get(monitor_idx).or_else(|| monitors.first())?;
                        let img = m.capture_image().ok()?;
                        // 将捕获图片转换并调整分辨率至 1280x720
                        let resized = image::imageops::resize(
                            &img,
                            width as u32,
                            height as u32,
                            image::imageops::FilterType::Triangle,
                        );
                        Some(resized.into_raw()) // RGBA 字节流 (width * height * 4)
                    });

                    if let Some(rgba_raw) = captured {
                        // RGBA -> RGB24
                        for i in 0..(width * height) {
                            rgb_buf[i * 3] = rgba_raw[i * 4];
                            rgb_buf[i * 3 + 1] = rgba_raw[i * 4 + 1];
                            rgb_buf[i * 3 + 2] = rgba_raw[i * 4 + 2];
                        }
                    } else {
                        // 屏幕不可用或受限时:绘制原生动态时钟/实时高清画面(保证流100%不间断)
                        let now_str = chrono_str();
                        render_dynamic_testcard(&mut rgb_buf, width, height, frame_seq, &now_str);
                    }

                    // openh264 编码:RGB24 -> YUVBuffer -> Bitstream
                    let yuv =
                        YUVBuffer::from_rgb8_source(RgbSliceU8::new(&rgb_buf, (width, height)));
                    if let Ok(bitstream) = encoder.encode(&yuv) {
                        let frame_bytes = bitstream.to_vec();
                        if !frame_bytes.is_empty() {
                            let is_key = frame_bytes.windows(4).any(|w| w == [0, 0, 0, 1])
                                && frame_bytes.iter().any(|&b| (b & 0x1f) == 5);
                            let _ = tx.send(Frame {
                                data: frame_bytes,
                                key_frame: is_key,
                            });
                        }
                    }

                    let elapsed = start_time.elapsed();
                    if elapsed < interval {
                        std::thread::sleep(interval - elapsed);
                    }
                }
            });

            Ok(LiveSource {
                rx,
                buf: std::collections::VecDeque::new(),
                fps,
                child: None,
                stop_flag,
                last_key: None,
                consecutive_empty: 0,
            })
        }

        /// 方案 A:FFmpeg 命令行子进程模式(依赖系统 ffmpeg 二进制文件)。
        /// 完整保留原有代码逻辑,方便切换对比。
        pub fn capture_ffmpeg(input: &str, fps: u32) -> Result<Self> {
            use std::io::Read;
            use std::process::{Command, Stdio};

            let ffmpeg = ffmpeg_bin().ok_or_else(|| {
                Error::Media(
                    "实时采集[方案 A]需要系统 ffmpeg 命令,但未找到;请安装 ffmpeg 或使用默认方案 B"
                        .into(),
                )
            })?;

            let fmt = if cfg!(target_os = "macos") {
                "avfoundation"
            } else if cfg!(target_os = "windows") {
                "dshow"
            } else {
                "v4l2"
            };
            let fps_s = fps.max(1).to_string();
            let clean_input = input
                .trim_start_matches("live:")
                .trim_start_matches("ffmpeg:");
            let input_arg = if clean_input.is_empty() {
                "0"
            } else {
                clean_input
            };

            let vf_scale = "scale=min(1280\\,iw):min(720\\,ih):force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2";
            let mut child = Command::new(&ffmpeg)
                .args([
                    "-f",
                    fmt,
                    "-framerate",
                    &fps_s,
                    "-i",
                    input_arg,
                    "-an",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-tune",
                    "zerolatency",
                    "-x264-params",
                    "slices=1:sliced-threads=0",
                    "-pix_fmt",
                    "yuv420p",
                    "-vf",
                    vf_scale,
                    "-crf",
                    "28",
                    "-g",
                    &fps_s,
                    "-f",
                    "h264",
                    "-",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| Error::Media(format!("方案 A 启动 ffmpeg 采集失败: {e}")))?;

            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| Error::Media("无法获取 ffmpeg stdout".into()))?;

            let err_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
            let err_handle = child.stderr.take().map(|mut se| {
                let eb = err_buf.clone();
                std::thread::spawn(move || {
                    let mut s = String::new();
                    let _ = se.read_to_string(&mut s);
                    if let Ok(mut g) = eb.lock() {
                        *g = s;
                    }
                })
            });

            let got_data = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (tx, rx) = std::sync::mpsc::channel::<Frame>();
            let gd = got_data.clone();
            std::thread::spawn(move || {
                use std::sync::atomic::Ordering;
                let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
                let mut chunk = [0u8; 32 * 1024];
                loop {
                    match stdout.read(&mut chunk) {
                        Ok(0) => break,
                        Ok(n) => {
                            gd.store(true, Ordering::Relaxed);
                            buf.extend_from_slice(&chunk[..n]);
                            drain_frames(&mut buf, &tx);
                        }
                        Err(_) => break,
                    }
                }
            });

            let take_err = |handle: Option<std::thread::JoinHandle<()>>| -> String {
                if let Some(h) = handle {
                    let _ = h.join();
                }
                let raw = err_buf.lock().ok().map(|g| g.clone()).unwrap_or_default();
                let tail: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
                tail.iter()
                    .rev()
                    .take(3)
                    .rev()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(" | ")
            };
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !got_data.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(status) = child
                    .try_wait()
                    .map_err(|e| Error::Media(format!("等待 ffmpeg 采集失败: {e}")))?
                {
                    return Err(Error::Media(format!(
                        "方案 A 实时采集启动失败(ffmpeg 退出:{status}):{}",
                        take_err(err_handle)
                    )));
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    return Err(Error::Media(format!(
                        "方案 A 实时采集 5s 内无数据(检查权限或设备 '{clean_input}'):{}",
                        take_err(err_handle)
                    )));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            Ok(LiveSource {
                rx,
                buf: std::collections::VecDeque::new(),
                fps,
                child: Some(child),
                stop_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                last_key: None,
                consecutive_empty: 0,
            })
        }
    }

    impl Drop for LiveSource {
        fn drop(&mut self) {
            self.stop_flag
                .store(true, std::sync::atomic::Ordering::Relaxed);
            if let Some(mut child) = self.child.take() {
                let _ = child.kill(); // 停止方案 A 子进程
            }
        }
    }

    impl VideoSource for LiveSource {
        fn is_live(&self) -> bool {
            true
        }

        fn next_frame(&mut self) -> Option<Frame> {
            // 先把采集线程新产的帧全部收进本地缓冲(FIFO,保持解码顺序)。
            while let Ok(f) = self.rx.try_recv() {
                self.buf.push_back(f);
            }
            // 有界快进:缓冲堆积过多(采集短暂快于推流)时,丢到最近一个关键帧起点,
            // 既降延迟又保证从 IDR 续帧、绝不把关键帧连同 SPS/PPS 一起丢掉(否则平台解不出图)。
            let cap = (self.fps.max(1) * 3) as usize;
            if self.buf.len() > cap {
                if let Some(idx) = self.buf.iter().rposition(|f| f.key_frame) {
                    self.buf.drain(..idx);
                }
            }
            if let Some(f) = self.buf.pop_front() {
                self.consecutive_empty = 0; // 有新帧,重置空计数
                if f.key_frame {
                    self.last_key = Some(f.clone());
                }
                Some(f)
            } else {
                self.consecutive_empty += 1;
                // 队列暂空:最多允许 last_key 兜底 2 拍(≈80ms@25fps),
                // 超过后返回 None 让 pusher 跳过本拍。
                // 目的:避免采集慢于推流时无限重复同一帧,导致平台判定流异常关连接。
                let max_repeat = (self.fps.max(1) / 12).max(2);
                if self.consecutive_empty <= max_repeat {
                    self.last_key.clone()
                } else {
                    None
                }
            }
        }
    }
}

pub(crate) trait FrameSender {
    fn send_frame(&self, frame: Frame);
}

impl FrameSender for std::sync::mpsc::Sender<Frame> {
    fn send_frame(&self, frame: Frame) {
        let _ = self.send(frame);
    }
}

impl FrameSender for std::sync::mpsc::SyncSender<Frame> {
    fn send_frame(&self, frame: Frame) {
        let _ = self.try_send(frame);
    }
}

/// 从缓冲区切出完整 H.264 访问单元并发送;保留尾部不完整数据。
/// 关键修复:只有包含 VCL NAL(type 1/5)的访问单元才发出,
/// 纯 SPS+PPS 片段（等待 IDR 的前缀）保留在缓冲区,不提前发出。
/// 否则缓冲末尾恰好以 IDR 起始码结束时,SPS+PPS 会被单独发出为"关键帧",
/// 导致平台收到 332/332 全是无效的纯头部碎片而关闭连接。
pub(crate) fn drain_frames<S: FrameSender>(buf: &mut Vec<u8>, tx: &S) {
    // 找所有起始码位置，同时保留 3/4 字节起始码长度，避免把四字节起始码
    // 的第一个 0 粘到前一个 NAL。
    let mut starts = Vec::<(usize, usize)>::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if i + 4 <= buf.len() && buf[i..i + 4] == [0, 0, 0, 1] {
            starts.push((i, 4));
            i += 4;
        } else if buf[i..i + 3] == [0, 0, 1] {
            starts.push((i, 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.len() < 2 {
        return; // 不足两个起始码,等更多数据
    }

    let mut frame_start = starts[0].0;
    let mut key_frame = false;
    let mut has_vcl = false;
    let mut drained_to = 0;

    for &(position, start_len) in &starts {
        let Some(&header) = buf.get(position + start_len) else {
            break;
        };
        let nal_type = header & 0x1f;
        let is_vcl = (1..=5).contains(&nal_type);
        // first_mb_in_slice 是 slice header 的首个 ue(v)。值为 0 时，RBSP
        // 的第一个 bit 必为 1；非首 slice 的值大于 0，以 0 开始。
        let first_slice = is_vcl
            && buf
                .get(position + start_len + 1)
                .is_some_and(|payload| payload & 0x80 != 0);
        // SPS/PPS/AUD/前缀 SEI 出现在已有 VCL 后时属于下一张图，必须先
        // 结束前一 AU。旧实现直到 IDR 才切，导致 SPS/PPS 错粘到前一张 P 图。
        let starts_next_access_unit = first_slice || matches!(nal_type, 6..=9);

        if position > frame_start && has_vcl && starts_next_access_unit {
            tx.send_frame(Frame {
                data: buf[frame_start..position].to_vec(),
                key_frame,
            });
            frame_start = position;
            key_frame = false;
            has_vcl = false;
            drained_to = position;
        }

        if nal_type == 5 {
            key_frame = true;
        }
        if is_vcl {
            has_vcl = true;
        }
    }

    // 只有见到下一 AU 的起点才能证明当前 AU 完整。未闭合的 VCL、参数集和
    // 多 slice 尾部全部保留，等下次 read 后重新解析。
    if drained_to > 0 {
        buf.drain(..drained_to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ffmpeg查找_返回可执行或none不panic() {
        // 不强求装了 ffmpeg;只验证查找逻辑健壮(PATH + 绝对路径探测)不 panic,
        // 且返回值要么 None 要么是真能跑 -version 的路径。
        if let Some(bin) = ffmpeg_bin() {
            let ok = std::process::Command::new(&bin)
                .arg("-version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "ffmpeg_bin 返回的 {bin} 应可执行");
        }
    }

    #[test]
    fn drain_frames_切帧并保留不完整尾部() {
        use std::sync::mpsc;
        // 两个完整访问单元(SPS+PPS+IDR / 非关键帧 slice)+ 一个不完整尾部起始码。
        let mut buf: Vec<u8> = Vec::new();
        // 帧1:SPS(7) PPS(8) IDR(5)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00]);
        buf.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]);
        buf.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x88, 0x11, 0x22]);
        // 帧2:非关键 slice(1)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x80, 0x44]);
        // 不完整尾部:又一个 slice 起点(应保留等后续数据)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x80]);
        let (tx, rx) = mpsc::channel::<Frame>();
        drain_frames(&mut buf, &tx);
        drop(tx);
        let frames: Vec<Frame> = rx.into_iter().collect();
        // 帧1(含 IDR)为关键帧,帧2 非关键;第三个不完整不发。
        assert_eq!(frames.len(), 2, "应切出 2 个完整帧");
        assert!(frames[0].key_frame, "SPS/PPS/IDR 帧应为关键帧");
        assert!(!frames[1].key_frame);
        // buf 应保留最后一个不完整帧(起始码 + 数据;3/4 字节起始码重叠使保留 5-6 字节均可)。
        assert!(
            buf.len() >= 5 && buf.len() <= 6,
            "保留不完整尾部,实际 {}",
            buf.len()
        );
        // 尾部应以起始码开头(下一轮拼接后可继续切帧)。
        assert!(buf.starts_with(&[0, 0, 1]) || buf.starts_with(&[0, 0, 0, 1]));
    }

    /// 造一个含 SPS/PPS/IDR + 非关键帧的 Annex B 流。
    fn sample_h264() -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 1, 0x67, 0xAA]); // SPS(7)
        v.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xBB]); // PPS(8)
        v.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x11]); // IDR slice(5)
        v.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x22]); // 非 IDR slice(1)
        v
    }

    #[test]
    fn 切帧_关键帧识别() {
        let src = FileSource::from_bytes(&sample_h264()).unwrap();
        assert_eq!(src.len(), 2); // IDR 帧(含SPS/PPS/IDR)+ 非关键帧
        let mut s = src;
        let f1 = s.next_frame().unwrap();
        assert!(f1.key_frame);
        let f2 = s.next_frame().unwrap();
        assert!(!f2.key_frame);
    }

    #[test]
    fn 文件源循环() {
        let mut s = FileSource::from_bytes(&sample_h264()).unwrap();
        let total = s.len();
        // 取 2*total+1 帧不 panic,且回到起点。
        for _ in 0..(total * 2 + 1) {
            assert!(s.next_frame().is_some());
        }
    }

    #[test]
    fn 有限文件源末帧后结束且循环构造不回归() {
        let mut once = FileSource::from_bytes_once(&sample_h264()).unwrap();
        let total = once.len();
        for _ in 0..total {
            assert!(once.next_frame().is_some());
        }
        assert!(once.next_frame().is_none());

        let mut looping = FileSource::from_bytes(&sample_h264()).unwrap();
        for _ in 0..(looping.len() * 2 + 1) {
            assert!(looping.next_frame().is_some());
        }
    }

    #[tokio::test]
    async fn shared_media只在有完整参数集时提供抓拍帧() {
        let empty = start_shared_media(Box::new(NoneSource), 25);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(empty.latest_config_frame().is_none());

        let source = FileSource::from_bytes(&sample_h264()).unwrap();
        let media = start_shared_media(Box::new(source), 25);
        for _ in 0..100 {
            if let Some((frame, codec)) = media.latest_config_frame() {
                assert!(frame.key_frame);
                assert!(crate::pusher::contains_codec_config(&frame.data, codec));
                media.stop();
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("共享媒体未保存可解码关键帧");
    }

    struct ErrorAtSharedMediaEof {
        timed: bool,
        error: Option<String>,
    }

    impl VideoSource for ErrorAtSharedMediaEof {
        fn next_frame(&mut self) -> Option<Frame> {
            if self.timed {
                None
            } else {
                self.error = Some("legacy source read failed".into());
                None
            }
        }

        fn supports_timed_events(&self) -> bool {
            self.timed
        }

        fn next_media_event(&mut self) -> Option<MediaEvent> {
            if self.timed {
                self.error = Some("timed source read failed".into());
            }
            None
        }

        fn is_live(&self) -> bool {
            false
        }

        fn take_error(&mut self) -> Option<String> {
            self.error.take()
        }
    }

    #[tokio::test]
    async fn shared_media有限源末次读取错误对订阅侧可见() {
        for timed in [true, false] {
            let media =
                start_shared_media(Box::new(ErrorAtSharedMediaEof { timed, error: None }), 25);
            for _ in 0..100 {
                if !media.is_alive() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let error = media.capture_error().expect("共享媒体应复制源末次读取错误");
            assert_eq!(
                error,
                if timed {
                    "timed source read failed"
                } else {
                    "legacy source read failed"
                }
            );
            media.stop();
        }
    }

    #[test]
    fn 空源不产帧() {
        let mut n = NoneSource;
        assert!(n.next_frame().is_none());
    }

    #[test]
    fn 空流报错() {
        assert!(FileSource::from_bytes(&[0xFF, 0xFF]).is_err());
    }

    #[test]
    fn annexb_起始码识别() {
        assert!(starts_with_annexb(&[0, 0, 1, 0x67]));
        assert!(starts_with_annexb(&[0, 0, 0, 1, 0x67]));
        assert!(!starts_with_annexb(&[
            0x00, 0x00, 0x00, 0x18, b'f', b't', b'y', b'p'
        ])); // MP4 ftyp box
        assert!(!starts_with_annexb(&[]));
    }

    // 容器转封装依赖系统 ffmpeg,默认忽略;需要时:
    //   ffmpeg -y -f lavfi -i testsrc=duration=2:size=320x240:rate=25 \
    //     -c:v libx264 -preset ultrafast /tmp/uvp_test.mp4
    //   cargo test -p media-rtp -- --ignored mp4_转封装
    #[test]
    #[ignore = "需系统 ffmpeg + /tmp/uvp_test.mp4"]
    fn mp4_转封装并切帧() {
        let mut s = FileSource::from_path("/tmp/uvp_test.mp4").expect("MP4 应能转封装加载");
        let mut n = 0;
        while let Some(f) = s.next_frame() {
            assert!(!f.data.is_empty());
            n += 1;
            if n > 5 {
                break;
            }
        }
        assert!(n > 0, "应从 MP4 切出帧");
    }

    #[test]
    #[ignore = "需系统 ffmpeg + ffprobe；验证历史 MP4 的源 PTS 与 AAC 音频事件"]
    fn 历史MP4按源PTS保留AAC事件() {
        use std::process::Command;

        let ffmpeg = ffmpeg_bin().expect("需要 ffmpeg");
        assert!(history_ffprobe_bin().is_some(), "需要 ffprobe");
        let path = std::env::temp_dir().join(format!(
            "uvp-history-timed-{}-{}.mp4",
            now_ms(),
            rand::random::<u32>()
        ));
        let output = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=5:duration=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=8000:duration=1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-bf",
                "0",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "32k",
                "-ar",
                "8000",
                "-ac",
                "1",
                "-shortest",
            ])
            .arg(&path)
            .output()
            .expect("启动 ffmpeg 失败");
        assert!(
            output.status.success(),
            "生成历史 MP4 失败: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        let mut source = FileSource::from_paths_once(std::slice::from_ref(&path))
            .expect("历史 MP4 应能按自身 PTS 加载");
        assert!(source.supports_timed_events());
        assert!(source.has_audio());
        assert_eq!(source.audio_codec(), crate::ps::AudioCodec::Aac);
        assert_eq!(source.audio_sample_rate_hz(), 8_000);

        let mut video_pts = Vec::new();
        let mut audio = Vec::new();
        while let Some(event) = source.next_media_event() {
            match event {
                MediaEvent::Video(video) => video_pts.push((video.pts_90k, video.duration_90k)),
                MediaEvent::Audio(audio_au) => audio.push(audio_au),
                MediaEvent::Discontinuity { .. } => unreachable!("历史 MP4 不应产生断点事件"),
            }
        }
        assert_eq!(video_pts.len(), 5, "5fps*1s 应有 5 个视频访问单元");
        assert!(video_pts
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 == 18_000));
        assert!(video_pts.iter().all(|(_, duration)| *duration == 18_000));
        assert!(audio.len() >= 8, "1s/1024@8k 至少应有 8 个 AAC 包");
        assert_eq!(audio[0].pts_90k, 0, "音频 priming PTS 归一化后应从零开始");
        assert!(audio.iter().all(|audio| {
            audio.codec == crate::ps::AudioCodec::Aac
                && audio.sample_rate_hz == 8_000
                && (1..=1_024).contains(&audio.sample_count)
                && audio.duration_90k
                    == crate::ps::AudioCodec::duration_90k_for_samples(
                        audio.codec,
                        audio.sample_rate_hz,
                        audio.sample_count,
                    )
                && !audio.data.is_empty()
        }));
        assert!(audio
            .windows(2)
            .all(|pair| pair[1].pts_90k - pair[0].pts_90k == 11_520));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn 轻量源按码率产帧() {
        // 200kbps @ 25fps → 每帧 200*1000/8/25 = 1000 字节。
        let mut s = LightSource::new(200, 25);
        let f0 = s.next_frame().unwrap();
        assert!(f0.key_frame); // 首帧为关键帧
        assert_eq!(f0.data.len(), 1000 + 5); // 起始码+NAL头+填充
        assert_eq!(&f0.data[..5], &[0, 0, 0, 1, 0x65]); // IDR
        let f1 = s.next_frame().unwrap();
        assert!(!f1.key_frame); // 第二帧非关键帧
        assert_eq!(&f1.data[..5], &[0, 0, 0, 1, 0x61]); // slice
    }

    #[test]
    fn 轻量源最小帧下限() {
        let mut s = LightSource::new(1, 25); // 极低码率 → 命中 64 字节下限
        let f = s.next_frame().unwrap();
        assert_eq!(f.data.len(), 64 + 5);
    }

    #[test]
    fn 定时音频访问单元按采样数换算90k时钟() {
        let aac_8k =
            TimedAudioAu::from_samples(vec![0x01], crate::ps::AudioCodec::Aac, 8_000, 1_024, 0);
        let aac_16k =
            TimedAudioAu::from_samples(vec![0x01], crate::ps::AudioCodec::Aac, 16_000, 1_024, 0);
        let g711 = TimedAudioAu::from_samples(
            vec![0x01; 160],
            crate::ps::AudioCodec::G711A,
            8_000,
            160,
            0,
        );
        assert_eq!(aac_8k.duration_90k, 11_520);
        assert_eq!(aac_16k.duration_90k, 5_760);
        assert_eq!(g711.duration_90k, 1_800);
    }

    #[test]
    fn 非整除采样率用余数累计避免长期漂移() {
        let mut clock = SampleClock::new(44_100).unwrap();
        let mut ticks = 0_u64;
        for _ in 0..(44_100 / 1_024) {
            ticks += clock.duration_for_samples(1_024);
        }
        ticks += clock.duration_for_samples(44_100 % 1_024);
        assert_eq!(ticks, 90_000);
        assert!(clock.remainder() < 44_100);
    }

    #[test]
    fn 虚拟时钟连续十分钟累计时间不漂移() {
        let mut video_clock = LegacyMediaClock::new(29, crate::ps::AudioCodec::G711A, 8_000);
        let mut video_ticks = 0_u64;
        for _ in 0..(29 * 600) {
            video_ticks += video_clock
                .video(Vec::new(), crate::ps::VideoCodec::H264, false)
                .duration_90k;
        }
        assert_eq!(video_ticks, 54_000_000);

        let mut audio_clock = SampleClock::new(44_100).unwrap();
        let mut audio_ticks = 0_u64;
        let total_samples = 44_100_u64 * 600;
        let full_aus = total_samples / 1_024;
        for _ in 0..full_aus {
            audio_ticks += audio_clock.duration_for_samples(1_024);
        }
        audio_ticks += audio_clock.duration_for_samples((total_samples % 1_024) as u32);
        assert_eq!(audio_ticks, 54_000_000);
    }

    struct TimedEventsSource {
        events: std::collections::VecDeque<MediaEvent>,
    }

    impl VideoSource for TimedEventsSource {
        fn next_frame(&mut self) -> Option<Frame> {
            None
        }

        fn supports_timed_events(&self) -> bool {
            true
        }

        fn next_media_event(&mut self) -> Option<MediaEvent> {
            self.events.pop_front()
        }

        fn is_live(&self) -> bool {
            false
        }

        fn has_audio(&self) -> bool {
            true
        }

        fn audio_codec(&self) -> crate::ps::AudioCodec {
            crate::ps::AudioCodec::G711A
        }
    }

    #[test]
    fn 事件总线新订阅者只缓存视频关键帧而不重放旧音频() {
        let events = std::collections::VecDeque::from([
            MediaEvent::Video(TimedVideoAu::new(
                vec![
                    0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3,
                ],
                crate::ps::VideoCodec::H264,
                true,
                90_000,
                3_600,
            )),
            MediaEvent::Audio(TimedAudioAu::from_samples(
                vec![0xD5; 160],
                crate::ps::AudioCodec::G711A,
                8_000,
                160,
                90_000,
            )),
        ]);
        let media = start_shared_media(Box::new(TimedEventsSource { events }), 25);
        for _ in 0..100 {
            if media.latest_config_frame().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        for _ in 0..100 {
            if !media.is_alive() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut subscriber = media.subscribe();
        let first = subscriber
            .next_media_event()
            .expect("新订阅者应从参数集关键帧开始");
        assert!(matches!(first, MediaEvent::Video(ref video) if video.pts_90k == 90_000));
        assert!(subscriber.next_media_event().is_none());
        media.stop();
    }

    /// 回归测试:缓冲区末尾以 IDR 起始码结束时,SPS+PPS 不得被单独发出。
    /// 旧实现会把 SPS+PPS 作为"关键帧"提前发出,导致平台收到 332/332 无效头部碎片关连接。
    #[test]
    fn drain_frames_缓冲末尾为_idr_起始码时_sps_pps_不单独发出() {
        use std::sync::mpsc;
        let mut buf: Vec<u8> = Vec::new();
        buf.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00]); // SPS(7)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]); // PPS(8)
        buf.extend_from_slice(&[0, 0, 0, 1]); // IDR 起始码(数据未到)
        let (tx, rx) = mpsc::channel::<Frame>();
        drain_frames(&mut buf, &tx);
        drop(tx);
        let frames: Vec<Frame> = rx.into_iter().collect();
        assert_eq!(
            frames.len(),
            0,
            "SPS+PPS 不构成完整 AU,不应发出,实际发出 {} 帧",
            frames.len()
        );
        assert!(!buf.is_empty(), "SPS+PPS+IDR起始码应保留在 buf");
    }

    #[test]
    fn drain_frames_重复参数集必须归入后续_idr() {
        use std::sync::mpsc;

        let mut buf = Vec::new();
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x80, 0x11]); // 前一张 P 图
        buf.extend_from_slice(&[0, 0, 0, 1, 0x67, 0x42, 0x00]); // 下一 GOP 的 SPS
        buf.extend_from_slice(&[0, 0, 0, 1, 0x68, 0xce]); // 下一 GOP 的 PPS
        buf.extend_from_slice(&[0, 0, 0, 1, 0x65, 0x80, 0x22]); // 下一 GOP 的 IDR
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x80, 0x33]); // 下一张 P 图，闭合 IDR AU
        let (tx, rx) = mpsc::channel::<Frame>();

        drain_frames(&mut buf, &tx);
        drop(tx);
        let frames: Vec<Frame> = rx.into_iter().collect();

        assert_eq!(frames.len(), 2);
        assert!(!frames[0].key_frame, "SPS/PPS 不得粘到前一张 P 图");
        assert!(
            frames[1].key_frame,
            "SPS/PPS/IDR 应组成可重启 decoder 的 AU"
        );
        let has_nal = |kind| {
            frames[1]
                .data
                .windows(4)
                .any(|nal| nal[..3] == [0, 0, 1] && nal[3] & 0x1f == kind)
                || frames[1]
                    .data
                    .windows(5)
                    .any(|nal| nal[..4] == [0, 0, 0, 1] && nal[4] & 0x1f == kind)
        };
        assert!(has_nal(7));
        assert!(has_nal(8));
        assert!(has_nal(5));
    }
}

#[cfg(any())]
#[allow(clippy::items_after_test_module)]
fn chrono_str() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let hours = (secs / 3600 % 24) + 8; // UTC+8
    let mins = secs / 60 % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", hours % 24, mins, s)
}

/// 绘制高画质动态测试卡与物理运动画面(用于受限/无界面环境下稳定输出 H.264 视频流)
#[cfg(any())]
#[allow(clippy::items_after_test_module)]
fn render_dynamic_testcard(buf: &mut [u8], width: usize, height: usize, seq: u64, _now_str: &str) {
    let t = seq as f64 * 0.05;
    let cx = (width as f64 * 0.5 + (t * 2.0).cos() * (width as f64 * 0.35)) as usize;
    let cy = (height as f64 * 0.5 + (t * 1.5).sin() * (height as f64 * 0.35)) as usize;
    let radius = 60usize;

    for y in 0..height {
        let r_val = ((y as f64 / height as f64 + (t * 0.3).sin()) * 128.0 + 64.0) as u8;
        for x in 0..width {
            let g_val = ((x as f64 / width as f64 + (t * 0.5).cos()) * 128.0 + 64.0) as u8;
            let b_val = (((x + y) as f64 / (width + height) as f64) * 200.0) as u8;

            let idx = (y * width + x) * 3;
            let dx = x.abs_diff(cx);
            let dy = y.abs_diff(cy);
            if dx * dx + dy * dy <= radius * radius {
                buf[idx] = 255;
                buf[idx + 1] = 230;
                buf[idx + 2] = 50;
            } else {
                buf[idx] = r_val;
                buf[idx + 1] = g_val;
                buf[idx + 2] = b_val;
            }
        }
    }
}
