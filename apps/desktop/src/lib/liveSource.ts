// 实时媒体源 URI 的编解码与校验 —— **纯函数**,与 Vue 无关。
//
// 单独抽出来的理由:这些函数决定了传给引擎的 `live:` URI 是否合法,写错直接导致
// "采集起不来"且报错信息隔了一层不好定位。抽成纯模块后可以直接单测(见
// `liveSource.test.ts`),不需要挂载组件。
//
// 与 Rust 侧 `media_rtp::LiveSourceSpec::parse` 的白名单保持一致:
// `live:camera:<index>?audio=<none|index>&audio_codec=<codec>&width=&height=&bitrate=&codec=&gop=`
// `live:screen:<display_id>?audio=<none|system|microphone:N>&audio_codec=<codec>&width=&height=&bitrate=&codec=&gop=`
// 编码参数(width/height/bitrate/codec)必须成组出现,gop 可单独给;两者都省略时用默认档。

export type MediaMode = "none" | "file" | "camera" | "screen";
export type ScreenCodec = "h264" | "h265";
export type AudioCodec = "g711a" | "g711u" | "aac" | "opus";
/** 屏幕采集的音频来源:不采 / 系统声音 / 指定麦克风。 */
export type ScreenAudioSource = "none" | "system" | `microphone:${number}`;

export const SCREEN_RESOLUTIONS = [
  { value: "640x360", label: "640 × 360（360p）" },
  { value: "640x480", label: "640 × 480（标清 4:3）" },
  { value: "1280x720", label: "1280 × 720（720p）" },
  { value: "1920x1080", label: "1920 × 1080（1080p）" },
  { value: "2560x1440", label: "2560 × 1440（2K）" },
  { value: "3840x2160", label: "3840 × 2160（4K）" },
] as const;

export const DEFAULT_SCREEN_PROFILE = {
  width: 1280,
  height: 720,
  bitrateKbps: 2500,
  codec: "h264" as ScreenCodec,
  /** 25 fps 下 2 秒一个关键帧,与 Rust 侧 DEFAULT_GOP 一致。 */
  gop: 50,
};

export const MIN_SCREEN_DIMENSION = 160;
export const MAX_SCREEN_LONG_SIDE = 3840;
export const MAX_SCREEN_SHORT_SIDE = 2160;
export const MIN_SCREEN_BITRATE_KBPS = 128;
/**
 * 码率上限:250 Mbps,只做"填错了"级别的合理性防护,不再是实际约束。
 *
 * 原来限 20 Mbps,但全 I 帧流(GOP 很小)的码率需求是普通 GOP 的 5~8 倍:
 * 4K@30 全 I 帧要 ~200 Mbps,1080p@25 全 I 帧要 ~60 Mbps,20 Mbps 会把
 * "小 GOP + 高码率"这条低延迟路线直接堵死。UI 码率单位是 Mbps,这个数只在
 * URI/引擎内部以 kbps 表示。
 */
export const MAX_SCREEN_BITRATE_KBPS = 250_000;
export const MIN_GOP = 1;
export const MAX_GOP = 600;

/**
 * 按分辨率/帧率/GOP 推荐一个"不塌质量"的码率(kbps)。
 *
 * 模型:每像素每帧比特数 bpp≈0.1(H.264 运动画面可接受质量的常见经验值),
 * 再按 GOP 做 I 帧开销放大 —— GOP 越小 I 帧占比越高,而一张 I 帧的代价约是
 * P 帧的 8 倍,等效乘子 ≈ 1 + 7/gop:
 *   · 1280×720@25, gop=50 → ~2.6 Mbps(与历史默认 2.5 基本一致)
 *   · 1280×720@25, gop=1(全 I 帧) → ~18 Mbps(这就是之前 GOP=1 灰闪的解药)
 *   · 1920×1080@25, gop=50 → ~5.9 Mbps
 * 结果四舍五入到 0.1 Mbps 并夹在合法区间内。
 */
export function suggestBitrateKbps(
  width: number,
  height: number,
  fps: number,
  gop: number
): number {
  const safeGop = Math.max(1, Math.floor(gop));
  const safeFps = Math.max(1, Math.floor(fps));
  const baseKbps = (width * height * safeFps * 0.1) / 1000;
  const intraBoost = 1 + 7 / safeGop;
  const raw = baseKbps * intraBoost;
  const rounded = Math.round(raw / 100) * 100; // 0.1 Mbps 步进
  return Math.min(MAX_SCREEN_BITRATE_KBPS, Math.max(MIN_SCREEN_BITRATE_KBPS, rounded));
}

/** UI 码率以 Mbps 输入,URI/引擎以 kbps 表示 —— 转换收拢在这里避免各处各自取整。 */
export function mbpsToKbps(mbps: number): number {
  return Math.round(mbps * 1000);
}

/** kbps → Mbps(显示用,保留两位小数以内的浮点误差无害)。 */
export function kbpsToMbps(kbps: number): number {
  return kbps / 1000;
}

/** 视频编码参数,摄像头与屏幕共用。 */
export interface VideoProfile {
  width: number;
  height: number;
  bitrateKbps: number;
  codec: ScreenCodec;
  gop: number;
}

export interface ParsedCameraSource extends VideoProfile {
  videoIndex: number;
  audio: string;
  audioCodec: AudioCodec;
}

/**
 * 屏幕采集目标:整块显示器 / 单个窗口 / 某个应用的全部窗口。
 *
 * 窗口与应用只能由 macOS 系统选择器(`SCContentSharingPicker`)给出身份 ——
 * 应用自己列窗口既拿不到跨应用的稳定 id,也不符合系统授权习惯。
 * `label` 只在本次会话内用于展示,不写进 URI(URI 只带稳定身份)。
 */
export type ShareTarget =
  | { kind: "display"; displayId: number }
  | { kind: "window"; windowId: number; label?: string }
  | { kind: "application"; bundleId: string; label?: string };

export interface ParsedScreenSource extends VideoProfile {
  target: ShareTarget;
  audio: ScreenAudioSource;
  audioCodec: AudioCodec;
}

/** 目标在 URI 里的 `live:<scheme>:<value>` 前缀。 */
export function shareTargetPrefix(target: ShareTarget): string {
  switch (target.kind) {
    case "display":
      return `live:screen:${target.displayId}`;
    case "window":
      return `live:window:${target.windowId}`;
    case "application":
      return `live:app:${target.bundleId}`;
  }
}

export function isAudioCodec(value: string): value is AudioCodec {
  return value === "g711a" || value === "g711u" || value === "aac" || value === "opus";
}

/** 取正整数查询参数,缺失或非法时回退默认值。 */
export function parseScreenNumber(value: string | null, fallback: number): number {
  if (value === null) return fallback;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

/** 从查询参数里取出编码档;`codec` 非法时返回 null(交给调用方判为非法 URI)。 */
function parseVideoProfile(params: URLSearchParams): VideoProfile | null {
  const codec = params.get("codec");
  if (codec !== null && codec !== "h264" && codec !== "h265") return null;
  return {
    width: parseScreenNumber(params.get("width"), DEFAULT_SCREEN_PROFILE.width),
    height: parseScreenNumber(params.get("height"), DEFAULT_SCREEN_PROFILE.height),
    bitrateKbps: parseScreenNumber(params.get("bitrate"), DEFAULT_SCREEN_PROFILE.bitrateKbps),
    codec: codec === "h265" ? "h265" : "h264",
    gop: parseScreenNumber(params.get("gop"), DEFAULT_SCREEN_PROFILE.gop),
  };
}

/** 把编码档渲染成 URI 查询串(不含前导 `&`)。 */
function videoProfileQuery(profile: VideoProfile): string {
  return (
    `width=${profile.width}&height=${profile.height}` +
    `&bitrate=${profile.bitrateKbps}&codec=${profile.codec}&gop=${profile.gop}`
  );
}

/** 解析摄像头源 URI;非法或非摄像头 URI 返回 null。 */
export function parseCameraSource(source: string): ParsedCameraSource | null {
  const match = source.match(/^live:camera:(\d+)\?(.+)$/);
  if (!match) return null;
  const videoIndex = Number(match[1]);
  const params = new URLSearchParams(match[2]);
  const audio = params.get("audio");
  const audioCodec = params.get("audio_codec") ?? "g711a";
  if (
    !Number.isSafeInteger(videoIndex) ||
    videoIndex < 0 ||
    (audio !== "none" && !/^\d+$/.test(audio ?? "")) ||
    !isAudioCodec(audioCodec)
  ) {
    return null;
  }
  const profile = parseVideoProfile(params);
  if (!profile) return null;
  return { videoIndex, audio: audio!, audioCodec, ...profile };
}

/** 解析屏幕源 URI(整屏 / 窗口 / 应用);非法或非屏幕 URI 返回 null。 */
export function parseScreenSource(source: string): ParsedScreenSource | null {
  const display = source.match(/^live:screen:(\d+)\?(.+)$/);
  const window = source.match(/^live:window:(\d+)\?(.+)$/);
  const application = source.match(/^live:app:([A-Za-z0-9._-]+)\?(.+)$/);
  const match = display ?? window ?? application;
  if (!match) return null;

  let target: ShareTarget;
  if (display) {
    const displayId = Number(match[1]);
    if (!Number.isSafeInteger(displayId) || displayId <= 0) return null;
    target = { kind: "display", displayId };
  } else if (window) {
    const windowId = Number(match[1]);
    if (!Number.isSafeInteger(windowId) || windowId <= 0) return null;
    target = { kind: "window", windowId };
  } else {
    target = { kind: "application", bundleId: match[1] };
  }

  const params = new URLSearchParams(match[2]);
  const audio = params.get("audio");
  const audioCodec = params.get("audio_codec") ?? "g711a";
  const validAudio = audio === "system" || audio === "none" || /^microphone:\d+$/.test(audio ?? "");
  if (!validAudio || !isAudioCodec(audioCodec)) return null;
  const profile = parseVideoProfile(params);
  if (!profile) return null;
  return { target, audio: audio as ScreenAudioSource, audioCodec, ...profile };
}

/** 构造摄像头源 URI。 */
export function buildCameraUri(source: ParsedCameraSource): string {
  return (
    `live:camera:${source.videoIndex}?audio=${source.audio}` +
    `&audio_codec=${source.audioCodec}&${videoProfileQuery(source)}`
  );
}

/** 构造屏幕源 URI(整屏 / 窗口 / 应用共用同一套查询参数)。 */
export function buildScreenUri(source: ParsedScreenSource): string {
  return (
    `${shareTargetPrefix(source.target)}?audio=${source.audio}` +
    `&audio_codec=${source.audioCodec}&${videoProfileQuery(source)}`
  );
}

/**
 * 校验视频编码参数(摄像头与屏幕共用)。返回错误文案,合法时返回 null。
 *
 * 这些边界与 Rust 侧一致:偶数宽高是 H.264/H.265 色度采样的硬要求,
 * 3840×2160 是 OpenH264 的支持上限。
 */
export function validateVideoProfile(input: {
  width: number;
  height: number;
  bitrateKbps: number;
  codec: string;
  gop?: number;
}): string | null {
  const { width, height, bitrateKbps, codec, gop } = input;
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height)) {
    return "分辨率必须是整数";
  }
  if (width % 2 !== 0 || height % 2 !== 0) {
    return "分辨率宽高必须为偶数";
  }
  if (width < MIN_SCREEN_DIMENSION || height < MIN_SCREEN_DIMENSION) {
    return `分辨率单边不能小于 ${MIN_SCREEN_DIMENSION}`;
  }
  if (
    Math.max(width, height) > MAX_SCREEN_LONG_SIDE ||
    Math.min(width, height) > MAX_SCREEN_SHORT_SIDE
  ) {
    return "分辨率超过 OpenH264 支持的 3840×2160 边界";
  }
  if (
    !Number.isSafeInteger(bitrateKbps) ||
    bitrateKbps < MIN_SCREEN_BITRATE_KBPS ||
    bitrateKbps > MAX_SCREEN_BITRATE_KBPS
  ) {
    return `码率必须在 ${kbpsToMbps(MIN_SCREEN_BITRATE_KBPS)}–${kbpsToMbps(MAX_SCREEN_BITRATE_KBPS)} Mbps 之间`;
  }
  if (codec !== "h264" && codec !== "h265") {
    return "编码格式必须为 H.264 或 H.265";
  }
  if (gop !== undefined && (!Number.isSafeInteger(gop) || gop < MIN_GOP || gop > MAX_GOP)) {
    return `GOP 必须在 ${MIN_GOP}–${MAX_GOP} 帧之间`;
  }
  return null;
}

/** 从任意视频源字符串推断当前媒体模式(用于页面初始化)。 */
export function inferMediaMode(source: string): MediaMode {
  const trimmed = source.trim();
  if (!trimmed) return "file";
  if (parseCameraSource(trimmed)) return "camera";
  // legacy 写法 live:0 / live:ffmpeg:0 明确映射主屏幕。
  if (parseScreenSource(trimmed) || trimmed === "live:0" || trimmed === "live:ffmpeg:0") {
    return "screen";
  }
  return "file";
}

/** 浏览器能否直接预览该文件(裸流不能)。 */
export function isLikelyPlayableFile(path: string): boolean {
  return /\.(mp4|m4v|mov|webm|mkv)$/i.test(path.trim());
}
