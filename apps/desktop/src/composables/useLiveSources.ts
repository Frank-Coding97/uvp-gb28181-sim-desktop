// 单设备控制台的媒体源状态(模块级单例):媒体模式、采集设备清单、编码参数与采集自检。
// 从 views/Device.vue 抽出,让 DevicePreview / MediaSourcePanel 共用同一份状态,
// 组件只做渲染与提示,业务逻辑集中在这里(URI 组装/校验的纯函数在 ../lib/liveSource.ts)。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  DEFAULT_SCREEN_PROFILE,
  SCREEN_RESOLUTIONS,
  buildCameraUri,
  buildScreenUri,
  inferMediaMode,
  parseCameraSource,
  parseScreenSource,
  validateVideoProfile,
  type AudioCodec,
  type MediaMode,
  type ScreenAudioSource,
  type ScreenCodec,
} from "../lib/liveSource";
import { persistForm, useDevice } from "../device";

export interface LiveAvDevice {
  index: number;
  name: string;
}
export interface LiveScreenDevice {
  display_id: number;
  width: number;
  height: number;
  name: string;
  uri: string;
}
export interface LiveSourceCatalog {
  ffmpeg_available: boolean;
  aac_available: boolean;
  opus_available: boolean;
  screens: LiveScreenDevice[];
  cameras: LiveAvDevice[];
  microphones: LiveAvDevice[];
  screen_error: string | null;
  avfoundation_error: string | null;
}
export interface LiveProbeResult {
  uri: string;
  video_ready: boolean;
  audio_requested: boolean;
  audio_ready: boolean | null;
  message: string;
}
export type ProbeState = {
  kind: "loading" | "success" | "warning" | "error";
  message: string;
};
/** 选文件结果:调用方负责弹提示(composable 不依赖 naive-ui 的 message)。 */
export type PickResult =
  | { ok: true; path: string | null }
  | { ok: false; error: string };
/** 采集自检结果:ready 表示视频就绪且(未请求音频或音频也就绪)。 */
export type ProbeOutcome = { ready: boolean; message: string };

const { form, startDisabled } = useDevice();

const initialSource = form.value.video_source.trim();
const cameraSource = parseCameraSource(initialSource);
const screenSource = parseScreenSource(initialSource);
// 初始编码档取自当前 URI(摄像头或屏幕),两者都不是时用默认档。
const initialProfile = cameraSource ?? screenSource ?? DEFAULT_SCREEN_PROFILE;

const mediaMode = ref<MediaMode>(inferMediaMode(initialSource));
const selectedCameraIndex = ref<number | null>(cameraSource?.videoIndex ?? null);
const selectedMicrophone = ref(cameraSource?.audio ?? "none");
const selectedCameraAudioCodec = ref<AudioCodec>(cameraSource?.audioCodec ?? "g711a");
const selectedDisplayId = ref<number | null>(screenSource?.displayId ?? null);
const selectedScreenAudio = ref<ScreenAudioSource>(screenSource?.audio ?? "none");
const selectedScreenAudioCodec = ref<AudioCodec>(screenSource?.audioCodec ?? "g711a");
const selectedScreenWidth = ref(initialProfile.width);
const selectedScreenHeight = ref(initialProfile.height);
const selectedScreenResolution = ref(`${selectedScreenWidth.value}x${selectedScreenHeight.value}`);
const selectedScreenBitrate = ref(initialProfile.bitrateKbps);
const selectedScreenCodec = ref<ScreenCodec>(initialProfile.codec);
// GOP 与编码档一起进 URI;帧率则走 DeviceConfig.video_fps(form.video_fps),
// 因为它对文件源同样生效,不属于 live: URI 的一部分。
const selectedGop = ref(initialProfile.gop);
const liveCatalog = ref<LiveSourceCatalog>({
  ffmpeg_available: false,
  aac_available: false,
  opus_available: false,
  screens: [],
  cameras: [],
  microphones: [],
  screen_error: null,
  avfoundation_error: null,
});
const sourceLoading = ref(false);
const probeState = ref<ProbeState | null>(null);

/** 音频规格由编码格式决定(与 Rust 侧 LiveAudioCodec 一致),不是可自由设置的项。 */
export function audioSpecText(codec: AudioCodec): string {
  switch (codec) {
    case "g711a":
    case "g711u":
      return "8 kHz · 单声道 · 64 kbps · 20 ms(G.711 固定规格)";
    case "aac":
      return "48 kHz · 单声道 · 1024 采样/帧(码率由 AAC 编码器自适应)";
    case "opus":
      return "48 kHz · 单声道 · 20 ms 包(PS 承载为私有扩展)";
  }
}

function audioCodecAvailable(codec: AudioCodec): boolean {
  if (codec === "aac") return liveCatalog.value.aac_available;
  if (codec === "opus") return liveCatalog.value.opus_available;
  return true;
}

const selectedLiveAudioCodecAvailable = computed(() => {
  if (mediaMode.value === "camera") {
    return (
      selectedMicrophone.value === "none" || audioCodecAvailable(selectedCameraAudioCodec.value)
    );
  }
  if (mediaMode.value === "screen") {
    return selectedScreenAudio.value === "none" || audioCodecAvailable(selectedScreenAudioCodec.value);
  }
  return true;
});

const screenResolutionOptions = computed(() => {
  const current = selectedScreenResolution.value;
  if (SCREEN_RESOLUTIONS.some((item) => item.value === current)) return SCREEN_RESOLUTIONS;
  return [
    {
      value: current,
      label: `${selectedScreenWidth.value} × ${selectedScreenHeight.value}（已有配置）`,
    },
    ...SCREEN_RESOLUTIONS,
  ];
});

const screenProfileValidationError = computed(() =>
  validateVideoProfile({
    width: selectedScreenWidth.value,
    height: selectedScreenHeight.value,
    bitrateKbps: selectedScreenBitrate.value,
    codec: selectedScreenCodec.value,
    gop: selectedGop.value,
  })
);
const screenProfileValid = computed(() => screenProfileValidationError.value === null);

const canProbeSource = computed(
  () =>
    !sourceLoading.value &&
    !startDisabled.value &&
    selectedLiveAudioCodecAvailable.value &&
    (mediaMode.value === "camera" || mediaMode.value === "screen") &&
    screenProfileValid.value &&
    form.value.video_source.startsWith("live:")
);

/** 当前编码档(摄像头与屏幕共用同一组参数控件)。 */
function currentProfile() {
  return {
    width: selectedScreenWidth.value,
    height: selectedScreenHeight.value,
    bitrateKbps: selectedScreenBitrate.value,
    codec: selectedScreenCodec.value,
    gop: selectedGop.value,
  };
}

function syncLiveSourceUri() {
  probeState.value = null;
  if (mediaMode.value === "camera") {
    form.value.video_source =
      selectedCameraIndex.value === null || !screenProfileValid.value
        ? ""
        : buildCameraUri({
            videoIndex: selectedCameraIndex.value,
            audio: selectedMicrophone.value,
            audioCodec: selectedCameraAudioCodec.value,
            ...currentProfile(),
          });
  } else if (mediaMode.value === "screen") {
    form.value.video_source =
      selectedDisplayId.value === null || !screenProfileValid.value
        ? ""
        : buildScreenUri({
            displayId: selectedDisplayId.value,
            audio: selectedScreenAudio.value,
            audioCodec: selectedScreenAudioCodec.value,
            ...currentProfile(),
          });
  }
}

function applyScreenResolution() {
  const [width, height] = selectedScreenResolution.value.split("x").map(Number);
  selectedScreenWidth.value = width;
  selectedScreenHeight.value = height;
  syncLiveSourceUri();
}

function setMediaMode(mode: MediaMode) {
  mediaMode.value = mode;
  probeState.value = null;
  if (mode === "none") {
    form.value.video_source = "";
  } else if (mode === "file") {
    // 从实时源切回文件模式时清掉 live: URI,避免把 URI 当路径提交。
    if (form.value.video_source.startsWith("live:")) form.value.video_source = "";
  } else {
    syncLiveSourceUri();
  }
  persistForm();
}

/**
 * 选择本地 H.264/H.265/容器文件作视频源(FR-8:C 档真实码流)。
 *
 * 对话框由 **Rust 侧**弹出(命令 `pick_video_source`)而不是前端调 plugin-dialog:
 * asset 协议的预览授权必须绑定在"用户真的选了这个文件"这件事上。若由前端选完再让
 * Rust 放行一个前端传过来的路径,任何能执行 JS 的途径都能凭空换取任意文件读取能力。
 * 现在授权与选择在同一次 Rust 调用里完成,前端只拿到结果。
 */
async function pickVideoSource(): Promise<PickResult> {
  try {
    const picked = await invoke<string | null>("pick_video_source");
    if (typeof picked !== "string") return { ok: true, path: null };
    mediaMode.value = "file";
    form.value.video_source = picked;
    probeState.value = null;
    return { ok: true, path: picked };
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}

/** 枚举屏幕/摄像头/麦克风,并把当前选择对齐到可用设备。 */
async function refreshLiveSources(): Promise<{ ok: boolean; error?: string }> {
  sourceLoading.value = true;
  probeState.value = null;
  try {
    const catalog = await invoke<LiveSourceCatalog>("list_live_sources");
    liveCatalog.value = catalog;
    if (selectedCameraIndex.value === null) {
      selectedCameraIndex.value = catalog.cameras[0]?.index ?? null;
    }
    if (selectedDisplayId.value === null) {
      selectedDisplayId.value = catalog.screens[0]?.display_id ?? null;
    }
    if (mediaMode.value === "camera" || mediaMode.value === "screen") syncLiveSourceUri();
    return { ok: true };
  } catch (e) {
    const error = `设备枚举失败：${String(e)}`;
    probeState.value = { kind: "error", message: error };
    return { ok: false, error };
  } finally {
    sourceLoading.value = false;
  }
}

/** 真正拉起后端采集管道探测音视频数据,结果同时写入 probeState 供面板展示。 */
async function probeLiveSource(): Promise<ProbeOutcome | null> {
  if (!canProbeSource.value) return null;
  probeState.value = { kind: "loading", message: "正在启动后端采集管道并探测音视频数据…" };
  try {
    const result = await invoke<LiveProbeResult>("probe_live_source", {
      uri: form.value.video_source,
    });
    const ready = result.video_ready && (!result.audio_requested || result.audio_ready === true);
    probeState.value = { kind: ready ? "success" : "warning", message: result.message };
    return { ready, message: result.message };
  } catch (e) {
    const message = `采集测试失败：${String(e)}`;
    probeState.value = { kind: "error", message };
    return { ready: false, message };
  }
}

export function useLiveSources() {
  return {
    // 状态
    mediaMode,
    liveCatalog,
    sourceLoading,
    probeState,
    selectedCameraIndex,
    selectedMicrophone,
    selectedCameraAudioCodec,
    selectedDisplayId,
    selectedScreenAudio,
    selectedScreenAudioCodec,
    selectedScreenWidth,
    selectedScreenHeight,
    selectedScreenResolution,
    selectedScreenBitrate,
    selectedScreenCodec,
    selectedGop,
    // 派生
    screenResolutionOptions,
    screenProfileValidationError,
    screenProfileValid,
    selectedLiveAudioCodecAvailable,
    canProbeSource,
    // 动作
    applyScreenResolution,
    syncLiveSourceUri,
    setMediaMode,
    pickVideoSource,
    refreshLiveSources,
    probeLiveSource,
  };
}
