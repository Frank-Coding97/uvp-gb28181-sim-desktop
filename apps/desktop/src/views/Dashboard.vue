<script setup lang="ts">
import { computed, onActivated, onDeactivated, onMounted, onUnmounted, reactive, ref, watch } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { useRouter } from "vue-router";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { NButton, NIcon, NInput, NInputNumber, NModal, NPopconfirm, NSelect, useMessage } from "naive-ui";
import {
  AddOutline,
  AlertCircleOutline,
  CameraOutline,
  CheckmarkCircleOutline,
  DesktopOutline,
  DocumentOutline,
  FolderOpenOutline,
  LinkOutline,
  LocationOutline,
  OptionsOutline,
  RadioOutline,
  RefreshOutline,
  SaveOutline,
  TrashOutline,
  VideocamOutline,
  VolumeHighOutline,
  WarningOutline,
} from "@vicons/ionicons5";
import { persistForm, useDevice } from "../device";
import { usePlatform, type AudioTransport } from "../platform";
import { buildLiveSourceUri, liveSourceAudio } from "../media-profile";
import {
  describeParseFailure,
  describeProvisionGaps,
  parseProvisionLink,
  readProvisionFailure,
  toProfilePatch,
  type ProvisionPayload,
} from "../provision-link";
import { usePreviewSession } from "../composables/preview/session";
import { previewEnabled } from "../composables/preview/optIn";
import { useBroadcastAudio } from "../composables/broadcastAudio";

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

type MediaPreparationPhase = "idle" | "preparing" | "ready" | "cancelled" | "failed";

interface MediaPreparationEvent {
  phase: Exclude<MediaPreparationPhase, "idle">;
  message: string;
}

type SubscriptionKind = "MobilePosition" | "Catalog" | "Alarm" | "PTZPosition";

interface SubscriptionState {
  kind: SubscriptionKind;
  active: boolean;
  notify_count: number;
}

const message = useMessage();
const router = useRouter();
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

const {
  state: broadcastState,
  packets: broadcastPackets,
  muted: broadcastMuted,
  unlock: unlockBroadcastAudio,
  start: startBroadcastAudio,
} = useBroadcastAudio();

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
const previewDecode = ref("");
const sourceProbe = ref("");
let unlistenCaptureState: UnlistenFn | null = null;
let unlistenDeviceError: UnlistenFn | null = null;
let unlistenSub: UnlistenFn | null = null;
let unlistenMediaPreparation: UnlistenFn | null = null;
let recordingRefreshTimer: number | null = null;
const recordingPhase = ref<"idle" | "starting" | "recording" | "finalizing" | "failed">("idle");
const mediaPreparation = ref<{ phase: MediaPreparationPhase; message: string }>({ phase: "idle", message: "" });
const recordingLabel = computed(() => ({
  idle: deviceLive.value ? "可录制" : "设备未启动",
  starting: "等待关键帧",
  recording: "录像中",
  finalizing: "正在收尾",
  failed: "录像失败",
}[recordingPhase.value]));

async function refreshRecordingState() {
  if (!isTauri) return;
  try {
    const state = await invoke<{ phase: typeof recordingPhase.value }>("get_recording_state");
    recordingPhase.value = state.phase;
  } catch {
    recordingPhase.value = "idle";
  }
}

const subscriptions = reactive<Record<SubscriptionKind, SubscriptionState>>({
  MobilePosition: { kind: "MobilePosition", active: false, notify_count: 0 },
  Catalog: { kind: "Catalog", active: false, notify_count: 0 },
  Alarm: { kind: "Alarm", active: false, notify_count: 0 },
  PTZPosition: { kind: "PTZPosition", active: false, notify_count: 0 },
});

function resetSubscriptions() {
  for (const item of Object.values(subscriptions)) {
    item.active = false;
    item.notify_count = 0;
  }
}

async function refreshSubscriptions() {
  if (!isTauri) return;
  try {
    const states = await invoke<SubscriptionState[]>("get_device_subscriptions");
    for (const state of states) {
      if (state.kind in subscriptions) subscriptions[state.kind] = state;
    }
  } catch {
    // 设备未启动时后端会拒绝；保持未订阅状态即可。
  }
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
  audio_transport: "TCP_ACTIVE" as AudioTransport,
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
// 实时源的音频选择，直接对应 URI 的 audio= 取值：
// 摄像头 `none | <麦克风序号>`；屏幕 `none | system | microphone:<序号>`。
const selectedAudio = ref<string>(liveSourceAudio(form.value.video_source));
const isMacHost = typeof navigator !== "undefined" && /Mac/i.test(navigator.userAgent);

const mediaModes = [
  { value: "camera", label: "摄像头", icon: CameraOutline },
  { value: "screen", label: "屏幕", icon: DesktopOutline },
  { value: "file", label: "视频文件", icon: DocumentOutline },
] as const;
const signalingTransportOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP", value: "TCP" },
] as const;
// 对讲流传输独立于信令:语音广播/对讲下行的 RTP 走哪条路。
const audioTransportOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP 主动", value: "TCP_ACTIVE" },
  { label: "TCP 被动", value: "TCP_PASSIVE" },
] as const;
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
const audioOptions = computed(() => {
  const none = { label: "不采集声音", value: "none" };
  if (mediaMode.value === "camera") {
    return [none, ...catalog.value.microphones.map((item) => ({ label: `麦克风 · ${item.name}`, value: String(item.index) }))];
  }
  if (mediaMode.value === "screen") {
    // Windows 屏幕采集的音频要等原生 WASAPI 采集落地，后端目前会直接拒绝。
    if (!isMacHost) return [none];
    return [
      none,
      { label: "系统声音", value: "system" },
      ...catalog.value.microphones.map((item) => ({ label: `麦克风 · ${item.name}`, value: `microphone:${item.index}` })),
    ];
  }
  return [none];
});
const audioPickerHint = computed(() =>
  mediaMode.value === "screen" && !isMacHost ? "Windows 屏幕采集暂不支持声音" : "",
);
const mediaModeLabel = computed(() => mediaModes.find((item) => item.value === mediaMode.value)?.label ?? "未配置");
// 设备列表为空时必须说明原因：枚举依赖 FFmpeg 的 DirectShow/AVFoundation 后端，
// 找不到 FFmpeg 时后端只会返回空列表，界面上就变成一个打不开的下拉框。
const sourceHint = computed(() => {
  if (sourceLoading.value || deviceLive.value) return "";
  if (mediaMode.value === "camera" && catalog.value.cameras.length === 0) {
    if (!catalog.value.ffmpeg_available) {
      return "未找到 FFmpeg，无法枚举摄像头：安装 FFmpeg 并加入 PATH，或用 UVP_FFMPEG_PATH 指向 ffmpeg.exe";
    }
    return catalog.value.avfoundation_error || "未检测到摄像头设备";
  }
  if (mediaMode.value === "screen" && catalog.value.screens.length === 0) {
    return catalog.value.screen_error || "未检测到可采集的屏幕";
  }
  return "";
});
const mediaSourceReady = computed(() => form.value.video_source.trim().length > 0);
const previewStateText = computed(() => (previewEnabled.value ? {
  idle: "等待启动",
  starting: "等待画面",
  playing: "采集中",
  stopped: "已停止",
  error: "采集异常",
}[previewState.value] : "预览已关闭"));
const previewBlockedReason = computed(() => {
  if (!isTauri) return "仅桌面端可预览";
  if (!deviceLive.value) return "注册设备后可开启预览";
  if (captureState.value !== "ready") return "等待采集就绪";
  return "";
});
const previewEmptyTitle = computed(() => {
  if (!previewEnabled.value) return "本地预览已关闭";
  if (!deviceLive.value) return "虚拟摄像机未启动";
  if (captureState.value === "stopped") return "尚未配置视频源";
  if (captureState.value === "error") return "视频采集启动失败";
  return "等待采集画面";
});
const previewEmptyHint = computed(() => {
  if (!previewEnabled.value) {
    return previewBlockedReason.value || "关闭预览不影响采集与平台推流；需要查看画面时再开启";
  }
  if (!deviceLive.value) return "选择采集来源并注册设备后显示同帧预览";
  if (captureState.value === "stopped") return "请选择摄像头、屏幕或视频文件后重新注册设备";
  return previewError.value || "正在等待首个关键帧";
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

// 注册按钮为什么不可点。
//
// 之前按钮的 disabled 由四个条件直接拼在模板里，而页面底部的提示只看 SIP 三个字段，
// 于是出现过「提示说 SIP 配置完整、可以启动模拟，按钮却是灰的」——实际卡在没选采集
// 来源（video_source 为空）。这里把判定收成一处，模板和提示共用，保证说出来的原因
// 就是真正挡住按钮的那一条。
// 顺序即优先级：编辑态最先（此时连注销也不该点），其次是采集来源、传输方式、SIP 字段。
const registerBlocker = computed(() => {
  if (editing.value) return "正在编辑配置，点「完成」保存后再操作";
  if (!deviceLive.value) {
    if (!mediaSourceReady.value) return "先在左侧选择采集来源（摄像头 / 屏幕 / 视频文件）";
    if (!deviceIdValid.value || !platformValid.value) return "服务器、服务器 ID、设备 ID 不能为空";
  }
  return "";
});
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
    draft.audio_transport = active.value.audio_transport ?? "TCP_ACTIVE";
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

// 「配置 N」的默认名取第一个未被占用的序号:删掉过配置后,
// 直接用 length+1 会和现存名称撞车,而 addProfile 不校验重名。
function nextProfileName() {
  const used = new Set(profiles.value.map((profile) => profile.name.trim()));
  let index = profiles.value.length + 1;
  while (used.has(`SIP 配置 ${index}`)) index += 1;
  return `SIP 配置 ${index}`;
}

async function addSipProfile() {
  if (deviceLive.value) return;
  const source = active.value;
  try {
    await addProfile({
      name: nextProfileName(),
      server_host: source?.server_host ?? "127.0.0.1",
      server_port: source?.server_port ?? 5060,
      server_id: source?.server_id ?? "34020000002000000001",
      server_domain: source?.server_domain ?? "3402000000",
      password: "",
      transport: source?.transport ?? "UDP",
      audio_transport: source?.audio_transport ?? "TCP_ACTIVE",
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

// ── 粘贴接入链接 ──────────────────────────────────────────
// 平台「扫码接入」卡片里的链接本是给设备扫的,桌面端没有摄像头,
// 粘贴成了唯一可用的传递方式。链路复用平台已有的一次性 token 兑换端点,
// 所以这里不需要任何新的平台接口。
const provisionVisible = ref(false);
const provisionLink = ref("");
const provisionName = ref("");
const provisionBusy = ref(false);
const provisionError = ref("");
const provisionNeedsInsecure = ref(false);

function openProvision() {
  if (deviceLive.value) {
    message.warning("设备运行中,请先注销设备再接入");
    return;
  }
  provisionVisible.value = true;
  provisionLink.value = "";
  // 粘贴接入 = 新增一份配置,所以默认名沿用新增配置的编号规则。
  provisionName.value = nextProfileName();
  provisionError.value = "";
  provisionNeedsInsecure.value = false;
}

async function applyProvision(allowInsecure: boolean) {
  if (provisionBusy.value) return;
  const parsed = parseProvisionLink(provisionLink.value);
  if (!parsed.ok) {
    provisionError.value = describeParseFailure(parsed.reason);
    provisionNeedsInsecure.value = false;
    return;
  }
  // 裸接入码不含平台地址。这里不拿当前配置的服务器地址去拼 ——
  // 那个地址是「要注册到的 SIP 服务器」,未必是签发这张链接的平台。
  if (!parsed.value.baseUrl) {
    provisionError.value = "请粘贴完整链接(以 http:// 或 https:// 开头)";
    provisionNeedsInsecure.value = false;
    return;
  }

  // 名称与链接一样要在兑换前校验:兑换是「一次即消费」,
  // 校验放到兑换之后,用户改个名字就得回平台重新生成一张码。
  const name = provisionName.value.trim();
  if (!name) {
    provisionError.value = "请填写配置名称";
    // 上一次若停在证书错误上,「仍然继续」不该在这一轮继续挂着。
    provisionNeedsInsecure.value = false;
    return;
  }
  if (profiles.value.some((profile) => profile.name.trim() === name)) {
    provisionError.value = `已有同名配置「${name}」,请换一个名称`;
    provisionNeedsInsecure.value = false;
    return;
  }

  provisionBusy.value = true;
  provisionError.value = "";
  provisionNeedsInsecure.value = false;
  try {
    const payload = await invoke<ProvisionPayload>("exchange_sip_provision", {
      baseUrl: parsed.value.baseUrl,
      token: parsed.value.token,
      allowInsecure,
    });
    const patch = toProfilePatch(payload);
    // 新建一份配置,而不是把接入参数覆盖到当前配置上:
    // 覆盖会把「本地平台」这份还能用的配置改成别的平台的地址,名称却还留在原处。
    await addProfile({
      name,
      gb_version: active.value?.gb_version ?? "V2022",
      signaling_encoding: active.value?.signaling_encoding ?? "Gb18030",
      audio_transport: "TCP_ACTIVE",
      ...patch,
    });
    // 落盘后 active 已切到新配置,再进编辑态让用户核对/微调。
    resetDraft();
    // 域值来自平台,不能被「服务器 ID 前 10 位」的联动规则改写。
    serverDomainManuallyEdited.value = true;
    editing.value = true;
    provisionVisible.value = false;
    provisionLink.value = "";

    const gaps = describeProvisionGaps(patch);
    if (gaps.length) {
      message.warning(`已新增配置「${name}」,但${gaps.join("、")}，请修正后保存`);
    } else {
      message.success(`已新增配置「${name}」,请核对后点击保存`);
    }
  } catch (cause) {
    const failure = readProvisionFailure(cause);
    provisionError.value = failure.message;
    // 证书不受信任不是终点:是否信任由用户显式决定,重试时才放宽校验。
    provisionNeedsInsecure.value = failure.code === "cert" && !allowInsecure;
  } finally {
    provisionBusy.value = false;
  }
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
        audio_transport: draft.audio_transport,
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
  // 当前模式不支持的取值（例如从屏幕切到摄像头时的 "system"）回落为不采集。
  if (!audioOptions.value.some((item) => item.value === selectedAudio.value)) {
    selectedAudio.value = "none";
  }
  const audioSource = selectedAudio.value;
  if (mediaMode.value === "camera") {
    form.value.video_source = selectedCamera.value === null
      ? ""
      : buildLiveSourceUri("camera", selectedCamera.value, audioSource);
  } else if (mediaMode.value === "screen") {
    form.value.video_source = selectedScreen.value === null
      ? ""
      : buildLiveSourceUri("screen", selectedScreen.value, audioSource);
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

async function cancelMediaPreparation() {
  if (mediaPreparation.value.phase !== "preparing") return;
  try {
    await invoke<string>("cancel_media_preparation");
    mediaPreparation.value = { phase: "cancelled", message: "正在取消媒体准备…" };
  } catch (error) {
    message.error(`取消媒体准备失败：${String(error)}`);
  }
}

async function registerDevice() {
  const result = await startDevice(active.value, active.value ? passwordFor(active.value.id) : "");
  if (result.ok) message.success(result.msg);
  else message.error(result.msg);
}

async function toggleRegistration() {
  if (registrationBusy.value || editing.value) return;
  // 用户手势里解锁 Web Audio:平台发起广播时才能直接出声。
  void unlockBroadcastAudio();
  if (!deviceLive.value && !mediaSourceReady.value) return;
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
  if (!previewEnabled.value || !previewPageActive || !isTauri || !deviceLive.value || captureState.value !== "ready" || previewState.value === "playing" || previewState.value === "starting") return;
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
      previewDecode.value = stats.mode === "webcodecs" ? `${codecLabel(stats.codec)} · 硬件解码` : "JPEG 回退";
    },
  });
  await session.start();
}

function codecLabel(codec: string) {
  if (codec.startsWith("avc1")) return "H.264";
  if (codec.startsWith("hvc1") || codec.startsWith("hev1")) return "H.265";
  return codec || "视频";
}

/** 用户主动开启预览。只在本次应用运行期间记住,重启后恢复为关闭。 */
async function enablePreview() {
  if (previewBlockedReason.value) return;
  previewEnabled.value = true;
  await startPreview();
}

async function disablePreview() {
  previewEnabled.value = false;
  await session?.stop();
  hasFrame.value = false;
  previewState.value = "idle";
  clearPreviewStats();
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
  previewDecode.value = "";
}

watch(deviceLive, (running) => {
  if (!running) {
    captureState.value = "stopped";
    hasFrame.value = false;
    previewState.value = "idle";
    clearPreviewStats();
    resetSubscriptions();
  } else {
    void refreshSubscriptions();
  }
});

watch(active, () => {
  if (!editing.value) resetDraft();
});

onMounted(async () => {
  resetDraft();
  if (!isTauri) return;
  void startBroadcastAudio();
  unlistenCaptureState = await listen<string>("capture_state", (event) => {
    captureState.value = event.payload as CaptureState;
    // 只恢复用户已开启的预览;默认不自动预览。
    if (event.payload === "ready" && previewPageActive && previewEnabled.value) void startPreview();
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
  });
  unlistenMediaPreparation = await listen<MediaPreparationEvent>("media_preparation", (event) => {
    mediaPreparation.value = event.payload;
  });
  const [runtime] = await Promise.all([reconcile(), refreshSources(false)]);
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
  await refreshSubscriptions();
  await refreshRecordingState();
  recordingRefreshTimer = window.setInterval(() => void refreshRecordingState(), 1000);
  if (previewEnabled.value) await startPreview();
});

onActivated(async () => {
  previewPageActive = true;
  const runtime = await reconcile();
  if (runtime) captureState.value = runtime.capture_state as CaptureState;
  await refreshSubscriptions();
  await refreshRecordingState();
  if (previewEnabled.value) await startPreview();
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
  unlistenMediaPreparation?.();
  if (recordingRefreshTimer) clearInterval(recordingRefreshTimer);
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
          <div class="preview-head-actions">
            <div class="preview-status" :class="previewEnabled ? previewState : 'off'"><i />{{ previewStateText }}</div>
            <n-button v-if="previewEnabled" size="tiny" quaternary @click="disablePreview">关闭预览</n-button>
          </div>
        </div>

        <div class="preview-slot">
          <div class="preview-stage">
            <canvas v-show="hasFrame" ref="canvasRef" aria-label="设备采集预览" />
            <div v-if="!hasFrame" class="preview-cover">
              <div class="cover-mark"><n-icon :size="42"><VideocamOutline /></n-icon></div>
              <strong>{{ previewEmptyTitle }}</strong>
              <span>{{ previewEmptyHint }}</span>
              <n-button
                v-if="!previewEnabled"
                class="preview-enable"
                size="small"
                type="primary"
                :disabled="!!previewBlockedReason"
                :title="previewBlockedReason"
                @click="enablePreview"
              >
                开启预览
              </n-button>
              <n-button v-else-if="previewState === 'error' && captureState === 'ready'" size="small" type="primary" secondary @click="retryPreview">
                重试预览
              </n-button>
            </div>
            <div class="preview-overlay top-left"><i :class="{ on: previewState === 'playing' }" /> UVP-SIM</div>
            <div class="preview-overlay bottom-row">
              <span>{{ sourceSummary }}</span>
              <span v-if="previewWidth !== null && previewHeight !== null">
                本地预览 {{ previewWidth }}×{{ previewHeight }} · {{ previewDecode }} · {{ previewFps }} FPS · {{ previewLatency }} ms · 跳 {{ previewSkipped }}
              </span>
              <span v-else>{{ previewEnabled ? "本地预览待启动" : "本地预览已关闭" }}</span>
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
          <div v-if="mediaMode === 'camera' || mediaMode === 'screen'" class="source-picker audio-picker" :title="audioPickerHint">
            <n-select
              v-model:value="selectedAudio"
              size="small"
              :options="audioOptions"
              :loading="sourceLoading"
              :disabled="deviceLive || audioOptions.length <= 1"
              aria-label="声音来源"
              placeholder="声音来源"
              @update:value="syncLiveSource"
            />
          </div>
          <n-button quaternary circle size="small" :loading="sourceLoading" :disabled="deviceLive" title="刷新采集设备" @click="refreshSources()">
            <template #icon><n-icon><RefreshOutline /></n-icon></template>
          </n-button>
          <n-button size="small" secondary :disabled="deviceLive || !form.video_source.startsWith('live:')" @click="probeSource">测试采集</n-button>
        </div>
        <div v-if="mediaPreparation.phase !== 'idle'" class="media-preparation" :class="mediaPreparation.phase">
          <span v-if="mediaPreparation.phase === 'preparing'" class="preparation-spinner" aria-hidden="true" />
          <span class="preparation-message">{{ mediaPreparation.message }}</span>
          <n-button v-if="mediaPreparation.phase === 'preparing'" size="tiny" quaternary @click="cancelMediaPreparation">取消准备</n-button>
        </div>
        <div v-if="sourceHint" class="source-hint">
          <n-icon :component="AlertCircleOutline" />
          <span>{{ sourceHint }}</span>
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
            <!-- 删除当前配置属破坏性操作,放在头部与「编辑配置」同处:
                 配置文件行只承载「切换 / 接入 / 新增」三类正向动作,
                 避免删除按钮和它们挤在同一行被挤断,也避免误点。 -->
            <template v-if="!editing">
              <n-button size="small" type="primary" secondary :disabled="deviceLive" @click="beginEdit">编辑配置</n-button>
              <n-popconfirm
                :disabled="deviceLive || profiles.length <= 1"
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
                    :disabled="deviceLive || profiles.length <= 1"
                    :title="profiles.length <= 1 ? '至少保留一份 SIP 配置' : '删除当前 SIP 配置'"
                  >
                    <template #icon><n-icon><TrashOutline /></n-icon></template>
                  </n-button>
                </template>
                删除“{{ active?.name }}”配置？
              </n-popconfirm>
            </template>
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
            clearable
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
          <!-- 接入链接会新建一份配置,与「新增」同类,所以共用同一组禁用条件。 -->
          <n-button
            size="small"
            type="info"
            secondary
            :disabled="deviceLive || editing"
            title="粘贴接入链接快速接入"
            @click="openProvision"
          >
            <template #icon><n-icon><LinkOutline /></n-icon></template>
            粘贴接入
          </n-button>
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
        </div>

        <div class="config-grid">
          <label class="field wide">
            <span>服务器</span>
            <div class="host-port">
              <n-input v-model:value="draft.server_host" size="medium" clearable :disabled="!editing" placeholder="例如 192.168.1.100" />
              <n-input-number v-model:value="draft.server_port" size="medium" clearable :disabled="!editing" :min="1" :max="65535" :show-button="false" placeholder="5060" />
            </div>
          </label>
          <label class="field">
            <span>服务器 ID</span>
            <n-input
              :value="draft.server_id"
              size="medium"
              clearable
              :disabled="!editing"
              maxlength="20"
              placeholder="例如 34020000002000000001"
              @update:value="updateServerId"
            >
              <template #suffix>
                <span class="char-count">{{ draft.server_id.length }}/20</span>
              </template>
            </n-input>
          </label>
          <label class="field">
            <span>服务器域</span>
            <n-input
              :value="draft.server_domain"
              size="medium"
              clearable
              :disabled="!editing"
              maxlength="10"
              placeholder="例如 3402000000"
              @update:value="updateServerDomain"
            >
              <template #suffix>
                <span class="char-count">{{ draft.server_domain.length }}/10</span>
              </template>
            </n-input>
          </label>
          <div class="field wide">
            <span>信令传输</span>
            <div class="transport-cards" role="radiogroup" aria-label="信令传输">
              <button
                v-for="option in signalingTransportOptions"
                :key="option.value"
                type="button"
                role="radio"
                class="transport-card"
                :class="{ selected: draft.transport === option.value }"
                :aria-checked="draft.transport === option.value"
                :disabled="!editing"
                @click="draft.transport = option.value"
              >
                <span class="transport-card-title">
                  <n-icon v-if="draft.transport === option.value"><CheckmarkCircleOutline /></n-icon>
                  {{ option.label }}
                </span>
              </button>
            </div>
          </div>
          <label class="field wide">
            <span>设备 ID</span>
            <n-input
              :value="draft.device_id"
              size="medium"
              clearable
              :disabled="!editing"
              maxlength="20"
              placeholder="例如 34020000001310000001"
              @update:value="updateDeviceId"
            >
              <template #suffix>
                <span class="char-count">{{ draft.device_id.length }}/20</span>
              </template>
            </n-input>
          </label>
          <label class="field wide">
            <span>注册密码</span>
            <n-input v-model:value="draft.password" size="medium" clearable :disabled="!editing" type="password" show-password-on="click" placeholder="上级平台配置的 SIP 密码" />
          </label>
          <div class="field wide">
            <span>对讲流传输模式</span>
            <div class="transport-cards" role="radiogroup" aria-label="对讲流传输模式">
              <button
                v-for="option in audioTransportOptions"
                :key="option.value"
                type="button"
                role="radio"
                class="transport-card"
                :class="{ selected: draft.audio_transport === option.value }"
                :aria-checked="draft.audio_transport === option.value"
                :disabled="!editing"
                @click="draft.audio_transport = option.value"
              >
                <span class="transport-card-title">
                  <n-icon v-if="draft.audio_transport === option.value"><CheckmarkCircleOutline /></n-icon>
                  {{ option.label }}
                </span>
              </button>
            </div>
          </div>
        </div>

        <div class="primary-actions">
          <n-button :type="deviceLive ? 'error' : 'primary'" size="large" :loading="registrationBusy" :disabled="registrationBusy || !!registerBlocker" :title="registerBlocker || undefined" @click="toggleRegistration">
            <template #icon><n-icon><RadioOutline /></n-icon></template>
            {{ registrationActionLabel }}
          </n-button>
        </div>

        <div v-if="broadcastState.active" class="broadcast-indicator" role="status">
          <n-icon class="broadcast-icon"><VolumeHighOutline /></n-icon>
          <div class="broadcast-text">
            <strong>对讲中</strong>
            <span>
              通道 {{ broadcastState.channel_id?.slice(-6) || "----" }}
              · {{ broadcastState.transport }}
              · 本地 {{ broadcastState.local_port }}
              · {{ broadcastPackets }} 包
            </span>
          </div>
          <n-button size="tiny" secondary @click="broadcastMuted = !broadcastMuted">
            {{ broadcastMuted ? "恢复声音" : "静音" }}
          </n-button>
        </div>

        <div class="quick-actions" aria-label="快捷业务">
          <button type="button" class="quick-card action" :class="{ active: recordingPhase === 'recording' }" title="打开录像中心" @click="router.push('/recordings')">
            <n-icon><VideocamOutline /></n-icon>
            <strong>录像</strong>
            <span>{{ recordingLabel }}</span>
          </button>
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
          <n-icon :component="registerBlocker ? AlertCircleOutline : CheckmarkCircleOutline" />
          <span>{{ registerBlocker || (deviceLive ? "SIP 已注册，设备运行中" : "SIP 配置完整，可以启动模拟") }}</span>
        </div>
      </aside>
    </main>

    <n-modal
      v-model:show="provisionVisible"
      preset="card"
      title="粘贴接入链接"
      style="width: 520px; max-width: calc(100vw - 32px)"
      :mask-closable="!provisionBusy"
    >
      <div class="provision-body">
        <p class="provision-desc">
          在平台「扫码接入」卡片里复制二维码链接，粘贴到下方即可自动生成一份 SIP 配置。
        </p>
        <label class="provision-field">
          <span>配置名称</span>
          <n-input
            v-model:value="provisionName"
            size="medium"
            clearable
            maxlength="40"
            :disabled="provisionBusy"
            placeholder="例如 会议室模拟器"
          />
        </label>
        <label class="provision-field">
          <span>接入链接</span>
          <n-input
            v-model:value="provisionLink"
            type="textarea"
            :rows="2"
            clearable
            :disabled="provisionBusy"
            placeholder="https://gb28181.example.com/gb28181/qr#t=..."
          />
        </label>
        <div v-if="provisionError" class="provision-error">
          <n-icon><AlertCircleOutline /></n-icon>
          <span>{{ provisionError }}</span>
          <n-button
            v-if="provisionNeedsInsecure"
            size="tiny"
            type="warning"
            secondary
            :loading="provisionBusy"
            @click="applyProvision(true)"
          >
            仍然继续
          </n-button>
        </div>
        <p class="provision-warning">
          接入码一次性有效（平台侧 5 分钟）。兑换成功后原链接与二维码立即失效，不能再用手机扫。
        </p>
      </div>
      <template #footer>
        <div class="provision-actions">
          <n-button :disabled="provisionBusy" @click="provisionVisible = false">取消</n-button>
          <n-button
            type="primary"
            :loading="provisionBusy"
            :disabled="!provisionLink.trim() || !provisionName.trim()"
            @click="applyProvision(false)"
          >
            接入
          </n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
/* 粘贴接入链接:一次性 token 的输入与失败反馈 */
.provision-body { display: flex; flex-direction: column; gap: 10px; }
.provision-desc { margin: 0; color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.provision-field { display: grid; gap: 4px; }
.provision-field > span { color: var(--text-secondary); font-size: 11px; font-weight: 650; }
.provision-error { display: flex; gap: 6px; align-items: center; padding: 7px 9px; color: var(--error); font-size: 12px; line-height: 1.5; background: rgb(220 38 38 / 8%); border-radius: 6px; }
.provision-error span { flex: 1; min-width: 0; }
.provision-warning { margin: 0; color: var(--text-tertiary); font-size: 11px; line-height: 1.6; }
.provision-actions { display: flex; gap: 8px; justify-content: flex-end; }

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
.preview-head-actions { display: inline-flex; align-items: center; gap: 8px; }
.preview-status.off i { background: var(--text-secondary); opacity: .5; }
.preview-enable { position: relative; margin-top: 4px; }
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
.media-preparation { display: flex; align-items: center; gap: 7px; min-width: 0; min-height: 25px; padding: 4px 8px; border-radius: 6px; background: var(--accent-dim); color: var(--text-secondary); font-size: 10.5px; }
.source-hint { display: flex; align-items: center; gap: 6px; min-width: 0; min-height: 25px; padding: 4px 8px; border-radius: 6px; background: color-mix(in srgb, var(--warning) 12%, transparent); color: var(--warning); font-size: 10.5px; }
.media-preparation.ready { background: color-mix(in srgb, var(--success) 10%, transparent); color: var(--success); }
.media-preparation.cancelled, .media-preparation.failed { background: color-mix(in srgb, var(--error) 10%, transparent); color: var(--error); }
.preparation-message { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.preparation-spinner { width: 12px; height: 12px; flex: 0 0 auto; border: 2px solid color-mix(in srgb, var(--accent) 25%, transparent); border-top-color: var(--accent); border-radius: 50%; animation: media-preparation-spin .8s linear infinite; }
@keyframes media-preparation-spin { to { transform: rotate(360deg); } }

.config-actions { display: flex; gap: 4px; }
/* 三列固定:名称输入/下拉 + 「粘贴接入」文字按钮 + 新增图标按钮。
   列数必须与实际子元素数一致,否则多出来的元素会掉到下一行。 */
.sip-profile-switcher { display: grid; grid-template-columns: minmax(0, 1fr) auto 28px; align-items: center; gap: 6px; margin-top: 8px; }
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
.transport-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(110px, 1fr)); gap: 6px; }
.transport-card {
  display: flex;
  align-items: center;
  justify-content: center;
  min-width: 0;
  padding: 7px 9px;
  font: inherit;
  color: inherit;
  cursor: pointer;
  border: 1px solid var(--border-default);
  border-radius: 8px;
  background: transparent;
  transition: border-color .15s, background .15s;
}
.transport-card:hover:not(:disabled) {
  border-color: color-mix(in srgb, var(--accent-primary, #2f7cf6) 55%, var(--border-default));
}
.transport-card.selected {
  border-color: var(--accent-primary, #2f7cf6);
  background: color-mix(in srgb, var(--accent-primary, #2f7cf6) 9%, transparent);
}
.transport-card:disabled { opacity: .65; cursor: not-allowed; }
.transport-card:focus-visible { outline: 2px solid var(--accent-primary, #2f7cf6); outline-offset: 1px; }
.transport-card-title { display: flex; align-items: center; gap: 4px; font-size: 12px; font-weight: 600; }
.transport-card-title .n-icon { color: var(--accent-primary, #2f7cf6); font-size: 14px; }
.char-count { color: var(--text-tertiary); font-size: 10px; font-variant-numeric: tabular-nums; }
.host-port { display: grid; grid-template-columns: minmax(0, 1fr) 78px; gap: 6px; }
.primary-actions { display: grid; grid-template-columns: minmax(0, 1fr); margin-top: 10px; }
.broadcast-indicator {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 7px;
  padding: 6px 9px;
  border: 1px solid color-mix(in srgb, var(--accent-primary, #2f7cf6) 45%, transparent);
  border-radius: 8px;
  background: color-mix(in srgb, var(--accent-primary, #2f7cf6) 10%, transparent);
}
.broadcast-icon { flex: 0 0 auto; color: var(--accent-primary, #2f7cf6); }
.broadcast-text { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
.broadcast-text strong { font-size: 12px; }
.broadcast-text span { font-size: 11px; color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
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
