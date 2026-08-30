<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { NButton, NIcon, NInput, NInputNumber, NPopconfirm, NSelect, useMessage } from "naive-ui";
import {
  AddOutline,
  AlertCircleOutline,
  CameraOutline,
  CheckmarkCircleOutline,
  DesktopOutline,
  DocumentOutline,
  FolderOpenOutline,
  LocationOutline,
  OptionsOutline,
  RadioOutline,
  RefreshOutline,
  SaveOutline,
  TrashOutline,
  VideocamOutline,
  WarningOutline,
} from "@vicons/ionicons5";
import { persistForm, useDevice } from "../device";
import { usePlatform } from "../platform";
import { usePreviewSession } from "../composables/preview/session";

type MediaMode = "camera" | "screen" | "file";
type PreviewState = "idle" | "starting" | "playing" | "stopped" | "error";
type CaptureState = "stopped" | "starting" | "ready" | "error";

interface LiveAvDevice {
  index: number;
  name: string;
}

interface LiveScreenDevice {
  display_id: number;
  width: number;
  height: number;
  name: string;
}

interface LiveSourceCatalog {
  ffmpeg_available: boolean;
  cameras: LiveAvDevice[];
  microphones: LiveAvDevice[];
  screens: LiveScreenDevice[];
  screen_error: string | null;
  avfoundation_error: string | null;
}

interface DeviceErrorEvent {
  scope?: string;
  message?: string;
}

type SubscriptionKind = "MobilePosition" | "Catalog" | "Alarm" | "PTZPosition";

interface SubscriptionState {
  kind: SubscriptionKind;
  active: boolean;
  notify_count: number;
}

interface DeviceRuntimeState {
  guarded: boolean;
  alarming: boolean;
  longitude: number;
  latitude: number;
  name: string;
  expiration: number;
  heartbeat_interval: number;
  heartbeat_count: number;
  video_record_plan_type: number | null;
  alarm_record_duration: number | null;
  picture_mask_enabled: number | null;
  frame_mirror_mode: number | null;
  alarm_report_enabled: number | null;
  osd_time_show: number | null;
  osd_show: number | null;
  last_change: string;
  updated_at_ms: number;
}

interface CmdEntry { kind: string; summary: string; ts_ms: number; }

const message = useMessage();
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const {
  config, profiles, activeId, active,
  setActive, addProfile, removeProfile, saveDesktopConfig,
  passwordFor,
} = usePlatform();
const {
  form,
  deviceState,
  deviceLive,
  effectiveConfig,
  statusMeta,
  canReport,
  startDevice,
  stopDevice,
  reconcile,
} = useDevice();

const editing = ref(false);
const saving = ref(false);
const registrationBusy = ref(false);
const registrationActionLabel = computed(() =>
  deviceState.value === "Failed" && deviceLive.value ? "停止重试" : deviceLive.value ? "注销设备" : "注册设备",
);
const sourceLoading = ref(false);
const previewState = ref<PreviewState>("idle");
const captureState = ref<CaptureState>("stopped");
const previewError = ref("");
const canvasRef = ref<HTMLCanvasElement | null>(null);
const hasFrame = ref(false);
let session: ReturnType<typeof usePreviewSession> | null = null;
let previewPageActive = true;
const previewLatency = ref<number | null>(null);
const previewFps = ref(0);
const previewSkipped = ref(0);
const previewWidth = ref<number | null>(null);
const previewHeight = ref<number | null>(null);
const sourceProbe = ref("");
let unlistenCaptureState: UnlistenFn | null = null;
let unlistenDeviceError: UnlistenFn | null = null;
let unlistenSub: UnlistenFn | null = null;
let unlistenPlatformCommand: UnlistenFn | null = null;
let runtimeRefreshTimer: number | null = null;

const subscriptions = reactive<Record<SubscriptionKind, SubscriptionState>>({
  MobilePosition: { kind: "MobilePosition", active: false, notify_count: 0 },
  Catalog: { kind: "Catalog", active: false, notify_count: 0 },
  Alarm: { kind: "Alarm", active: false, notify_count: 0 },
  PTZPosition: { kind: "PTZPosition", active: false, notify_count: 0 },
});
const runtimeState = ref<DeviceRuntimeState | null>(null);
const runtimeStateError = ref("无运行中设备");
const runtimeUpdatedAt = computed(() => runtimeState.value?.updated_at_ms
  ? new Date(runtimeState.value.updated_at_ms).toLocaleString("zh-CN", { hour12: false })
  : "—");

function resetSubscriptions() {
  for (const item of Object.values(subscriptions)) {
    item.active = false;
    item.notify_count = 0;
  }
}

function resetRuntimeUi() {
  runtimeState.value = null;
  runtimeStateError.value = "无运行中设备";
  resetSubscriptions();
  if (runtimeRefreshTimer) {
    clearTimeout(runtimeRefreshTimer);
    runtimeRefreshTimer = null;
  }
}

async function refreshRuntimeState() {
  if (!isTauri || !deviceLive.value) {
    resetRuntimeUi();
    return;
  }
  try {
    runtimeState.value = await invoke<DeviceRuntimeState>("get_device_runtime_state");
    runtimeStateError.value = "";
  } catch (error) {
    runtimeState.value = null;
    runtimeStateError.value = String(error).includes("设备未启动") ? "无运行中设备" : `状态读取失败：${String(error)}`;
  }
}

function scheduleRuntimeRefresh() {
  void refreshRuntimeState();
  if (runtimeRefreshTimer) clearTimeout(runtimeRefreshTimer);
  // Tauri 事件与后续 invoke 处在不同调度队列，补一次短延迟对账，避免界面读到命令前快照。
  runtimeRefreshTimer = window.setTimeout(() => {
    runtimeRefreshTimer = null;
    void refreshRuntimeState();
  }, 80);
}

const draft = reactive({
  name: "",
  server_host: "",
  server_port: 5060 as number | null,
  server_id: "",
  server_domain: "",
  password: "",
  device_id: "",
  transport: "UDP" as "UDP" | "TCP",
});
const serverDomainManuallyEdited = ref(false);

const catalog = ref<LiveSourceCatalog>({
  ffmpeg_available: false,
  cameras: [],
  microphones: [],
  screens: [],
  screen_error: null,
  avfoundation_error: null,
});

function detectMediaMode(source: string): MediaMode {
  if (source.startsWith("live:camera:")) return "camera";
  if (source.startsWith("live:screen:")) return "screen";
  if (source) return "file";
  return "camera";
}

const mediaMode = ref<MediaMode>(detectMediaMode(form.value.video_source.trim()));
const selectedCamera = ref<number | null>(null);
const selectedScreen = ref<number | null>(null);

const mediaModes = [
  { value: "camera", label: "摄像头", icon: CameraOutline },
  { value: "screen", label: "屏幕", icon: DesktopOutline },
  { value: "file", label: "视频文件", icon: DocumentOutline },
] as const;
const signalingTransportOptions = [
  { label: "UDP（当前支持）", value: "UDP" },
  { label: "TCP（信令层尚未实现）", value: "TCP" },
];
const sipProfileOptions = computed(() =>
  profiles.value.map((profile) => ({ label: profile.name, value: profile.id })),
);

const cameraOptions = computed(() =>
  catalog.value.cameras.map((item) => ({ label: `[${item.index}] ${item.name}`, value: item.index })),
);
const screenOptions = computed(() =>
  catalog.value.screens.map((item) => ({
    label: `${item.name} · ${item.width}x${item.height}`,
    value: item.display_id,
  })),
);
const mediaModeLabel = computed(() => mediaModes.find((item) => item.value === mediaMode.value)?.label ?? "未配置");
const mediaSourceReady = computed(() => form.value.video_source.trim().length > 0);
const previewStateText = computed(() => ({
  idle: "等待启动",
  starting: "等待画面",
  playing: "采集中",
  stopped: "已停止",
  error: "采集异常",
}[previewState.value]));
const previewEmptyTitle = computed(() => {
  if (!deviceLive.value) return "虚拟摄像机未启动";
  if (captureState.value === "stopped") return "尚未配置视频源";
  if (captureState.value === "error") return "视频采集启动失败";
  return "等待采集画面";
});
const previewEmptyHint = computed(() => {
  if (!deviceLive.value) return "选择采集来源并注册设备后显示同帧预览";
  if (captureState.value === "stopped") return "请选择摄像头、屏幕或视频文件后重新注册设备";
  return previewError.value || "采集源就绪后会自动订阅并显示首帧";
});
const sourceSummary = computed(() => {
  const source = form.value.video_source.trim();
  if (!source) {
    if (mediaMode.value === "camera") return "尚未选择摄像头";
    if (mediaMode.value === "screen") return "尚未选择屏幕";
    return "尚未选择视频文件";
  }
  if (source.startsWith("live:camera:")) return `电脑摄像头 ${source.match(/^live:camera:(\d+)/)?.[1] ?? ""}`;
  if (source.startsWith("live:screen:")) return `电脑屏幕 ${source.match(/^live:screen:(\d+)/)?.[1] ?? ""}`;
  return source.split(/[\\/]/).pop() || source;
});
const deviceIdValid = computed(() => /^\d{20}$/.test(draft.device_id));
const platformValid = computed(() =>
  draft.name.trim().length > 0 &&
  draft.server_host.trim().length > 0 &&
  (draft.server_port === null || draft.server_port > 0) &&
  /^\d{20}$/.test(draft.server_id) &&
  (draft.server_domain === "" || /^\d{10}$/.test(draft.server_domain)),
);
const canSave = computed(() => editing.value && deviceIdValid.value && platformValid.value && !deviceLive.value);
const displayedDevice = computed(() => effectiveConfig.value?.device ?? config.value?.device);
const displayedProfile = computed(() => effectiveConfig.value?.profile ?? active.value);

function resetDraft() {
  if (active.value) {
    draft.name = active.value.name;
    draft.server_host = active.value.server_host;
    draft.server_port = active.value.server_port;
    draft.server_id = active.value.server_id;
    draft.server_domain = active.value.server_domain;
    draft.password = active.value.password;
    draft.transport = active.value.transport;
  }
  draft.device_id = config.value?.device.device_id ?? form.value.device_id;
  serverDomainManuallyEdited.value = false;
}

function beginEdit() {
  if (deviceLive.value) return;
  resetDraft();
  editing.value = true;
}

async function switchSipProfile(id: string) {
  if (deviceLive.value || id === activeId.value) return;
  editing.value = false;
  try {
    await setActive(id);
    resetDraft();
  } catch (error) {
    message.error(`切换配置失败：${String(error)}`);
  }
}

async function addSipProfile() {
  if (deviceLive.value) return;
  const source = active.value;
  try {
    await addProfile({
      name: `SIP 配置 ${profiles.value.length + 1}`,
      server_host: source?.server_host ?? "127.0.0.1",
      server_port: source?.server_port ?? 5060,
      server_id: source?.server_id ?? "34020000002000000001",
      server_domain: source?.server_domain ?? "3402000000",
      password: "",
      transport: source?.transport ?? "UDP",
      gb_version: source?.gb_version ?? "V2022",
      signaling_encoding: source?.signaling_encoding ?? "Gb18030",
    });
    resetDraft();
    editing.value = true;
  } catch (error) {
    message.error(`新增配置失败：${String(error)}`);
  }
}

async function deleteSipProfile() {
  if (deviceLive.value || profiles.value.length <= 1) return;
  editing.value = false;
  try {
    await removeProfile(activeId.value);
    resetDraft();
  } catch (error) {
    message.error(`删除配置失败：${String(error)}`);
  }
}

function updateServerId(value: string) {
  draft.server_id = value.replace(/\D/g, "").slice(0, 20);
  if (!serverDomainManuallyEdited.value) draft.server_domain = draft.server_id.slice(0, 10);
}

function updateServerDomain(value: string) {
  draft.server_domain = value.replace(/\D/g, "").slice(0, 10);
  serverDomainManuallyEdited.value = true;
}

function updateDeviceId(value: string) {
  draft.device_id = value.replace(/\D/g, "").slice(0, 20);
}

function clearDraft() {
  draft.server_host = "";
  draft.server_port = null;
  draft.server_id = "";
  draft.server_domain = "";
  draft.password = "";
  draft.device_id = "";
  serverDomainManuallyEdited.value = false;
}

async function saveConfig() {
  if (!canSave.value || !active.value || !config.value) return;
  saving.value = true;
  try {
    const profileId = active.value.id;
    await saveDesktopConfig({
      ...config.value,
      profiles: config.value.profiles.map((profile) => profile.id === profileId ? {
        ...profile,
        name: draft.name.trim(),
        server_host: draft.server_host.trim(),
        server_port: draft.server_port ?? 5060,
        server_id: draft.server_id.trim(),
        server_domain: draft.server_domain.trim(),
        password: draft.password,
        transport: draft.transport,
      } : profile),
      device: { ...config.value.device, device_id: draft.device_id.trim() },
    });
    editing.value = false;
    message.success("首页配置已保存");
  } catch (error) {
    message.error(`保存失败：${String(error)}`);
  } finally {
    saving.value = false;
  }
}

function cancelEdit() {
  resetDraft();
  editing.value = false;
}

function syncLiveSource() {
  sourceProbe.value = "";
  if (mediaMode.value === "camera") {
    form.value.video_source = selectedCamera.value === null
      ? ""
      : `live:camera:${selectedCamera.value}?audio=none&audio_codec=g711a`;
  } else if (mediaMode.value === "screen") {
    form.value.video_source = selectedScreen.value === null
      ? ""
      : `live:screen:${selectedScreen.value}?audio=none&audio_codec=g711a&width=1280&height=720&bitrate=2500&codec=h264`;
  }
  persistForm();
}

async function setMediaMode(mode: MediaMode) {
  if (deviceLive.value) return;
  if (mode === "file") {
    await pickVideoFile();
    return;
  }
  mediaMode.value = mode;
  if (mode === "camera" && selectedCamera.value === null) {
    selectedCamera.value = catalog.value.cameras[0]?.index ?? null;
  }
  if (mode === "screen" && selectedScreen.value === null) {
    selectedScreen.value = catalog.value.screens[0]?.display_id ?? null;
  }
  syncLiveSource();
}

async function pickVideoFile() {
  if (!isTauri) {
    message.info("浏览器预览不能读取本机文件，请在桌面应用中选择");
    return;
  }
  const picked = await openDialog({
    multiple: false,
    directory: false,
    filters: [{ name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] }],
  });
  if (typeof picked === "string") {
    form.value.video_source = picked;
    mediaMode.value = "file";
    persistForm();
  }
}

async function refreshSources(showFeedback = true) {
  if (!isTauri) return;
  sourceLoading.value = true;
  try {
    catalog.value = await invoke<LiveSourceCatalog>("list_live_sources");
    const cameraMatch = form.value.video_source.match(/^live:camera:(\d+)/);
    const screenMatch = form.value.video_source.match(/^live:screen:(\d+)/);
    selectedCamera.value = cameraMatch ? Number(cameraMatch[1]) : catalog.value.cameras[0]?.index ?? null;
    selectedScreen.value = screenMatch ? Number(screenMatch[1]) : catalog.value.screens[0]?.display_id ?? null;
    if (mediaMode.value === "camera" || mediaMode.value === "screen") syncLiveSource();
    if (showFeedback) message.success("采集设备已刷新");
  } catch (error) {
    sourceProbe.value = `设备枚举失败：${String(error)}`;
  } finally {
    sourceLoading.value = false;
  }
}

async function probeSource() {
  if (!isTauri || !form.value.video_source.startsWith("live:")) return;
  sourceProbe.value = "正在测试采集…";
  try {
    const result = await invoke<{ message: string }>("probe_live_source", { uri: form.value.video_source });
    sourceProbe.value = result.message;
  } catch (error) {
    sourceProbe.value = `采集测试失败：${String(error)}`;
  }
}

async function registerDevice() {
  const result = await startDevice(active.value, active.value ? passwordFor(active.value.id) : "");
  if (result.ok) message.success(result.msg);
  else message.error(result.msg);
}

async function toggleRegistration() {
  if (registrationBusy.value || editing.value) return;
  if (!deviceLive.value && !mediaSourceReady.value) return;
  if (!deviceLive.value && active.value?.transport === "TCP") {
    message.error("信令 TCP 尚未实现，请改用 UDP");
    return;
  }
  registrationBusy.value = true;
  try {
    if (!deviceLive.value) {
      await registerDevice();
      return;
    }
    const result = await stopDevice();
    if (result.ok) message.success(result.msg);
    else message.error(result.msg);
  } finally {
    registrationBusy.value = false;
  }
}

async function fireAlarm() {
  try {
    message.success(await invoke<string>("fire_alarm", { description: "移动侦测报警" }));
  } catch (error) {
    message.error(String(error));
  }
}

async function startPreview() {
  if (!previewPageActive || !isTauri || !deviceLive.value || captureState.value !== "ready" || previewState.value === "playing" || previewState.value === "starting") return;
  previewError.value = "";
  previewState.value = "starting";
  if (!session) session = usePreviewSession(canvasRef.value, {
    onState: (next) => { previewState.value = next; },
    onError: (error) => { previewError.value = error; },
    onFrame: (stats) => {
      hasFrame.value = true;
      previewLatency.value = stats.latencyMs;
      previewFps.value = stats.fps;
      previewSkipped.value = stats.skipped;
      previewWidth.value = canvasRef.value?.width ?? null;
      previewHeight.value = canvasRef.value?.height ?? null;
    },
  });
  await session.start();
}

async function retryPreview() {
  if (!session || !previewPageActive) return;
  previewError.value = "";
  try {
    await session.retry();
  } catch (error) {
    previewState.value = "error";
    previewError.value = `预览重试失败：${String(error)}`;
  }
}

function clearPreviewStats() {
  previewLatency.value = null;
  previewFps.value = 0;
  previewSkipped.value = 0;
  previewWidth.value = null;
  previewHeight.value = null;
}

watch(deviceLive, (running) => {
  if (!running) {
    captureState.value = "stopped";
    hasFrame.value = false;
    previewState.value = "idle";
    clearPreviewStats();
    resetRuntimeUi();
  } else {
    scheduleRuntimeRefresh();
  }
});

watch(active, () => {
  if (!editing.value) resetDraft();
});

onMounted(async () => {
  resetDraft();
  if (!isTauri) return;
  unlistenCaptureState = await listen<string>("capture_state", (event) => {
    captureState.value = event.payload as CaptureState;
    if (event.payload === "ready" && previewPageActive) void startPreview();
    if (event.payload === "stopped" || event.payload === "error") {
      hasFrame.value = false;
      clearPreviewStats();
      previewState.value = event.payload === "error" ? "error" : "idle";
    }
  });
  unlistenDeviceError = await listen<DeviceErrorEvent>("device_error", (event) => {
    if (event.payload.scope === "register" && event.payload.message) {
      message.error(event.payload.message);
    }
    if (event.payload.scope === "capture" && event.payload.message) {
      previewError.value = event.payload.message;
    }
  });
  unlistenSub = await listen<SubscriptionState>("subscription_state", (event) => {
    if (event.payload.kind in subscriptions) {
      subscriptions[event.payload.kind] = event.payload;
    }
    void refreshRuntimeState();
  });
  unlistenPlatformCommand = await listen<CmdEntry>("platform_command", scheduleRuntimeRefresh);
  const [runtime] = await Promise.all([reconcile(), refreshSources(false)]);
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
  await refreshRuntimeState();
  await startPreview();
});

onActivated(async () => {
  previewPageActive = true;
  const runtime = await reconcile();
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
  await refreshRuntimeState();
  await startPreview();
});

onDeactivated(() => {
  previewPageActive = false;
  void session?.stop();
});

onUnmounted(() => {
  previewPageActive = false;
  void session?.stop();
  unlistenCaptureState?.();
  unlistenDeviceError?.();
  unlistenSub?.();
  unlistenPlatformCommand?.();
  if (runtimeRefreshTimer) clearTimeout(runtimeRefreshTimer);
});
</script>

<template>
  <div class="home-page">
    <section class="state-strip">
      <div class="device-state">
        <span class="state-dot" :style="{ background: statusMeta.color }" />
        <div><small>设备状态</small><b :style="{ color: statusMeta.color }">{{ statusMeta.text }}</b></div>
      </div>
      <div class="state-item"><small>设备 ID</small><b class="mono">{{ displayedDevice?.device_id ?? "—" }}</b></div>
      <div class="state-item"><small>目标平台</small><b>{{ displayedProfile?.name ?? "未配置" }}</b></div>
      <div class="state-item"><small>协议版本</small><b>GB/T 28181-{{ displayedProfile?.gb_version === "V2016" ? "2016" : "2022" }}</b></div>
      <div class="state-item"><small>采集来源</small><b>{{ mediaModeLabel }}</b></div>
    </section>

    <main class="home-workspace">
      <section class="preview-panel">
        <div class="panel-head">
          <div>
            <span class="panel-kicker">VIDEO CAPTURE</span>
            <h2>视频采集预览</h2>
          </div>
          <div class="preview-status" :class="previewState"><i />{{ previewStateText }}</div>
        </div>

        <div class="preview-slot">
          <div class="preview-stage">
            <canvas v-show="hasFrame" ref="canvasRef" aria-label="设备采集预览" />
            <div v-if="!hasFrame" class="preview-cover">
              <div class="cover-mark"><n-icon :size="42"><VideocamOutline /></n-icon></div>
              <strong>{{ previewEmptyTitle }}</strong>
              <span>{{ previewEmptyHint }}</span>
              <n-button v-if="previewState === 'error' && captureState === 'ready'" size="small" type="primary" secondary @click="retryPreview">
                重试预览
              </n-button>
            </div>
            <div class="preview-overlay top-left"><i :class="{ on: previewState === 'playing' }" /> UVP-SIM</div>
            <div class="preview-overlay bottom-row">
              <span>{{ sourceSummary }}</span>
              <span v-if="previewWidth !== null && previewHeight !== null">
                本地预览 {{ previewWidth }}×{{ previewHeight }} · {{ previewFps }} FPS · {{ previewLatency }} ms · 跳 {{ previewSkipped }}
              </span>
              <span v-else>本地预览待启动</span>
            </div>
          </div>
        </div>

        <div class="source-bar">
          <div class="source-modes" aria-label="媒体来源">
            <button
              v-for="item in mediaModes"
              :key="item.value"
              type="button"
              :class="{ active: mediaMode === item.value }"
              :disabled="deviceLive"
              @click="setMediaMode(item.value)"
            >
              <n-icon :component="item.icon" />
              {{ item.label }}
            </button>
          </div>
          <div class="source-picker">
            <n-select
              v-if="mediaMode === 'camera'"
              v-model:value="selectedCamera"
              size="small"
              :options="cameraOptions"
              :loading="sourceLoading"
              :disabled="deviceLive"
              placeholder="选择摄像头"
              @update:value="syncLiveSource"
            />
            <n-select
              v-else-if="mediaMode === 'screen'"
              v-model:value="selectedScreen"
              size="small"
              :options="screenOptions"
              :loading="sourceLoading"
              :disabled="deviceLive"
              placeholder="选择屏幕"
              @update:value="syncLiveSource"
            />
            <button v-else-if="mediaMode === 'file'" type="button" class="file-source" :disabled="deviceLive" @click="pickVideoFile">
              {{ sourceSummary }}
            </button>
          </div>
          <n-button quaternary circle size="small" :loading="sourceLoading" :disabled="deviceLive" title="刷新采集设备" @click="refreshSources()">
            <template #icon><n-icon><RefreshOutline /></n-icon></template>
          </n-button>
          <n-button size="small" secondary :disabled="deviceLive || !form.video_source.startsWith('live:')" @click="probeSource">测试采集</n-button>
        </div>
        <div v-if="sourceProbe" class="source-feedback">{{ sourceProbe }}</div>
      </section>

      <aside class="config-panel">
        <div class="panel-head compact">
          <div>
            <span class="panel-kicker">SIP CONNECTION</span>
            <h2>SIP 配置</h2>
          </div>
          <div class="config-actions">
            <n-button v-if="!editing" size="small" type="primary" secondary :disabled="deviceLive" @click="beginEdit">编辑配置</n-button>
            <template v-else>
              <n-button size="small" quaternary @click="clearDraft">重置</n-button>
              <n-button size="small" quaternary @click="cancelEdit">取消</n-button>
              <n-button size="small" type="primary" :loading="saving" :disabled="!canSave" @click="saveConfig">
                <template #icon><n-icon><SaveOutline /></n-icon></template>
                完成
              </n-button>
            </template>
          </div>
        </div>

        <div class="sip-profile-switcher">
          <n-input
            v-if="editing"
            v-model:value="draft.name"
            size="medium"
            aria-label="配置名称"
            placeholder="例如 本地平台"
          />
          <n-select
            v-else
            size="medium"
            :value="activeId"
            :options="sipProfileOptions"
            :disabled="deviceLive"
            aria-label="SIP 配置"
            @update:value="switchSipProfile"
          />
          <n-button
            size="small"
            type="success"
            secondary
            circle
            :disabled="deviceLive || editing"
            title="复制新增 SIP 配置"
            @click="addSipProfile"
          >
            <template #icon><n-icon><AddOutline /></n-icon></template>
          </n-button>
          <n-popconfirm
            :disabled="deviceLive || editing || profiles.length <= 1"
            positive-text="删除"
            negative-text="取消"
            @positive-click="deleteSipProfile"
          >
            <template #trigger>
              <n-button
                size="small"
                type="error"
                secondary
                circle
                :disabled="deviceLive || editing || profiles.length <= 1"
                title="删除当前 SIP 配置"
              >
                <template #icon><n-icon><TrashOutline /></n-icon></template>
              </n-button>
            </template>
            删除“{{ active?.name }}”配置？
          </n-popconfirm>
        </div>

        <div class="config-grid">
          <label class="field wide">
            <span>服务器</span>
            <div class="host-port">
              <n-input v-model:value="draft.server_host" size="medium" :disabled="!editing" placeholder="例如 192.168.1.100" />
              <n-input-number v-model:value="draft.server_port" size="medium" :disabled="!editing" :min="1" :max="65535" :show-button="false" placeholder="5060" />
            </div>
          </label>
          <label class="field wide">
            <span>服务器 ID</span>
            <n-input
              :value="draft.server_id"
              size="medium"
              :disabled="!editing"
              maxlength="20"
              placeholder="例如 34020000002000000001"
              @update:value="updateServerId"
            />
          </label>
          <label class="field">
            <span>服务器域</span>
            <n-input
              :value="draft.server_domain"
              size="medium"
              :disabled="!editing"
              maxlength="10"
              placeholder="例如 3402000000"
              @update:value="updateServerDomain"
            />
          </label>
          <label class="field">
            <span>信令传输</span>
            <n-select v-model:value="draft.transport" size="medium" :disabled="!editing" :options="signalingTransportOptions" />
          </label>
          <label class="field wide">
            <span>设备 ID</span>
            <n-input
              :value="draft.device_id"
              size="medium"
              :disabled="!editing"
              maxlength="20"
              placeholder="例如 34020000001310000001"
              @update:value="updateDeviceId"
            />
          </label>
          <label class="field wide">
            <span>注册密码</span>
            <n-input v-model:value="draft.password" size="medium" :disabled="!editing" type="password" show-password-on="click" placeholder="上级平台配置的 SIP 密码" />
          </label>
          <div class="field wide capability-note">对讲传输将在后续阶段接入；当前不保存伪配置。</div>
        </div>

        <div class="primary-actions">
          <n-button :type="deviceLive ? 'error' : 'primary'" size="large" :loading="registrationBusy" :disabled="registrationBusy || editing || (!deviceLive && !mediaSourceReady) || (!deviceLive && active?.transport === 'TCP')" @click="toggleRegistration">
            <template #icon><n-icon><RadioOutline /></n-icon></template>
            {{ registrationActionLabel }}
          </n-button>
        </div>

        <div class="quick-actions" aria-label="快捷业务">
          <div class="quick-card unavailable" title="桌面端录像能力尚未接入">
            <n-icon><VideocamOutline /></n-icon>
            <strong>录像</strong>
            <span>未就绪</span>
          </div>
          <button
            type="button"
            class="quick-card action"
            :class="{ subscribed: subscriptions.Alarm.active }"
            :disabled="!canReport"
            @click="fireAlarm"
          >
            <n-icon><WarningOutline /></n-icon>
            <strong>报警</strong>
            <span>{{ canReport ? "可触发" : "未就绪" }}</span>
            <i v-if="subscriptions.Alarm.active" class="subscription-dot" title="报警订阅已建立" />
          </button>
          <div class="quick-card subscription" :class="{ active: subscriptions.MobilePosition.active }">
            <n-icon><LocationOutline /></n-icon>
            <strong>位置订阅</strong>
            <span><i />{{ subscriptions.MobilePosition.active ? "已订阅" : "未订阅" }}</span>
          </div>
          <div class="quick-card subscription" :class="{ active: subscriptions.Catalog.active }">
            <n-icon><FolderOpenOutline /></n-icon>
            <strong>目录订阅</strong>
            <span><i />{{ subscriptions.Catalog.active ? "已订阅" : "未订阅" }}</span>
          </div>
          <div class="quick-card subscription" :class="{ active: subscriptions.PTZPosition.active }">
            <n-icon><OptionsOutline /></n-icon>
            <strong class="long-label">PTZ 精准位置订阅</strong>
            <span><i />{{ subscriptions.PTZPosition.active ? "已订阅" : "未订阅" }}</span>
          </div>
        </div>

        <section class="runtime-truth" aria-labelledby="runtime-truth-title">
          <div class="runtime-head">
            <div>
              <span class="panel-kicker">DEVICE RUNTIME</span>
              <h3 id="runtime-truth-title">设备状态真相</h3>
            </div>
            <small v-if="runtimeState">{{ runtimeUpdatedAt }}</small>
          </div>
          <div v-if="runtimeState" class="runtime-grid">
            <div><span>布防状态</span><b :class="runtimeState.guarded ? 'on' : ''">{{ runtimeState.guarded ? "已布防" : "未布防" }}</b></div>
            <div><span>报警状态</span><b :class="runtimeState.alarming ? 'alarm' : ''">{{ runtimeState.alarming ? "报警中" : "无报警" }}</b></div>
            <div><span>报警订阅</span><b :class="subscriptions.Alarm.active ? 'on' : ''">{{ subscriptions.Alarm.active ? "平台已订阅" : "未订阅" }}</b></div>
            <div><span>当前位置</span><b class="mono">{{ runtimeState.longitude.toFixed(4) }}, {{ runtimeState.latitude.toFixed(4) }}</b></div>
            <div><span>最近变更</span><b class="mono">{{ runtimeState.last_change }}</b></div>
            <div class="runtime-wide"><span>会话参数</span><b>{{ runtimeState.name || "未命名设备" }} · 注册 {{ runtimeState.expiration }}s · 心跳 {{ runtimeState.heartbeat_interval }}s × {{ runtimeState.heartbeat_count }}</b></div>
            <div class="runtime-wide"><span>平台已应用配置</span><b>录像计划 {{ runtimeState.video_record_plan_type ?? "—" }} · 报警录像 {{ runtimeState.alarm_record_duration ?? "—" }}s · 遮挡 {{ runtimeState.picture_mask_enabled === 1 ? "开" : "关" }} · 镜像 {{ runtimeState.frame_mirror_mode ?? "—" }} · 报警上报 {{ runtimeState.alarm_report_enabled === 1 ? "开" : "关" }} · OSD {{ runtimeState.osd_show === 1 ? "开" : "关" }}</b></div>
          </div>
          <div v-else class="runtime-empty">{{ runtimeStateError || "无运行中设备" }}</div>
        </section>

        <div class="config-foot">
          <n-icon :component="deviceIdValid && platformValid ? CheckmarkCircleOutline : AlertCircleOutline" />
          <span>{{ deviceIdValid && platformValid ? "SIP 配置完整，可以启动模拟" : "服务器、服务器 ID、设备 ID 不能为空" }}</span>
        </div>
      </aside>
    </main>
  </div>
</template>

<style scoped>
.home-page {
  width: min(1440px, 100%);
  height: 100%;
  min-height: 0;
  margin: 0 auto;
  display: grid;
  grid-template-rows: 62px minmax(0, 1fr);
  gap: 10px;
  overflow: hidden;
}
.preview-status i { width: 7px; height: 7px; border-radius: 50%; background: var(--error); }

.state-strip {
  display: grid;
  grid-template-columns: 1.05fr 1.55fr 1.15fr 1.15fr .85fr;
  align-items: center;
  min-width: 0;
  padding: 8px 14px;
  border: 1px solid var(--border-default);
  border-radius: 8px;
  background: rgba(255, 255, 255, .58);
}
.device-state, .state-item { min-width: 0; padding: 0 13px; border-right: 1px solid var(--border-subtle); }
.device-state { display: flex; align-items: center; gap: 10px; padding-left: 2px; }
.state-item:last-child { border-right: 0; }
.state-dot { width: 9px; height: 9px; flex: 0 0 auto; border-radius: 50%; }
.state-strip small { display: block; color: var(--text-tertiary); font-size: 10px; }
.state-strip b { display: block; margin-top: 2px; overflow: hidden; color: var(--text-primary); font-size: 12px; font-weight: 650; text-overflow: ellipsis; white-space: nowrap; }
.mono { font-family: "SF Mono", Menlo, monospace; }

.home-workspace {
  min-height: 0;
  display: grid;
  grid-template-columns: minmax(0, 1.45fr) minmax(345px, .8fr);
  gap: 12px;
}
.preview-panel, .config-panel {
  min-width: 0;
  min-height: 0;
  padding: 14px;
  border: 1px solid var(--border-default);
  border-radius: 8px;
  background: rgba(255, 255, 255, .62);
  box-shadow: 0 8px 24px rgba(32, 51, 79, .06);
}
.preview-panel { display: grid; grid-template-rows: 42px minmax(0, 1fr) 40px auto; gap: 9px; }
.config-panel {
  display: flex;
  flex-direction: column;
  overflow-x: hidden;
  overflow-y: auto;
}
.panel-head { display: flex; align-items: center; justify-content: space-between; min-width: 0; }
.panel-head.compact { min-height: 38px; }
.panel-kicker { display: block; color: var(--accent); font-size: 9px; font-weight: 750; }
.panel-head h2 { margin: 1px 0 0; color: var(--text-primary); font-size: 16px; font-weight: 720; }
.preview-status { display: inline-flex; align-items: center; gap: 7px; color: var(--text-secondary); font-size: 11px; }
.preview-status.playing i { background: var(--success); }
.preview-status.starting i { background: var(--warning); }
.preview-status.error i { background: var(--error); }

.preview-slot {
  min-width: 0;
  min-height: 0;
  display: grid;
  place-items: center;
  container-type: size;
}
.preview-stage {
  position: relative;
  width: min(100cqw, calc(100cqh * 16 / 9));
  aspect-ratio: 16 / 9;
  overflow: hidden;
  border-radius: 6px;
  background: #0a1728;
}
.preview-stage > img, .preview-stage > canvas { width: 100%; height: 100%; object-fit: contain; }
.preview-cover { position: absolute; inset: 0; display: grid; place-content: center; justify-items: center; gap: 8px; color: rgba(218, 230, 248, .68); text-align: center; }
.preview-cover::before { content: ""; position: absolute; inset: 0; background: linear-gradient(rgba(93, 132, 183, .07) 1px, transparent 1px), linear-gradient(90deg, rgba(93, 132, 183, .07) 1px, transparent 1px); background-size: 42px 42px; mask-image: linear-gradient(to bottom, transparent, #000 24%, #000 76%, transparent); }
.cover-mark { position: relative; display: grid; place-items: center; width: 72px; height: 72px; border: 1px solid rgba(92, 156, 245, .28); border-radius: 50%; color: #74adff; background: rgba(56, 132, 255, .08); }
.preview-cover strong, .preview-cover span { position: relative; }
.preview-cover strong { color: #e4edf9; font-size: 14px; }
.preview-cover span { max-width: 420px; font-size: 11px; }
.preview-overlay { position: absolute; z-index: 2; color: rgba(229, 238, 251, .78); font-family: "SF Mono", Menlo, monospace; font-size: 10px; }
.top-left { top: 11px; left: 12px; display: inline-flex; align-items: center; gap: 6px; }
.top-left i { width: 6px; height: 6px; border-radius: 50%; background: rgba(229, 238, 251, .35); }
.top-left i.on { background: #43d59d; }
.bottom-row { right: 0; bottom: 0; left: 0; display: flex; justify-content: space-between; gap: 12px; padding: 9px 12px; background: linear-gradient(transparent, rgba(3, 10, 20, .78)); }
.bottom-row span { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

.source-bar { display: grid; grid-template-columns: auto minmax(120px, 1fr) auto auto; align-items: center; gap: 7px; min-width: 0; }
.source-modes { display: inline-flex; gap: 3px; padding: 3px; border-radius: 6px; background: var(--bg-hover); }
.source-modes button { height: 27px; display: inline-flex; align-items: center; gap: 4px; padding: 0 8px; border: 0; border-radius: 4px; color: var(--text-tertiary); background: transparent; cursor: pointer; font-size: 10.5px; }
.source-modes button.active { color: var(--accent); background: #fff; box-shadow: 0 1px 4px rgba(31, 55, 88, .1); }
.source-modes button:disabled { cursor: not-allowed; opacity: .55; }
.source-picker { min-width: 0; }
.file-source { width: 100%; height: 30px; overflow: hidden; padding: 0 9px; border: 1px solid var(--border-default); border-radius: 6px; color: var(--text-secondary); background: #fff; cursor: pointer; font-size: 11px; text-align: left; text-overflow: ellipsis; white-space: nowrap; }
.source-feedback { overflow: hidden; color: var(--text-secondary); font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap; }

.config-actions { display: flex; gap: 4px; }
.sip-profile-switcher { display: grid; grid-template-columns: minmax(0, 1fr) 28px 28px; align-items: center; gap: 6px; margin-top: 8px; }
.config-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; margin-top: 10px; }
.config-panel :deep(.n-input--disabled) {
  --n-text-color-disabled: var(--text-secondary) !important;
  --n-color-disabled: rgba(248, 250, 253, .9) !important;
  --n-icon-color-disabled: var(--text-tertiary) !important;
}
.config-panel :deep(.n-base-selection--disabled) {
  --n-text-color-disabled: var(--text-secondary) !important;
  --n-color-disabled: rgba(248, 250, 253, .9) !important;
  --n-arrow-color-disabled: var(--text-tertiary) !important;
}
.field { display: grid; gap: 3px; min-width: 0; }
.field.wide { grid-column: 1 / -1; }
.field > span { color: var(--text-tertiary); font-size: 10px; }
.host-port { display: grid; grid-template-columns: minmax(0, 1fr) 78px; gap: 6px; }
.primary-actions { display: grid; grid-template-columns: minmax(0, 1fr); margin-top: 10px; }
.quick-actions { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 3px; margin-top: 7px; }
.quick-card {
  position: relative;
  min-width: 0;
  height: 74px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 2px;
  padding: 5px 2px;
  border: 1px solid var(--border-default);
  border-radius: 8px;
  color: var(--text-tertiary);
  background: rgba(255, 255, 255, .7);
  font-family: inherit;
}
.quick-card > .n-icon { font-size: 19px; }
.quick-card strong {
  width: 100%;
  overflow: hidden;
  color: currentColor;
  font-size: 10.5px;
  font-weight: 650;
  line-height: 1.2;
  text-align: center;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.quick-card strong.long-label { min-height: 22px; display: grid; place-items: center; font-size: 8px; line-height: 1.15; white-space: normal; }
.quick-card > span { display: inline-flex; align-items: center; gap: 3px; font-family: "SF Mono", Menlo, monospace; font-size: 8px; opacity: .72; white-space: nowrap; }
.quick-card.subscription > span > i { width: 5px; height: 5px; border-radius: 50%; background: currentColor; }
.quick-card.action { color: var(--accent); cursor: pointer; }
.quick-card.action:hover:not(:disabled) { border-color: var(--accent); background: var(--accent-soft); }
.quick-card.action:disabled { cursor: not-allowed; color: var(--text-tertiary); }
.quick-card.subscription.active { border-color: rgba(24, 160, 88, .38); color: var(--success); background: rgba(24, 160, 88, .07); }
.subscription-dot { position: absolute; top: 7px; right: 7px; width: 7px; height: 7px; border-radius: 50%; background: var(--success); }
.runtime-truth { margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--border-subtle); }
.runtime-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 8px; }
.runtime-head h3 { margin: 1px 0 0; color: var(--text-primary); font-size: 13px; }
.runtime-head small { color: var(--text-tertiary); font-size: 9px; white-space: nowrap; }
.runtime-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 5px; margin-top: 7px; }
.runtime-grid > div { min-width: 0; padding: 7px 8px; border: 1px solid var(--border-default); border-radius: 7px; background: rgba(255,255,255,.48); }
.runtime-grid span { display: block; margin-bottom: 2px; color: var(--text-tertiary); font-size: 9px; }
.runtime-grid b { display: block; overflow: hidden; color: var(--text-secondary); font-size: 10px; font-weight: 600; line-height: 1.4; text-overflow: ellipsis; white-space: nowrap; }
.runtime-grid b.on { color: var(--success); }
.runtime-grid b.alarm { color: var(--error); }
.runtime-grid .runtime-wide { grid-column: 1 / -1; }
.runtime-grid .runtime-wide b { white-space: normal; }
.runtime-empty { margin-top: 7px; padding: 11px; border: 1px dashed var(--border-default); border-radius: 7px; color: var(--text-tertiary); text-align: center; font-size: 10px; }
.config-foot { display: flex; align-items: center; gap: 6px; margin-top: 8px; color: var(--text-tertiary); font-size: 10px; }

@media (max-width: 1080px) {
  .home-workspace { grid-template-columns: minmax(0, 1fr) 330px; }
  .state-strip { grid-template-columns: 1fr 1.45fr 1fr 1fr; }
  .state-item:last-child { display: none; }
  .source-modes button { padding-inline: 6px; }
}
@media (max-height: 680px) {
  .home-page { grid-template-rows: 54px minmax(0, 1fr); gap: 7px; }
  .preview-panel, .config-panel { padding: 10px; }
  .preview-panel { grid-template-rows: 36px minmax(0, 1fr) 36px auto; gap: 6px; }
  .sip-profile-switcher { margin-top: 5px; }
  .config-grid { gap: 5px; margin-top: 6px; }
  .primary-actions { margin-top: 6px; }
  .quick-actions, .config-foot { margin-top: 5px; }
}
</style>
