<script setup lang="ts">
// 单设备联调控制台(UC-1),高保真对齐参考原型 frost-blue:
// 顶部 4 指标卡 + 双栏配置卡(SIP 服务器 / 设备身份)。
import { ref, onMounted, onUnmounted, onActivated, computed } from "vue";
import { NButton, useMessage } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import ThreePtzCamera from "../components/ThreePtzCamera.vue";

const message = useMessage();

// 选择本地 H.264 文件作视频源(FR-8:C 档真实码流)。
async function pickVideoSource() {
  // Vite 的 localhost 页面可用于前端调试，但浏览器没有 Tauri IPC，也无法把本机
  // 文件路径交给 Rust 推流；文件源选择必须在 Tauri 桌面窗口中完成。
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    message.error("当前是浏览器调试页面，请切换到 Tauri 桌面窗口后选择视频文件");
    return;
  }
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
    }
  } catch (e) {
    message.error("选择文件失败:" + String(e));
  }
}

// 平台连接参数(server_host/port/domain/password/transport)来自顶栏全局平台档案,
// 本页只管设备自身参数(设备 ID/版本/通道/视频源/模板)。
import { usePlatform } from "../platform";
import { useDevice } from "../device";
const { active: activePlatform } = usePlatform();

// 设备配置/状态/注册逻辑来自共享 store(与顶栏共用同一份)。
const {
  form, startDisabled, stopDisabled, canReport,
  startDevice: storeStart, stopDevice: storeStop, reconcile,
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
      : initialSource ? "file" : "none"
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
const registrationDisabled = computed(() =>
  startDisabled.value ||
  ((mediaMode.value === "camera" || mediaMode.value === "screen") &&
    !selectedLiveAudioCodecAvailable.value)
);

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
  } else if (mode === "file") {
    if (form.value.video_source.startsWith("live:")) form.value.video_source = "";
  } else {
    syncLiveSourceUri();
  }
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

const sourceSummary = computed(() => {
  if (mediaMode.value === "none") return "不发送媒体，只进行 SIP 信令联调";
  if (mediaMode.value === "file") return form.value.video_source || "尚未选择视频文件";
  if (mediaMode.value === "screen" && screenProfileValidationError.value) {
    return `屏幕参数无效：${screenProfileValidationError.value}`;
  }
  return form.value.video_source || "尚未选择可用设备";
});

async function probeLiveSource() {
  if (!canProbeSource.value) return;
  probeState.value = { kind: "loading", message: "正在启动采集并等待音视频数据…" };
  try {
    const result = await invoke<LiveProbeResult>("probe_live_source", {
      uri: form.value.video_source,
    });
    const ready = result.video_ready && (!result.audio_requested || result.audio_ready === true);
    probeState.value = {
      kind: ready ? "success" : "warning",
      message: result.message,
    };
  } catch (e) {
    probeState.value = { kind: "error", message: `采集测试失败：${String(e)}` };
  }
}

// OSD 配置状态:反映**平台下发的 OSD 配置命令**(国标 A.2.3.2.11),设备已按其设置。
// 非本地随意填写——osd_config 事件由后端在收到平台 DeviceConfig+OSDConfig 时推来。
const osd = ref<{ received: boolean; time_show: boolean; osd_show: boolean; at: string }>({
  received: false, time_show: false, osd_show: false, at: "",
});
const now = ref(new Date().toLocaleString("zh-CN", { hour12: false }));
let osdTimer: number | null = null;
let unlistenOsd: (() => void) | null = null;

// 页面内注册/注销:走 store,结果用本页 message 提示(顶栏也可操作同一台设备)。
async function startDevice() {
  if (!selectedLiveAudioCodecAvailable.value &&
      (mediaMode.value === "camera" || mediaMode.value === "screen")) {
    message.error("当前内嵌 FFmpeg 不支持所选音频编码，请改用 G.711A/G.711U");
    return;
  }
  const r = await storeStart(activePlatform.value);
  if (r.ok) message.success(r.msg); else message.error(r.msg);
}
async function stopDevice() {
  const r = await storeStop();
  if (r.ok) message.info(r.msg); else message.error(r.msg);
}
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
const MAX_TRACE = 200;
const expandedTrace = ref<number | null>(null);
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
function clearTraces() { traces.value = []; expandedTrace.value = null; }

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


    <!-- 设备身份与媒体源占满整行，避免窄列挤压复杂配置。 -->
    <div class="glass-card panel identity-panel">
          <div class="panel-title">设备身份</div>
          <div class="identity-grid">
          <div class="fg">
            <label>国标版本</label>
            <div class="seg">
              <button :class="{ on: form.gb_version === '2022' }" @click="form.gb_version = '2022'">GB/T 2022</button>
              <button :class="{ on: form.gb_version === '2016' }" @click="form.gb_version = '2016'">GB/T 2016</button>
            </div>
          </div>
          <div class="fg">
            <label>设备编号</label>
            <input v-model="form.device_id" class="inp" placeholder="20 位国标 ID" />
          </div>
          <div class="fg">
            <label>通道名称</label>
            <input v-model="form.channel_name" class="inp" />
          </div>
          <div class="fg">
            <label>目录模板</label>
            <select v-model="form.catalog_template" class="inp">
              <option value="">单通道(默认)</option>
              <option value="nvr-8ch">8 通道 NVR</option>
              <option value="civil-3x2">跨区划(3 区 × 2 通道)</option>
              <option value="large-16ch">16 通道大型监控</option>
            </select>
          </div>
          </div>
          <div class="fg media-source-field">
            <div class="source-heading">
              <label>媒体源(可选)</label>
              <span class="source-lock" v-if="startDisabled">设备运行中，配置已锁定</span>
            </div>
            <div class="source-modes" role="radiogroup" aria-label="媒体源类型">
              <button type="button" :disabled="startDisabled" :class="{ on: mediaMode === 'none' }" @click="setMediaMode('none')">
                <b>无媒体</b><span>仅信令联调</span>
              </button>
              <button type="button" :disabled="startDisabled" :class="{ on: mediaMode === 'file' }" @click="setMediaMode('file')">
                <b>视频文件</b><span>循环推送本地文件</span>
              </button>
              <button type="button" :disabled="startDisabled" :class="{ on: mediaMode === 'camera' }" @click="setMediaMode('camera')">
                <b>电脑摄像头</b><span>含 iPhone 连续互通</span>
              </button>
              <button type="button" :disabled="startDisabled" :class="{ on: mediaMode === 'screen' }" @click="setMediaMode('screen')">
                <b>电脑屏幕实时画面</b><span>系统声音或电脑麦克风</span>
              </button>
            </div>

            <div v-if="mediaMode === 'file'" class="source-config">
              <div class="file-row">
                <input v-model="form.video_source" :disabled="startDisabled" class="inp" placeholder="选择 H.264/H.265 裸流或 MP4、MOV 等视频文件" />
                <button type="button" class="file-btn" :disabled="startDisabled" @click="pickVideoSource">选择文件</button>
              </div>
              <div class="fg-hint">容器文件会在设备启动前转封装；有音频轨时自动转为 G.711A 后与视频复用。</div>
            </div>

            <div v-else-if="mediaMode === 'camera'" class="source-config">
              <div class="source-capability">
                <span class="cap-badge" :class="liveCatalog.ffmpeg_available ? 'ok' : 'warn'">
                  FFmpeg {{ liveCatalog.ffmpeg_available ? '可用' : '未找到' }}
                </span>
                <span>AVFoundation 摄像头采集</span>
              </div>
              <div class="source-grid camera-source-grid">
                <div>
                  <label>视频设备</label>
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
                <div>
                  <label>音频设备</label>
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
                <div>
                  <label>音频编码</label>
                  <select v-model="selectedCameraAudioCodec" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                    <option value="g711a">G.711A / PCMA · 8 kHz</option>
                    <option value="g711u">G.711U / PCMU · 8 kHz</option>
                    <option value="aac" :disabled="!liveCatalog.aac_available">AAC-LC / ADTS · 48 kHz{{ liveCatalog.aac_available ? '' : '（编码器不可用）' }}</option>
                    <option value="opus" :disabled="!liveCatalog.opus_available">Opus · 48 kHz · 20 ms（扩展{{ liveCatalog.opus_available ? '' : '，编码器不可用' }}）</option>
                  </select>
                </div>
              </div>
              <div v-if="liveCatalog.avfoundation_error" class="source-notice error">{{ liveCatalog.avfoundation_error }}</div>
              <div v-else class="fg-hint">iPhone 的“相机”和“桌上视角”是两个真实视频视角，不是屏幕镜像。选择麦克风后，视频与音频在同一个 AVFoundation 会话中采集，避免连续互通设备被两个进程争用。首次采集必须允许麦克风权限；如仍无声，请前往“系统设置 → 隐私与安全性 → 麦克风”确认本应用已开启。</div>
            </div>

            <div v-else-if="mediaMode === 'screen'" class="source-config">
              <div class="source-capability">
                <span class="cap-badge ok">ScreenCaptureKit</span>
                <span>原生屏幕与系统音频采集</span>
              </div>
              <div class="source-grid">
                <div>
                  <label>显示器</label>
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
                <div>
                  <label>声音</label>
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
              </div>
              <div class="screen-profile-grid">
                <div>
                  <label>输出分辨率</label>
                  <select v-model="selectedScreenResolution" :disabled="startDisabled || sourceLoading" class="inp" @change="applyScreenResolution">
                    <option v-for="resolution in screenResolutionOptions" :key="resolution.value" :value="resolution.value">
                      {{ resolution.label }}
                    </option>
                  </select>
                </div>
                <div>
                  <label>视频码率</label>
                  <div class="screen-bitrate-input">
                    <input v-model.number="selectedScreenBitrate" :disabled="startDisabled || sourceLoading" class="inp" type="number" min="128" max="20000" step="50" aria-label="屏幕视频码率（kbps）" @change="syncLiveSourceUri" />
                    <span>kbps</span>
                  </div>
                </div>
                <div>
                  <label>编码格式</label>
                  <select v-model="selectedScreenCodec" :disabled="startDisabled || sourceLoading" class="inp" @change="syncLiveSourceUri">
                    <option value="h264">H.264 / AVC（兼容性优先）</option>
                    <option value="h265">H.265 / HEVC（更低带宽）</option>
                  </select>
                </div>
                <div>
                  <label>音频格式</label>
                  <select v-model="selectedScreenAudioCodec" :disabled="startDisabled || sourceLoading" class="inp" aria-label="实时音频格式" @change="syncLiveSourceUri">
                    <option value="g711a">G.711A / PCMA · 8 kHz · 单声道</option>
                    <option value="g711u">G.711U / PCMU · 8 kHz · 单声道</option>
                    <option value="aac" :disabled="!liveCatalog.aac_available">AAC-LC / ADTS · 48 kHz · 单声道{{ liveCatalog.aac_available ? '' : '（编码器不可用）' }}</option>
                    <option value="opus" :disabled="!liveCatalog.opus_available">Opus · 48 kHz · 20 ms（扩展{{ liveCatalog.opus_available ? '' : '，编码器不可用' }}）</option>
                  </select>
                </div>
              </div>
              <div v-if="screenProfileValidationError" class="source-notice error">{{ screenProfileValidationError }}</div>
              <div v-if="liveCatalog.screen_error" class="source-notice error">
                {{ liveCatalog.screen_error }}
                <span>请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许本应用。</span>
              </div>
              <div v-else class="fg-hint">声音可选择系统播放或任意 AVFoundation 麦克风。G.711A/U 兼容性最高；AAC-LC / ADTS 是 FLV 最常用的标准音频格式；Opus 使用私有 PS 扩展，只有明确支持 Opus 的平台才能播放。麦克风无声时请检查“系统设置 → 隐私与安全性 → 麦克风”。</div>
            </div>

            <div v-if="mediaMode !== 'none'" class="source-footer">
              <div class="source-uri">
                <span>{{ mediaMode === 'file' ? '当前文件' : '规范实时源 URI' }}</span>
                <code>{{ sourceSummary }}</code>
              </div>
              <div v-if="mediaMode === 'camera' || mediaMode === 'screen'" class="source-actions">
                <button type="button" class="file-btn compact" :disabled="startDisabled || sourceLoading" @click="refreshLiveSources()">
                  {{ sourceLoading ? '刷新中…' : '刷新设备' }}
                </button>
                <button type="button" class="file-btn compact primary" :disabled="!canProbeSource || probeState?.kind === 'loading'" @click="probeLiveSource">
                  {{ probeState?.kind === 'loading' ? '测试中…' : '测试采集' }}
                </button>
              </div>
            </div>
            <div v-if="probeState" class="probe-result" :class="probeState.kind">{{ probeState.message }}</div>
          </div>
          <div class="action-row">
            <n-button type="primary" :disabled="registrationDisabled" @click="startDevice">注册上线</n-button>
            <n-button :disabled="stopDisabled" @click="stopDevice">注销</n-button>
            <n-button :disabled="!canReport" @click="fireAlarm">上报报警</n-button>
            <n-button :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
          </div>
    </div>

    <!-- 下方工作区：所有卡片全宽单列，按云台、状态、时间线顺序阅读。 -->
    <div class="workspace">
      <div class="ws-col">
        <div class="glass-card panel ptz-panel">
          <div class="panel-title">云台控制</div>
          <div class="ptz-sub">平台下发 PTZ 时，桌面球机按真实水平转台、球形机芯和光学变倍实时联动</div>
          <div class="ptz-body">
            <div class="ptz-stage" :class="{ active: ptzActive, seeking }">
              <div class="stage-grid"></div>
              <div class="stage-caption">
                <span class="live-dot"></span>
                <b>PTZ LIVE</b>
                <em>{{ statusText }}</em>
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
              <div class="axis-label pan-axis">PAN · 水平 360°</div>
              <div class="axis-label tilt-axis">TILT · 俯仰 ±90°</div>
            </div>
            <div class="ptz-info">
              <div class="ptz-stat pose-stat"><span>朝向</span><b class="pose">{{ poseText }}</b></div>
              <div class="ptz-stat"><span>方向</span><b :class="{ hot: seeking || moving }">{{ dirText }}</b></div>
              <div class="ptz-stat"><span>变倍</span><b :class="{ hot: ptz.zoom_in || ptz.zoom_out }">{{ zoomText }}</b></div>
              <div class="ptz-stat"><span>速度</span><b :class="{ hot: moving || ptz.zoom_in || ptz.zoom_out }">{{ speedText }}</b></div>
              <div class="ptz-stat"><span>状态</span><b :class="{ hot: ptzActive || seeking }">{{ statusText }}</b></div>
              <div class="ptz-stat action-state">
                <span>平台按钮实时状态 · {{ lastPtzAt }}</span>
                <span class="action-chips">
                  <b v-for="button in ptzButtons" :key="button.key" class="action-chip" :class="{ on: button.active, stop: button.key === 'stop' }">{{ button.label }}</b>
                </span>
                <em>最近命令：{{ lastPtzAction }}</em>
              </div>
              <div class="ptz-stat presets">
                <span>预置位 · GB/T 28181 PresetName</span>
                <span class="preset-chips">
                  <b v-for="id in PRESET_IDS" :key="id" class="pchip" :class="{ on: activePreset === id && seeking }">
                    <small>{{ id }}</small>{{ presetNames[id] }}
                  </b>
                </span>
              </div>
            </div>
          </div>
          <section class="ptz-runtime" aria-labelledby="ptz-runtime-title">
            <div id="ptz-runtime-title" class="ptz-runtime-title">平台实时状态</div>
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
          </section>
        </div>
      </div>

      <div class="ws-col">
        <div class="glass-card panel">
          <div class="panel-title">平台命令时间线</div>
          <div v-if="commands.length === 0" class="rt-idle">等待平台下发命令(注册后平台会查目录、下发控制等)…</div>
          <div v-else class="cmd-list">
            <div v-for="(c, i) in commands" :key="i" class="cmd-item">
              <span class="cmd-ts">{{ fmtCmdTs(c.ts_ms) }}</span>
              <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
              <span class="cmd-sum">{{ c.summary }}</span>
            </div>
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
          <button class="trace-clear" @click="clearTraces">清空</button>
        </div>
      </div>
      <div class="trace-log">
        <div v-if="!traces.length" class="trace-empty">注册上线后,收发的 SIP 报文将实时显示在这里(新的在最上,点击展开详情)</div>
        <template v-for="(t, i) in traces" :key="i">
          <div class="trace-row" :class="[t.direction, { open: expandedTrace === i }]" @click="toggleTraceRow(i)">
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
</template>

<style scoped>
.page { max-width: 1480px; padding-bottom: 28px; }
.page-header { margin-bottom: 20px; padding: 4px 2px; }
.page-title { font-size: 24px; font-weight: 750; color: var(--text-primary); letter-spacing: -.3px; }
.page-sub { font-size: 13px; color: var(--text-tertiary); margin-top: 6px; }

/* 所有功能卡片按阅读顺序全宽单列排列，避免复杂信息被窄栏压缩。 */
.identity-panel { margin-bottom: 18px; }
.identity-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 14px; }
.identity-grid .fg { margin-bottom: 0; }
.workspace { display: flex; flex-direction: column; gap: 18px; }
.ws-col { display: contents; }
.workspace > .ws-col > .panel { width: 100%; box-sizing: border-box; }
.panel {
  position: relative; overflow: hidden;
  border: 1px solid color-mix(in srgb, var(--border-default) 82%, white);
  box-shadow: 0 12px 34px rgba(32, 51, 79, .07), inset 0 1px 0 rgba(255,255,255,.65);
}
.panel::before {
  content: ""; position: absolute; inset: 0 auto 0 0; width: 3px;
  background: linear-gradient(180deg, var(--accent), color-mix(in srgb, var(--accent) 25%, transparent));
  opacity: .68;
}
@media (max-width: 1080px) {
  .identity-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
}
@media (max-width: 640px) { .identity-grid { grid-template-columns: 1fr; } }

/* 实时状态分块 */
.rt-block { padding: 10px 0; border-top: 1px solid rgba(120,120,120,0.08); }
.rt-block:first-of-type { border-top: none; padding-top: 0; }
.rt-label { font-size: 12px; color: var(--text-secondary); font-weight: 600; margin-bottom: 8px; }
.rt-idle { font-size: 12.5px; color: var(--text-tertiary); }
.rt-time { font-size: 11.5px; color: var(--text-tertiary); margin-left: 6px; }

/* 目标平台参数网格 + 操作按钮行 */
.pr-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 4px 16px; }
.action-row { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 16px; }

.panel { padding: 22px 22px 24px; }
.panel-title {
  font-size: 12px; font-weight: 600; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: .5px; margin-bottom: 16px;
}
.fg { margin-bottom: 14px; }
.fg-row { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.fg label { display: block; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 6px; }
.inp {
  width: 100%; box-sizing: border-box; height: 38px; padding: 0 12px;
  border: 1px solid var(--border-default); border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.7); color: var(--text-primary);
  font-size: 13px; font-family: "SF Mono", Menlo, monospace;
  transition: border-color var(--transition), box-shadow var(--transition); outline: none;
}
.inp:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-dim); }
.seg {
  display: inline-flex; padding: 3px; gap: 3px; border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--border-default);
}
.seg button {
  border: none; background: transparent; padding: 5px 18px; border-radius: 6px;
  font-size: 13px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.seg button.on { background: var(--accent); color: #fff; box-shadow: 0 2px 6px var(--accent-glow); }

/* 密码显示/隐藏:框内右侧眼睛图标(与平台配置页交互一致) */
.pwd-wrap { position: relative; }
.pwd-wrap .inp { width: 100%; padding-right: 40px; }
.pwd-eye {
  position: absolute; top: 50%; right: 10px; transform: translateY(-50%);
  display: inline-flex; align-items: center; justify-content: center;
  border: none; background: transparent; padding: 2px; cursor: pointer;
  color: var(--text-tertiary);
}
.pwd-eye:hover { color: var(--accent); }

/* 专业媒体源选择器 */
.media-source-field { margin-top: 18px; }
.source-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.source-lock { font-size: 11px; color: var(--warning); }
.source-modes {
  display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px;
}
.source-modes button {
  min-width: 0; border: 1px solid var(--border-default); border-radius: 10px;
  background: rgba(255,255,255,0.52); padding: 10px 12px; text-align: left;
  color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.source-modes button:hover:not(:disabled) { border-color: var(--accent); background: rgba(56,132,255,0.06); }
.source-modes button.on {
  border-color: var(--accent); background: rgba(56,132,255,0.1);
  box-shadow: inset 0 0 0 1px rgba(56,132,255,0.12);
}
.source-modes button:disabled { cursor: not-allowed; opacity: .62; }
.source-modes b { display: block; font-size: 12.5px; color: var(--text-primary); margin-bottom: 3px; }
.source-modes button.on b { color: var(--accent); }
.source-modes span { display: block; font-size: 10.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.source-config {
  margin-top: 10px; padding: 12px; border-radius: 10px;
  border: 1px solid rgba(120,130,150,0.13); background: rgba(255,255,255,0.34);
}
.source-grid { display: grid; grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr); gap: 10px; }
.camera-source-grid { grid-template-columns: minmax(0, 1.3fr) minmax(0, 1fr) minmax(0, .85fr); }
.source-grid label, .screen-profile-grid label { display: block; font-size: 11.5px; color: var(--text-secondary); margin-bottom: 5px; }
.screen-profile-grid {
  display: grid; grid-template-columns: minmax(0, 1.25fr) minmax(0, .8fr) minmax(0, 1fr) minmax(0, 1fr);
  gap: 10px; margin-top: 10px;
}
.screen-bitrate-input { display: flex; align-items: center; gap: 6px; }
.screen-bitrate-input .inp { min-width: 0; }
.screen-bitrate-input span { flex: 0 0 auto; color: var(--text-tertiary); font-size: 12px; }
.screen-bitrate-input .inp { flex: 1 1 auto; }
.source-capability {
  display: flex; align-items: center; gap: 8px; margin-bottom: 10px;
  color: var(--text-secondary); font-size: 11.5px;
}
.cap-badge { padding: 2px 8px; border-radius: 10px; font-size: 10.5px; font-weight: 650; }
.cap-badge.ok { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); }
.cap-badge.warn { color: var(--warning); background: color-mix(in srgb, var(--warning) 12%, transparent); }
.file-row { display: flex; gap: 8px; align-items: stretch; }
.file-row .inp { flex: 1 1 auto; }
.fg-hint { font-size: 11.5px; color: var(--text-tertiary); margin-top: 8px; line-height: 1.5; }
.file-btn {
  flex: 0 0 auto; border: 1px solid var(--border-default); background: rgba(255,255,255,0.6);
  border-radius: var(--radius-sm); padding: 0 14px; font-size: 12px; color: var(--text-secondary);
  cursor: pointer; white-space: nowrap; min-height: 34px;
}
.file-btn:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
.file-btn:disabled { cursor: not-allowed; opacity: .55; }
.file-btn.compact { min-height: 30px; padding: 0 11px; }
.file-btn.primary { border-color: var(--accent); color: #fff; background: var(--accent); }
.source-notice { display: flex; flex-direction: column; gap: 3px; margin-top: 9px; padding: 8px 10px; border-radius: 7px; font-size: 11px; line-height: 1.45; }
.source-notice.error { color: var(--error); background: color-mix(in srgb, var(--error) 8%, transparent); }
.source-footer { display: flex; align-items: flex-end; justify-content: space-between; gap: 10px; margin-top: 10px; }
.source-uri { min-width: 0; flex: 1; }
.source-uri > span { display: block; font-size: 10.5px; color: var(--text-tertiary); margin-bottom: 3px; }
.source-uri code {
  display: block; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  color: var(--text-secondary); font-size: 10.5px; font-family: "SF Mono", Menlo, monospace;
}
.source-actions { display: flex; gap: 7px; flex: 0 0 auto; }
.probe-result { margin-top: 9px; padding: 8px 10px; border-radius: 7px; font-size: 11.5px; line-height: 1.45; }
.probe-result.loading { color: var(--accent); background: rgba(56,132,255,0.08); }
.probe-result.success { color: var(--success); background: color-mix(in srgb, var(--success) 10%, transparent); }
.probe-result.warning { color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); }
.probe-result.error { color: var(--error); background: color-mix(in srgb, var(--error) 9%, transparent); }
@media (max-width: 580px) {
  .source-modes, .source-grid, .screen-profile-grid { grid-template-columns: 1fr; }
  .source-footer { align-items: stretch; flex-direction: column; }
}

/* 云台控制可视化 */
.ptz-panel { margin-top: 0; }
.ptz-sub { font-size: 12px; color: var(--text-tertiary); margin-bottom: 16px; }
.ptz-body { display: flex; gap: 28px; align-items: stretch; }
@media (max-width: 820px) {
  .ptz-body { flex-direction: column; }
  .ptz-stage { flex-basis: 300px; min-height: 300px; width: 100%; }
}

/* 桌面球机：固定底座承载水平转台，U 型支架夹持可俯仰球形机芯。 */
.ptz-stage {
  position: relative; flex: 0 0 390px; min-height: 340px; overflow: hidden;
  border-radius: 20px; perspective: 900px; isolation: isolate;
  background:
    radial-gradient(circle at 50% 38%, rgba(78,145,255,.2), transparent 31%),
    linear-gradient(155deg, rgba(245,250,255,.98), rgba(215,229,245,.78));
  border: 1px solid rgba(91,126,171,.2);
  box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -28px 60px rgba(57,87,126,.09);
}
.stage-grid {
  position: absolute; inset: 54% -12% -36%; transform: rotateX(65deg); transform-origin: center top;
  background-image: linear-gradient(rgba(68,111,166,.12) 1px, transparent 1px), linear-gradient(90deg, rgba(68,111,166,.12) 1px, transparent 1px);
  background-size: 28px 28px; mask-image: linear-gradient(to bottom, #000, transparent 82%);
}
.stage-caption {
  position: absolute; z-index: 9; left: 15px; top: 14px; display: flex; align-items: center; gap: 6px;
  padding: 5px 9px; border-radius: 9px; color: #527196; background: rgba(255,255,255,.66);
  border: 1px solid rgba(92,126,168,.14); box-shadow: 0 4px 14px rgba(39,67,103,.08);
}
.stage-caption .live-dot { width: 6px; height: 6px; border-radius: 50%; background: #22c55e; box-shadow: 0 0 8px #22c55e; }
.stage-caption b { font: 800 9px/1 "SF Mono", Menlo, monospace; letter-spacing: .7px; }
.stage-caption em { padding-left: 6px; border-left: 1px solid rgba(82,113,150,.18); font-size: 9px; font-style: normal; color: var(--text-tertiary); }
.ptz-stage.active .stage-caption .live-dot { background: var(--accent); box-shadow: 0 0 10px var(--accent); animation: liveBlink 1s ease-in-out infinite; }
@keyframes liveBlink { 50% { opacity: .35; } }
.compass-ring {
  position: absolute; z-index: 2; left: 50%; top: 63%; width: 246px; height: 82px;
  margin-left: -123px; border-radius: 50%; transform: rotateX(68deg);
  border: 2px solid rgba(56,132,255,.3); box-shadow: 0 0 0 8px rgba(255,255,255,.3), inset 0 0 24px rgba(56,132,255,.12);
}
.compass-dial {
  position: absolute; inset: 5px; border-radius: 50%; transition: transform .12s linear;
  background: repeating-conic-gradient(from -1deg, rgba(61,105,158,.58) 0 2deg, transparent 2deg 10deg);
}
.compass-dial::after { content: ""; position: absolute; inset: 27%; border-radius: 50%; border: 1px dashed rgba(56,132,255,.24); }
.compass-dial span { position: absolute; color: #52749e; font-size: 10px; font-weight: 800; text-shadow: 0 1px #fff; }
.compass-dial .north { left: 50%; top: -2px; transform: translateX(-50%); color: var(--accent); }
.compass-dial .east { right: 2px; top: 50%; transform: translateY(-50%); }
.compass-dial .south { left: 50%; bottom: -2px; transform: translateX(-50%); }
.compass-dial .west { left: 2px; top: 50%; transform: translateY(-50%); }
.heading-readout {
  position: absolute; z-index: 2; left: 50%; top: 50%; transform: translate(-50%, -50%) rotateX(-68deg);
  color: var(--accent); font: 800 11px/1 "SF Mono", Menlo, monospace; background: rgba(255,255,255,.88);
  padding: 4px 8px; border-radius: 9px; box-shadow: 0 2px 9px rgba(43,68,100,.14);
}
.direction-hud span {
  position: absolute; z-index: 10; display: grid; place-items: center; width: 28px; height: 28px;
  border-radius: 9px; color: #8ba0bb; background: rgba(255,255,255,.58); border: 1px solid rgba(101,129,166,.17);
  font-size: 17px; transition: .18s ease;
}
.direction-hud span.on { color: #fff; background: var(--accent); box-shadow: 0 0 18px var(--accent-glow); transform: scale(1.13); }
.hud-up { top: 14px; left: calc(50% - 14px); }
.hud-down { bottom: 14px; left: calc(50% - 14px); }
.hud-left { left: 14px; top: calc(50% - 14px); }
.hud-right { right: 14px; top: calc(50% - 14px); }
.tilt-gauge {
  position: absolute; z-index: 9; right: 22px; top: 69px; width: 46px; height: 100px;
  color: #7890ad; font: 700 8px/1 "SF Mono", Menlo, monospace;
}
.tilt-gauge > span { position: absolute; right: 0; }
.tilt-plus { top: 0; } .tilt-zero { top: 46px; } .tilt-minus { bottom: 0; }
.tilt-track { position: absolute; left: 5px; top: 4px; width: 2px; height: 92px; border-radius: 2px; background: linear-gradient(var(--accent), #a9bdd5); }
.tilt-track::before, .tilt-track::after { content: ""; position: absolute; left: -3px; width: 8px; height: 1px; background: #7890ad; }
.tilt-track::before { top: 0; } .tilt-track::after { bottom: 0; }
.tilt-marker {
  position: absolute; left: -1px; top: 43px; min-width: 29px; height: 15px; padding-left: 10px; border-radius: 7px;
  color: #fff; background: var(--accent); font-size: 8px; line-height: 15px; box-shadow: 0 3px 9px var(--accent-glow);
  transition: transform .12s linear;
}
.tilt-marker::before { content: ""; position: absolute; left: -3px; top: 5px; border-width: 3px 4px 3px 0; border-style: solid; border-color: transparent var(--accent) transparent transparent; }
.axis-label { position: absolute; z-index: 8; color: #7890ad; font: 700 8px/1 "SF Mono", Menlo, monospace; letter-spacing: .4px; }
.pan-axis { left: 22px; bottom: 20px; } .tilt-axis { right: 18px; bottom: 20px; }
.ptz-stage.active { box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -28px 60px rgba(57,87,126,.09), 0 0 0 2px rgba(56,132,255,.18); }
.ptz-stage.seeking { animation: stageSeekPulse 1.15s ease-in-out infinite; }
@keyframes stageSeekPulse { 50% { box-shadow: inset 0 1px 0 rgba(255,255,255,.95), inset 0 -28px 60px rgba(57,87,126,.09), 0 0 26px rgba(56,132,255,.32); } }

.ptz-info { flex: 1 1 auto; display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; min-width: 0; align-content: center; }
.ptz-stat {
  display: flex; flex-direction: column; gap: 6px; min-width: 0; padding: 13px 14px; border-radius: 12px;
  background: linear-gradient(145deg, rgba(255,255,255,.7), rgba(239,245,252,.46));
  border: 1px solid rgba(105,132,167,.14);
}
.ptz-stat span { font-size: 11px; color: var(--text-tertiary); width: auto; flex-shrink: 0; text-transform: uppercase; letter-spacing: .4px; }
.ptz-stat b { font-size: 14px; color: var(--text-primary); min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.ptz-stat b.hot { color: var(--accent); }
.ptz-stat b.pose { font-variant-numeric: tabular-nums; font-size: 13px; }
.ptz-stat.pose-stat, .ptz-stat.action-state, .ptz-stat.presets { grid-column: 1 / -1; }
.action-chips { display: flex; flex-wrap: wrap; gap: 6px; }
.action-chip {
  display: inline-flex; align-items: center; justify-content: center; min-width: 38px; height: 24px; padding: 0 8px;
  border-radius: 7px; color: var(--text-tertiary); background: rgba(255,255,255,.55);
  border: 1px solid var(--border-default); font-size: 11px; transition: all .16s ease;
}
.action-chip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 12px var(--accent-glow); transform: translateY(-1px); }
.action-chip.stop.on { background: #64748b; border-color: #64748b; box-shadow: 0 0 12px rgba(100,116,139,.25); }
.action-state em { color: var(--text-secondary); font-size: 11px; font-style: normal; }
.ptz-stat.presets { justify-content: center; }
.preset-chips { display: flex; gap: 6px; flex-wrap: wrap; }
.pchip {
  display: inline-flex; align-items: center; justify-content: center; gap: 6px;
  min-width: 84px; height: 26px; padding: 0 9px; border-radius: 7px;
  font-size: 11px; font-weight: 600; color: var(--text-secondary);
  background: rgba(255,255,255,0.5); border: 1px solid var(--border-default); transition: all 0.15s;
}
.pchip small {
  display: inline-grid; place-items: center; width: 16px; height: 16px; border-radius: 5px;
  color: var(--accent); background: var(--accent-dim); font: 700 9px/1 "SF Mono", Menlo, monospace;
}
.pchip.on { color: #fff; background: var(--accent); border-color: var(--accent); box-shadow: 0 0 10px var(--accent-glow); transform: translateY(-1px); }
.pchip.on small { color: var(--accent); background: #fff; }

.ptz-runtime {
  margin-top: 20px; padding-top: 18px; border-top: 1px solid rgba(105,132,167,.16);
}
.ptz-runtime-title {
  margin-bottom: 11px; color: var(--text-secondary); font-size: 12px; font-weight: 650;
  letter-spacing: .4px; text-transform: uppercase;
}
.ptz-runtime-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.ptz-runtime .rt-block {
  min-width: 0; padding: 12px 14px; border: 1px solid rgba(105,132,167,.13); border-radius: 11px;
  background: rgba(255,255,255,.38);
}
.ptz-runtime .rt-label { margin-bottom: 7px; }
.ptz-runtime .sub-list { margin-top: 0; }
.ptz-runtime .progress-block { grid-column: 1 / -1; }
@media (max-width: 820px) {
  .ptz-runtime-grid { grid-template-columns: 1fr; }
  .ptz-runtime .progress-block { grid-column: auto; }
}

/* 长任务进度 */
.prog-wrap { display: flex; align-items: center; gap: 12px; margin-top: 8px; }
.prog-bar { flex: 1; height: 8px; background: rgba(120,120,120,0.15); border-radius: 4px; overflow: hidden; }
.prog-fill { height: 100%; background: var(--accent); border-radius: 4px; transition: width 0.4s; }
.prog-txt { font-size: 12.5px; color: var(--text-secondary); font-family: monospace; }

/* 活跃订阅面板 */
.sub-list { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 8px; }
.sub-item { display: inline-flex; align-items: center; gap: 8px; padding: 6px 12px;
  background: rgba(5,150,105,0.08); border-radius: 8px; font-size: 13px; }
.sub-dot { width: 8px; height: 8px; border-radius: 50%; background: #059669;
  box-shadow: 0 0 0 3px rgba(5,150,105,0.15); }
.sub-kind { font-weight: 600; color: var(--text-primary); }
.sub-count { color: var(--text-tertiary); font-size: 12px; }

/* 平台命令时间线 */
.cmd-empty { color: var(--text-tertiary); font-size: 13px; padding: 12px 0; }
.cmd-list { margin-top: 8px; max-height: 260px; overflow-y: auto; }
.cmd-item { display: flex; align-items: center; gap: 10px; padding: 5px 0;
  border-bottom: 1px solid rgba(120,120,120,0.08); font-size: 12.5px; }
.cmd-ts { color: var(--text-tertiary); font-family: monospace; font-size: 11.5px; flex-shrink: 0; }
.cmd-tag { flex-shrink: 0; font-size: 11px; font-weight: 600; padding: 1px 8px; border-radius: 8px;
  background: rgba(56,132,255,0.12); color: var(--accent); }
.cmd-tag.k-control { background: rgba(234,88,12,0.12); color: #ea580c; }
.cmd-tag.k-invite { background: rgba(5,150,105,0.12); color: #059669; }
.cmd-tag.k-broadcast { background: rgba(139,92,246,0.12); color: #8b5cf6; }
.cmd-sum { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

/* SIP 信令追踪面板 */
.trace-panel { margin-top: 18px; }
.trace-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
.trace-ctl { display: inline-flex; align-items: center; gap: 14px; }
.trace-toggle { font-size: 13px; color: var(--text-secondary); cursor: pointer; user-select: none; }
.trace-clear {
  border: 1px solid var(--border-default); background: rgba(255,255,255,0.5);
  border-radius: 6px; padding: 3px 12px; font-size: 12px; color: var(--text-secondary); cursor: pointer;
}
.trace-log {
  height: 300px; overflow-y: auto; overflow-x: hidden; border-radius: var(--radius-sm);
  background: rgba(15, 23, 42, 0.03); border: 1px solid var(--border-default);
  padding: 8px 10px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px;
}
.trace-empty { color: var(--text-secondary); padding: 12px; text-align: center; }
.trace-row {
  display: flex; align-items: baseline; gap: 8px; padding: 3px 4px;
  border-bottom: 1px solid rgba(15,23,42,0.04); white-space: nowrap;
  cursor: pointer; transition: background 0.12s;
}
.trace-row:hover { background: rgba(56,132,255,0.05); }
.trace-row.open { background: rgba(56,132,255,0.08); }
.trace-caret { flex: 0 0 auto; color: var(--text-tertiary); font-size: 10px; width: 10px; }
.trace-raw {
  margin: 0 0 6px 22px; padding: 10px 12px; background: #0f172a; color: #cbd5e1;
  border-radius: 6px; font-size: 11.5px; line-height: 1.5; white-space: pre-wrap;
  word-break: break-all; max-height: 300px; overflow-y: auto;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }

/* OSD 设置(平台下发) */
.osd-panel { margin-top: 18px; }
.osd-empty { color: var(--text-tertiary); font-size: 13px; padding: 8px 0; }
.osd-applied { padding: 6px 0; }
.osd-row { display: flex; align-items: center; gap: 12px; padding: 8px 0; }
.osd-label { width: 110px; color: var(--text-secondary); font-size: 13px; }
.osd-badge {
  font-size: 12px; font-weight: 600; padding: 2px 10px; border-radius: 10px;
}
.osd-badge.on { color: #059669; background: rgba(5,150,105,0.12); }
.osd-badge.off { color: var(--text-tertiary); background: rgba(120,120,120,0.12); }
.osd-sample {
  font-family: monospace; font-size: 13px; color: var(--text-primary);
  background: #0f172a; color: #fff; padding: 2px 10px; border-radius: 4px;
}
</style>
