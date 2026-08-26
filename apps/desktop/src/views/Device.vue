<script setup lang="ts">
// 单设备联调控制台(UC-1),高保真对齐参考原型 frost-blue:
// 顶部 4 指标卡 + 双栏配置卡(SIP 服务器 / 设备身份)。
import { ref, onMounted, onUnmounted, onActivated, computed, watch, nextTick } from "vue";
import { NButton, useMessage } from "naive-ui";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import ThreePtzCamera from "../components/ThreePtzCamera.vue";

const message = useMessage();

// 选择本地 H.264 文件作视频源(FR-8:C 档真实码流)。
async function pickVideoSource() {
  try {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] },
      ],
    });
    if (typeof picked === "string") {
      mediaMode.value = "file";
      form.value.video_source = picked;
      probeState.value = null;
      refreshVideoPreview(true);
    }
  } catch (e) {
    message.error("选择文件失败:" + String(e));
  }
}

// 平台连接参数(server_host/port/domain/password/transport)来自顶栏全局平台档案,
// 本页只管设备自身参数(设备 ID/版本/通道/视频源/模板)。
import { useDevice, persistForm } from "../device";

// 设备配置/状态/注册逻辑来自共享 store(与顶栏共用同一份)。
const {
  form, startDisabled, canReport, reconcile,
} = useDevice();

type MediaMode = "none" | "file" | "camera" | "screen";
type ScreenCodec = "h264" | "h265";
type AudioCodec = "g711a" | "g711u" | "aac" | "opus";
function isAudioCodec(value: string): value is AudioCodec {
  return value === "g711a" || value === "g711u" || value === "aac" || value === "opus";
}
type ScreenAudioSource = "none" | "system" | `microphone:${number}`;
const SCREEN_RESOLUTIONS = [
  { value: "640x360", label: "640 × 360（360p）" },
  { value: "640x480", label: "640 × 480（标清 4:3）" },
  { value: "1280x720", label: "1280 × 720（720p）" },
  { value: "1920x1080", label: "1920 × 1080（1080p）" },
  { value: "2560x1440", label: "2560 × 1440（2K）" },
  { value: "3840x2160", label: "3840 × 2160（4K）" },
] as const;
const DEFAULT_SCREEN_PROFILE = {
  width: 1280,
  height: 720,
  bitrateKbps: 2500,
  codec: "h264" as const,
};
const MIN_SCREEN_DIMENSION = 160;
const MAX_SCREEN_LONG_SIDE = 3840;
const MAX_SCREEN_SHORT_SIDE = 2160;
const MIN_SCREEN_BITRATE_KBPS = 128;
const MAX_SCREEN_BITRATE_KBPS = 20_000;

interface LiveAvDevice { index: number; name: string; }
interface LiveScreenDevice {
  display_id: number; width: number; height: number; name: string; uri: string;
}
interface LiveSourceCatalog {
  ffmpeg_available: boolean;
  aac_available: boolean;
  opus_available: boolean;
  screens: LiveScreenDevice[];
  cameras: LiveAvDevice[];
  microphones: LiveAvDevice[];
  screen_error: string | null;
  avfoundation_error: string | null;
}
interface LiveProbeResult {
  uri: string;
  video_ready: boolean;
  audio_requested: boolean;
  audio_ready: boolean | null;
  message: string;
}
type ProbeState = { kind: "loading" | "success" | "warning" | "error"; message: string };

interface ParsedCameraSource {
  videoIndex: number;
  audio: string;
  audioCodec: AudioCodec;
}
interface ParsedScreenSource {
  displayId: number;
  audio: ScreenAudioSource;
  audioCodec: AudioCodec;
  width: number;
  height: number;
  bitrateKbps: number;
  codec: ScreenCodec;
}

function parseCameraSource(source: string): ParsedCameraSource | null {
  const match = source.match(/^live:camera:(\d+)\?(.+)$/);
  if (!match) return null;
  const videoIndex = Number(match[1]);
  const params = new URLSearchParams(match[2]);
  const audio = params.get("audio");
  const audioCodec = params.get("audio_codec") ?? "g711a";
  if (!Number.isSafeInteger(videoIndex) || videoIndex < 0 ||
      (audio !== "none" && !/^\d+$/.test(audio ?? "")) ||
      !isAudioCodec(audioCodec)) return null;
  return { videoIndex, audio: audio!, audioCodec };
}

function parseScreenNumber(value: string | null, fallback: number): number {
  if (value === null) return fallback;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : fallback;
}

function parseScreenSource(source: string): ParsedScreenSource | null {
  const match = source.match(/^live:screen:(\d+)\?(.+)$/);
  if (!match) return null;
  const displayId = Number(match[1]);
  if (!Number.isSafeInteger(displayId) || displayId <= 0) return null;
  const params = new URLSearchParams(match[2]);
  const audio = params.get("audio");
  const audioCodec = params.get("audio_codec") ?? "g711a";
  const codec = params.get("codec");
  const validAudio = audio === "system" || audio === "none" || /^microphone:\d+$/.test(audio ?? "");
  if (!validAudio ||
      !isAudioCodec(audioCodec) ||
      (codec !== null && codec !== "h264" && codec !== "h265")) return null;
  return {
    displayId,
    audio: audio as ScreenAudioSource,
    audioCodec,
    width: parseScreenNumber(params.get("width"), DEFAULT_SCREEN_PROFILE.width),
    height: parseScreenNumber(params.get("height"), DEFAULT_SCREEN_PROFILE.height),
    bitrateKbps: parseScreenNumber(params.get("bitrate"), DEFAULT_SCREEN_PROFILE.bitrateKbps),
    codec: codec === "h265" ? "h265" : "h264",
  };
}

const initialSource = form.value.video_source.trim();
const cameraSource = parseCameraSource(initialSource);
const screenSource = parseScreenSource(initialSource);
const mediaMode = ref<MediaMode>(
  cameraSource ? "camera"
    : screenSource || initialSource === "live:0" || initialSource === "live:ffmpeg:0" ? "screen"
      : "file"
);
const selectedCameraIndex = ref<number | null>(cameraSource?.videoIndex ?? null);
const selectedMicrophone = ref(cameraSource?.audio ?? "none");
const selectedCameraAudioCodec = ref<AudioCodec>(cameraSource?.audioCodec ?? "g711a");
const selectedDisplayId = ref<number | null>(screenSource?.displayId ?? null);
const selectedScreenAudio = ref<ScreenAudioSource>(screenSource?.audio ?? "none");
const selectedScreenAudioCodec = ref<AudioCodec>(screenSource?.audioCodec ?? "g711a");
const selectedScreenWidth = ref(screenSource?.width ?? DEFAULT_SCREEN_PROFILE.width);
const selectedScreenHeight = ref(screenSource?.height ?? DEFAULT_SCREEN_PROFILE.height);
const selectedScreenResolution = ref(`${selectedScreenWidth.value}x${selectedScreenHeight.value}`);
const selectedScreenBitrate = ref(screenSource?.bitrateKbps ?? DEFAULT_SCREEN_PROFILE.bitrateKbps);
const selectedScreenCodec = ref<ScreenCodec>(screenSource?.codec ?? DEFAULT_SCREEN_PROFILE.codec);
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

function audioCodecAvailable(codec: AudioCodec): boolean {
  if (codec === "aac") return liveCatalog.value.aac_available;
  if (codec === "opus") return liveCatalog.value.opus_available;
  return true;
}

const selectedLiveAudioCodecAvailable = computed(() => {
  if (mediaMode.value === "camera") {
    return selectedMicrophone.value === "none" ||
      audioCodecAvailable(selectedCameraAudioCodec.value);
  }
  if (mediaMode.value === "screen") {
    return selectedScreenAudio.value === "none" ||
      audioCodecAvailable(selectedScreenAudioCodec.value);
  }
  return true;
});


const screenResolutionOptions = computed(() => {
  const current = selectedScreenResolution.value;
  if (SCREEN_RESOLUTIONS.some((item) => item.value === current)) return SCREEN_RESOLUTIONS;
  return [{ value: current, label: `${selectedScreenWidth.value} × ${selectedScreenHeight.value}（已有配置）` }, ...SCREEN_RESOLUTIONS];
});

function applyScreenResolution() {
  const [width, height] = selectedScreenResolution.value.split("x").map(Number);
  selectedScreenWidth.value = width;
  selectedScreenHeight.value = height;
  syncLiveSourceUri();
}

const screenProfileValidationError = computed(() => {
  const width = selectedScreenWidth.value;
  const height = selectedScreenHeight.value;
  const bitrateKbps = selectedScreenBitrate.value;
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height)) {
    return "分辨率必须是整数";
  }
  if (width % 2 !== 0 || height % 2 !== 0) {
    return "分辨率宽高必须为偶数";
  }
  if (width < MIN_SCREEN_DIMENSION || height < MIN_SCREEN_DIMENSION) {
    return `分辨率单边不能小于 ${MIN_SCREEN_DIMENSION}`;
  }
  if (Math.max(width, height) > MAX_SCREEN_LONG_SIDE || Math.min(width, height) > MAX_SCREEN_SHORT_SIDE) {
    return "分辨率超过 OpenH264 支持的 3840×2160 边界";
  }
  if (!Number.isSafeInteger(bitrateKbps) || bitrateKbps < MIN_SCREEN_BITRATE_KBPS || bitrateKbps > MAX_SCREEN_BITRATE_KBPS) {
    return `码率必须在 ${MIN_SCREEN_BITRATE_KBPS}–${MAX_SCREEN_BITRATE_KBPS} kbps 之间`;
  }
  if (selectedScreenCodec.value !== "h264" && selectedScreenCodec.value !== "h265") {
    return "编码格式必须为 H.264 或 H.265";
  }
  return null;
});
const screenProfileValid = computed(() => screenProfileValidationError.value === null);

const selectedScreenFps = ref(25);
const selectedScreenGop = ref(50);
const selectedAudioSampleRate = ref(8000);
const selectedAudioChannels = ref(1);
const selectedAudioBitrate = ref(64);
const videoFitMode = ref<"contain" | "cover">("contain");

function toggleVideoFitMode() {
  videoFitMode.value = videoFitMode.value === "contain" ? "cover" : "contain";
}

function syncLiveSourceUri() {
  probeState.value = null;
  if (mediaMode.value === "camera") {
    form.value.video_source = selectedCameraIndex.value === null
      ? ""
      : `live:camera:${selectedCameraIndex.value}?audio=${selectedMicrophone.value}&audio_codec=${selectedCameraAudioCodec.value}`;
  } else if (mediaMode.value === "screen") {
    form.value.video_source = selectedDisplayId.value === null || !screenProfileValid.value
      ? ""
      : `live:screen:${selectedDisplayId.value}?audio=${selectedScreenAudio.value}&audio_codec=${selectedScreenAudioCodec.value}&width=${selectedScreenWidth.value}&height=${selectedScreenHeight.value}&bitrate=${selectedScreenBitrate.value}&codec=${selectedScreenCodec.value}`;
  }
}

function setMediaMode(mode: MediaMode) {
  mediaMode.value = mode;
  probeState.value = null;
  if (mode === "none") {
    form.value.video_source = "";
    stopPreviewStream();
  } else if (mode === "file") {
    if (form.value.video_source.startsWith("live:")) form.value.video_source = "";
  } else {
    syncLiveSourceUri();
  }
  persistForm();
}

async function refreshLiveSources(notify = true) {
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
    if (notify) message.success("采集设备列表已刷新");
  } catch (e) {
    probeState.value = { kind: "error", message: `设备枚举失败：${String(e)}` };
  } finally {
    sourceLoading.value = false;
  }
}

const canProbeSource = computed(() =>
  !sourceLoading.value && !startDisabled.value && selectedLiveAudioCodecAvailable.value &&
  (mediaMode.value === "camera" || mediaMode.value === "screen") &&
  (mediaMode.value !== "screen" || screenProfileValid.value) &&
  form.value.video_source.startsWith("live:")
);



async function probeLiveSource() {
  if (!canProbeSource.value) return;
  probeState.value = { kind: "loading", message: "正在启动后端采集管道并探测音视频数据…" };
  const loadingMsg = message.loading("正在探测硬件采集通道与编码器…", { duration: 0 });
  try {
    const result = await invoke<LiveProbeResult>("probe_live_source", {
      uri: form.value.video_source,
    });
    loadingMsg.destroy();
    const ready = result.video_ready && (!result.audio_requested || result.audio_ready === true);
    probeState.value = {
      kind: ready ? "success" : "warning",
      message: result.message,
    };
    if (ready) {
      message.success(result.message);
      // 联动刷新画面预览
      refreshVideoPreview(true);
    } else {
      message.warning(result.message);
    }
  } catch (e) {
    loadingMsg.destroy();
    const errMsg = `采集测试失败：${String(e)}`;
    probeState.value = { kind: "error", message: errMsg };
    message.error(errMsg);
  }
}


const videoPreviewEl = ref<HTMLVideoElement | null>(null);
const previewUrl = ref("");
const previewState = ref<{ tone: "idle" | "live" | "warn"; text: string }>({ tone: "idle", text: "未选择媒体源" });
let previewStream: MediaStream | null = null;

const previewTitle = computed(() => {
  if (mediaMode.value === "file") return "本地文件预览";
  if (mediaMode.value === "camera") return "摄像头实时预览";
  if (mediaMode.value === "screen") return "屏幕实时预览";
  return "媒体预览";
});
const previewMeta = computed(() => {
  if (mediaMode.value === "none") return "当前仅做 SIP 信令联调";
  if (mediaMode.value === "file") return form.value.video_source ? "预览源文件；平台点播时由引擎转 PS/RTP 推送" : "选择 MP4/MOV 等容器文件可直接预览";
  if (mediaMode.value === "camera") return "预览使用 WebView 本地媒体流；实际推流使用后端 FFmpeg 采集";
  return "预览使用屏幕共享授权；实际推流使用后端 ScreenCaptureKit";
});

function stopPreviewStream() {
  if (previewStream) {
    previewStream.getTracks().forEach((track) => track.stop());
    previewStream = null;
  }
  if (videoPreviewEl.value) videoPreviewEl.value.srcObject = null;
}

function isLikelyPlayableFile(path: string) {
  return /\.(mp4|m4v|mov|webm|ogg)$/i.test(path);
}

async function refreshVideoPreview(auto = false) {
  await nextTick();
  stopPreviewStream();
  previewUrl.value = "";
  const video = videoPreviewEl.value;
  if (!video) return;
  if (mediaMode.value === "none") {
    previewState.value = { tone: "idle", text: "空媒体模式：不会发送视频画面" };
    return;
  }
  if (mediaMode.value === "file") {
    const path = form.value.video_source.trim();
    if (!path) {
      previewState.value = { tone: "idle", text: "请选择一个视频文件" };
      return;
    }
    if (!isLikelyPlayableFile(path)) {
      previewState.value = { tone: "warn", text: "裸 H.264/H.265 可推流，但浏览器无法直接预览" };
      return;
    }
    previewUrl.value = convertFileSrc(path);
    previewState.value = { tone: "live", text: "正在预览本地视频文件" };
    return;
  }
  if (!navigator.mediaDevices) {
    previewState.value = { tone: "warn", text: "当前 WebView 不支持本地媒体预览" };
    return;
  }
  try {
    previewState.value = { tone: "idle", text: "正在请求本地预览权限…" };
    if (mediaMode.value === "camera") {
      previewStream = await navigator.mediaDevices.getUserMedia({ video: true, audio: selectedMicrophone.value !== "none" });
    } else {
      previewStream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: selectedScreenAudio.value !== "none" });
    }
    video.srcObject = previewStream;
    await video.play().catch(() => undefined);
    previewState.value = { tone: "live", text: auto ? "实时预览已刷新" : "实时预览中" };
  } catch (e) {
    previewState.value = { tone: "warn", text: `预览未开启：${String(e)}` };
  }
}

watch(() => [mediaMode.value, form.value.video_source], () => {
  if (mediaMode.value === "file") refreshVideoPreview(true);
  else if (mediaMode.value === "none") { stopPreviewStream(); previewState.value = { tone: "idle", text: "空媒体模式：不会发送视频画面" }; previewUrl.value = ""; }
});

// OSD 配置状态:反映**平台下发的 OSD 配置命令**(国标 A.2.3.2.11),设备已按其设置。
// 非本地随意填写——osd_config 事件由后端在收到平台 DeviceConfig+OSDConfig 时推来。
const osd = ref<{ received: boolean; time_show: boolean; osd_show: boolean; at: string }>({
  received: false, time_show: false, osd_show: false, at: "",
});
const now = ref(new Date().toLocaleString("zh-CN", { hour12: false }));
let osdTimer: number | null = null;
let unlistenOsd: (() => void) | null = null;


async function fireAlarm() {
  try { message.success(await invoke<string>("fire_alarm", { description: "移动侦测报警" })); }
  catch (e) { message.error(String(e)); }
}
async function firePosition() {
  try { message.success(await invoke<string>("fire_position", { longitude: 116.397, latitude: 39.908 })); }
  catch (e) { message.error(String(e)); }
}

// 平台命令时间线 / 活跃订阅 / 长任务进度(展示后端语义事件)。
interface CmdEntry { kind: string; summary: string; ts_ms: number; }
const commands = ref<CmdEntry[]>([]);
const MAX_CMD = 100;
let unlistenCmd: (() => void) | null = null;

interface SubState { kind: string; active: boolean; notify_count: number; }
const subs = ref<Record<string, SubState>>({});
let unlistenSub: (() => void) | null = null;

interface TaskProgress { kind: string; current: number; total: number; percent: number; }
const progress = ref<TaskProgress | null>(null);
let unlistenProg: (() => void) | null = null;
let progHideTimer: number | null = null;

const subList = computed(() => Object.values(subs.value).filter((s) => s.active));
const kindLabel: Record<string, string> = {
  query: "查询", control: "控制", invite: "点播", broadcast: "广播",
  Catalog: "目录", Alarm: "报警", MobilePosition: "移动位置", PTZPosition: "PTZ精准位置",
  snapshot: "抓拍上传", upgrade: "在线升级",
};
function fmtCmdTs(ms: number): string {
  const d = new Date(ms);
  return d.toLocaleTimeString("zh-CN", { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0");
}

// SIP 信令追踪(FR-43):订阅 sip_trace 事件,滚动展示最近 N 条。
interface TraceEntry {
  ts_ms: number; direction: "in" | "out"; method: string;
  status?: number; cseq?: string; call_id?: string; peer: string; summary: string; raw?: string;
}
const traces = ref<TraceEntry[]>([]);
const traceOn = ref(true);
const traceDirectionFilter = ref<"all" | "in" | "out">("all");
const traceMethodFilter = ref("all");
const MAX_TRACE = 200;
const expandedTrace = ref<number | null>(null);
const traceMethodOptions = computed(() => {
  const methods = Array.from(new Set(traces.value.map((t) => t.method || "响应"))).sort();
  return ["all", ...methods];
});
const filteredTraces = computed(() => traces.value.filter((t) =>
  (traceDirectionFilter.value === "all" || t.direction === traceDirectionFilter.value) &&
  (traceMethodFilter.value === "all" || (t.method || "响应") === traceMethodFilter.value)
));
function traceTone(t: TraceEntry) {
  if (t.status && t.status >= 400 && t.status !== 401) return "error";
  if (t.status && t.status >= 300) return "warn";
  return "";
}
function toggleTraceRow(i: number) {
  expandedTrace.value = expandedTrace.value === i ? null : i;
}
function fmtTs(ms: number) {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2,"0")}:${String(d.getMinutes()).padStart(2,"0")}:${String(d.getSeconds()).padStart(2,"0")}`;
}
async function toggleTrace() {
  try {
    await invoke<string>("set_sip_trace", { enabled: traceOn.value });
  } catch (e) { message.error(String(e)); }
}
function clearTraces() { traces.value = []; expandedTrace.value = null; traceMethodFilter.value = "all"; traceDirectionFilter.value = "all"; }

// 云台控制可视化:订阅 ptz_action 事件,累积转动角度驱动 3D 球机。
interface PtzAction {
  up: boolean; down: boolean; left: boolean; right: boolean;
  zoom_in: boolean; zoom_out: boolean;
  pan_speed: number; tilt_speed: number; zoom_speed: number;
}
interface PtzPresetPayload { id: number; name: string; }
const PRESET_IDS = [1, 2, 3, 4, 5] as const;
const DEFAULT_PRESET_NAMES: Record<number, string> = {
  1: "大门入口",
  2: "停车场",
  3: "接待大厅",
  4: "东侧通道",
  5: "西侧通道",
};
const ptz = ref<PtzAction>({
  up: false, down: false, left: false, right: false,
  zoom_in: false, zoom_out: false, pan_speed: 0, tilt_speed: 0, zoom_speed: 0,
});
const ptzActive = computed(() =>
  ptz.value.up || ptz.value.down || ptz.value.left || ptz.value.right ||
  ptz.value.zoom_in || ptz.value.zoom_out);
const lastPtzAction = ref("STOP");
const lastPtzAt = ref("尚未收到平台 PTZ 命令");
const ptzButtons = computed(() => [
  { key: "up", label: "上", active: ptz.value.up },
  { key: "down", label: "下", active: ptz.value.down },
  { key: "left", label: "左", active: ptz.value.left },
  { key: "right", label: "右", active: ptz.value.right },
  { key: "zoom-in", label: "放大", active: ptz.value.zoom_in },
  { key: "zoom-out", label: "缩小", active: ptz.value.zoom_out },
  { key: "stop", label: "STOP", active: !ptzActive.value && !seeking.value },
]);

function ptzTimestamp(): string {
  return new Date().toLocaleTimeString("zh-CN", { hour12: false });
}

function describePtzAction(action: PtzAction): string {
  const labels: string[] = [];
  if (action.up) labels.push("上");
  if (action.down) labels.push("下");
  if (action.left) labels.push("左");
  if (action.right) labels.push("右");
  if (action.zoom_in) labels.push("放大");
  if (action.zoom_out) labels.push("缩小");
  return labels.length ? labels.join(" + ") : "STOP";
}

// 云台连续姿态：pan 保留累计角度，可连续跨越任意圈；tilt/zoom 保持物理边界。
const pan = ref(0);
const tilt = ref(0);   // 俯仰 -90~90（UI 模拟物理范围，并非国标强制机械限位）
const zoom = ref(1);   // 变倍 1~4
// 预置位目标位(演示用固定映射:每个预置位一个 pan/tilt/zoom)。
const PRESET_POS: Record<number, { pan: number; tilt: number; zoom: number }> = {
  1: { pan: -120, tilt: 20, zoom: 1 },
  2: { pan: 90, tilt: -15, zoom: 2 },
  3: { pan: 0, tilt: 30, zoom: 1.5 },
  4: { pan: 160, tilt: 0, zoom: 3 },
  5: { pan: -60, tilt: -30, zoom: 2.5 },
};
// 预置位巡航(转到目标)状态:目标 + 是否在补间。
const target = ref<{ pan: number; tilt: number; zoom: number } | null>(null);
const seeking = ref(false);
const activePreset = ref<number | null>(null);
const activePresetName = ref("");
const presetNames = ref<Record<number, string>>({ ...DEFAULT_PRESET_NAMES });

// 是否在方向转动(不含变焦,变焦不算"转动")。
const moving = computed(() => {
  const m = ptz.value;
  return m.up || m.down || m.left || m.right;
});
const zoomText = computed(() => {
  const value = `${zoom.value.toFixed(1)}×`;
  if (ptz.value.zoom_in) return `${value} · 放大中`;
  if (ptz.value.zoom_out) return `${value} · 缩小中`;
  return value;
});
const dirText = computed(() => {
  if (seeking.value && activePreset.value) {
    return `转向 ${activePresetName.value || `预置位 ${activePreset.value}`}`;
  }
  const m = ptz.value;
  const v = m.up ? "上" : m.down ? "下" : "";
  const h = m.left ? "左" : m.right ? "右" : "";
  return (v + h) || "—";
});
// 速度:方向转动时显示水平/垂直速度(0-255),变焦时显示变焦速度(0-15)。
const speedText = computed(() => {
  const m = ptz.value;
  if (m.zoom_in || m.zoom_out) return `变焦 ${m.zoom_speed}/15`;
  if (moving.value) {
    const parts: string[] = [];
    if (m.left || m.right) parts.push(`水平 ${m.pan_speed}`);
    if (m.up || m.down) parts.push(`垂直 ${m.tilt_speed}`);
    return parts.join(" · ") || "—";
  }
  return "—";
});
// 状态:变焦单独显示"变焦中",不与转动混淆。
const statusText = computed(() => {
  if (seeking.value) return "巡航中";
  if (moving.value) return "转动中";
  if (ptz.value.zoom_in || ptz.value.zoom_out) return "变焦中";
  return "STOP · 静止";
});
const normalizedPan = computed(() => ((pan.value % 360) + 360) % 360);
// 刻度盘反向旋转，保持固定指针显示当前绝对方位。
const compassDialStyle = computed(() => ({
  transform: `rotate(${(-normalizedPan.value).toFixed(1)}deg)`,
}));
const tiltMarkerStyle = computed(() => ({
  transform: `translateY(${((-tilt.value / 90) * 30).toFixed(1)}px)`,
}));

// 视野方位角文本(演示"摄像头当前朝向")。
const poseText = computed(() =>
  `方位 ${normalizedPan.value.toFixed(0).padStart(3, "0")}° · 累计 ${pan.value.toFixed(0)}° · 俯仰 ${tilt.value.toFixed(0)}° · ${zoom.value.toFixed(1)}×`);

function nearestEquivalentPan(heading: number): number {
  const normalizedTarget = ((heading % 360) + 360) % 360;
  const delta = ((normalizedTarget - normalizedPan.value + 540) % 360) - 180;
  return pan.value + delta;
}

// 60ms 动画帧:手动 PTZ 按速度连续改 pan/tilt/zoom;预置位巡航平滑补间到目标。
let poseTimer: number | null = null;
let ptzStopTimer: number | null = null;
function poseTick() {
  if (seeking.value && target.value) {
    const t = target.value;
    const ease = 0.08;
    pan.value += (t.pan - pan.value) * ease;
    tilt.value += (t.tilt - tilt.value) * ease;
    zoom.value += (t.zoom - zoom.value) * ease;
    if (Math.abs(t.pan - pan.value) < 0.5 && Math.abs(t.tilt - tilt.value) < 0.5 && Math.abs(t.zoom - zoom.value) < 0.02) {
      pan.value = t.pan; tilt.value = t.tilt; zoom.value = t.zoom;
      seeking.value = false; target.value = null;
    }
    return;
  }
  const m = ptz.value;
  const ps = (m.pan_speed / 255) * 4 + 1.5;
  const ts = (m.tilt_speed / 255) * 4 + 1.5;
  if (m.left)  pan.value -= ps;
  if (m.right) pan.value += ps;
  if (m.up)    tilt.value = Math.min(90, tilt.value + ts);
  if (m.down)  tilt.value = Math.max(-90, tilt.value - ts);
  if (m.zoom_in)  zoom.value = Math.min(4, zoom.value + 0.03);
  if (m.zoom_out) zoom.value = Math.max(1, zoom.value - 0.03);
}

// device_state 由 App.vue 统一订阅并写入共享状态,本页订阅 sip_trace + ptz_action + ptz_preset。
let unlistenTrace: UnlistenFn | null = null;
let unlistenPtz: UnlistenFn | null = null;
let unlistenPreset: UnlistenFn | null = null;
onMounted(async () => {
  unlistenPtz = await listen<PtzAction>("ptz_action", (e) => {
    const m = e.payload;
    const active = m.up || m.down || m.left || m.right || m.zoom_in || m.zoom_out;
    // 手动命令（包括显式 STOP）立即抢占预置位巡航，避免姿态补间吞掉平台控制。
    seeking.value = false;
    target.value = null;
    activePreset.value = null;
    activePresetName.value = "";
    // 收到显式停止(全 false)立即归零(WVP 松手会发 0x00 停止命令,设备已实时应答)。
    ptz.value = e.payload;
    lastPtzAction.value = describePtzAction(m);
    lastPtzAt.value = ptzTimestamp();
    if (ptzStopTimer) { clearTimeout(ptzStopTimer); ptzStopTimer = null; }
    // 仅作安全兜底:极少数平台松手不发停止命令时,3s 无新命令才自动归零。
    // 放宽到 3s 避免"按住时因平台重发间隔较长被误判停止"。
    if (active) {
      ptzStopTimer = window.setTimeout(() => {
        ptz.value = { up: false, down: false, left: false, right: false,
          zoom_in: false, zoom_out: false, pan_speed: 0, tilt_speed: 0, zoom_speed: 0 };
        lastPtzAction.value = "STOP（超时保护）";
        lastPtzAt.value = ptzTimestamp();
      }, 3000);
    }
  });
  // 预置位调用:使用设备按国标 PresetName 保存的名称，并平滑巡航到演示姿态。
  unlistenPreset = await listen<PtzPresetPayload>("ptz_preset", (e) => {
    const { id, name } = e.payload;
    if (ptzStopTimer) { clearTimeout(ptzStopTimer); ptzStopTimer = null; }
    ptz.value = { up: false, down: false, left: false, right: false,
      zoom_in: false, zoom_out: false, pan_speed: 0, tilt_speed: 0, zoom_speed: 0 };
    activePreset.value = id;
    activePresetName.value = name || `预置位${id}`;
    presetNames.value = { ...presetNames.value, [id]: activePresetName.value };
    const preset = PRESET_POS[id] ?? {
      pan: (id * 53) % 360, tilt: ((id * 37) % 180) - 90, zoom: 1 + (id % 3),
    };
    target.value = { ...preset, pan: nearestEquivalentPan(preset.pan) };
    lastPtzAction.value = `调用 ${activePresetName.value}（${id}）`;
    lastPtzAt.value = ptzTimestamp();
    seeking.value = true;
  });
  unlistenTrace = await listen<TraceEntry>("sip_trace", (e) => {
    if (!traceOn.value) return;
    traces.value.unshift(e.payload); // 新的在最上(倒序)
    if (traces.value.length > MAX_TRACE) traces.value.splice(MAX_TRACE);
    // 展开态锚在原条目上:新条目插到头部后,展开索引下移一位。
    if (expandedTrace.value !== null) expandedTrace.value += 1;
  });
  poseTimer = window.setInterval(poseTick, 60);
  osdTimer = window.setInterval(() => { now.value = new Date().toLocaleString("zh-CN", { hour12: false }); }, 1000);
  // 订阅平台下发的 OSD 配置命令(国标 A.2.3.2.11)。
  unlistenOsd = await listen<{ time_show: boolean; osd_show: boolean }>("osd_config", (e) => {
    osd.value = {
      received: true,
      time_show: e.payload.time_show,
      osd_show: e.payload.osd_show,
      at: new Date().toLocaleString("zh-CN", { hour12: false }),
    };
  });
  // 平台命令时间线。
  unlistenCmd = await listen<CmdEntry>("platform_command", (e) => {
    commands.value.unshift(e.payload);
    if (commands.value.length > MAX_CMD) commands.value.splice(MAX_CMD);
  });
  // 活跃订阅。
  unlistenSub = await listen<SubState>("subscription_state", (e) => {
    subs.value = { ...subs.value, [e.payload.kind]: e.payload };
  });
  // 长任务进度(抓拍/升级);完成后 3s 自动隐藏。
  unlistenProg = await listen<TaskProgress>("task_progress", (e) => {
    progress.value = e.payload;
    if (progHideTimer) clearTimeout(progHideTimer);
    if (e.payload.percent >= 100) {
      progHideTimer = window.setTimeout(() => { progress.value = null; }, 3000);
    }
  });
  await refreshLiveSources(false);
  refreshVideoPreview(true);
});
onUnmounted(() => {
  unlistenTrace?.();
  unlistenPtz?.();
  unlistenCmd?.();
  unlistenSub?.();
  unlistenProg?.();
  if (progHideTimer) clearTimeout(progHideTimer);
  unlistenPreset?.();
  unlistenOsd?.();
  if (poseTimer) clearInterval(poseTimer);
  if (osdTimer) clearInterval(osdTimer);
  if (ptzStopTimer) clearTimeout(ptzStopTimer);
  stopPreviewStream();
});

// keep-alive 激活时与引擎对账(store 内统一实现),避免切页后按钮态错乱。
onActivated(reconcile);

// 注册状态/在线时长/传输模式已上移到全局顶栏(App.vue),所有菜单页可见,本页不再重复展示。
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div class="page-title">单设备联调</div>
      <div class="page-sub">把本机模拟成一台国标下级设备,注册到上级平台并实时观察平台交互</div>
    </div>

    <!-- 1. 顶部：视频预览 与 云台控制 左右各占一半 (50% / 50%) -->
    <section class="top-media-ptz-grid">
      <!-- 左侧 50%：视频预览卡片 -->
      <aside class="glass-card panel preview-panel">
        <div class="preview-head">
          <div>
            <div class="panel-title">视频预览</div>
            <div class="preview-sub">{{ previewTitle }}</div>
          </div>
          <div class="preview-head-right">
            <button type="button" class="file-btn compact toggle-fit-btn" @click="toggleVideoFitMode" :title="videoFitMode === 'contain' ? '当前：保持比例适应；点击切换为完全铺满' : '当前：铺满裁剪；点击切换为保持比例'">
              {{ videoFitMode === 'cover' ? '模式：铺满' : '模式：适应' }}
            </button>
            <span class="preview-badge" :class="previewState.tone">{{ previewState.tone === 'live' ? 'PREVIEW' : previewState.tone === 'warn' ? 'CHECK' : 'IDLE' }}</span>
            <div class="preview-actions">
              <button type="button" class="file-btn compact primary" :disabled="mediaMode === 'none'" @click="refreshVideoPreview()">刷新</button>
              <button type="button" class="file-btn compact" :disabled="mediaMode !== 'file' || startDisabled" @click="pickVideoSource">选文件</button>
            </div>
          </div>
        </div>
        <div class="video-frame" :class="[previewState.tone, `fit-${videoFitMode}`]">
          <video
            ref="videoPreviewEl"
            :src="previewUrl || undefined"
            autoplay
            muted
            playsinline
            controls
            :class="`video-${videoFitMode}`"
          />
          <div v-if="!previewUrl && previewState.tone !== 'live'" class="preview-empty">
            <div class="scan-mark">GB</div>
            <strong>{{ previewState.text }}</strong>
            <span>{{ previewMeta }}</span>
          </div>
        </div>
        <div class="preview-status-line">
          <div class="preview-status" :class="previewState.tone">
            <i />
            <span>{{ previewState.text }}</span>
          </div>
          <span class="preview-note">本地预览画面（支持适应/铺满），推送到平台为标准 PS/RTP 流</span>
        </div>
      </aside>

      <!-- 右侧 50%：云台控制卡片 -->
      <div class="glass-card panel ptz-panel">
        <div class="ptz-head">
          <div class="panel-title">云台控制</div>
          <div class="ptz-sub">平台下发 PTZ 时机芯与光学变倍实时联动</div>
        </div>
        <div class="ptz-body">
          <div class="ptz-stage" :class="{ active: ptzActive, seeking }">
            <div class="stage-grid"></div>
            <!-- 左上角 PTZ LIVE 标识 -->
            <div class="stage-caption">
              <span class="live-dot"></span>
              <b>PTZ LIVE</b>
            </div>
            <!-- 右上角 对称呼应的运行状态胶囊 -->
            <div class="stage-caption stage-caption-right" :class="{ active: ptzActive, seeking }">
              <span class="motion-dot"></span>
              <b>{{ statusText }}</b>
            </div>

            <div class="compass-ring" aria-hidden="true">
              <div class="compass-dial" :style="compassDialStyle">
                <span class="north">N</span><span class="east">E</span><span class="south">S</span><span class="west">W</span>
              </div>
              <b class="heading-readout">{{ normalizedPan.toFixed(0).padStart(3, "0") }}°</b>
            </div>
            <div class="direction-hud" aria-hidden="true">
              <span class="hud-up" :class="{ on: ptz.up }">↑</span>
              <span class="hud-down" :class="{ on: ptz.down }">↓</span>
              <span class="hud-left" :class="{ on: ptz.left }">←</span>
              <span class="hud-right" :class="{ on: ptz.right }">→</span>
            </div>
            <div class="tilt-gauge" aria-hidden="true">
              <span class="tilt-plus">+90°</span><span class="tilt-zero">0°</span><span class="tilt-minus">−90°</span>
              <i class="tilt-track"></i>
              <b class="tilt-marker" :style="tiltMarkerStyle">{{ tilt.toFixed(0) }}°</b>
            </div>
            <ThreePtzCamera
              :pan="pan"
              :tilt="tilt"
              :zoom="zoom"
              :active="ptzActive"
              :seeking="seeking"
            />
            <div class="axis-label pan-axis">PAN 360°</div>
            <div class="axis-label tilt-axis">TILT ±90°</div>
          </div>
          <div class="ptz-info">
            <div class="ptz-stat-grid">
              <div class="ptz-stat"><span>朝向</span><b class="pose">{{ poseText }}</b></div>
              <div class="ptz-stat"><span>方向</span><b :class="{ hot: seeking || moving }">{{ dirText }}</b></div>
              <div class="ptz-stat"><span>变倍</span><b :class="{ hot: ptz.zoom_in || ptz.zoom_out }">{{ zoomText }}</b></div>
              <div class="ptz-stat"><span>速度</span><b :class="{ hot: moving || ptz.zoom_in || ptz.zoom_out }">{{ speedText }}</b></div>
            </div>
            <div class="ptz-stat action-state">
              <span>平台命令 · {{ lastPtzAt }}</span>
              <span class="action-chips">
                <b v-for="button in ptzButtons" :key="button.key" class="action-chip" :class="{ on: button.active, stop: button.key === 'stop' }">{{ button.label }}</b>
              </span>
            </div>
            <div class="ptz-stat presets">
              <span>预置位</span>
              <span class="preset-chips">
                <b v-for="id in PRESET_IDS" :key="id" class="pchip" :class="{ on: activePreset === id && seeking }">
                  <small>{{ id }}</small>{{ presetNames[id] }}
                </b>
              </span>
            </div>
          </div>
        </div>
      </div>
    </section>

    <!-- 2. 中间：设备身份与国标参数 卡片（全宽，紧凑空间） -->
    <section class="device-identity-section">
      <div class="glass-card panel identity-panel">
        <div class="panel-head-tight">
          <div class="panel-title">设备身份与国标参数</div>
          <span class="param-badge">GB/T 28181</span>
        </div>

        <!-- 国标基础与扩展参数：4 列网格 -->
        <div class="identity-grid">
          <div class="fg">
            <label>国标版本</label>
            <div class="seg">
              <button :class="{ on: form.gb_version === '2022' }" @click="form.gb_version = '2022'">GB/T 2022</button>
              <button :class="{ on: form.gb_version === '2016' }" @click="form.gb_version = '2016'">GB/T 2016</button>
            </div>
          </div>
          <div class="fg">
            <label>设备国标编号</label>
            <input v-model="form.device_id" class="inp" placeholder="20 位国标 ID" />
          </div>
          <div class="fg">
            <label>主通道名称</label>
            <input v-model="form.channel_name" class="inp" placeholder="通道名称" />
          </div>
          <div class="fg">
            <label>目录组织模板</label>
            <select v-model="form.catalog_template" class="inp">
              <option value="">单通道(默认)</option>
              <option value="nvr-8ch">8 通道 NVR</option>
              <option value="civil-3x2">跨区划(3 区 × 2 通道)</option>
              <option value="large-16ch">16 通道大型监控</option>
            </select>
          </div>
          <div class="fg">
            <label>设备厂商 (Manufacturer)</label>
            <input v-model="form.manufacturer" class="inp" placeholder="厂商名称" />
          </div>
          <div class="fg">
            <label>设备型号 (Model)</label>
            <input v-model="form.model" class="inp" placeholder="产品型号" />
          </div>
          <div class="fg">
            <label>固件版本 (Firmware)</label>
            <input v-model="form.firmware" class="inp" placeholder="固件版本号" />
          </div>
          <div class="fg">
            <label>注册有效期 (Expires · 秒)</label>
            <input v-model.number="form.expires" type="number" min="60" max="86400" step="60" class="inp" placeholder="3600" />
          </div>
          <div class="fg">
            <label>心跳周期 (Interval · 秒)</label>
            <input v-model.number="form.heartbeat_interval" type="number" min="5" max="300" step="5" class="inp" placeholder="60" />
          </div>
          <div class="fg">
            <label>心跳超时次数 (Max Timeout)</label>
            <input v-model.number="form.max_heartbeat_timeout" type="number" min="1" max="10" step="1" class="inp" placeholder="3" />
          </div>
        </div>

        <!-- 媒体源类型与操作行：下拉框 + 右侧上报操作（同行紧凑排布） -->
        <div class="media-control-bar">
          <div class="media-select-wrap">
            <label>媒体源类型</label>
            <select v-model="mediaMode" :disabled="startDisabled" class="inp media-mode-select" @change="setMediaMode(mediaMode)">
              <option value="file">视频文件（循环推送本地视频）</option>
              <option value="screen">电脑屏幕实时画面（ScreenCaptureKit 原生采集）</option>
              <option value="camera">电脑摄像头（含 iPhone 连续互通）</option>
              <option value="none">无媒体（仅信令联调）</option>
            </select>
          </div>
          <div class="media-actions-wrap">
            <button v-if="mediaMode === 'camera' || mediaMode === 'screen'" type="button" class="file-btn compact" :disabled="startDisabled || sourceLoading" @click="refreshLiveSources()">
              {{ sourceLoading ? '刷新中…' : '刷新设备' }}
            </button>
            <button v-if="mediaMode === 'camera' || mediaMode === 'screen'" type="button" class="file-btn compact primary" :disabled="!canProbeSource || probeState?.kind === 'loading'" @click="probeLiveSource">
              {{ probeState?.kind === 'loading' ? '测试中…' : '测试采集' }}
            </button>
            <n-button size="small" tertiary :disabled="!canReport" @click="fireAlarm">上报报警</n-button>
            <n-button size="small" tertiary :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
          </div>
        </div>

        <!-- 1. 视频文件模式：仅显示文件选择行，音视频参数隐藏 -->
        <div v-if="mediaMode === 'file'" class="source-config file-config-box">
          <div class="file-row">
            <input v-model="form.video_source" :disabled="startDisabled" class="inp" placeholder="选择 H.264/H.265 裸流或 MP4、MOV 等视频文件" />
            <button type="button" class="file-btn" :disabled="startDisabled" @click="pickVideoSource">选择文件</button>
          </div>
          <div class="fg-hint">容器文件会在设备启动前转封装；有音频轨时自动转为 G.711A 后与视频复用。</div>
        </div>

        <!-- 2. 摄像头模式：显示支持修改的音视频参数 -->
        <div v-else-if="mediaMode === 'camera'" class="source-config">
          <div class="source-capability">
            <span class="cap-badge" :class="liveCatalog.ffmpeg_available ? 'ok' : 'warn'">
              FFmpeg {{ liveCatalog.ffmpeg_available ? '可用' : '未找到' }}
            </span>
            <span>AVFoundation 摄像头与音频采集</span>
          </div>
          <!-- 左右分栏：左半列摄像头视频参数，右半列音频参数 -->
          <div class="media-param-columns">
            <div class="param-col">
              <div class="col-title">🎥 摄像头视频参数</div>
              <div class="fg">
                <label>视频采集设备</label>
                <select v-model.number="selectedCameraIndex" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option v-if="!liveCatalog.cameras.length && selectedCameraIndex === null" :value="null" disabled>未发现摄像头</option>
                  <option v-if="selectedCameraIndex !== null && !liveCatalog.cameras.some(item => item.index === selectedCameraIndex)" :value="selectedCameraIndex" disabled>
                    [{{ selectedCameraIndex }}] 当前摄像头不可用
                  </option>
                  <option v-for="camera in liveCatalog.cameras" :key="camera.index" :value="camera.index">
                    [{{ camera.index }}] {{ camera.name }}
                  </option>
                </select>
              </div>
              <div class="fg">
                <label>输出分辨率</label>
                <select v-model="selectedScreenResolution" :disabled="startDisabled || sourceLoading" class="inp" @change="applyScreenResolution">
                  <option v-for="resolution in screenResolutionOptions" :key="resolution.value" :value="resolution.value">
                    {{ resolution.label }}
                  </option>
                </select>
              </div>
              <div class="fg">
                <label>视频帧率 (FPS)</label>
                <select v-model.number="selectedScreenFps" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="15">15 fps（低开销）</option>
                  <option :value="25">25 fps（国标推荐）</option>
                  <option :value="30">30 fps（标准）</option>
                  <option :value="60">60 fps（高流畅）</option>
                </select>
              </div>
              <div class="fg">
                <label>视频码率</label>
                <div class="screen-bitrate-input">
                  <input v-model.number="selectedScreenBitrate" :disabled="startDisabled || sourceLoading" class="inp" type="number" min="128" max="20000" step="50" aria-label="屏幕视频码率（kbps）" @change="syncLiveSourceUri" />
                  <span>kbps</span>
                </div>
              </div>
              <div class="fg">
                <label>视频编码格式</label>
                <select v-model="selectedScreenCodec" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option value="h264">H.264 / AVC（兼容性优先）</option>
                  <option value="h265">H.265 / HEVC（更高压缩率）</option>
                </select>
              </div>
              <div class="fg">
                <label>GOP 关键帧间隔</label>
                <select v-model.number="selectedScreenGop" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="25">25 帧 (1秒 GOP)</option>
                  <option :value="50">50 帧 (2秒 GOP · 推荐)</option>
                  <option :value="100">100 帧 (4秒 GOP)</option>
                </select>
              </div>
            </div>

            <div class="param-col">
              <div class="col-title">🎙 音频流参数</div>
              <div class="fg">
                <label>音频采集设备</label>
                <select v-model="selectedMicrophone" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option value="none">不采集音频（画面将没有声音）</option>
                  <option v-if="selectedMicrophone !== 'none' && !liveCatalog.microphones.some(item => String(item.index) === selectedMicrophone)" :value="selectedMicrophone" disabled>
                    [{{ selectedMicrophone }}] 当前音频设备不可用
                  </option>
                  <option v-for="mic in liveCatalog.microphones" :key="mic.index" :value="String(mic.index)">
                    [{{ mic.index }}] {{ mic.name }}
                  </option>
                </select>
              </div>
              <div class="fg">
                <label>音频编码格式</label>
                <select v-model="selectedCameraAudioCodec" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option value="g711a">G.711A / PCMA · 8 kHz</option>
                  <option value="g711u">G.711U / PCMU · 8 kHz</option>
                  <option value="aac" :disabled="!liveCatalog.aac_available">AAC-LC / ADTS · 48 kHz{{ liveCatalog.aac_available ? '' : '（不可用）' }}</option>
                  <option value="opus" :disabled="!liveCatalog.opus_available">Opus · 48 kHz（扩展{{ liveCatalog.opus_available ? '' : '，不可用' }}）</option>
                </select>
              </div>
              <div class="fg">
                <label>音频采样率 (Sample Rate)</label>
                <select v-model.number="selectedAudioSampleRate" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="8000">8 000 Hz（G.711 标准）</option>
                  <option :value="16000">16 000 Hz（宽带语音）</option>
                  <option :value="32000">32 000 Hz</option>
                  <option :value="44100">44 100 Hz（CD 级）</option>
                  <option :value="48000">48 000 Hz（专业级 AAC/Opus）</option>
                </select>
              </div>
              <div class="fg">
                <label>声道模式 (Channels)</label>
                <select v-model.number="selectedAudioChannels" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="1">单声道 (Mono · 默认)</option>
                  <option :value="2">双声道 (Stereo)</option>
                </select>
              </div>
              <div class="fg">
                <label>音频比特率 (Bitrate)</label>
                <select v-model.number="selectedAudioBitrate" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="32">32 kbps</option>
                  <option :value="64">64 kbps（G.711/AAC 推荐）</option>
                  <option :value="128">128 kbps（高音质）</option>
                </select>
              </div>
            </div>
          </div>

          <div v-if="liveCatalog.avfoundation_error" class="source-notice error">{{ liveCatalog.avfoundation_error }}</div>
        </div>

        <!-- 3. 屏幕录制模式：显示支持修改的音视频参数 -->
        <div v-else-if="mediaMode === 'screen'" class="source-config">
          <div class="source-capability">
            <span class="cap-badge ok">ScreenCaptureKit</span>
            <span>原生屏幕与系统音频采集</span>
          </div>
          <!-- 左右分栏：左半列屏幕视频参数，右半列音频参数 -->
          <div class="media-param-columns">
            <div class="param-col">
              <div class="col-title">🎥 屏幕画面参数</div>
              <div class="fg">
                <label>显示器选择</label>
                <select v-model.number="selectedDisplayId" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option v-if="!liveCatalog.screens.length && selectedDisplayId === null" :value="null" disabled>未发现可用显示器</option>
                  <option v-if="selectedDisplayId !== null && !liveCatalog.screens.some(item => item.display_id === selectedDisplayId)" :value="selectedDisplayId" disabled>
                    Display {{ selectedDisplayId }}（当前不可用）
                  </option>
                  <option v-for="screen in liveCatalog.screens" :key="screen.display_id" :value="screen.display_id">
                    {{ screen.name }}
                  </option>
                </select>
              </div>
              <div class="fg">
                <label>输出分辨率</label>
                <select v-model="selectedScreenResolution" :disabled="startDisabled || sourceLoading" class="inp" @change="applyScreenResolution">
                  <option v-for="resolution in screenResolutionOptions" :key="resolution.value" :value="resolution.value">
                    {{ resolution.label }}
                  </option>
                </select>
              </div>
              <div class="fg">
                <label>视频帧率 (FPS)</label>
                <select v-model.number="selectedScreenFps" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="15">15 fps（低开销）</option>
                  <option :value="25">25 fps（国标推荐）</option>
                  <option :value="30">30 fps（标准）</option>
                  <option :value="60">60 fps（高流畅）</option>
                </select>
              </div>
              <div class="fg">
                <label>视频码率</label>
                <div class="screen-bitrate-input">
                  <input v-model.number="selectedScreenBitrate" :disabled="startDisabled || sourceLoading" class="inp" type="number" min="128" max="20000" step="50" aria-label="屏幕视频码率（kbps）" @change="syncLiveSourceUri" />
                  <span>kbps</span>
                </div>
              </div>
              <div class="fg">
                <label>视频编码格式</label>
                <select v-model="selectedScreenCodec" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option value="h264">H.264 / AVC（兼容性优先）</option>
                  <option value="h265">H.265 / HEVC（更低带宽）</option>
                </select>
              </div>
              <div class="fg">
                <label>GOP 关键帧间隔</label>
                <select v-model.number="selectedScreenGop" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="25">25 帧 (1秒 GOP)</option>
                  <option :value="50">50 帧 (2秒 GOP · 推荐)</option>
                  <option :value="100">100 帧 (4秒 GOP)</option>
                </select>
              </div>
            </div>

            <div class="param-col">
              <div class="col-title">🎙 音频流参数</div>
              <div class="fg">
                <label>声音采集源</label>
                <select v-model="selectedScreenAudio" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                  <option value="system">系统播放声音</option>
                  <option v-if="selectedScreenAudio.startsWith('microphone:') && !liveCatalog.microphones.some(item => `microphone:${item.index}` === selectedScreenAudio)" :value="selectedScreenAudio" disabled>
                    麦克风 {{ selectedScreenAudio.slice('microphone:'.length) }}（当前不可用）
                  </option>
                  <option v-for="mic in liveCatalog.microphones" :key="`screen-mic-${mic.index}`" :value="`microphone:${mic.index}`">
                    麦克风 · [{{ mic.index }}] {{ mic.name }}
                  </option>
                  <option value="none">不采集声音</option>
                </select>
              </div>
              <div class="fg">
                <label>音频编码格式</label>
                <select v-model="selectedScreenAudioCodec" :disabled="startDisabled || sourceLoading" class="inp" aria-label="实时音频格式" @change="syncLiveSourceUri">
                  <option value="g711a">G.711A / PCMA · 8 kHz · 单声道</option>
                  <option value="g711u">G.711U / PCMU · 8 kHz · 单声道</option>
                  <option value="aac" :disabled="!liveCatalog.aac_available">AAC-LC / ADTS · 48 kHz · 单声道{{ liveCatalog.aac_available ? '' : '（不可用）' }}</option>
                  <option value="opus" :disabled="!liveCatalog.opus_available">Opus · 48 kHz · 20 ms（扩展{{ liveCatalog.opus_available ? '' : '，不可用' }}）</option>
                </select>
              </div>
              <div class="fg">
                <label>音频采样率 (Sample Rate)</label>
                <select v-model.number="selectedAudioSampleRate" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="8000">8 000 Hz（G.711 标准）</option>
                  <option :value="16000">16 000 Hz（宽带语音）</option>
                  <option :value="32000">32 000 Hz</option>
                  <option :value="44100">44 100 Hz（CD 级）</option>
                  <option :value="48000">48 000 Hz（专业级 AAC/Opus）</option>
                </select>
              </div>
              <div class="fg">
                <label>声道模式 (Channels)</label>
                <select v-model.number="selectedAudioChannels" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="1">单声道 (Mono · 默认)</option>
                  <option :value="2">双声道 (Stereo)</option>
                </select>
              </div>
              <div class="fg">
                <label>音频比特率 (Bitrate)</label>
                <select v-model.number="selectedAudioBitrate" :disabled="startDisabled || sourceLoading" class="inp">
                  <option :value="32">32 kbps</option>
                  <option :value="64">64 kbps（G.711/AAC 推荐）</option>
                  <option :value="128">128 kbps（高音质）</option>
                </select>
              </div>
            </div>
          </div>

          <div v-if="screenProfileValidationError" class="source-notice error">{{ screenProfileValidationError }}</div>
          <div v-if="liveCatalog.screen_error" class="source-notice error">
            {{ liveCatalog.screen_error }}
            <span>请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许本应用。</span>
          </div>
        </div>

        <div v-if="probeState" class="probe-result" :class="probeState.kind">{{ probeState.message }}</div>
      </div>
    </section>

    <!-- 3. 下方工作区：平台实时状态与平台命令时间线并排各占 50%，最下方放 SIP 信令追踪 -->
    <div class="workspace">
      <div class="runtime-timeline-grid">
        <!-- 平台实时状态：占左侧 50% 宽度，与上方视频预览等宽对齐 -->
        <div class="glass-card panel runtime-panel">
          <div class="panel-head-tight">
            <div class="panel-title">平台实时状态</div>
            <span class="param-badge">运行时订阅与 OSD</span>
          </div>
          <div class="ptz-runtime-grid">
            <div class="rt-block">
              <div class="rt-label">活跃订阅</div>
              <div v-if="subList.length" class="sub-list">
                <div v-for="s in subList" :key="s.kind" class="sub-item">
                  <span class="sub-dot" /><span class="sub-kind">{{ kindLabel[s.kind] ?? s.kind }}</span>
                  <span class="sub-count">NOTIFY {{ s.notify_count }}</span>
                </div>
              </div>
              <div v-else class="rt-idle">无（平台订阅目录、报警或位置后显示）</div>
            </div>
            <div class="rt-block">
              <div class="rt-label">OSD 设置（平台下发）</div>
              <div v-if="osd.received" class="osd-applied">
                <span class="osd-badge" :class="osd.time_show ? 'on' : 'off'">时间 {{ osd.time_show ? '开' : '关' }}</span>
                <span class="osd-badge" :class="osd.osd_show ? 'on' : 'off'">信息 {{ osd.osd_show ? '开' : '关' }}</span>
                <span class="rt-time">{{ osd.at }}</span>
              </div>
              <div v-else class="rt-idle">无（平台下发 OSDConfig 后显示）</div>
            </div>
            <div v-if="progress" class="rt-block progress-block">
              <div class="rt-label">{{ kindLabel[progress.kind] ?? progress.kind }}进度</div>
              <div class="prog-wrap">
                <div class="prog-bar"><div class="prog-fill" :style="{ width: progress.percent + '%' }" /></div>
                <div class="prog-txt">{{ progress.current }}/{{ progress.total }} · {{ progress.percent }}%</div>
              </div>
            </div>
          </div>
        </div>

        <!-- 平台命令时间线：占右侧 50% 宽度，与上方云台控制卡片等宽对齐 -->
        <div class="glass-card panel timeline-panel">
          <div class="panel-head-tight">
            <div class="panel-title">平台命令时间线</div>
            <span class="param-badge">交互命令流水</span>
          </div>
          <div v-if="commands.length === 0" class="rt-idle empty-timeline">等待平台下发命令(注册后平台会查目录、下发控制等)…</div>
          <div v-else class="cmd-list">
            <div v-for="(c, i) in commands" :key="i" class="cmd-item">
              <span class="cmd-ts">{{ fmtCmdTs(c.ts_ms) }}</span>
              <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
              <span class="cmd-sum">{{ c.summary }}</span>
            </div>
          </div>
        </div>
      </div>

      <!-- SIP 信令实时追踪(FR-43) -->
      <div class="glass-card panel trace-panel">
        <div class="trace-head">
          <div class="panel-title" style="margin: 0">SIP 信令追踪</div>
          <div class="trace-ctl">
            <label class="trace-toggle">
              <input type="checkbox" v-model="traceOn" @change="toggleTrace" /> 追踪
            </label>
            <select v-model="traceDirectionFilter" class="trace-filter" aria-label="方向过滤">
              <option value="all">全部方向</option>
              <option value="in">只看接收</option>
              <option value="out">只看发送</option>
            </select>
            <select v-model="traceMethodFilter" class="trace-filter" aria-label="方法过滤">
              <option v-for="m in traceMethodOptions" :key="m" :value="m">{{ m === 'all' ? '全部方法' : m }}</option>
            </select>
            <button class="trace-clear" @click="clearTraces">清空</button>
          </div>
        </div>
        <div class="trace-log">
          <div v-if="!traces.length" class="trace-empty">注册上线后,收发的 SIP 报文将实时显示在这里(新的在最上,点击展开详情)</div>
          <div v-else-if="!filteredTraces.length" class="trace-empty">当前过滤条件下没有匹配的 SIP 报文</div>
          <template v-for="(t, i) in filteredTraces" :key="`${t.ts_ms}-${i}-${t.cseq ?? ''}`">
            <div class="trace-row" :class="[t.direction, traceTone(t), { open: expandedTrace === i }]" @click="toggleTraceRow(i)">
              <span class="trace-caret">{{ expandedTrace === i ? "▾" : "▸" }}</span>
              <span class="trace-ts">{{ fmtTs(t.ts_ms) }}</span>
              <span class="trace-dir" :class="t.direction">{{ t.direction === "in" ? "◀ 收" : "▶ 发" }}</span>
              <span class="trace-sum">{{ t.summary }}</span>
              <span class="trace-cseq" v-if="t.cseq">{{ t.cseq }}</span>
            </div>
            <pre v-if="expandedTrace === i" class="trace-raw">{{ t.raw }}</pre>
          </template>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1480px; padding-bottom: 16px; }
.page-header { margin-bottom: 10px; padding: 0 2px; }
.page-title { font-size: 17px; font-weight: 700; color: var(--text-primary); letter-spacing: -.2px; }
.page-sub { font-size: 11.5px; color: var(--text-tertiary); margin-top: 2px; }

/* 1. 顶部：视频预览与云台控制左右并排各占 50%，高度增高 1/3 (400px) */
.top-media-ptz-grid {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 10px; align-items: stretch; margin-bottom: 10px;
}

/* 2. 中间：设备身份与国标参数全宽区域 */
.device-identity-section { margin-bottom: 10px; }
.identity-panel {
  box-sizing: border-box; display: flex; flex-direction: column;
}
.panel-head-tight {
  display: flex; align-items: center; justify-content: space-between; gap: 8px; margin-bottom: 8px;
}
.param-badge {
  font-size: 9.5px; color: var(--text-tertiary); background: rgba(255,255,255,0.5);
  padding: 1px 6px; border-radius: 4px; border: 1px solid var(--border-default); font-weight: 600;
}
.identity-grid {
  display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 6px 10px;
}
.identity-grid .fg { margin-bottom: 0; }

.panel {
  position: relative; overflow: hidden; padding: 11px 13px;
  border-radius: var(--radius-md);
  border: 1px solid var(--border-default);
  box-shadow: 0 4px 16px rgba(32, 51, 79, .04), inset 0 1px 0 rgba(255,255,255,.65);
}
.panel::before {
  content: ""; position: absolute; inset: 0 auto 0 0; width: 2.5px;
  background: linear-gradient(180deg, var(--accent), color-mix(in srgb, var(--accent) 25%, transparent));
  opacity: .75;
}

/* 本地视频预览卡片：顶部左侧 50% 宽度 */
.preview-panel {
  box-sizing: border-box; display: flex; flex-direction: column;
}
.preview-head {
  display: flex; align-items: flex-start; justify-content: space-between;
  gap: 8px; margin-bottom: 6px; flex: 0 0 auto;
}
.preview-head .panel-title { margin-bottom: 1px; }
.preview-head-right { display: flex; align-items: center; gap: 6px; }
.preview-sub { color: var(--text-secondary); font-size: 10.5px; }
.preview-badge {
  padding: 2px 6px; border-radius: 999px; font: 700 9px/1 "SF Mono", Menlo, monospace; letter-spacing: .5px;
  color: var(--text-tertiary); background: rgba(255,255,255,.55); border: 1px solid var(--border-default);
}
.preview-badge.live { color: var(--success); background: rgba(8,179,136,.1); border-color: rgba(8,179,136,.22); }
.preview-badge.warn { color: var(--warning); background: rgba(232,147,32,.1); border-color: rgba(232,147,32,.22); }

/* 视频框：严格 16:9 比例展示，支持 Cover 与 Contain 切换 */
.video-frame {
  position: relative; width: 100%; aspect-ratio: 16 / 9;
  display: flex; align-items: center; justify-content: center; overflow: hidden; border-radius: 8px; background:
    radial-gradient(circle at 50% 35%, rgba(56,132,255,.18), transparent 32%),
    linear-gradient(145deg, #0f1b2e, #14243a 58%, #0d1727);
  border: 1px solid rgba(255,255,255,.45); box-shadow: inset 0 1px 0 rgba(255,255,255,.15), 0 6px 16px rgba(15,23,42,.08);
}
.video-frame::after {
  content: ""; position: absolute; inset: 0; pointer-events: none; opacity: .18;
  background-image: linear-gradient(rgba(255,255,255,.12) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,.08) 1px, transparent 1px);
  background-size: 20px 20px; mask-image: linear-gradient(to bottom, #000, transparent 88%);
}
.video-frame video {
  width: 100%; height: 100%; background: #050914; z-index: 1;
}
.video-frame video.video-cover { object-fit: cover; }
.video-frame video.video-contain { object-fit: contain; }

.preview-empty { position: absolute; inset: 0; z-index: 2; display: grid; place-items: center; align-content: center; gap: 4px; padding: 12px; text-align: center; color: rgba(231,241,255,.92); }
.preview-empty strong { font-size: 11.5px; }
.preview-empty span { max-width: 240px; color: rgba(210,226,246,.68); font-size: 10px; line-height: 1.4; }
.scan-mark {
  display: grid; place-items: center; width: 34px; height: 34px; border-radius: 50%;
  border: 1px solid rgba(122,181,255,.45); color: #9ec8ff; font: 800 11px/1 "SF Mono", Menlo, monospace;
  box-shadow: 0 0 0 6px rgba(56,132,255,.06), inset 0 0 14px rgba(56,132,255,.14);
}
.preview-status-line {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
  margin-top: 5px; flex: 0 0 auto;
}
.preview-status { display: flex; align-items: center; gap: 5px; color: var(--text-secondary); font-size: 10.5px; line-height: 1.35; }
.preview-status i { width: 5px; height: 5px; border-radius: 50%; background: var(--text-tertiary); flex: 0 0 auto; }
.preview-status.live i { background: var(--success); box-shadow: 0 0 0 2px rgba(8,179,136,.12); }
.preview-status.warn i { background: var(--warning); box-shadow: 0 0 0 2px rgba(232,147,32,.12); }
.preview-actions { display: flex; gap: 4px; }
.toggle-fit-btn { background: rgba(56,132,255,0.08); border-color: rgba(56,132,255,0.2); color: var(--accent); }
.preview-note { color: var(--text-tertiary); font-size: 9.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

/* 云台控制卡片：顶部右侧 50% 宽度，与左侧等高 */
.ptz-panel {
  box-sizing: border-box; display: flex; flex-direction: column;
}
.ptz-head {
  display: flex; align-items: baseline; justify-content: space-between;
  gap: 8px; margin-bottom: 6px; flex: 0 0 auto;
}
.ptz-head .panel-title { margin-bottom: 0; }
.ptz-sub { font-size: 10.5px; color: var(--text-tertiary); }
.ptz-body {
  flex: 1 1 0; min-height: 0; display: flex; gap: 10px; align-items: stretch;
}

/* 桌面球机 3D 舞台：弹性自动放大，占卡片主要宽度并随窗口舒展 */
.ptz-stage {
  position: relative; flex: 1.35 1 280px; height: 100%; min-height: 140px; overflow: hidden;
  border-radius: var(--radius-sm); perspective: 900px; isolation: isolate;
  background:
    radial-gradient(circle at 50% 38%, rgba(78,145,255,.18), transparent 31%),
    linear-gradient(155deg, rgba(245,250,255,.98), rgba(215,229,245,.78));
  border: 1px solid rgba(91,126,171,.18);
  box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -16px 32px rgba(57,87,126,.07);
}
.stage-grid {
  position: absolute; inset: 54% -12% -36%; transform: rotateX(65deg); transform-origin: center top;
  background-image: linear-gradient(rgba(68,111,166,.1) 1px, transparent 1px), linear-gradient(90deg, rgba(68,111,166,.1) 1px, transparent 1px);
  background-size: 18px 18px; mask-image: linear-gradient(to bottom, #000, transparent 82%);
}
.stage-caption {
  position: absolute; z-index: 9; left: 6px; top: 6px; display: flex; align-items: center; gap: 4px;
  padding: 2px 6px; border-radius: 5px; color: #527196; background: rgba(255,255,255,.75);
  border: 1px solid rgba(92,126,168,.14); box-shadow: 0 2px 6px rgba(39,67,103,.05);
}
.stage-caption.stage-caption-right {
  left: auto; right: 6px; color: var(--text-secondary);
}
.stage-caption.stage-caption-right.active {
  color: var(--accent); border-color: rgba(56,132,255,0.3); background: rgba(255,255,255,0.9);
}
.stage-caption .live-dot { width: 5px; height: 5px; border-radius: 50%; background: #22c55e; box-shadow: 0 0 5px #22c55e; }
.stage-caption .motion-dot { width: 5px; height: 5px; border-radius: 50%; background: #64748b; }
.stage-caption.active .motion-dot { background: var(--accent); box-shadow: 0 0 5px var(--accent); }
.stage-caption b { font: 800 8px/1 "SF Mono", Menlo, monospace; letter-spacing: .5px; }

.ptz-stage.active .stage-caption .live-dot { background: var(--accent); box-shadow: 0 0 7px var(--accent); animation: liveBlink 1s ease-in-out infinite; }
@keyframes liveBlink { 50% { opacity: .35; } }
.compass-ring {
  position: absolute; z-index: 2; left: 50%; top: 64%; width: 170px; height: 58px;
  margin-left: -85px; border-radius: 50%; transform: rotateX(68deg);
  border: 1.5px solid rgba(56,132,255,.28); box-shadow: 0 0 0 5px rgba(255,255,255,.25), inset 0 0 14px rgba(56,132,255,.1);
}
.compass-dial {
  position: absolute; inset: 3px; border-radius: 50%; transition: transform .12s linear;
  background: repeating-conic-gradient(from -1deg, rgba(61,105,158,.58) 0 2deg, transparent 2deg 10deg);
}
.compass-dial::after { content: ""; position: absolute; inset: 27%; border-radius: 50%; border: 1px dashed rgba(56,132,255,.24); }
.compass-dial span { position: absolute; color: #52749e; font-size: 8px; font-weight: 800; text-shadow: 0 1px #fff; }
.compass-dial .north { left: 50%; top: -2px; transform: translateX(-50%); color: var(--accent); }
.compass-dial .east { right: 2px; top: 50%; transform: translateY(-50%); }
.compass-dial .south { left: 50%; bottom: -2px; transform: translateX(-50%); }
.compass-dial .west { left: 2px; top: 50%; transform: translateY(-50%); }
.heading-readout {
  position: absolute; z-index: 2; left: 50%; top: 50%; transform: translate(-50%, -50%) rotateX(-68deg);
  color: var(--accent); font: 800 9.5px/1 "SF Mono", Menlo, monospace; background: rgba(255,255,255,.9);
  padding: 2px 5px; border-radius: 5px; box-shadow: 0 2px 5px rgba(43,68,100,.12);
}
.direction-hud span {
  position: absolute; z-index: 10; display: grid; place-items: center; width: 18px; height: 18px;
  border-radius: 4px; color: #8ba0bb; background: rgba(255,255,255,.6); border: 1px solid rgba(101,129,166,.15);
  font-size: 11px; transition: .15s ease;
}
.direction-hud span.on { color: #fff; background: var(--accent); box-shadow: 0 0 10px var(--accent-glow); transform: scale(1.1); }
.hud-up { top: 6px; left: calc(50% - 9px); }
.hud-down { bottom: 6px; left: calc(50% - 9px); }
.hud-left { left: 6px; top: calc(50% - 9px); }
.hud-right { right: 6px; top: calc(50% - 9px); }
.tilt-gauge {
  position: absolute; z-index: 9; right: 10px; top: 38px; width: 32px; height: 68px;
  color: #7890ad; font: 700 7px/1 "SF Mono", Menlo, monospace;
}
.tilt-gauge > span { position: absolute; right: 0; }
.tilt-plus { top: 0; } .tilt-zero { top: 32px; } .tilt-minus { bottom: 0; }
.tilt-track { position: absolute; left: 3px; top: 2px; width: 2px; height: 64px; border-radius: 2px; background: linear-gradient(var(--accent), #a9bdd5); }
.tilt-track::before, .tilt-track::after { content: ""; position: absolute; left: -2px; width: 5px; height: 1px; background: #7890ad; }
.tilt-track::before { top: 0; } .tilt-track::after { bottom: 0; }
.tilt-marker {
  position: absolute; left: -1px; top: 29px; min-width: 22px; height: 12px; padding-left: 6px; border-radius: 4px;
  color: #fff; background: var(--accent); font-size: 7px; line-height: 12px; box-shadow: 0 2px 5px var(--accent-glow);
  transition: transform .12s linear;
}
.tilt-marker::before { content: ""; position: absolute; left: -2px; top: 3.5px; border-width: 2.5px 3px 2.5px 0; border-style: solid; border-color: transparent var(--accent) transparent transparent; }
.axis-label { position: absolute; z-index: 8; color: #7890ad; font: 700 7px/1 "SF Mono", Menlo, monospace; letter-spacing: .3px; }
.pan-axis { left: 8px; bottom: 8px; } .tilt-axis { right: 8px; bottom: 8px; }
.ptz-stage.active { box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -16px 32px rgba(57,87,126,.07), 0 0 0 2px rgba(56,132,255,.16); }
.ptz-stage.seeking { animation: stageSeekPulse 1.15s ease-in-out infinite; }
@keyframes stageSeekPulse { 50% { box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -16px 32px rgba(57,87,126,.07), 0 0 16px rgba(56,132,255,.28); } }

/* 云台右侧参数信息：顶部对齐、2x2 指标卡、下部预置位卡片占满剩余空间 */
.ptz-info {
  flex: 1 1 210px; display: flex; flex-direction: column; justify-content: flex-start;
  gap: 6px; min-width: 0; overflow: hidden; height: 100%;
}
.ptz-stat-grid {
  display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 5px; flex: 0 0 auto;
}
.ptz-stat {
  display: flex; flex-direction: column; gap: 2px; min-width: 0; padding: 5px 8px; border-radius: var(--radius-xs);
  background: linear-gradient(145deg, rgba(255,255,255,.7), rgba(239,245,252,.46));
  border: 1px solid rgba(105,132,167,.12);
}
.ptz-stat span { font-size: 8.5px; color: var(--text-tertiary); width: auto; flex-shrink: 0; text-transform: uppercase; letter-spacing: .2px; }
.ptz-stat b { font-size: 10.5px; color: var(--text-primary); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ptz-stat b.hot { color: var(--accent); }
.ptz-stat b.pose { font-variant-numeric: tabular-nums; font-size: 10.5px; }

.ptz-stat.action-state { flex: 0 0 auto; }
.action-chips { display: flex; flex-wrap: wrap; gap: 3px; margin-top: 2px; }
.action-chip {
  display: inline-flex; align-items: center; justify-content: center; min-width: 24px; height: 17px; padding: 0 4px;
  border-radius: 4px; color: var(--text-tertiary); background: rgba(255,255,255,.55);
  border: 1px solid var(--border-default); font-size: 9px; transition: all .15s ease;
}
.action-chip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 6px var(--accent-glow); transform: translateY(-1px); }
.action-chip.stop.on { background: #64748b; border-color: #64748b; box-shadow: 0 0 6px rgba(100,116,139,.2); }

/* 预置位卡片：自适应占满剩余垂直空间 */
.ptz-stat.presets {
  flex: 1 1 0; min-height: 0; display: flex; flex-direction: column;
}
.preset-chips {
  display: flex; gap: 4px; flex-wrap: wrap; align-content: flex-start; margin-top: 3px; overflow-y: auto;
}
.pchip {
  display: inline-flex; align-items: center; justify-content: center; gap: 3px;
  min-width: 58px; height: 20px; padding: 0 5px; border-radius: 4px;
  font-size: 9.5px; font-weight: 600; color: var(--text-secondary);
  background: rgba(255,255,255,0.5); border: 1px solid var(--border-default); transition: all 0.15s;
}
.pchip small {
  display: inline-grid; place-items: center; width: 13px; height: 13px; border-radius: 3px;
  color: var(--accent); background: var(--accent-dim); font: 700 8px/1 "SF Mono", Menlo, monospace;
}
.pchip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 6px var(--accent-glow); transform: translateY(-1px); }
.pchip.on small { color: var(--accent); background: #fff; }

/* 下方工作区：平台实时状态与时间线双列等宽网格(各占 50%，分别对齐左侧与右侧) */
.workspace { display: flex; flex-direction: column; gap: 10px; margin-top: 10px; }
.runtime-timeline-grid {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 10px;
}
.runtime-panel, .timeline-panel { height: 100%; box-sizing: border-box; display: flex; flex-direction: column; }
.empty-timeline { padding: 12px 0; font-size: 11px; }

.ptz-runtime-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; }
.ptz-runtime-grid .rt-block {
  min-width: 0; padding: 7px 9px; border: 1px solid rgba(105,132,167,.1); border-radius: var(--radius-xs);
  background: rgba(255,255,255,.38);
}
.ptz-runtime-grid .progress-block { grid-column: 1 / -1; }
.rt-block { padding: 6px 0; border-top: 1px solid rgba(120,120,120,0.08); }
.rt-block:first-of-type { border-top: none; padding-top: 0; }
.rt-label { font-size: 10.5px; color: var(--text-secondary); font-weight: 600; margin-bottom: 4px; }
.rt-idle { font-size: 11px; color: var(--text-tertiary); }
.rt-time { font-size: 10px; color: var(--text-tertiary); margin-left: 4px; }

/* 活跃订阅面板 */
.sub-list { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }
.sub-item { display: inline-flex; align-items: center; gap: 4px; padding: 3px 6px;
  background: rgba(5,150,105,0.08); border-radius: 4px; font-size: 10.5px; }
.sub-dot { width: 5px; height: 5px; border-radius: 50%; background: #059669;
  box-shadow: 0 0 0 2px rgba(5,150,105,0.15); }
.sub-kind { font-weight: 600; color: var(--text-primary); }
.sub-count { color: var(--text-tertiary); font-size: 9.5px; }

/* 长任务进度 */
.prog-wrap { display: flex; align-items: center; gap: 6px; margin-top: 4px; }
.prog-bar { flex: 1; height: 5px; background: rgba(120,120,120,0.15); border-radius: 3px; overflow: hidden; }
.prog-fill { height: 100%; background: var(--accent); border-radius: 3px; transition: width 0.4s; }
.prog-txt { font-size: 10.5px; color: var(--text-secondary); font-family: monospace; }

.panel-title {
  font-size: 11px; font-weight: 650; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: .4px; margin-bottom: 0;
}
.fg { margin-bottom: 6px; }
.fg-row { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
.fg label { display: block; font-size: 10.5px; color: var(--text-secondary); margin-bottom: 2px; font-weight: 500; }
.inp {
  width: 100%; box-sizing: border-box; height: 26px; padding: 0 8px;
  border: 1px solid var(--border-default); border-radius: var(--radius-xs);
  background: rgba(255, 255, 255, 0.7); color: var(--text-primary);
  font-size: 11px; font-family: "SF Mono", Menlo, monospace;
  transition: border-color var(--transition), box-shadow var(--transition); outline: none;
}
.inp:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.seg {
  display: inline-flex; padding: 2px; gap: 2px; border-radius: var(--radius-xs);
  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--border-default);
}
.seg button {
  border: none; background: transparent; padding: 2px 10px; border-radius: 4px;
  font-size: 10.5px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.seg button.on { background: var(--accent); color: #fff; box-shadow: 0 1px 4px var(--accent-glow); }

/* 媒体源类型与操作行：下拉框 + 右侧上报操作（同行紧凑排布） */
.media-control-bar {
  display: flex; align-items: flex-end; justify-content: space-between;
  gap: 10px; margin-top: 8px; padding-top: 8px;
  border-top: 1px solid rgba(120,130,150,0.12);
}
.media-select-wrap {
  display: flex; flex-direction: column; gap: 2px; flex: 1 1 260px; max-width: 360px;
}
.media-select-wrap label {
  font-size: 10.5px; color: var(--text-secondary); font-weight: 600;
}
.media-mode-select {
  font-weight: 600; color: var(--text-primary);
}
.media-actions-wrap {
  display: flex; align-items: center; gap: 6px; flex-wrap: wrap; flex: 0 0 auto;
}
.file-config-box { margin-top: 6px; }

.source-config {
  margin-top: 6px; padding: 7px 8px; border-radius: var(--radius-sm);
  border: 1px solid rgba(120,130,150,0.13); background: rgba(255,255,255,0.34);
}

/* 左右分栏媒体参数网格 */
.media-param-columns {
  display: grid; grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px; margin-top: 4px;
}
.param-col {
  background: rgba(255,255,255,0.3); padding: 6px 8px; border-radius: 6px;
  border: 1px solid rgba(120,130,150,0.1);
}
.col-title {
  font-size: 10px; font-weight: 700; color: var(--text-primary); margin-bottom: 5px;
  padding-bottom: 3px; border-bottom: 1px solid rgba(120,130,150,0.1);
}
.param-col .fg { margin-bottom: 5px; }
.param-col .fg:last-child { margin-bottom: 0; }

.screen-bitrate-input { display: flex; align-items: center; gap: 4px; }
.screen-bitrate-input .inp { min-width: 0; }
.screen-bitrate-input span { flex: 0 0 auto; color: var(--text-tertiary); font-size: 10px; }
.screen-bitrate-input .inp { flex: 1 1 auto; }
.source-capability {
  display: flex; align-items: center; gap: 6px; margin-bottom: 5px;
  color: var(--text-secondary); font-size: 10px;
}
.cap-badge { padding: 1px 5px; border-radius: 6px; font-size: 9px; font-weight: 650; }
.cap-badge.ok { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); }
.cap-badge.warn { color: var(--warning); background: color-mix(in srgb, var(--warning) 12%, transparent); }
.file-row { display: flex; gap: 6px; align-items: stretch; }
.file-row .inp { flex: 1 1 auto; }
.fg-hint { font-size: 9.5px; color: var(--text-tertiary); margin-top: 4px; line-height: 1.35; }
.file-btn {
  flex: 0 0 auto; border: 1px solid var(--border-default); background: rgba(255,255,255,0.6);
  border-radius: var(--radius-xs); padding: 0 8px; font-size: 10.5px; color: var(--text-secondary);
  cursor: pointer; white-space: nowrap; min-height: 24px;
}
.file-btn:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
.file-btn:disabled { cursor: not-allowed; opacity: .55; }
.file-btn.compact { min-height: 22px; padding: 0 6px; font-size: 10px; }
.file-btn.primary { border-color: var(--accent); color: #fff; background: var(--accent); }
.source-notice { display: flex; flex-direction: column; gap: 2px; margin-top: 5px; padding: 4px 6px; border-radius: 4px; font-size: 9.5px; line-height: 1.3; }
.source-notice.error { color: var(--error); background: color-mix(in srgb, var(--error) 8%, transparent); }
.source-footer { display: flex; align-items: flex-end; justify-content: space-between; gap: 6px; margin-top: 6px; }
.source-uri { min-width: 0; flex: 1; }
.source-uri > span { display: block; font-size: 8.5px; color: var(--text-tertiary); margin-bottom: 1px; }
.source-uri code {
  display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  color: var(--text-secondary); font-size: 8.5px; font-family: "SF Mono", Menlo, monospace;
}
.source-actions { display: flex; gap: 4px; flex: 0 0 auto; }
.probe-result { margin-top: 5px; padding: 4px 6px; border-radius: 4px; font-size: 9.5px; line-height: 1.3; }
.probe-result.loading { color: var(--accent); background: rgba(56,132,255,0.08); }
.probe-result.success { color: var(--success); background: color-mix(in srgb, var(--success) 10%, transparent); }
.probe-result.warning { color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); }
.probe-result.error { color: var(--error); background: color-mix(in srgb, var(--error) 9%, transparent); }

/* 平台命令时间线 */
.cmd-empty { color: var(--text-tertiary); font-size: 11px; padding: 6px 0; }
.cmd-list { margin-top: 4px; max-height: 180px; overflow-y: auto; }
.cmd-item { display: flex; align-items: center; gap: 6px; padding: 3px 0;
  border-bottom: 1px solid rgba(120,120,120,0.06); font-size: 10.5px; }
.cmd-ts { color: var(--text-tertiary); font-family: monospace; font-size: 9.5px; flex-shrink: 0; }
.cmd-tag { flex-shrink: 0; font-size: 9px; font-weight: 600; padding: 1px 5px; border-radius: 4px;
  background: rgba(56,132,255,0.12); color: var(--accent); }
.cmd-tag.k-control { background: rgba(234,88,12,0.12); color: #ea580c; }
.cmd-tag.k-invite { background: rgba(5,150,105,0.12); color: #059669; }
.cmd-tag.k-broadcast { background: rgba(139,92,246,0.12); color: #8b5cf6; }
.cmd-sum { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

/* SIP 信令追踪面板 */
.trace-panel { margin-top: 0; }
.trace-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
.trace-ctl { display: inline-flex; align-items: center; gap: 5px; flex-wrap: wrap; justify-content: flex-end; }
.trace-toggle { font-size: 10.5px; color: var(--text-secondary); cursor: pointer; user-select: none; }
.trace-filter, .trace-clear {
  border: 1px solid var(--border-default); background: rgba(255,255,255,0.5);
  border-radius: 4px; padding: 1px 6px; font-size: 10px; color: var(--text-secondary); cursor: pointer;
}
.trace-filter { height: 22px; outline: none; }
.trace-filter:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.trace-log {
  height: 220px; overflow-y: auto; overflow-x: hidden; border-radius: var(--radius-xs);
  background: rgba(15, 23, 42, 0.03); border: 1px solid var(--border-default);
  padding: 5px 7px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 10.5px;
}
.trace-empty { color: var(--text-secondary); padding: 8px; text-align: center; font-size: 11px; }
.trace-row {
  display: flex; align-items: baseline; gap: 5px; padding: 2px 4px 2px 5px;
  border-left: 2px solid transparent; border-bottom: 1px solid rgba(15,23,42,0.03); white-space: nowrap;
  cursor: pointer; transition: background 0.12s, border-color 0.12s;
}
.trace-row.in { border-left-color: #0891b2; }
.trace-row.out { border-left-color: #7c3aed; }
.trace-row.warn { background: rgba(232,147,32,.06); }
.trace-row.error { background: rgba(229,72,92,.08); border-left-color: var(--error); }
.trace-row:hover { background: rgba(56,132,255,0.05); }
.trace-row.open { background: rgba(56,132,255,0.08); }
.trace-caret { flex: 0 0 auto; color: var(--text-tertiary); font-size: 8px; width: 7px; }
.trace-raw {
  margin: 0 0 4px 14px; padding: 6px 8px; background: #0f172a; color: #cbd5e1;
  border-radius: 4px; font-size: 10px; line-height: 1.4; white-space: pre-wrap;
  word-break: break-all; max-height: 200px; overflow-y: auto;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }

/* OSD 设置(平台下发) */
.osd-empty { color: var(--text-tertiary); font-size: 10.5px; padding: 4px 0; }
.osd-applied { display: flex; align-items: center; gap: 6px; padding: 2px 0; }
.osd-badge {
  font-size: 9.5px; font-weight: 600; padding: 1px 6px; border-radius: 6px;
}
.osd-badge.on { color: #059669; background: rgba(5,150,105,0.12); }
.osd-badge.off { color: var(--text-tertiary); background: rgba(120,120,120,0.12); }

@media (max-width: 1080px) {
  .device-console-grid { grid-template-columns: 1fr; }
  .runtime-timeline-grid { grid-template-columns: 1fr; }
  .preview-ptz-stack { height: auto; }
  .preview-panel { min-height: 240px; }
  .ptz-panel { min-height: 280px; }
}
@media (max-width: 760px) {
  .identity-grid, .source-grid, .media-param-columns { grid-template-columns: 1fr; }
  .source-modes { grid-template-columns: 1fr; }
  .ptz-body { flex-direction: column; }
  .ptz-stage { flex-basis: 200px; width: 100%; }
}
</style>
