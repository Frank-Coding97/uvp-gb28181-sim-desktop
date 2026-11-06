//! 录像缩略图提取。
//!
//! 录像列表光有时间戳分不清内容，需要一张能一眼认出的首帧。缩略图不随录像落盘
//! （每段录像开录时未必有可用帧，且会平白多占一份磁盘），而是**回放时按需从文件
//! 里抽一帧**，抽出来的 JPEG 缓存在独立目录里，同一段录像只抽一次。
//!
//! 抽帧依赖外部 `ffmpeg`（与本项目其它采集路径同一套二进制定位逻辑）。
//! 找不到 ffmpeg 或文件损坏时返回错误，由调用方退化为占位图 —— 缩略图是增强项，
//! 不该因为缺一个可选依赖就让整页录像列表不可用。

use std::path::{Path, PathBuf};

/// 缩略图目标宽度（高度按比例）。列表行高约 40px，2 倍屏取 320 足够清晰。
pub const THUMB_WIDTH: u32 = 320;

/// 缩略图目录名（相对数据根目录）。
pub const THUMB_DIR: &str = "recording-thumbs";

/// 抽帧位置：优先取 1 秒处，避开部分容器首帧尚未出画面（黑帧）的情况。
pub const DEFAULT_SEEK_SECS: f64 = 1.0;
/// 时长不足 1 秒的录像退到这个位置。
pub const SHORT_SEEK_SECS: f64 = 0.0;

/// 录像 ID 只接受单段文件名字符。和抓拍 ID 同一套规则：
/// 这些 ID 会被拼进路径，放开 `/` `.` 就等于开放任意文件读写。
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// 计算某段录像的缩略图缓存路径；ID 非法时返回 None。
pub fn cache_path(dir: &Path, id: &str) -> Option<PathBuf> {
    is_valid_id(id).then(|| dir.join(format!("{id}.jpg")))
}

/// 按录像时长选抽帧位置：太短的录像从头抽，否则从 1 秒处抽。
///
/// 时长未知（0）时同样回退到 1 秒处 —— 调用方拿不到时长通常意味着索引缺元数据，
/// 但文件本身多半是正常的。
pub fn seek_secs_for(duration_secs: f64) -> f64 {
    if duration_secs > 0.0 && duration_secs <= DEFAULT_SEEK_SECS {
        SHORT_SEEK_SECS
    } else {
        DEFAULT_SEEK_SECS
    }
}

/// 构造 ffmpeg 参数（不含可执行文件名）。
///
/// `-ss` 放在 `-i` 之前是关键帧级快速定位：放在后面会先解码整段再丢掉，
/// 长录像上能差出好几秒。`-frames:v 1` 只要一帧，`-q:v 5` 在体积与清晰度间取中。
pub fn extract_args(input: &Path, output: &Path, seek_secs: f64) -> Vec<String> {
    let seek = if seek_secs.is_finite() && seek_secs > 0.0 {
        seek_secs
    } else {
        0.0
    };
    vec![
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-ss".into(),
        format!("{seek:.3}"),
        "-i".into(),
        input.to_string_lossy().into_owned(),
        "-frames:v".into(),
        "1".into(),
        // 宽定高变，且强制偶数宽（部分编码器要求 yuv420 对齐）。
        "-vf".into(),
        format!("scale={}:force_original_aspect_ratio=decrease,scale=trunc(iw/2)*2:trunc(ih/2)*2", THUMB_WIDTH),
        "-q:v".into(),
        "5".into(),
        "-y".into(),
        output.to_string_lossy().into_owned(),
    ]
}

/// 抽一帧存成 JPEG。返回写出的字节数。
///
/// 目录不存在时自动创建：缓存目录是派生物，丢了重建即可，不该让调用方先探测。
pub fn extract(input: &Path, output: &Path, seek_secs: f64, ffmpeg: &str) -> Result<u64, String> {
    if !input.is_file() {
        return Err(format!("录像文件不存在: {}", input.display()));
    }
    if let Some(dir) = output.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("创建缩略图目录 {} 失败: {error}", dir.display()))?;
    }
    let status = common::hidden_command(ffmpeg)
        .args(extract_args(input, output, seek_secs))
        .output()
        .map_err(|error| format!("调用 ffmpeg 失败: {error}"))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(format!("ffmpeg 抽帧失败: {}", stderr.trim()));
    }
    let len = std::fs::metadata(output)
        .map_err(|error| format!("缩略图未生成: {error}"))?
        .len();
    if len == 0 {
        return Err("ffmpeg 输出为空文件".into());
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 合法id可用于缓存文件名() {
        assert!(is_valid_id("rec-20261003-120000-1"));
        assert!(is_valid_id("abc_123-XYZ"));
        assert_eq!(
            cache_path(Path::new("/tmp/t"), "rec-1"),
            Some(PathBuf::from("/tmp/t/rec-1.jpg"))
        );
    }

    #[test]
    fn 非法id被拒绝以阻断路径穿越() {
        for bad in ["", "../etc/passwd", "a/b", "a\\b", "a.b", "录像"] {
            assert!(!is_valid_id(bad), "应拒绝: {bad}");
            assert_eq!(cache_path(Path::new("/tmp/t"), bad), None, "应拒绝: {bad}");
        }
    }

    #[test]
    fn 超长id被拒绝() {
        let long = "a".repeat(129);
        assert!(!is_valid_id(&long));
        assert!(is_valid_id(&"a".repeat(128)));
    }

    #[test]
    fn 抽帧位置按时长回退() {
        // 正常录像从 1 秒处抽，避开首帧黑屏。
        assert_eq!(seek_secs_for(30.0), DEFAULT_SEEK_SECS);
        // 太短的录像从头抽，1 秒处可能已经超出文件末尾。
        assert_eq!(seek_secs_for(0.5), SHORT_SEEK_SECS);
        assert_eq!(seek_secs_for(1.0), SHORT_SEEK_SECS);
        // 时长未知时仍按 1 秒处理。
        assert_eq!(seek_secs_for(0.0), DEFAULT_SEEK_SECS);
    }

    #[test]
    fn 参数把定位放在输入之前且只要一帧() {
        let args = extract_args(
            Path::new("/data/rec-1.ps"),
            Path::new("/cache/rec-1.jpg"),
            1.0,
        );
        let seek = args.iter().position(|a| a == "-ss").expect("必须有 -ss");
        let input = args.iter().position(|a| a == "-i").expect("必须有 -i");
        assert!(seek < input, "-ss 必须在 -i 之前才是关键帧级定位");
        assert!(args.contains(&"1".to_string()), "-frames:v 1 只取一帧");
        assert!(args.iter().any(|a| a.contains("scale=320")), "宽度固定 320");
        assert_eq!(args.last().map(String::as_str), Some("/cache/rec-1.jpg"));
    }

    #[test]
    fn 非法定位值退化为零而不是写出nan() {
        for bad in [f64::NAN, f64::INFINITY, -3.0] {
            let args = extract_args(Path::new("in.ps"), Path::new("out.jpg"), bad);
            let position = args.iter().position(|a| a == "-ss").expect("-ss");
            assert_eq!(args[position + 1], "0.000", "非法定位值应退化为 0");
        }
    }

    #[test]
    fn 输入文件缺失时直接报错不启动ffmpeg() {
        let error = extract(
            Path::new("/definitely/missing/rec.ps"),
            Path::new("/tmp/never.jpg"),
            1.0,
            "ffmpeg",
        )
        .unwrap_err();
        assert!(error.contains("录像文件不存在"), "实际: {error}");
    }
}
