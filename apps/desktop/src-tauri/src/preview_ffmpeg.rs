//! FFmpeg 预览解码后端选择与证据解析。
//!
//! 该模块不把 `-hwaccel` 参数当作硬件解码成功；只有运行日志/输出像素格式
//! 能证明后端实际工作时才标记 confirmed。创建失败由调用方使用同一输入访问
//! 单元重启 software plan。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfmpegBackend {
    VideoToolbox,
    D3d11va,
    Dxva2,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendConfidence {
    Requested,
    Confirmed,
    Software,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCapabilities {
    pub hardware_accelerators: Vec<String>,
    pub decoders: Vec<String>,
}

impl FfmpegCapabilities {
    pub fn from_command_output(hwaccels: &str, decoders: &str) -> Self {
        Self {
            hardware_accelerators: lines(hwaccels),
            decoders: lines(decoders),
        }
    }

    fn has_accel(&self, name: &str) -> bool {
        self.hardware_accelerators
            .iter()
            .any(|item| item.eq_ignore_ascii_case(name))
    }

    fn has_decoder_backend(&self, name: &str) -> bool {
        self.decoders
            .iter()
            .any(|item| item.to_ascii_lowercase().contains(name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegDecodePlan {
    pub backend: FfmpegBackend,
    pub confidence: BackendConfidence,
    pub args: Vec<String>,
}

impl FfmpegDecodePlan {
    pub fn software(input_format: &str) -> Self {
        Self {
            backend: FfmpegBackend::Software,
            confidence: BackendConfidence::Software,
            args: vec!["-c:v".into(), input_format_decoder(input_format).into()],
        }
    }

    pub fn hardware(input_format: &str, backend: FfmpegBackend) -> Self {
        let name = match backend {
            FfmpegBackend::VideoToolbox => "videotoolbox",
            FfmpegBackend::D3d11va => "d3d11va",
            FfmpegBackend::Dxva2 => "dxva2",
            FfmpegBackend::Software => return Self::software(input_format),
        };
        Self {
            backend,
            confidence: BackendConfidence::Requested,
            args: vec![
                "-hwaccel".into(),
                name.into(),
                "-c:v".into(),
                input_format_decoder(input_format).into(),
            ],
        }
    }
}

/// 根据目标平台和 FFmpeg 实际列出的能力选择一次硬件尝试；没有能力时软件解码。
pub fn select_plan(
    capabilities: &FfmpegCapabilities,
    input_format: &str,
    target: &str,
) -> FfmpegDecodePlan {
    let target = target.to_ascii_lowercase();
    if target.contains("macos") || target.contains("darwin") {
        if capabilities.has_accel("videotoolbox") {
            return FfmpegDecodePlan::hardware(input_format, FfmpegBackend::VideoToolbox);
        }
    } else if target.contains("windows") {
        if capabilities.has_accel("d3d11va") || capabilities.has_decoder_backend("d3d11va") {
            return FfmpegDecodePlan::hardware(input_format, FfmpegBackend::D3d11va);
        }
        if capabilities.has_accel("dxva2") || capabilities.has_decoder_backend("dxva2") {
            return FfmpegDecodePlan::hardware(input_format, FfmpegBackend::Dxva2);
        }
    }
    FfmpegDecodePlan::software(input_format)
}

/// 仅在日志/像素格式出现可核实证据时标记 Confirmed。
pub fn classify_evidence(plan: &FfmpegDecodePlan, stderr: &str) -> BackendConfidence {
    if plan.backend == FfmpegBackend::Software {
        return BackendConfidence::Software;
    }
    let text = stderr.to_ascii_lowercase();
    let backend = match plan.backend {
        FfmpegBackend::VideoToolbox => "videotoolbox",
        FfmpegBackend::D3d11va => "d3d11va",
        FfmpegBackend::Dxva2 => "dxva2",
        FfmpegBackend::Software => return BackendConfidence::Software,
    };
    if text.contains(backend)
        && (text.contains("using") || text.contains("initialized") || text.contains("pixel format"))
    {
        BackendConfidence::Confirmed
    } else if text.contains("error") || text.contains("failed") || text.contains("unsupported") {
        BackendConfidence::Unknown
    } else {
        BackendConfidence::Requested
    }
}

/// 持续 drain stderr 时只保留末尾，避免 FFmpeg 错误输出撑满管道或内存。
pub fn append_stderr_tail(tail: &mut Vec<u8>, chunk: &[u8], max_len: usize) {
    tail.extend_from_slice(chunk);
    if tail.len() > max_len {
        let drain = tail.len() - max_len;
        tail.drain(..drain);
    }
}

fn input_format_decoder(input_format: &str) -> &str {
    match input_format.to_ascii_lowercase().as_str() {
        "hevc" | "h265" => "hevc",
        _ => "h264",
    }
}

fn lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("Hardware acceleration"))
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_capability_selects_videotoolbox_but_only_requested() {
        let caps = FfmpegCapabilities::from_command_output(
            "Hardware acceleration methods:\nvideotoolbox\n",
            " VFS..D h264 H.264",
        );
        let plan = select_plan(&caps, "h264", "macOS");
        assert_eq!(plan.backend, FfmpegBackend::VideoToolbox);
        assert_eq!(plan.confidence, BackendConfidence::Requested);
        assert!(plan.args.contains(&"-hwaccel".to_string()));
    }

    #[test]
    fn windows_without_backend_falls_back_to_software() {
        let caps = FfmpegCapabilities::from_command_output("cuda\n", " VFS..D h264 H.264");
        let plan = select_plan(&caps, "h264", "Windows");
        assert_eq!(plan.backend, FfmpegBackend::Software);
        assert_eq!(plan.confidence, BackendConfidence::Software);
    }

    #[test]
    fn hardware_evidence_requires_runtime_log() {
        let caps = FfmpegCapabilities::from_command_output("videotoolbox", "h264");
        let plan = select_plan(&caps, "h264", "macOS");
        assert_eq!(
            classify_evidence(
                &plan,
                "Initialized videotoolbox hwaccel; pixel format videotoolbox_vld"
            ),
            BackendConfidence::Confirmed
        );
        assert_eq!(
            classify_evidence(&plan, "-hwaccel videotoolbox"),
            BackendConfidence::Requested
        );
        assert_eq!(
            classify_evidence(&plan, "Error videotoolbox failed"),
            BackendConfidence::Unknown
        );
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let mut tail = Vec::new();
        append_stderr_tail(&mut tail, b"123456", 4);
        assert_eq!(tail, b"3456");
    }
}
