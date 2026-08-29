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

type MediaMode = "none" | "file" | "camera" | "screen";
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

const message = useMessage();
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const { profiles, activeId, active, setActive, addProfile, updateProfile, removeProfile } = usePlatform();
const {
  form,
  deviceLive,
  statusMeta,
  canReport,
  startDevice,
  stopDevice,
  reconcile,
} = useDevice();

const editing = ref(false);
const saving = ref(false);
const registrationBusy = ref(false);
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
const sourceProbe = ref("");
let unlistenCaptureState: UnlistenFn | null = null;
let unlistenDeviceError: UnlistenFn | null = null;
let unlistenSubscription: UnlistenFn | null = null;

const subscriptions = reactive<Record<SubscriptionKind, SubscriptionState>>({
  MobilePosition: { kind: "MobilePosition", active: false, notify_count: 0 },
  Catalog: { kind: "Catalog", active: false, notify_count: 0 },
  Alarm: { kind: "Alarm", active: false, notify_count: 0 },
  PTZPosition: { kind: "PTZPosition", active: false, notify_count: 0 },
});

const draft = reactive({
  name: "",
  server_host: "",
  server_port: 5060 as number | null,
  server_id: "",
  server_domain: "",
  password: "",
  device_id: "",
  transport: "UDP" as "UDP" | "TCP",
  audio_transport: "TCP_ACTIVE" as "UDP" | "TCP_ACTIVE" | "TCP_PASSIVE",
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
  return "none";
}

const mediaMode = ref<MediaMode>(detectMediaMode(form.value.video_source.trim()));
const selectedCamera = ref<number | null>(null);
const selectedScreen = ref<number | null>(null);

const mediaModes = [
  { value: "none", label: "仅信令", icon: RadioOutline },
  { value: "camera", label: "摄像头", icon: CameraOutline },
  { value: "screen", label: "屏幕", icon: DesktopOutline },
  { value: "file", label: "视频文件", icon: DocumentOutline },
] as const;
const signalingTransportOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP", value: "TCP" },
];
const audioTransportOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP 主动", value: "TCP_ACTIVE" },
  { label: "TCP 被动", value: "TCP_PASSIVE" },
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
  if (!source) return "当前仅进行 SIP 信令模拟";
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

function resetDraft() {
  if (active.value) {
    draft.name = active.value.name;
    draft.server_host = active.value.server_host;
    draft.server_port = active.value.server_port;
    draft.server_id = active.value.server_id;
    draft.server_domain = active.value.server_domain;
    draft.password = active.value.password;
    draft.transport = active.value.transport;
    draft.audio_transport = active.value.audio_transport;
  }
  draft.device_id = active.value?.device_id || form.value.device_id;
  serverDomainManuallyEdited.value = false;
}

function syncDeviceIdFromProfile() {
  if (!active.value?.device_id) return;
  form.value.device_id = active.value.device_id;
  persistForm();
}

function switchSipProfile(id: string) {
  if (deviceLive.value || id === activeId.value) return;
  editing.value = false;
  setActive(id);
  syncDeviceIdFromProfile();
  resetDraft();
}

function addSipProfile() {
  if (deviceLive.value) return;
  const source = active.value;
  addProfile({
    name: `SIP 配置 ${profiles.value.length + 1}`,
    server_host: source?.server_host ?? "",
    server_port: source?.server_port ?? 5060,
    server_id: source?.server_id ?? "",
    server_domain: source?.server_domain ?? "",
    device_id: source?.device_id || form.value.device_id,
    password: source?.password ?? "",
    transport: source?.transport ?? "UDP",
    audio_transport: source?.audio_transport ?? "TCP_ACTIVE",
    signaling_encoding: source?.signaling_encoding ?? "GB18030",
  });
  resetDraft();
  editing.value = true;
}

function deleteSipProfile() {
  if (deviceLive.value || profiles.value.length <= 1) return;
  editing.value = false;
  removeProfile(activeId.value);
  syncDeviceIdFromProfile();
  resetDraft();
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

function saveConfig() {
  if (!canSave.value || !active.value) return;
  saving.value = true;
  updateProfile(active.value.id, {
    name: draft.name.trim(),
    server_host: draft.server_host.trim(),
    server_port: draft.server_port ?? 5060,
    server_id: draft.server_id.trim(),
    server_domain: draft.server_domain.trim(),
    device_id: draft.device_id.trim(),
    password: draft.password,
    transport: draft.transport,
    audio_transport: draft.audio_transport,
  });
  form.value.device_id = draft.device_id.trim();
  persistForm();
  editing.value = false;
  saving.value = false;
  message.success("首页配置已保存");
}

function cancelEdit() {
  resetDraft();
  editing.value = false;
}

function syncLiveSource() {
  sourceProbe.value = "";
  if (mediaMode.value === "none") {
    form.value.video_source = "";
  } else if (mediaMode.value === "camera" && selectedCamera.value !== null) {
    form.value.video_source = `live:camera:${selectedCamera.value}?audio=none&audio_codec=g711a`;
  } else if (mediaMode.value === "screen" && selectedScreen.value !== null) {
    form.value.video_source = `live:screen:${selectedScreen.value}?audio=none&audio_codec=g711a&width=1280&height=720&bitrate=2500&codec=h264`;
  }
  persistForm();
}

async function setMediaMode(mode: MediaMode) {
  if (deviceLive.value) return;
  mediaMode.value = mode;
  if (mode === "file") {
    await pickVideoFile();
    return;
  }
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
  } else if (!form.value.video_source) {
    mediaMode.value = "none";
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
  const result = await startDevice(active.value);
  if (result.ok) message.success(result.msg);
  else message.error(result.msg);
}

async function toggleRegistration() {
  if (registrationBusy.value) return;
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

watch(deviceLive, (running) => {
  if (!running) {
    captureState.value = "stopped";
    hasFrame.value = false;
    previewState.value = "idle";
    previewLatency.value = null;
  }
});

watch(active, () => {
  syncDeviceIdFromProfile();
  if (!editing.value) resetDraft();
});

onMounted(async () => {
  syncDeviceIdFromProfile();
  resetDraft();
  if (!isTauri) return;
  unlistenCaptureState = await listen<string>("capture_state", (event) => {
    captureState.value = event.payload as CaptureState;
    if (event.payload === "ready" && previewPageActive) void startPreview();
    if (event.payload === "stopped" || event.payload === "error") {
      hasFrame.value = false;
      previewState.value = event.payload === "error" ? "error" : "idle";
    }
  });
  unlistenDeviceError = await listen<DeviceErrorEvent>("device_error", (event) => {
    if (event.payload.scope === "capture" && event.payload.message) {
      previewError.value = event.payload.message;
    }
  });
  unlistenSubscription = await listen<SubscriptionState>("subscription_state", (event) => {
    if (event.payload.kind in subscriptions) {
      subscriptions[event.payload.kind] = event.payload;
    }
  });
  const [runtime] = await Promise.all([reconcile(), refreshSources(false)]);
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
  await startPreview();
});

onActivated(async () => {
  previewPageActive = true;
  const runtime = await reconcile();
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
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
  unlistenSubscription?.();
});
</script>

<template>
  <div class="home-page">
    <section class="state-strip">
      <div class="device-state">
        <span class="state-dot" :style="{ background: statusMeta.color }" />
        <div><small>设备状态</small><b :style="{ color: statusMeta.color }">{{ statusMeta.text }}</b></div>
      </div>
      <div class="state-item"><small>设备 ID</small><b class="mono">{{ form.device_id }}</b></div>
      <div class="state-item"><small>目标平台</small><b>{{ active?.name ?? "未配置" }}</b></div>
      <div class="state-item"><small>协议版本</small><b>GB/T 28181-{{ form.gb_version }}</b></div>
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
            <span v-if="previewLatency !== null">{{ previewFps }} FPS · {{ previewLatency }} ms · 跳 {{ previewSkipped }}</span>
            <span v-else>1280x720 · H.264</span>
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
            <span v-else class="source-empty">不发送媒体流</span>
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
            <n-button v-if="!editing" size="small" type="primary" secondary :disabled="deviceLive" @click="editing = true">编辑</n-button>
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
          <n-select
            size="medium"
            :value="activeId"
            :options="sipProfileOptions"
            :disabled="deviceLive || editing"
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
          <label v-if="editing" class="field wide">
            <span>配置名称</span>
            <n-input v-model:value="draft.name" size="medium" placeholder="例如 本地平台" />
          </label>
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
          <label class="field wide">
            <span>对讲传输</span>
            <n-select v-model:value="draft.audio_transport" size="medium" :disabled="!editing" :options="audioTransportOptions" />
          </label>
        </div>

        <div class="primary-actions">
          <n-button :type="deviceLive ? 'error' : 'primary'" size="large" :loading="registrationBusy" :disabled="registrationBusy" @click="toggleRegistration">
            <template #icon><n-icon><RadioOutline /></n-icon></template>
            {{ deviceLive ? "注销设备" : "注册设备" }}
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
.config-panel { display: flex; flex-direction: column; }
.panel-head { display: flex; align-items: center; justify-content: space-between; min-width: 0; }
.panel-head.compact { min-height: 38px; }
.panel-kicker { display: block; color: var(--accent); font-size: 9px; font-weight: 750; }
.panel-head h2 { margin: 1px 0 0; color: var(--text-primary); font-size: 16px; font-weight: 720; }
.preview-status { display: inline-flex; align-items: center; gap: 7px; color: var(--text-secondary); font-size: 11px; }
.preview-status.playing i { background: var(--success); }
.preview-status.starting i { background: var(--warning); }
.preview-status.error i { background: var(--error); }

.preview-stage { position: relative; min-height: 260px; overflow: hidden; border-radius: 6px; background: #0a1728; }
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
.source-empty { display: block; color: var(--text-tertiary); font-size: 11px; }
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
