//! OSD 水印：本地样式配置模型 + FFmpeg 滤镜生成。
//!
//! 只有经 FFmpeg 实时编码的采集路径(Windows 摄像头/屏幕、macOS 摄像头)会把
//! OSD 烧进码流；macOS 屏幕走 ScreenCaptureKit + OpenH264，文件循环与压测
//! 不重新编码，画面中不带 OSD。
//!
//! 样式规则对齐移动端(uvp-gb28181-sim `OsdTextPass`/`IosOsdLayout`)：
//! - 字号 = max(16, 画面高 × 0.035/0.050/0.070)
//! - 默认锚点：时间左上、通道名右上，边距 6px
//! - 水印整屏斜铺，旋转 30°，透明度 0.28，横向间距 1.5 倍文字宽，纵向步长 4 倍字高
//! - 遮挡黑框最后绘制，盖住 OSD 文字
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 文字最多字符数(与 GB28181 OSDConfig/Item/Text 上限一致)。
pub const OSD_TEXT_MAX_CHARS: usize = 32;
/// 平台下发的 OSD 文字行上限。
pub const OSD_ITEMS_MAX: usize = 8;
/// 遮挡区域上限。
pub const OSD_MASKS_MAX: usize = 4;
const MARGIN_PX: u32 = 6;
const WATERMARK_ALPHA: &str = "0.28";

/// 字号档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OsdSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl OsdSize {
    /// 按画面高度换算像素字号。
    pub fn pixel_size(self, frame_height: u32) -> u32 {
        let ratio = match self {
            Self::Small => 0.035,
            Self::Medium => 0.050,
            Self::Large => 0.070,
        };
        ((f64::from(frame_height) * ratio).floor() as u32).max(16)
    }
}

/// 一层 OSD 的开关与样式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OsdLayer {
    pub enabled: bool,
    /// 仅水印层使用；时间层由时钟生成，通道名层取设备通道名。
    pub text: String,
    pub size: OsdSize,
    /// `#RRGGBB`
    pub fill_color: String,
    /// `#RRGGBB`
    pub outline_color: String,
}

impl Default for OsdLayer {
    fn default() -> Self {
        Self {
            enabled: true,
            text: String::new(),
            size: OsdSize::Medium,
            fill_color: "#FFFFFF".into(),
            outline_color: "#000000".into(),
        }
    }
}
/// 桌面端本地 OSD 配置(三层，对齐移动端 `OsdConfig`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OsdSettings {
    pub timestamp: OsdLayer,
    pub channel_name: OsdLayer,
    pub watermark: OsdLayer,
}

impl Default for OsdSettings {
    fn default() -> Self {
        Self {
            timestamp: OsdLayer::default(),
            channel_name: OsdLayer::default(),
            watermark: OsdLayer {
                enabled: false,
                ..OsdLayer::default()
            },
        }
    }
}

impl OsdSettings {
    /// 校验颜色格式与文字长度；不合法时返回中文原因。
    pub fn validate(&self) -> Result<(), String> {
        for (label, layer) in [
            ("时间", &self.timestamp),
            ("通道名", &self.channel_name),
            ("水印", &self.watermark),
        ] {
            if parse_hex_color(&layer.fill_color).is_none() {
                return Err(format!("{label}填充色必须是 #RRGGBB"));
            }
            if parse_hex_color(&layer.outline_color).is_none() {
                return Err(format!("{label}描边色必须是 #RRGGBB"));
            }
            if layer.text.chars().count() > OSD_TEXT_MAX_CHARS {
                return Err(format!("{label}文字不能超过 {OSD_TEXT_MAX_CHARS} 个字符"));
            }
        }
        if self.watermark.enabled && self.watermark.text.trim().is_empty() {
            return Err("开启水印时必须填写水印文字".into());
        }
        Ok(())
    }
}

/// `#RRGGBB` → `0xRRGGBB`(FFmpeg 颜色写法)。
pub fn parse_hex_color(value: &str) -> Option<String> {
    let hex = value.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| format!("0x{}", hex.to_ascii_uppercase()))
}

/// 时间显示格式(GB28181 OSDConfig/TimeType)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OsdTimeFormat {
    /// `YYYY-MM-DD HH:MM:SS`(TimeType=0 或缺省)
    #[default]
    Dash,
    /// `YYYY年MM月DD日HH:MM:SS`(TimeType=1)
    Chinese,
}

/// 文字位置。坐标都在 [`OsdOverlay::base_width`]×[`OsdOverlay::base_height`] 坐标系内。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsdAnchor {
    /// 左上角像素坐标(平台 OSDConfig 的 X/Y)。
    Pixel { x: u32, y: u32 },
    /// 右上角，边距 6px(本地默认通道名位置)。
    TopRight,
}

/// 已解析好的一段文字。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsdText {
    pub text: String,
    pub anchor: OsdAnchor,
    pub style: OsdLayer,
}
/// 时间层。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsdTime {
    pub anchor: OsdAnchor,
    pub format: OsdTimeFormat,
    pub style: OsdLayer,
}

/// 遮挡矩形(左上 + 右下)，坐标系同 [`OsdOverlay`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsdRect {
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// 一次采集要烧进画面的全部内容(本地样式 + 平台坐标 + 遮挡已合并)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsdOverlay {
    /// 坐标系宽(平台 OSDConfig/Length 或输出宽)。
    pub base_width: u32,
    /// 坐标系高(平台 OSDConfig/Width 或输出高)。
    pub base_height: u32,
    pub time: Option<OsdTime>,
    pub texts: Vec<OsdText>,
    /// 斜铺水印，固定铺满整屏。
    pub watermark: Option<OsdText>,
    /// 遮挡坐标系(通常为媒体分辨率)，与文字坐标系可能不同。
    pub mask_base: (u32, u32),
    pub masks: Vec<OsdRect>,
}

impl OsdOverlay {
    /// 没有任何需要绘制的内容。
    pub fn is_empty(&self) -> bool {
        self.time.is_none()
            && self.texts.is_empty()
            && self.watermark.is_none()
            && self.masks.is_empty()
    }
}

/// 查找支持中文的系统字体。`UVP_OSD_FONT` 优先。
pub fn font_file() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("UVP_OSD_FONT") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = Vec::<PathBuf>::new();
    if cfg!(windows) {
        let root = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
        for name in ["msyh.ttc", "simhei.ttf", "simsun.ttc"] {
            candidates.push(PathBuf::from(&root).join("Fonts").join(name));
        }
    } else if cfg!(target_os = "macos") {
        candidates.push("/System/Library/Fonts/PingFang.ttc".into());
        candidates.push("/System/Library/Fonts/STHeiti Medium.ttc".into());
    } else {
        candidates.push("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc".into());
        candidates.push("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc".into());
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// 检查 FFmpeg 是否带 drawtext(需要 libfreetype)和 drawbox 滤镜。结果按路径缓存。
pub fn ffmpeg_supports_osd(ffmpeg: &str) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(value) = cache.lock().ok().and_then(|map| map.get(ffmpeg).copied()) {
        return value;
    }
    let supported = common::hidden_command(ffmpeg)
        .args(["-hide_banner", "-filters"])
        .output()
        .map(|out| filters_support_osd(&String::from_utf8_lossy(&out.stdout)))
        .unwrap_or(false);
    if let Ok(mut map) = cache.lock() {
        map.insert(ffmpeg.to_string(), supported);
    }
    supported
}

fn filters_support_osd(listing: &str) -> bool {
    let has = |name: &str| {
        listing
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(name))
    };
    has("drawtext") && has("drawbox")
}
fn escape_chars(value: &str, special: &[char]) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if special.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// 两级转义：先按滤镜选项(`:` 分隔)，再按滤镜图(`,;[]`)。参数直接传给进程，不经 shell。
fn escape_value(value: &str) -> String {
    let option = escape_chars(value, &['\\', '\'', ':']);
    escape_chars(&option, &['\\', '\'', '[', ']', ',', ';'])
}

fn time_expansion(format: OsdTimeFormat) -> &'static str {
    // drawtext 函数参数以 `:` 分隔，strftime 格式里的冒号必须写成 `\:`。
    match format {
        OsdTimeFormat::Dash => r"%{localtime:%Y-%m-%d %H\:%M\:%S}",
        // Windows CRT strftime 遇到格式串里的非 ASCII 字节会整体输出空串，
        // 中文分隔字必须放在 localtime 调用之外拼接。
        OsdTimeFormat::Chinese => {
            r"%{localtime:%Y}年%{localtime:%m}月%{localtime:%d}日%{localtime:%H\:%M\:%S}"
        }
    }
}

struct Frame {
    width: u32,
    height: u32,
    base_width: u32,
    base_height: u32,
}

impl Frame {
    fn scale(&self, value: u32, base: u32, out: u32) -> u32 {
        if base == 0 {
            return value;
        }
        (u64::from(value) * u64::from(out) / u64::from(base)) as u32
    }

    fn position(&self, anchor: OsdAnchor) -> (String, String) {
        match anchor {
            OsdAnchor::Pixel { x, y } => (
                self.scale(x, self.base_width, self.width).to_string(),
                self.scale(y, self.base_height, self.height).to_string(),
            ),
            OsdAnchor::TopRight => (format!("w-text_w-{MARGIN_PX}"), MARGIN_PX.to_string()),
        }
    }
}

fn drawtext(
    font: &str,
    text: &str,
    expand: bool,
    style: &OsdLayer,
    size: u32,
    alpha: Option<&str>,
    (x, y): (&str, &str),
) -> String {
    let color = |value: &str, fallback: &str| {
        let base = parse_hex_color(value).unwrap_or_else(|| fallback.into());
        match alpha {
            Some(alpha) => format!("{base}@{alpha}"),
            None => base,
        }
    };
    format!(
        "drawtext=fontfile={font}:expansion={expansion}:text={text}:fontsize={size}:\
fontcolor={fill}:borderw={border}:bordercolor={outline}:x={x}:y={y}",
        expansion = if expand { "normal" } else { "none" },
        text = escape_value(text),
        fill = color(&style.fill_color, "0xFFFFFF"),
        outline = color(&style.outline_color, "0x000000"),
        border = (size / 18).max(1),
    )
}
/// 斜铺水印：在透明方形画布上逐行绘制一次(单帧源，overlay 重复使用最后一帧)，
/// 旋转 30° 后居中裁回输出尺寸。每帧只做一次 alpha 叠加，不重复排版文字。
fn watermark_chain(font: &str, mark: &OsdText, width: u32, height: u32) -> String {
    let side = {
        let diag = (f64::from(width).powi(2) + f64::from(height).powi(2))
            .sqrt()
            .ceil() as u32;
        diag + diag % 2
    };
    let size = mark.style.size.pixel_size(height);
    let chars = mark.text.chars().count().max(1) as u32;
    // 间距 = 1.5 倍文字宽，用全角空格(约 1 个字号宽)近似。
    let gap_count = (chars * 3).div_ceil(2);
    let unit = format!("{}{}", mark.text, "\u{3000}".repeat(gap_count as usize));
    // 按最窄字符(半个字号)估算周期，保证一行足够铺满画布。
    let period_min = (size * chars / 2 + size * gap_count).max(1);
    let copies = side / period_min + 2;
    let row_text = unit.repeat(copies as usize);
    let step_y = size * 4;
    let rows = side / step_y + 1;
    let mut chain = format!("color=c=0x00000000:s={side}x{side}:r=1:d=1,format=rgba");
    for row in 0..rows {
        let x = if row % 2 == 1 {
            format!("-text_w/{copies}/2")
        } else {
            "0".into()
        };
        let y = (row * step_y).to_string();
        chain.push(',');
        chain.push_str(&drawtext(
            font,
            &row_text,
            false,
            &mark.style,
            size,
            Some(WATERMARK_ALPHA),
            (&x, &y),
        ));
    }
    chain.push_str(&format!(
        ",rotate=-PI/6:c=none:ow=iw:oh=ih,crop={width}:{height}"
    ));
    chain
}

/// 已确认可烧录的 OSD：FFmpeg 带 drawtext/drawbox，且找到了中文字体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsdBurnIn {
    pub overlay: OsdOverlay,
    pub font: PathBuf,
}

impl OsdBurnIn {
    /// 不满足烧录条件时返回 None 并记录原因；采集照常进行，只是不带 OSD。
    pub fn resolve(ffmpeg: &str, overlay: OsdOverlay) -> Option<Self> {
        if overlay.is_empty() {
            return None;
        }
        if !ffmpeg_supports_osd(ffmpeg) {
            tracing::warn!(
                ffmpeg,
                "FFmpeg 缺少 drawtext/drawbox 滤镜(需 libfreetype)，OSD 不烧入码流"
            );
            return None;
        }
        let Some(font) = font_file() else {
            tracing::warn!("未找到中文字体(可用 UVP_OSD_FONT 指定)，OSD 不烧入码流");
            return None;
        };
        Some(Self { overlay, font })
    }

    /// 在基础滤镜之后追加 OSD，输出尺寸为 `width`×`height`。
    pub fn graph(&self, base_chain: &str, width: u32, height: u32) -> String {
        filter_graph(base_chain, &self.overlay, width, height, &self.font)
    }
}

/// 在 `base_chain`(如 scale/pad，可为空)之后追加 OSD 绘制，返回可直接用于
/// `-vf` 的滤镜图(含水印时带内部标签)。调用方需要输出标签时自行追加 `[v]`。
pub fn filter_graph(
    base_chain: &str,
    overlay: &OsdOverlay,
    width: u32,
    height: u32,
    font: &std::path::Path,
) -> String {
    let base = if base_chain.is_empty() {
        "null"
    } else {
        base_chain
    };
    if overlay.is_empty() {
        return base.to_string();
    }
    let font = escape_value(&font.to_string_lossy().replace('\\', "/"));
    let frame = Frame {
        width,
        height,
        base_width: overlay.base_width,
        base_height: overlay.base_height,
    };
    let mut draws = Vec::new();
    if let Some(time) = &overlay.time {
        let (x, y) = frame.position(time.anchor);
        let size = time.style.size.pixel_size(height);
        draws.push(drawtext(
            &font,
            time_expansion(time.format),
            true,
            &time.style,
            size,
            None,
            (&x, &y),
        ));
    }
    for text in overlay.texts.iter().take(OSD_ITEMS_MAX) {
        if text.text.is_empty() {
            continue;
        }
        let (x, y) = frame.position(text.anchor);
        let size = text.style.size.pixel_size(height);
        draws.push(drawtext(
            &font,
            &text.text,
            false,
            &text.style,
            size,
            None,
            (&x, &y),
        ));
    }
    let (mask_w, mask_h) = overlay.mask_base;
    for rect in overlay.masks.iter().take(OSD_MASKS_MAX) {
        let left = frame.scale(rect.left, mask_w, width).min(width);
        let top = frame.scale(rect.top, mask_h, height).min(height);
        let right = frame.scale(rect.right, mask_w, width).min(width);
        let bottom = frame.scale(rect.bottom, mask_h, height).min(height);
        if right <= left || bottom <= top {
            continue;
        }
        draws.push(format!(
            "drawbox=x={left}:y={top}:w={}:h={}:color=black:t=fill",
            right - left,
            bottom - top
        ));
    }
    let tail: String = draws.iter().map(|draw| format!(",{draw}")).collect();
    match &overlay.watermark {
        Some(mark) if !mark.text.is_empty() => format!(
            "{base}[osdbase];{wm}[osdwm];[osdbase][osdwm]overlay=0:0:eof_action=repeat{tail}",
            wm = watermark_chain(&font, mark, width, height),
        ),
        _ => format!("{base}{tail}"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn sample(watermark: bool) -> OsdOverlay {
        let style = OsdLayer::default();
        OsdOverlay {
            base_width: 1920,
            base_height: 1080,
            time: Some(OsdTime {
                anchor: OsdAnchor::Pixel { x: 10, y: 10 },
                format: OsdTimeFormat::Chinese,
                style: style.clone(),
            }),
            texts: vec![OsdText {
                text: "通道:一,[A];'b'\\".into(),
                anchor: OsdAnchor::Pixel { x: 960, y: 540 },
                style: style.clone(),
            }],
            watermark: watermark.then(|| OsdText {
                text: "内部测试".into(),
                anchor: OsdAnchor::TopRight,
                style: style.clone(),
            }),
            mask_base: (1920, 1080),
            masks: vec![OsdRect {
                left: 0,
                top: 540,
                right: 960,
                bottom: 1080,
            }],
        }
    }

    #[test]
    fn 字号按画面高度换算且不小于16() {
        assert_eq!(OsdSize::Small.pixel_size(720), 25);
        assert_eq!(OsdSize::Medium.pixel_size(720), 36);
        assert_eq!(OsdSize::Large.pixel_size(720), 50);
        assert_eq!(OsdSize::Small.pixel_size(240), 16);
    }

    #[test]
    fn 默认配置与移动端一致且校验颜色和长度() {
        let settings = OsdSettings::default();
        assert!(settings.timestamp.enabled && settings.channel_name.enabled);
        assert!(!settings.watermark.enabled);
        assert!(settings.validate().is_ok());
        let mut bad = settings.clone();
        bad.timestamp.fill_color = "white".into();
        assert!(bad.validate().is_err());
        let mut bad = settings.clone();
        bad.watermark.text = "字".repeat(33);
        assert!(bad.validate().is_err());
        let mut bad = settings;
        bad.watermark.enabled = true;
        assert!(bad.validate().is_err(), "开水印必须有文字");
    }

    #[test]
    fn 滤镜转义特殊字符() {
        // 选项层 `a\:b`，滤镜图层再把 `\` 转义一次。
        assert_eq!(escape_value("a:b"), r"a\\:b");
        assert_eq!(escape_value("a,b"), r"a\,b");
        assert_eq!(escape_value("a'b"), r"a\\\'b");
    }

    #[test]
    fn 坐标按平台坐标系缩放到输出分辨率() {
        let graph = filter_graph(
            "",
            &sample(false),
            1280,
            720,
            std::path::Path::new("C:\\f.ttc"),
        );
        assert!(graph.starts_with("null,drawtext="), "{graph}");
        assert!(graph.contains("x=640:y=360"), "{graph}");
        assert!(
            graph.contains("drawbox=x=0:y=360:w=640:h=360:color=black:t=fill"),
            "{graph}"
        );
        assert!(graph.contains(r"fontfile=C\\:/f.ttc"), "{graph}");
        // 遮挡在文字之后，盖住 OSD。
        assert!(graph.rfind("drawtext").unwrap() < graph.find("drawbox").unwrap());
    }

    #[test]
    fn 水印走透明画布旋转叠加() {
        let graph = filter_graph(
            "scale=1280:720",
            &sample(true),
            1280,
            720,
            std::path::Path::new("/f.ttc"),
        );
        assert!(
            graph.starts_with("scale=1280:720[osdbase];color=c=0x00000000"),
            "{graph}"
        );
        assert!(graph.contains("rotate=-PI/6:c=none"), "{graph}");
        assert!(
            graph.contains("[osdbase][osdwm]overlay=0:0:eof_action=repeat,drawtext"),
            "{graph}"
        );
        assert!(graph.contains("fontcolor=0xFFFFFF@0.28"), "{graph}");
    }

    #[test]
    fn 空叠加不改变基础滤镜() {
        let empty = OsdOverlay {
            base_width: 1,
            base_height: 1,
            time: None,
            texts: vec![],
            watermark: None,
            mask_base: (1, 1),
            masks: vec![],
        };
        assert_eq!(
            filter_graph("", &empty, 640, 480, std::path::Path::new("/f")),
            "null"
        );
    }

    #[test]
    fn 识别滤镜列表() {
        let listing = " T.. drawbox  V->V  Draw\n T.. drawtext  V->V  Draw text\n";
        assert!(filters_support_osd(listing));
        assert!(!filters_support_osd(" T.. drawbox  V->V  Draw\n"));
    }
    /// 真实 FFmpeg 解析并执行生成的滤镜图；缺 FFmpeg/drawtext/中文字体时跳过。
    #[test]
    fn 生成的滤镜图能被ffmpeg执行() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            return;
        };
        let Some(font) = font_file() else { return };
        if !ffmpeg_supports_osd(&ffmpeg) {
            return;
        }
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/osd-filter-test.png");
        let graph = filter_graph("", &sample(true), 1280, 720, &font);
        let status = common::hidden_command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=1280x720:rate=25",
                "-vf",
            ])
            .arg(&graph)
            .args(["-frames:v", "1", "-update", "1", "-y"])
            .arg(&out)
            .output()
            .expect("ffmpeg 应能启动");
        assert!(
            status.status.success(),
            "{}\n{graph}",
            String::from_utf8_lossy(&status.stderr)
        );
        assert!(out.metadata().map(|m| m.len() > 0).unwrap_or(false));
    }

    /// 两种时间格式都必须真的画出文字(纯色底图上有白色像素)。
    #[test]
    fn 两种时间格式都能渲染出文字() {
        let Some(ffmpeg) = crate::source::ffmpeg_bin() else {
            return;
        };
        let Some(font) = font_file() else { return };
        if !ffmpeg_supports_osd(&ffmpeg) {
            return;
        }
        for format in [OsdTimeFormat::Dash, OsdTimeFormat::Chinese] {
            let overlay = OsdOverlay {
                base_width: 640,
                base_height: 120,
                time: Some(OsdTime {
                    anchor: OsdAnchor::Pixel { x: 6, y: 6 },
                    format,
                    style: OsdLayer::default(),
                }),
                texts: vec![],
                watermark: None,
                mask_base: (640, 120),
                masks: vec![],
            };
            let graph = filter_graph("format=gray", &overlay, 640, 120, &font) + ",format=gray";
            let out = common::hidden_command(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=640x120",
                    "-vf",
                ])
                .arg(&graph)
                .args(["-frames:v", "1", "-f", "rawvideo", "pipe:1"])
                .output()
                .expect("ffmpeg 应能启动");
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            let bright = out.stdout.iter().filter(|&&p| p > 200).count();
            assert!(bright > 200, "{format:?} 没有画出时间文字: {graph}");
        }
    }
}
