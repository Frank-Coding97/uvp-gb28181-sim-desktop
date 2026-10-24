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
  suggestBitrateKbps,
  validateVideoProfile,
  type AudioCodec,
  type MediaMode,
  type ScreenAudioSource,
  type ScreenCodec,
  type ShareTarget,
} from "../lib/liveSource";
import { persistForm, useDevice } from "../device";
import { formatError } from "../lib/ipcError";

// 类型来自 Rust 侧 ts-rs 生成(见 src/types/generated),别名保持既有导入路径稳定。
import type { LiveAvDeviceDto } from "../types/generated/LiveAvDeviceDto";
import type { LiveScreenDto } from "../types/generated/LiveScreenDto";
import type { LiveSourceCatalogDto } from "../types/generated/LiveSourceCatalogDto";
import type { ShareTargetDto } from "../types/generated/ShareTargetDto";

type LiveAvDevice = LiveAvDeviceDto;
type LiveScreenDevice = LiveScreenDto;
type LiveSourceCatalog = LiveSourceCatalogDto;
export type { LiveAvDevice, LiveScreenDevice, LiveSourceCatalog, ShareTargetDto };

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
const selectedDisplayId = ref<number | null>(
  screenSource?.target.kind === "display" ? screenSource.target.displayId : null
);
// 采集目标:整屏(默认)或系统选择器选定的窗口/应用。窗口/应用没有可枚举的 id 列表,
// 只能来自系统选择器,所以这里存的是"身份",不是当前是否存在。
const selectedShareTarget = ref<ShareTarget | null>(screenSource?.target ?? null);
/** 系统选择器给出的展示标签;重启后无法从 URI 还原,只在本次会话内有效。 */
const shareTargetLabel = ref<string | null>(null);
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
/** 系统选择器是否正在等待用户选择(防止连点弹出多个系统窗口)。 */
const sharePickerBusy = ref(false);

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
    // 目标优先级:系统选择器选定的窗口/应用 > 显示器下拉框。
    // 窗口/应用目标时显示器下拉框不参与(采集范围由系统选择器决定)。
    const target =
      selectedShareTarget.value ??
      (selectedDisplayId.value === null
        ? null
        : ({ kind: "display", displayId: selectedDisplayId.value } as ShareTarget));
    form.value.video_source =
      target === null || !screenProfileValid.value
        ? ""
        : buildScreenUri({
            target,
            audio: selectedScreenAudio.value,
            audioCodec: selectedScreenAudioCodec.value,
            ...currentProfile(),
          });
  }
}

/** 采集目标是否已由系统选择器锁定到某个窗口/应用(此时显示器下拉框不适用)。 */
const shareTargetPinned = computed(
  () =>
    mediaMode.value === "screen" &&
    selectedShareTarget.value !== null &&
    selectedShareTarget.value.kind !== "display"
);

/** 采集目标的人类可读描述(界面展示用)。 */
const shareTargetText = computed(() => {
  const target = selectedShareTarget.value;
  if (mediaMode.value !== "screen" || target === null) return "未选择";
  if (target.kind === "display") {
    const screen = liveCatalog.value.screens.find(
      (item) => item.display_id === target.displayId
    );
    return `整块显示器 · ${screen?.name ?? `Display ${target.displayId}`}`;
  }
  const kind = target.kind === "window" ? "窗口" : "应用";
  if (shareTargetLabel.value) return `${kind} · ${shareTargetLabel.value}`;
  // 重启后标签丢失:URI 只带稳定身份,这里如实显示身份而不是编一个标题。
  return target.kind === "window"
    ? `${kind} #${target.windowId}(标题需重新选择才能显示)`
    : `${kind} ${target.bundleId}`;
});

/** 把当前目标切回整块显示器(清除窗口/应用目标)。 */
function useDisplayTarget() {
  selectedShareTarget.value =
    selectedDisplayId.value === null
      ? null
      : { kind: "display", displayId: selectedDisplayId.value };
  shareTargetLabel.value = null;
  syncLiveSourceUri();
  persistForm();
}

/**
 * 弹出 macOS 系统内容选择器,让用户按「显示器 / 窗口 / 应用」挑真正要推的画面。
 *
 * 这一步只确定**目标身份**;真正采集由后端 ScreenCaptureKit 按该身份重建过滤器,
 * 所以每次平台 INVITE 重建采集管道时目标都不会丢。
 */
async function pickShareTarget(): Promise<{ ok: boolean; label?: string; error?: string }> {
  if (sharePickerBusy.value) {
    return { ok: false, error: "系统选择器已经打开,请先在系统窗口里完成选择" };
  }
  sharePickerBusy.value = true;
  try {
    const picked = await invoke<ShareTargetDto | null>("pick_share_target");
    if (!picked) return { ok: true }; // 用户取消,不是错误
    const target: ShareTarget =
      picked.kind === "display"
        ? { kind: "display", displayId: picked.display_id ?? 0 }
        : picked.kind === "window"
          ? { kind: "window", windowId: picked.window_id ?? 0 }
          : { kind: "application", bundleId: picked.bundle_id ?? "" };
    if (
      (target.kind === "display" && target.displayId <= 0) ||
      (target.kind === "window" && target.windowId <= 0) ||
      (target.kind === "application" && !target.bundleId)
    ) {
      return { ok: false, error: "系统选择器返回了无法识别的采集目标" };
    }
    selectedShareTarget.value = target;
    shareTargetLabel.value = picked.label;
    if (target.kind === "display") selectedDisplayId.value = target.displayId;
    syncLiveSourceUri();
    persistForm();
    return { ok: true, label: picked.label };
  } catch (e) {
    return { ok: false, error: formatError(e, "系统选择器失败") };
  } finally {
    sharePickerBusy.value = false;
  }
}

function applyScreenResolution() {
  const [width, height] = selectedScreenResolution.value.split("x").map(Number);
  selectedScreenWidth.value = width;
  selectedScreenHeight.value = height;
  autoFillBitrate();
}

/**
 * 按当前分辨率/帧率/GOP 重算推荐码率并覆盖码率输入。
 *
 * 触发点:切换分辨率、改帧率、改 GOP —— 这三个变化都直接改变"每帧要多少比特",
 * 尤其 GOP 调小(全 I 帧倾向)时码率不跟上会直接灰闪(见 suggestBitrateKbps 的注释)。
 * 用户随后仍可手改;下次任一参数再变时会重新推荐覆盖。
 */
function autoFillBitrate() {
  selectedScreenBitrate.value = suggestBitrateKbps(
    selectedScreenWidth.value,
    selectedScreenHeight.value,
    form.value.video_fps ?? 25,
    selectedGop.value
  );
  syncLiveSourceUri();
  persistForm();
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
    return { ok: false, error: formatError(e, "选择文件失败") };
  }
}

/** 枚举屏幕/摄像头/麦克风,并把当前选择对齐到可用设备。 */
async function refreshLiveSources(): Promise<{ ok: boolean; error?: string }> {
  sourceLoading.value = true;
  probeState.value = null;
  try {
    const catalog = await invoke<LiveSourceCatalog>("list_live_sources");
    liveCatalog.value = {
      ...catalog,
      screen_error: catalog.screen_error
        ? formatError(catalog.screen_error, "屏幕枚举失败")
        : null,
      avfoundation_error: catalog.avfoundation_error
        ? formatError(catalog.avfoundation_error, "采集设备枚举失败")
        : null,
    };
    if (selectedCameraIndex.value === null) {
      selectedCameraIndex.value = catalog.cameras[0]?.index ?? null;
    }
    if (selectedDisplayId.value === null) {
      selectedDisplayId.value = catalog.screens[0]?.display_id ?? null;
    }
    if (mediaMode.value === "camera" || mediaMode.value === "screen") syncLiveSourceUri();
    return { ok: true };
  } catch (e) {
    const error = formatError(e, "设备枚举失败");
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
    const message = formatError(e, "采集测试失败");
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
    selectedShareTarget,
    shareTargetLabel,
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
    shareTargetPinned,
    shareTargetText,
    sharePickerBusy,
    screenProfileValidationError,
    screenProfileValid,
    selectedLiveAudioCodecAvailable,
    canProbeSource,
    // 动作
    applyScreenResolution,
    autoFillBitrate,
    pickShareTarget,
    useDisplayTarget,
    syncLiveSourceUri,
    setMediaMode,
    pickVideoSource,
    refreshLiveSources,
    probeLiveSource,
  };
}
