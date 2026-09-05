export type MediaVideoCodec = "h264" | "h265";
export type MediaAudioCodec = "g711_a" | "g711_u" | "aac";
export type MediaQualityPresetId = "smooth" | "standard" | "hd" | "uhd";

export interface MediaProfile {
  width: number;
  height: number;
  video_fps: number;
  bitrate_kbps: number;
  keyframe_interval_seconds: number;
  video_codec: MediaVideoCodec;
  audio_codec: MediaAudioCodec;
  audio_sample_rate_hz: number;
}

export interface MediaQualityPreset {
  id: MediaQualityPresetId;
  label: string;
  description: string;
  width: number;
  height: number;
  video_fps: number;
  bitrate_kbps: number;
  keyframe_interval_seconds: number;
}

export const MEDIA_PRESETS: readonly MediaQualityPreset[] = [
  { id: "smooth", label: "流畅", description: "640×480 · 15 FPS", width: 640, height: 480, video_fps: 15, bitrate_kbps: 600, keyframe_interval_seconds: 1 },
  { id: "standard", label: "标准", description: "1280×720 · 20 FPS", width: 1280, height: 720, video_fps: 20, bitrate_kbps: 1200, keyframe_interval_seconds: 1 },
  { id: "hd", label: "高清", description: "1280×720 · 25 FPS", width: 1280, height: 720, video_fps: 25, bitrate_kbps: 2000, keyframe_interval_seconds: 1 },
  { id: "uhd", label: "超清", description: "1920×1080 · 25 FPS", width: 1920, height: 1080, video_fps: 25, bitrate_kbps: 4000, keyframe_interval_seconds: 1 },
];

export const MEDIA_RESOLUTIONS = [
  { label: "640 × 480", width: 640, height: 480 },
  { label: "1280 × 720", width: 1280, height: 720 },
  { label: "1920 × 1080", width: 1920, height: 1080 },
] as const;

export const MEDIA_FPS_OPTIONS = [15, 20, 25, 30] as const;
export const MEDIA_BITRATE_OPTIONS = [600, 1200, 2000, 4000, 6000, 8000] as const;
export const MEDIA_GOP_OPTIONS = [1, 2, 4] as const;
export const MEDIA_VIDEO_CODECS: readonly { label: string; value: MediaVideoCodec }[] = [
  { label: "H.264", value: "h264" },
  { label: "H.265", value: "h265" },
];
export const MEDIA_AUDIO_CODECS: readonly { label: string; value: MediaAudioCodec }[] = [
  { label: "G.711A", value: "g711_a" },
  { label: "G.711U", value: "g711_u" },
  { label: "AAC", value: "aac" },
];
export const MEDIA_SAMPLE_RATES = [8000, 16000] as const;

export const DEFAULT_MEDIA_PROFILE: MediaProfile = {
  width: 1280,
  height: 720,
  video_fps: 25,
  bitrate_kbps: 2000,
  keyframe_interval_seconds: 1,
  video_codec: "h264",
  audio_codec: "g711_a",
  // 手机配置保存 16 kHz 原值；G.711 的实际有效采样率由 effectiveAudioSampleRate() 固定为 8 kHz。
  audio_sample_rate_hz: 16000,
};

const PROFILE_KEYS: readonly (keyof MediaProfile)[] = [
  "width",
  "height",
  "video_fps",
  "bitrate_kbps",
  "keyframe_interval_seconds",
  "video_codec",
  "audio_codec",
  "audio_sample_rate_hz",
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function integerValue(value: unknown): number | null {
  if (typeof value === "number" && Number.isInteger(value)) return value;
  if (typeof value === "string" && /^\d+$/.test(value.trim())) return Number(value);
  return null;
}

function isResolution(width: number, height: number): boolean {
  return MEDIA_RESOLUTIONS.some((item) => item.width === width && item.height === height);
}

export function cloneMediaProfile(profile: MediaProfile): MediaProfile {
  return { ...profile };
}

/** 将旧配置或不完整的后端响应补齐为可编辑的媒体草稿，保留合法的自定义 FPS。 */
export function normalizeMediaProfile(raw: unknown, fallback: MediaProfile = DEFAULT_MEDIA_PROFILE): MediaProfile {
  const next = cloneMediaProfile(fallback);
  if (!isRecord(raw)) return next;

  const width = integerValue(raw.width);
  const height = integerValue(raw.height);
  if (width !== null && height !== null && isResolution(width, height)) {
    next.width = width;
    next.height = height;
  }

  const fps = integerValue(raw.video_fps);
  if (fps !== null && fps >= 1 && fps <= 120) next.video_fps = fps;

  const bitrate = integerValue(raw.bitrate_kbps);
  if (bitrate !== null && MEDIA_BITRATE_OPTIONS.includes(bitrate as typeof MEDIA_BITRATE_OPTIONS[number])) {
    next.bitrate_kbps = bitrate;
  }

  const gop = integerValue(raw.keyframe_interval_seconds);
  if (gop !== null && MEDIA_GOP_OPTIONS.includes(gop as typeof MEDIA_GOP_OPTIONS[number])) {
    next.keyframe_interval_seconds = gop;
  }

  if (raw.video_codec === "h264" || raw.video_codec === "h265") next.video_codec = raw.video_codec;
  if (raw.audio_codec === "g711_a" || raw.audio_codec === "g711_u" || raw.audio_codec === "aac") {
    next.audio_codec = raw.audio_codec;
  }

  const sampleRate = integerValue(raw.audio_sample_rate_hz);
  if (sampleRate === 8000 || sampleRate === 16000) next.audio_sample_rate_hz = sampleRate;
  return next;
}

export function effectiveAudioSampleRate(profile: Pick<MediaProfile, "audio_codec" | "audio_sample_rate_hz">): number {
  return profile.audio_codec === "aac" ? profile.audio_sample_rate_hz : 8000;
}

export function applyMediaPreset(profile: MediaProfile, presetId: MediaQualityPresetId): MediaProfile {
  const preset = MEDIA_PRESETS.find((item) => item.id === presetId);
  if (!preset) return cloneMediaProfile(profile);
  return {
    ...profile,
    width: preset.width,
    height: preset.height,
    video_fps: preset.video_fps,
    bitrate_kbps: preset.bitrate_kbps,
    keyframe_interval_seconds: preset.keyframe_interval_seconds,
  };
}

export function matchedPreset(profile: MediaProfile): MediaQualityPresetId | null {
  return MEDIA_PRESETS.find((preset) =>
    preset.width === profile.width
    && preset.height === profile.height
    && preset.video_fps === profile.video_fps
    && preset.bitrate_kbps === profile.bitrate_kbps
    && preset.keyframe_interval_seconds === profile.keyframe_interval_seconds,
  )?.id ?? null;
}

export function mediaProfileEqual(left: MediaProfile, right: MediaProfile): boolean {
  return PROFILE_KEYS.every((key) => left[key] === right[key]);
}

/** 后端刷新快照时保留用户尚未提交的媒体草稿；显式重置/成功保存可强制同步。 */
export function syncMediaDraft(
  draft: MediaProfile | null,
  persisted: MediaProfile,
  force = false,
): MediaProfile {
  if (!force && draft && !mediaProfileEqual(draft, persisted)) return cloneMediaProfile(draft);
  return normalizeMediaProfile(persisted);
}

export function validateMediaProfile(profile: MediaProfile): string | null {
  if (!isResolution(profile.width, profile.height)) return `不支持的输出分辨率：${profile.width}×${profile.height}`;
  if (!Number.isInteger(profile.video_fps) || profile.video_fps < 1 || profile.video_fps > 120) {
    return "视频帧率必须在 1 到 120 FPS 之间";
  }
  if (!MEDIA_BITRATE_OPTIONS.includes(profile.bitrate_kbps as typeof MEDIA_BITRATE_OPTIONS[number])) {
    return `视频码率必须是 ${MEDIA_BITRATE_OPTIONS.join("、")} kbps 之一`;
  }
  if (!MEDIA_GOP_OPTIONS.includes(profile.keyframe_interval_seconds as typeof MEDIA_GOP_OPTIONS[number])) {
    return "关键帧间隔必须是 1、2 或 4 秒";
  }
  if (!MEDIA_VIDEO_CODECS.some((item) => item.value === profile.video_codec)) return "视频编码不受支持";
  if (!MEDIA_AUDIO_CODECS.some((item) => item.value === profile.audio_codec)) return "音频编码不受支持";
  if (profile.audio_sample_rate_hz !== 8000 && profile.audio_sample_rate_hz !== 16000) {
    return "音频采样率必须是 8000 或 16000 Hz";
  }
  return null;
}

export function mediaProfileSummary(profile: MediaProfile): string {
  const audioRate = effectiveAudioSampleRate(profile);
  const videoCodec = profile.video_codec === "h264" ? "H.264" : "H.265";
  const audioCodec = profile.audio_codec === "g711_a"
    ? "G.711A"
    : profile.audio_codec === "g711_u" ? "G.711U" : "AAC";
  return `${profile.width}×${profile.height} · ${profile.video_fps} FPS · ${profile.bitrate_kbps} kbps · GOP ${profile.keyframe_interval_seconds}s · ${videoCodec} / ${audioCodec} · ${audioRate / 1000} kHz`;
}

export function buildLiveSourceUri(mode: "camera" | "screen", index: number, audioSource = "none"): string {
  return `live:${mode}:${index}?audio=${encodeURIComponent(audioSource)}`;
}

export function liveSourceAudio(source: string): string {
  if (!source.startsWith("live:")) return "none";
  const query = source.split("?", 2)[1] ?? "";
  const value = new URLSearchParams(query).get("audio");
  return value?.trim() || "none";
}
