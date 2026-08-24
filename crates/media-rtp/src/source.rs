//! 视频源抽象(可插拔)。
//!
//! 对应压测三档媒体强度(docs/10-functional/stress-testing.md#2):
//! - `NoneSource`:空媒体(不产帧)—— A 档
//! - `LightSource`:合成低码率伪包 —— B 档(测 RTP 通道/带宽,内容不要求可解码)
//! - `FileSource`:H.264 文件循环 —— C 档 / 单设备联调

use common::{Error, Result};

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
}

/// 注册期唯一采集源的帧总线。预览和平台点播各自订阅，任何慢消费者都不会阻塞采集。
pub struct SharedMedia {
    frames: tokio::sync::broadcast::Sender<SharedFrame>,
    /// 最近一帧同时包含参数集和关键图像的访问单元。
    /// 新订阅者必须从它开始，否则延迟加入的 FFmpeg/RTP 解码器只有 IDR、没有 SPS/PPS。
    latest_config_keyframe: std::sync::Mutex<Option<SharedFrame>>,
    stop: std::sync::atomic::AtomicBool,
    alive: std::sync::atomic::AtomicBool,
    has_audio: bool,
    audio_codec: crate::ps::AudioCodec,
    video_codec: crate::ps::VideoCodec,
    error: std::sync::Mutex<Option<String>>,
    frames_seen: std::sync::atomic::AtomicU64,
    seek_permille_plus1: std::sync::atomic::AtomicU32,
}

#[derive(Clone)]
struct SharedFrame {
    frame: Frame,
    audio: Vec<Vec<u8>>,
}

/// 从一个源启动唯一采集线程。实时源在设备注册期间持续运行，直到调用 [`SharedMedia::stop`]。
pub fn start_shared_media(
    mut source: Box<dyn VideoSource>,
    fps: u32,
    preview: Option<std::sync::Arc<dyn crate::pusher::PreviewSink>>,
) -> std::sync::Arc<SharedMedia> {
    let fps = fps.max(1);
    let (frames, _) = tokio::sync::broadcast::channel((fps * 4).max(32) as usize);
    let media = std::sync::Arc::new(SharedMedia {
        frames,
        latest_config_keyframe: std::sync::Mutex::new(None),
        stop: std::sync::atomic::AtomicBool::new(false),
        alive: std::sync::atomic::AtomicBool::new(true),
        has_audio: source.has_audio(),
        audio_codec: source.audio_codec(),
        video_codec: source.codec(),
        error: std::sync::Mutex::new(None),
        frames_seen: std::sync::atomic::AtomicU64::new(0),
        seek_permille_plus1: std::sync::atomic::AtomicU32::new(0),
    });
    let producer = std::sync::Arc::clone(&media);
    std::thread::spawn(move || {
        let interval = std::time::Duration::from_micros(1_000_000 / u64::from(fps));
        let mut pts_90k = 0_u64;
        let session_id = crate::pusher::next_preview_session_id();
        let mut sequence = 0_u64;
        let pts_step = u64::from(crate::rtp::CLOCK_HZ / fps);
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
            if let Some(frame) = source.next_frame() {
                let audio = source.next_audio();
                let shared_frame = SharedFrame { frame, audio };
                producer
                    .frames_seen
                    .fetch_add(1, std::sync::atomic::Ordering::Release);
                if shared_frame.frame.key_frame
                    && crate::pusher::contains_codec_config(
                        &shared_frame.frame.data,
                        producer.video_codec,
                    )
                {
                    *producer.latest_config_keyframe.lock().unwrap() = Some(shared_frame.clone());
                }
                if let Some(sink) = &preview {
                    sequence = sequence.saturating_add(1);
                    sink.publish(crate::pusher::PreviewPacket {
                        data: shared_frame.frame.data.clone(),
                        key_frame: shared_frame.frame.key_frame,
                        codec: producer.video_codec,
                        session_id,
                        sequence,
                        fps,
                        pts_90k,
                        captured_at_ms: now_ms(),
                    });
                }
                let _ = producer.frames.send(shared_frame);
                pts_90k = pts_90k.wrapping_add(pts_step);
            } else if !source.is_live() {
                break;
            }
            if let Some(remaining) = interval.checked_sub(tick.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
        producer
            .alive
            .store(false, std::sync::atomic::Ordering::Release);
        if let Some(sink) = preview {
            sink.stopped();
        }
    });
    media
}

impl SharedMedia {
    /// 为一路 RTP 推流建立独立消费者；发生积压时从下一关键帧恢复。
    pub fn subscribe(self: &std::sync::Arc<Self>) -> SharedVideoSource {
        SharedVideoSource {
            media: std::sync::Arc::clone(self),
            frames: self.frames.subscribe(),
            pending_audio: Vec::new(),
            bootstrap: self.latest_config_keyframe.lock().unwrap().clone(),
            need_key_frame: true,
        }
    }

    /// 设备注销时停止唯一采集线程并释放底层摄像头/屏幕句柄。
    pub fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
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

    fn seek(&self, permille: u32) {
        self.seek_permille_plus1
            .store(permille.min(1000) + 1, std::sync::atomic::Ordering::Release);
    }
}

impl Drop for SharedMedia {
    fn drop(&mut self) {
        self.stop();
    }
}

/// [`SharedMedia`] 的单路 RTP 读取端。
pub struct SharedVideoSource {
    media: std::sync::Arc<SharedMedia>,
    frames: tokio::sync::broadcast::Receiver<SharedFrame>,
    pending_audio: Vec<Vec<u8>>,
    bootstrap: Option<SharedFrame>,
    need_key_frame: bool,
}

impl VideoSource for SharedVideoSource {
    fn next_frame(&mut self) -> Option<Frame> {
        loop {
            if let Some(packet) = self.bootstrap.take() {
                self.need_key_frame = false;
                self.pending_audio = packet.audio;
                return Some(packet.frame);
            }
            match self.frames.try_recv() {
                Ok(packet) => {
                    if self.need_key_frame && !packet.frame.key_frame {
                        continue;
                    }
                    self.need_key_frame = false;
                    self.pending_audio = packet.audio;
                    return Some(packet.frame);
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                    self.need_key_frame = true;
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
                | Err(tokio::sync::broadcast::error::TryRecvError::Closed) => return None,
            }
        }
    }

    fn next_audio(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.pending_audio)
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

fn now_ms() -> u64 {
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

impl VideoSource for FileSource {
    fn next_frame(&mut self) -> Option<Frame> {
        if self.frames.is_empty() {
            return None;
        }
        let f = self.frames[self.cursor].clone();
        self.cursor = (self.cursor + 1) % self.frames.len(); // 循环
        Some(f)
    }

    fn has_audio(&self) -> bool {
        !self.audio.is_empty()
    }

    fn seek(&mut self, permille: u32) {
        if self.frames.is_empty() {
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

    fn codec(&self) -> crate::ps::VideoCodec {
        self.codec
    }
}

/// 从 NAL 流探测视频编码。H.265 存在 VPS(nal_type=32),H.264 无此类型;
/// 以此区分。NAL 头首字节 = bytes[3](起始码后),H.265 type=(b>>1)&0x3F。
fn detect_codec(nals: &[Nal<'_>]) -> crate::ps::VideoCodec {
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

trait FrameSender {
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
fn drain_frames<S: FrameSender>(buf: &mut Vec<u8>, tx: &S) {
    // 找所有起始码位置。
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    if starts.len() < 2 {
        return; // 不足两个起始码,等更多数据
    }

    let last = *starts.last().unwrap();
    let complete = &buf[..last];
    let mut positions: Vec<usize> = starts.iter().copied().filter(|&p| p < last).collect();
    positions.push(last);

    let mut cur: Vec<u8> = Vec::new();
    let mut cur_key = false;
    let mut has_vcl = false;
    // 记录最后一次成功发送帧之后的 buf 位置,只 drain 到这里。
    let mut drained_to: usize = 0;

    for w in positions.windows(2) {
        let nal = &complete[w[0]..w[1]];
        let hdr_off = if nal.len() >= 4 && nal[2] == 0 { 4 } else { 3 };
        let nal_type = nal.get(hdr_off).map(|b| b & 0x1f).unwrap_or(0);
        let is_vcl = nal_type == 1 || nal_type == 5;
        if is_vcl && has_vcl {
            // 当前积累的 AU 含 VCL,可以安全发出。
            tx.send_frame(Frame {
                data: std::mem::take(&mut cur),
                key_frame: cur_key,
            });
            cur_key = false;
            has_vcl = false;
            // 已发送到 w[0](下一 NAL 的起始位置)之前的所有数据。
            drained_to = w[0];
        }
        if nal_type == 5 || nal_type == 7 || nal_type == 8 {
            cur_key = true;
        }
        if is_vcl {
            has_vcl = true;
        }
        cur.extend_from_slice(nal);
    }

    // 只有 cur 包含 VCL 才发尾部帧;纯 SPS+PPS（等待 IDR）保留在 buf 中。
    if has_vcl && !cur.is_empty() {
        tx.send_frame(Frame {
            data: cur,
            key_frame: cur_key,
        });
        drained_to = last;
    }

    // 只 drain 已确实发出的部分,未发出的 SPS+PPS 保留供下次拼接。
    if drained_to > 0 {
        buf.drain(..drained_to);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PreviewCollector(std::sync::Mutex<Vec<crate::pusher::PreviewPacket>>);

    impl crate::pusher::PreviewSink for PreviewCollector {
        fn publish(&self, packet: crate::pusher::PreviewPacket) {
            self.0.lock().unwrap().push(packet);
        }
    }

    struct SyntheticLive {
        sequence: u8,
    }

    impl VideoSource for SyntheticLive {
        fn next_frame(&mut self) -> Option<Frame> {
            self.sequence = self.sequence.wrapping_add(1);
            Some(Frame {
                data: vec![0, 0, 0, 1, 0x65, self.sequence],
                key_frame: self.sequence % 5 == 1,
            })
        }

        fn is_live(&self) -> bool {
            true
        }
    }

    struct ConfigThenFrames {
        sequence: u8,
    }

    impl VideoSource for ConfigThenFrames {
        fn next_frame(&mut self) -> Option<Frame> {
            self.sequence = self.sequence.wrapping_add(1);
            let data = if self.sequence == 1 {
                vec![
                    0,
                    0,
                    0,
                    1,
                    0x67,
                    0x42,
                    0x00, // SPS
                    0,
                    0,
                    0,
                    1,
                    0x68,
                    0xce, // PPS
                    0,
                    0,
                    0,
                    1,
                    0x65,
                    self.sequence, // IDR
                ]
            } else {
                vec![0, 0, 0, 1, 0x65, self.sequence]
            };
            Some(Frame {
                data,
                key_frame: true,
            })
        }

        fn is_live(&self) -> bool {
            true
        }
    }

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
    fn 注册期共享媒体_预览先收到帧_点播订阅仍能收到同一流() {
        let preview = std::sync::Arc::new(PreviewCollector(std::sync::Mutex::new(Vec::new())));
        let media = start_shared_media(
            Box::new(SyntheticLive { sequence: 0 }),
            100,
            Some(preview.clone() as std::sync::Arc<dyn crate::pusher::PreviewSink>),
        );
        let mut rtp_source = media.subscribe();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        let mut first_rtp = None;
        while std::time::Instant::now() < deadline {
            if let Some(frame) = rtp_source.next_frame() {
                first_rtp = Some(frame);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        media.stop();
        assert!(first_rtp.is_some(), "点播订阅应能从注册期共享源收到帧");
        assert!(
            !preview.0.lock().unwrap().is_empty(),
            "注册期预览应先收到编码帧"
        );
    }

    #[test]
    fn 延迟点播订阅_首帧使用最近的参数关键帧() {
        let preview = std::sync::Arc::new(PreviewCollector(std::sync::Mutex::new(Vec::new())));
        let media = start_shared_media(
            Box::new(ConfigThenFrames { sequence: 0 }),
            200,
            Some(preview.clone() as std::sync::Arc<dyn crate::pusher::PreviewSink>),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while preview.0.lock().unwrap().len() < 5 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut rtp_source = media.subscribe();
        let first = rtp_source
            .next_frame()
            .expect("延迟订阅应立即得到 bootstrap 参数关键帧");
        let packet = crate::pusher::PreviewPacket {
            data: first.data,
            key_frame: first.key_frame,
            codec: crate::ps::VideoCodec::H264,
            session_id: 1,
            sequence: 1,
            fps: 200,
            pts_90k: 0,
            captured_at_ms: 0,
        };
        assert!(
            crate::pusher::is_config_keyframe(&packet),
            "延迟订阅的首帧必须包含 SPS/PPS/IDR"
        );
        media.stop();
    }

    #[test]
    fn 参数集识别_支持_h264_和_h265() {
        let h264 = crate::pusher::PreviewPacket {
            data: vec![0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3],
            key_frame: true,
            codec: crate::ps::VideoCodec::H264,
            session_id: 1,
            sequence: 1,
            fps: 25,
            pts_90k: 0,
            captured_at_ms: 0,
        };
        let h265 = crate::pusher::PreviewPacket {
            data: vec![
                0, 0, 1, 0x40, 1, // VPS (type 32)
                0, 0, 1, 0x42, 1, // SPS (type 33)
                0, 0, 1, 0x44, 1, // PPS (type 34)
            ],
            key_frame: true,
            codec: crate::ps::VideoCodec::H265,
            session_id: 1,
            sequence: 1,
            fps: 25,
            pts_90k: 0,
            captured_at_ms: 0,
        };
        assert!(crate::pusher::is_config_keyframe(&h264));
        assert!(crate::pusher::is_config_keyframe(&h265));
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
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x33, 0x44]);
        // 不完整尾部:又一个 slice 起点(应保留等后续数据)
        buf.extend_from_slice(&[0, 0, 0, 1, 0x61, 0x55]);
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
