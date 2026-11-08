<script setup lang="ts">
import { computed, onUnmounted, ref, watch } from "vue";
import { useDevice } from "../device";
import {
  MEDIA_AUDIO_CODECS,
  MEDIA_BITRATE_OPTIONS,
  MEDIA_FPS_OPTIONS,
  MEDIA_GOP_OPTIONS,
  MEDIA_PRESETS,
  MEDIA_SAMPLE_RATES,
  MEDIA_VIDEO_CODECS,
  applyMediaPreset,
  cameraFpsOptions,
  cameraResolutionOptions,
  cameraSupportsResolution,
  clampToCamera,
  liveSourceCameraIndex,
  matchedPreset,
  mediaProfileEqual,
  mediaProfileSummary,
  validateMediaProfile,
  type CameraMode,
  type MediaProfile,
  type MediaQualityPresetId,
} from "../media-profile";
import {
  usePlatform,
  listCameraModes,
  listLocalIpAddresses,
  type BindMode,
  type DeviceSettings,
  type GbVersion,
  type LocalIpOption,
  type NetworkSettings,
  type OsdCapability,
  type OsdLayer,
  type OsdSettings,
  type SignalingEncoding,
  getOsdCapability,
  getEncoderStatus,
  ENCODER_BACKEND_LABELS,
  type EncoderStatus,
} from "../platform";
import {
  OSD_SIZE_OPTIONS,
  OSD_TEXT_MAX_CHARS,
  defaultOsdSettings,
  formatOsdTime,
  normalizeOsdSettings,
  osdPixelSize,
  osdSettingsEqual,
  validateOsdSettings,
} from "../osd-settings";
import {
  defaultFor,
  effective,
  effectiveSummary,
  normalizeForVersion,
  optionsFor,
  parseEncoding,
} from "../signaling-charset.mjs";

type SettingKey = "device" | "media" | "osd" | "network" | "about";

const props = defineProps<{ section: SettingKey }>();

const settings = [
  { key: "device" as const, icon: "◈", title: "设备配置", description: "设备身份、注册周期与出厂信息" },
  { key: "media" as const, icon: "◉", title: "音视频配置", description: "画质预设、视频编码与音频参数" },
  { key: "osd" as const, icon: "▤", title: "OSD 水印", description: "时间、通道名与斜铺水印，实时采集时烧进码流" },
  { key: "network" as const, icon: "⌁", title: "网络配置", description: "本机绑定与 SIP 追踪" },
  { key: "about" as const, icon: "ⓘ", title: "关于", description: "版本、协议与开源信息" },
];

const activeKey = computed(() => props.section);
const charsetOptions = computed(() => optionsFor(profileVersion.value));
/** 「实际出站」回显：保存值与版本不符时会点明兜底原因，避免以为保存没生效。 */
const charsetSummary = computed(() => effectiveSummary(profileEncoding.value, profileVersion.value));
const saving = ref(false);
const feedback = ref<{ ok: boolean; text: string } | null>(null);
const { deviceLive, deviceState, effectiveConfig, form } = useDevice();
const {
  config,
  active: activePlatform,
  saveDesktopConfig,
  saveMediaConfig,
  saveOsdConfig,
  resetDesktopConfig,
} = usePlatform();
const locked = computed(() => deviceLive.value
  || saving.value
  || deviceState.value === "Registering"
  || deviceState.value === "Registered"
  || deviceState.value === "InCall");
const editableSection = computed(() => activeKey.value !== "about");

const deviceDraft = ref<DeviceSettings | null>(null);
const mediaDraft = ref<MediaProfile | null>(null);
const mediaDraftBase = ref<MediaProfile | null>(null);
const mediaDraftRevision = ref(0);
const networkDraft = ref<NetworkSettings | null>(null);
const osdDraft = ref<OsdSettings | null>(null);
const osdDraftBase = ref<OsdSettings | null>(null);
const osdDraftRevision = ref(0);
const osdCapability = ref<OsdCapability | null>(null);
const osdNow = ref(new Date());
let osdClock: ReturnType<typeof setInterval> | null = null;
const profileVersion = ref<GbVersion>("V2022");
// 信令字符集(§6.10)。与协议版本绑定：2016 只有 GB2312，2022 只有 GB18030，UTF-8 两版都能选。
const profileEncoding = ref<SignalingEncoding>(defaultFor("V2022"));
const mediaAdvancedOpen = ref(false);
const localIpOptions = ref<LocalIpOption[]>([]);
const localIpLoading = ref(false);
const localIpError = ref("");

function sameDeviceDraft(left: DeviceSettings, right: DeviceSettings): boolean {
  return left.device_id === right.device_id
    && left.device_name === right.device_name
    && left.manufacturer === right.manufacturer
    && left.model === right.model
    && left.firmware === right.firmware
    && left.hardware_version === right.hardware_version
    && left.channel_name === right.channel_name
    && left.register_expires_secs === right.register_expires_secs
    && left.heartbeat_interval_secs === right.heartbeat_interval_secs
    && left.heartbeat_fail_threshold === right.heartbeat_fail_threshold;
}

function sameNetworkDraft(left: NetworkSettings, right: NetworkSettings): boolean {
  return left.bind_mode === right.bind_mode
    && left.bind_address === right.bind_address
    && left.sip_trace === right.sip_trace;
}

function replaceDrafts(force = false) {
  if (!config.value) return;
  const preserveDevice = !force && deviceDraft.value && !sameDeviceDraft(deviceDraft.value, config.value.device);
  const preserveNetwork = !force && networkDraft.value && !sameNetworkDraft(networkDraft.value, config.value.network);
  const preserveVersion = !force
    && activePlatform.value
    && profileVersion.value !== activePlatform.value.gb_version;
  // 字符集与版本同源：版本被改过（未保存）时不能单独把字符集拉回后端值，
  // 否则用户刚切的版本会配上旧版本的编码。后端值一律先按版本归一再回填。
  const backendVersion = activePlatform.value?.gb_version ?? "V2022";
  const backendEncoding = effective(parseEncoding(activePlatform.value?.signaling_encoding), backendVersion);
  const preserveEncoding = !force && (preserveVersion || profileEncoding.value !== backendEncoding);
  if (!preserveDevice) deviceDraft.value = { ...config.value.device };
  if (force || !mediaDraft.value || !mediaDraftBase.value
    || mediaProfileEqual(mediaDraft.value, mediaDraftBase.value)) {
    mediaDraft.value = { ...config.value.media };
    mediaDraftBase.value = { ...config.value.media };
    mediaDraftRevision.value = config.value.config_revision;
  }
  if (!preserveNetwork) networkDraft.value = { ...config.value.network };
  // OSD 草稿与媒体草稿同一策略：没有未保存修改时才跟随后端刷新。
  if (force || !osdDraft.value || !osdDraftBase.value || osdSettingsEqual(osdDraft.value, osdDraftBase.value)) {
    osdDraft.value = normalizeOsdSettings(config.value.osd);
    osdDraftBase.value = normalizeOsdSettings(config.value.osd);
    osdDraftRevision.value = config.value.config_revision;
  }
  if (!preserveVersion) profileVersion.value = activePlatform.value?.gb_version ?? "V2022";
  if (!preserveEncoding) profileEncoding.value = backendEncoding;
}

/** 切版本时把不适用的字符集纠到新版本默认，避免界面选中项与实际出站不一致。 */
function pickVersion(version: GbVersion) {
  profileVersion.value = version;
  profileEncoding.value = normalizeForVersion(profileEncoding.value, version);
}

watch(config, () => replaceDrafts(), { immediate: true });
watch(activePlatform, () => replaceDrafts());

async function refreshLocalIpOptions() {
  if (localIpLoading.value) return;
  localIpLoading.value = true;
  localIpError.value = "";
  try {
    localIpOptions.value = await listLocalIpAddresses();
    if (networkDraft.value?.bind_mode === "specific"
      && !networkDraft.value.bind_address
      && localIpOptions.value.length > 0) {
      networkDraft.value.bind_address = localIpOptions.value[0].address;
    }
  } catch (cause) {
    localIpError.value = String(cause);
  } finally {
    localIpLoading.value = false;
  }
}

watch(activeKey, (key) => {
  if (key === "network") void refreshLocalIpOptions();
  if (key === "osd") {
    if (!osdCapability.value) {
      getOsdCapability()
        .then((value) => { osdCapability.value = value; })
        .catch(() => { osdCapability.value = { ffmpeg: false, filters: false, font: null }; });
    }
    osdClock ??= setInterval(() => { osdNow.value = new Date(); }, 1000);
  } else if (osdClock) {
    clearInterval(osdClock);
    osdClock = null;
  }
}, { immediate: true });
onUnmounted(() => { if (osdClock) clearInterval(osdClock); });

const osdLayers: { key: keyof OsdSettings; label: string; hint: string }[] = [
  { key: "timestamp", label: "时间", hint: "默认左上角，YYYY-MM-DD HH:MM:SS" },
  { key: "channel_name", label: "通道名", hint: "默认右上角，文字取当前通道名称" },
  { key: "watermark", label: "斜铺水印", hint: "整屏 30° 斜铺，透明度 28%" },
];
const osdDirty = computed(() => !!osdDraft.value && !!osdDraftBase.value
  && !osdSettingsEqual(osdDraft.value, osdDraftBase.value));
const osdBurnable = computed(() => !!osdCapability.value?.filters && !!osdCapability.value?.font);
const osdPreviewSize = computed(() => ({
  width: config.value?.media.width ?? 1920,
  height: config.value?.media.height ?? 1080,
}));
const osdChannelName = computed(() => config.value?.device.channel_name || "通道 1");
/** 预览框按 16:9 宽度 100% 显示，字号按「像素 / 画面高」换算成百分比。 */
function osdPreviewFont(item: OsdLayer): string {
  const { height } = osdPreviewSize.value;
  return `${(osdPixelSize(item.size, height) / height) * 100}cqh`;
}
function osdTextStyle(item: OsdLayer) {
  return {
    fontSize: osdPreviewFont(item),
    color: item.fill_color,
    textShadow: `-1px -1px 0 ${item.outline_color}, 1px -1px 0 ${item.outline_color}, -1px 1px 0 ${item.outline_color}, 1px 1px 0 ${item.outline_color}`,
  };
}
const osdWatermarkTiles = computed(() => Array.from({ length: 24 }, (_, index) => index));
function resetOsdDraft() {
  osdDraft.value = defaultOsdSettings();
}

const activeSetting = computed(() => settings.find((item) => item.key === activeKey.value)!);
const platformSummary = computed(() => {
  const profile = effectiveConfig.value?.profile ?? activePlatform.value;
  return profile ? `${profile.name} · ${profile.server_host}:${profile.server_port}` : "配置尚未加载";
});
const runtimeSummary = computed(() => {
  const effective = effectiveConfig.value;
  return effective
    ? `${effective.local_host}:${effective.local_port} · ${effective.network.bind_mode}`
    : "设备未运行";
});
const mediaPreset = computed(() => mediaDraft.value ? matchedPreset(mediaDraft.value) : null);
const mediaPresetLabel = computed(() => MEDIA_PRESETS.find((item) => item.id === mediaPreset.value)?.label ?? "自定义");
const mediaSummary = computed(() => mediaDraft.value ? mediaProfileSummary(mediaDraft.value) : "配置尚未加载");
const mediaDirty = computed(() => Boolean(
  config.value && mediaDraft.value && !mediaProfileEqual(mediaDraft.value, config.value.media),
));

// ── 摄像头能力约束 ───────────────────────────────────────────────
// 音视频设置里的分辨率/帧率必须是这台摄像头真能做到的档位，
// 否则用户只能等启动采集时才拿到一个 DirectShow 报错。
const cameraModes = ref<CameraMode[] | null>(null);
const cameraModesLoading = ref(false);
const cameraModesError = ref("");
/** 已探测过能力的摄像头索引，避免反复拉起 FFmpeg 打开设备。 */
const probedCameraIndex = ref<number | null>(null);

const cameraIndex = computed(() => liveSourceCameraIndex(form.value.video_source));
const resolutionOptions = computed(() => cameraResolutionOptions(cameraModes.value));
const fpsOptions = computed(() => {
  const draft = mediaDraft.value;
  return draft
    ? cameraFpsOptions(cameraModes.value, draft.width, draft.height)
    : [...MEDIA_FPS_OPTIONS];
});
const cameraCapabilityNote = computed(() => {
  if (cameraIndex.value === null) return "当前媒体源不是摄像头，分辨率按通用档位显示";
  if (cameraModesLoading.value) return "正在读取摄像头支持的分辨率…";
  if (cameraModesError.value) {
    return `无法读取摄像头能力（${cameraModesError.value}），暂按通用档位显示`;
  }
  if (!cameraModes.value || cameraModes.value.length === 0) return "";
  if (resolutionOptions.value.length === 0) {
    return "该摄像头没有可用的输出分辨率（平台仅支持 640×480 / 1280×720 / 1920×1080），请更换摄像头或改用其它媒体源";
  }
  const sizes = resolutionOptions.value.map((item) => item.label.replace(/ /g, ""));
  const maxFps = Math.max(...cameraModes.value.map((mode) => mode.max_fps));
  return `摄像头支持：${sizes.join(" / ")}（最高 ${Math.round(maxFps)} FPS），设备做不到的档位已隐藏`;
});

function presetSupported(preset: { width: number; height: number }): boolean {
  return cameraSupportsResolution(cameraModes.value, preset.width, preset.height);
}

/** 把草稿收敛到摄像头能力内；已保存的配置可能是给另一台设备配的。 */
function applyCameraClamp() {
  const draft = mediaDraft.value;
  if (!draft || locked.value) return false;
  const clamped = clampToCamera(draft, cameraModes.value);
  if (!clamped) return false;
  mediaDraft.value = clamped.profile;
  feedback.value = { ok: true, text: `${clamped.notice}，确认后保存` };
  return true;
}

async function refreshCameraModes() {
  const index = cameraIndex.value;
  if (index === null) {
    cameraModes.value = null;
    cameraModesError.value = "";
    probedCameraIndex.value = null;
    return;
  }
  if (cameraModesLoading.value || probedCameraIndex.value === index) return;
  cameraModesLoading.value = true;
  cameraModesError.value = "";
  try {
    cameraModes.value = await listCameraModes(index);
    probedCameraIndex.value = index;
    applyCameraClamp();
  } catch (cause) {
    // 探测失败不阻断设置页：退回通用档位，把原因写在提示里。
    cameraModes.value = null;
    cameraModesError.value = String(cause);
  } finally {
    cameraModesLoading.value = false;
  }
}

watch([activeKey, cameraIndex], () => {
  if (activeKey.value !== "media") return;
  if (cameraIndex.value !== probedCameraIndex.value) {
    // 换了摄像头：上一台设备的能力数据不再适用。
    cameraModes.value = null;
    cameraModesError.value = "";
  }
  void refreshCameraModes();
}, { immediate: true });

// 编码后端只探测一次(后端有缓存),进入音视频页时加载。
const encoderStatus = ref<EncoderStatus[] | null>(null);
const encoderStatusError = ref("");
watch(activeKey, (key) => {
  if (key !== "media" || encoderStatus.value) return;
  getEncoderStatus()
    .then((value) => { encoderStatus.value = value; encoderStatusError.value = ""; })
    .catch((cause) => { encoderStatusError.value = String(cause); });
}, { immediate: true });
const selectedEncoder = computed(() =>
  encoderStatus.value?.find((item) => item.codec === mediaDraft.value?.video_codec) ?? null);
const encoderBadge = computed(() => {
  const encoder = selectedEncoder.value;
  if (!encoder) return encoderStatusError.value ? "编码器未知" : "检测编码器…";
  return encoder.hardware
    ? `GPU 编码（${ENCODER_BACKEND_LABELS[encoder.backend]}）`
    : "CPU 编码";
});
const encoderWarning = computed(() => {
  const encoder = selectedEncoder.value;
  if (!encoder || encoder.hardware) return "";
  return encoder.codec === "h265"
    ? "本机没有可用的 H.265 硬件编码器，CPU 编码 H.265 占用很高，建议改用 H.264 或 720p。"
    : "本机没有可用的硬件编码器，CPU 编码占用较高，1080p 时建议降到 720p。";
});

// 草稿晚于能力数据就绪时（先探测、后载入配置），同样要收敛一次，
// 否则已保存的超能力配置会原样留在表单里。
watch(mediaDraft, (draft) => {
  if (draft && cameraModes.value) applyCameraClamp();
});

function selectMediaPreset(id: MediaQualityPresetId) {
  if (!mediaDraft.value || locked.value) return;
  const preset = MEDIA_PRESETS.find((item) => item.id === id);
  if (preset && !presetSupported(preset)) {
    feedback.value = {
      ok: false,
      text: `摄像头不支持 ${preset.width}×${preset.height}，请选择已列出的分辨率`,
    };
    return;
  }
  mediaDraft.value = applyMediaPreset(mediaDraft.value, id);
}

function setResolution(event: Event) {
  if (!mediaDraft.value) return;
  const width = Number((event.target as HTMLSelectElement).value);
  const resolution = resolutionOptions.value.find((item) => item.width === width);
  if (!resolution) return;
  mediaDraft.value.width = resolution.width;
  mediaDraft.value.height = resolution.height;
  // 换到更低的分辨率后原帧率可能超出上限，一并收敛。
  const maxFps = Math.max(...cameraFpsOptions(cameraModes.value, resolution.width, resolution.height));
  if (mediaDraft.value.video_fps > maxFps) mediaDraft.value.video_fps = maxFps;
}

function toggleAudioCodec(codec: MediaProfile["audio_codec"]) {
  if (!mediaDraft.value || locked.value) return;
  mediaDraft.value.audio_codec = codec;
}

function validateDraft(): string | null {
  const device = deviceDraft.value;
  const network = networkDraft.value;
  if (activeKey.value === "media") {
    if (!mediaDraft.value) return "配置尚未加载";
    const invalid = validateMediaProfile(mediaDraft.value);
    if (invalid) return invalid;
    // 能力未知时不拦截（探测失败不该卡住保存），已知则必须拦下。
    if (!cameraSupportsResolution(cameraModes.value, mediaDraft.value.width, mediaDraft.value.height)) {
      return `摄像头不支持 ${mediaDraft.value.width}×${mediaDraft.value.height}，请重新选择输出分辨率`;
    }
    return null;
  }
  if (!device || !network) return "配置尚未加载";
  if (!/^\d{20}$/.test(device.device_id)) return "设备 ID 必须是 20 位数字";
  if (device.register_expires_secs < 3600) return "注册有效期不得短于 3600 秒";
  if (device.heartbeat_interval_secs < 1) return "心跳间隔必须大于 0 秒";
  if (device.heartbeat_fail_threshold < 1) return "连续心跳失败阈值必须大于 0";
  // 硬件版本会直接拼进 DeviceInfo XML，长度与 XML 元字符都要挡住。
  if (device.hardware_version.trim().length > 64) return "硬件版本不得超过 64 个字符";
  if (/[<>&]/.test(device.hardware_version)) return "硬件版本不能包含 < > & 字符";
  if (network.bind_mode === "specific" && !network.bind_address.trim()) return "指定绑定模式必须填写本机 IP";
  return null;
}

async function saveCurrent() {
  if (locked.value || !config.value || !deviceDraft.value || !networkDraft.value) return;
  const validation = validateDraft();
  if (validation) {
    feedback.value = { ok: false, text: validation };
    return;
  }
  if (activeKey.value === "osd" && osdDraft.value) {
    const osdError = validateOsdSettings(osdDraft.value);
    if (osdError) {
      feedback.value = { ok: false, text: osdError };
      return;
    }
  }
  saving.value = true;
  try {
    if (activeKey.value === "osd" && osdDraft.value) {
      await saveOsdConfig(normalizeOsdSettings(osdDraft.value), osdDraftRevision.value);
      replaceDrafts(true);
      feedback.value = { ok: true, text: "OSD 设置已保存，下次启动设备时生效" };
      return;
    }
    if (activeKey.value === "media" && mediaDraft.value) {
      await saveMediaConfig({ ...mediaDraft.value }, mediaDraftRevision.value);
      replaceDrafts(true);
      feedback.value = { ok: true, text: "音视频设置已保存，下次启动设备时生效" };
      return;
    }
    const activeId = config.value.active_profile_id;
    await saveDesktopConfig({
      ...config.value,
      profiles: config.value.profiles.map((profile) => profile.id === activeId
        ? {
            ...profile,
            gb_version: profileVersion.value,
            // 写回归一后的值：切过版本后配置里不留"与版本不匹配"的无效字符集。
            signaling_encoding: normalizeForVersion(profileEncoding.value, profileVersion.value),
          }
        : profile),
      device: { ...deviceDraft.value },
      network: { ...networkDraft.value },
    });
    replaceDrafts(true);
    feedback.value = { ok: true, text: "有效设置已由 Rust 保存并回读" };
  } catch (cause) {
    feedback.value = { ok: false, text: String(cause) };
    if (activeKey.value === "media" && config.value && /版本冲突/.test(String(cause))) {
      mediaDraftRevision.value = config.value.config_revision;
      feedback.value.text += "；草稿已保留，请核对后再次保存。";
    }
    if (activeKey.value === "osd" && config.value && /版本冲突/.test(String(cause))) {
      osdDraftRevision.value = config.value.config_revision;
      feedback.value.text += "；草稿已保留，请核对后再次保存。";
    }
  } finally {
    saving.value = false;
  }
}

async function resetCurrent() {
  if (locked.value) return;
  saving.value = true;
  try {
    await resetDesktopConfig();
    replaceDrafts(true);
    feedback.value = { ok: true, text: "全部有效设置已恢复为 Rust 默认值" };
  } catch (cause) {
    feedback.value = { ok: false, text: String(cause) };
  } finally {
    saving.value = false;
  }
}

function setBindMode(mode: BindMode) {
  if (!networkDraft.value) return;
  networkDraft.value.bind_mode = mode;
  if (mode === "specific" && !networkDraft.value.bind_address && localIpOptions.value.length > 0) {
    networkDraft.value.bind_address = localIpOptions.value[0].address;
  }
}
</script>

<template>
  <div class="settings-page">
    <div class="settings-header">
      <div>
        <div class="page-title">{{ activeSetting.title }}</div>
        <div class="page-sub">{{ activeSetting.description }}</div>
      </div>
      <div class="settings-header-meta">
        <span class="platform-summary">{{ platformSummary }}</span>
        <span class="lock-badge" :class="{ running: locked }">{{ locked ? "设备运行中 · 配置已锁定" : "设备未运行 · 可编辑" }}</span>
      </div>
    </div>

    <main class="settings-main glass-card">
        <div v-if="editableSection" class="content-heading">
          <div class="content-context"><span class="content-icon">{{ activeSetting.icon }}</span>模拟器有效配置</div>
          <div v-if="activeKey !== 'media'" class="content-actions">
            <button v-if="activeKey === 'osd'" class="btn ghost" type="button" :disabled="locked" @click="resetOsdDraft">恢复 OSD 默认</button>
            <button v-else class="btn ghost" type="button" :disabled="locked" @click="resetCurrent">恢复全部默认</button>
            <button class="btn primary" type="button" :disabled="locked || !config" @click="saveCurrent">保存有效设置</button>
          </div>
        </div>

        <div v-if="feedback" class="feedback" :class="{ error: !feedback.ok }">{{ feedback.text }}</div>

        <section v-if="activeKey === 'device' && deviceDraft" class="settings-section">
          <div class="section-label">协议版本</div>
          <div class="segmented wide">
            <button type="button" :class="{ on: profileVersion === 'V2016' }" :disabled="locked" @click="pickVersion('V2016')">GB/T 28181-2016</button>
            <button type="button" :class="{ on: profileVersion === 'V2022' }" :disabled="locked" @click="pickVersion('V2022')">GB/T 28181-2022</button>
          </div>

          <div class="section-label">信令字符集（§6.10）</div>
          <div class="segmented wide">
            <button
              v-for="item in charsetOptions"
              :key="item.value"
              type="button"
              :class="{ on: profileEncoding === item.value }"
              :title="item.hint"
              :disabled="locked || item.disabled"
              @click="profileEncoding = item.value"
            >{{ item.label }}</button>
          </div>
          <p class="charset-hint">{{ charsetSummary }}</p>

          <div class="section-label">设备与注册</div>
          <div class="field-grid two">
            <label class="field"><span>设备名称</span><input v-model="deviceDraft.device_name" :disabled="locked" /></label>
            <label class="field"><span>设备 ID</span><input v-model="deviceDraft.device_id" maxlength="20" :disabled="locked" /></label>
            <label class="field"><span>注册有效期（秒）</span><input v-model.number="deviceDraft.register_expires_secs" type="number" min="3600" :disabled="locked" /></label>
            <label class="field"><span>心跳间隔（秒）</span><input v-model.number="deviceDraft.heartbeat_interval_secs" type="number" min="1" :disabled="locked" /></label>
            <label class="field"><span>连续心跳失败阈值</span><input v-model.number="deviceDraft.heartbeat_fail_threshold" type="number" min="1" :disabled="locked" /></label>
            <button class="future-field" disabled>目录分页（后续阶段）</button>
          </div>

          <div class="section-label">DeviceInfo 应答字段</div>
          <div class="field-grid two">
            <label class="field"><span>厂商</span><input v-model="deviceDraft.manufacturer" :disabled="locked" /></label>
            <label class="field"><span>型号</span><input v-model="deviceDraft.model" :disabled="locked" /></label>
            <label class="field"><span>固件版本</span><input v-model="deviceDraft.firmware" :disabled="locked" /></label>
            <label class="field">
              <span>硬件版本{{ profileVersion === "V2022" ? "" : "（2016 不输出）" }}</span>
              <input
                v-model="deviceDraft.hardware_version"
                maxlength="64"
                placeholder="留空则不应答该字段"
                :disabled="locked"
              />
            </label>
          </div>
        </section>

        <section v-else-if="activeKey === 'media' && mediaDraft" class="settings-section media-section">
          <div class="section-label">参数摘要</div>
          <div class="media-summary-card">
            <div>
              <span class="media-summary-caption">{{ mediaDirty ? "待保存参数" : "当前有效参数" }}</span>
              <strong>{{ mediaPresetLabel }}</strong>
              <small>{{ mediaSummary }}</small>
            </div>
            <span class="media-dirty" :class="{ saved: !mediaDirty }">{{ mediaDirty ? "有未保存修改" : "已保存" }}</span>
          </div>

          <div class="section-label">画质预设</div>
          <div class="media-presets" role="list" aria-label="画质预设">
            <button
              v-for="preset in MEDIA_PRESETS"
              :key="preset.id"
              type="button"
              class="media-preset"
              :class="{ selected: mediaPreset === preset.id }"
              :disabled="locked || !presetSupported(preset)"
              :aria-pressed="mediaPreset === preset.id"
              :title="presetSupported(preset) ? '' : '摄像头不支持该分辨率'"
              @click="selectMediaPreset(preset.id)"
            >
              <strong>{{ preset.label }}</strong>
              <span>{{ preset.description }}</span>
              <small>{{ preset.bitrate_kbps }} kbps · GOP {{ preset.keyframe_interval_seconds }}s</small>
            </button>
          </div>

          <div class="section-label">编码</div>
          <div class="field-grid media-codec-grid">
            <label class="field">
              <span>视频编码 <em class="encoder-badge" :class="{ hardware: selectedEncoder?.hardware, software: selectedEncoder && !selectedEncoder.hardware }" :title="encoderStatusError || ''">{{ encoderBadge }}</em></span>
              <select v-model="mediaDraft.video_codec" :disabled="locked">
                <option v-for="codec in MEDIA_VIDEO_CODECS" :key="codec.value" :value="codec.value">{{ codec.label }}</option>
              </select>
              <small v-if="encoderWarning" class="encoder-warning" role="note">{{ encoderWarning }}</small>
            </label>
            <label class="field">
              <span>音频编码</span>
              <select v-model="mediaDraft.audio_codec" :disabled="locked" @change="toggleAudioCodec(mediaDraft.audio_codec)">
                <option v-for="codec in MEDIA_AUDIO_CODECS" :key="codec.value" :value="codec.value">{{ codec.label }}</option>
              </select>
            </label>
            <label class="field">
              <span>有效采样率</span>
              <select v-if="mediaDraft.audio_codec === 'aac'" v-model.number="mediaDraft.audio_sample_rate_hz" :disabled="locked">
                <option v-for="rate in MEDIA_SAMPLE_RATES" :key="rate" :value="rate">{{ rate / 1000 }} kHz</option>
              </select>
              <select v-else value="8000" disabled>
                <option value="8000">8 kHz（G.711 固定）</option>
              </select>
            </label>
          </div>

          <button
            type="button"
            class="media-advanced-toggle"
            :aria-expanded="mediaAdvancedOpen"
            @click="mediaAdvancedOpen = !mediaAdvancedOpen"
          >
            <span>自定义参数</span>
            <small>{{ mediaAdvancedOpen ? "收起" : "展开" }} · 分辨率、FPS、码率、关键帧间隔</small>
            <b>{{ mediaAdvancedOpen ? "⌃" : "⌄" }}</b>
          </button>
          <div v-if="mediaAdvancedOpen" class="field-grid two media-advanced-fields">
            <label class="field">
              <span>输出分辨率</span>
              <select :value="mediaDraft.width" :disabled="locked" @change="setResolution">
                <option v-for="resolution in resolutionOptions" :key="resolution.width" :value="resolution.width">{{ resolution.label }}</option>
              </select>
            </label>
            <label class="field">
              <span>视频帧率</span>
              <select v-model.number="mediaDraft.video_fps" :disabled="locked">
                <option v-if="!fpsOptions.includes(mediaDraft.video_fps)" :value="mediaDraft.video_fps">{{ mediaDraft.video_fps }} FPS（当前值，摄像头不支持）</option>
                <option v-for="fps in fpsOptions" :key="fps" :value="fps">{{ fps }} FPS</option>
              </select>
            </label>
            <label class="field">
              <span>视频码率</span>
              <select v-model.number="mediaDraft.bitrate_kbps" :disabled="locked">
                <option v-for="bitrate in MEDIA_BITRATE_OPTIONS" :key="bitrate" :value="bitrate">{{ bitrate }} kbps</option>
              </select>
            </label>
            <label class="field">
              <span>关键帧间隔</span>
              <select v-model.number="mediaDraft.keyframe_interval_seconds" :disabled="locked">
                <option v-for="gop in MEDIA_GOP_OPTIONS" :key="gop" :value="gop">{{ gop }} 秒</option>
              </select>
            </label>
          </div>
          <div v-if="cameraCapabilityNote" class="info-note camera-capability-note">{{ cameraCapabilityNote }}</div>
          <div class="info-note">保存后，下次启动设备时生效；运行中请先注销后修改。</div>
          <div class="media-save-row">
            <button class="btn primary" type="button" :disabled="locked || !config || !mediaDirty" @click="saveCurrent">保存音视频设置</button>
          </div>
        </section>

        <section v-else-if="activeKey === 'osd' && osdDraft" class="settings-section osd-section">
          <div class="section-label">生效范围</div>
          <div class="osd-scope" :class="{ warn: osdCapability && !osdBurnable }">
            <template v-if="!osdCapability">正在检测 FFmpeg 与字体…</template>
            <template v-else-if="osdBurnable">
              Windows 摄像头 / 屏幕实时采集：烧进推流、录像与本地预览画面。文件回放、压测：不重新编码，画面中不带 OSD。
              平台下发 OSDConfig 后，时间与文字的位置、开关以平台为准，样式沿用此处设置；平台 PictureMask 开启时叠加黑色遮挡。
            </template>
            <template v-else-if="!osdCapability.ffmpeg">未找到 FFmpeg，OSD 无法烧进码流。</template>
            <template v-else-if="!osdCapability.filters">当前 FFmpeg 缺少 drawtext/drawbox 滤镜(需 libfreetype 编译)，OSD 不会烧进码流。</template>
            <template v-else>未找到中文字体，OSD 不会烧进码流；可设置环境变量 UVP_OSD_FONT 指定字体文件。</template>
          </div>

          <div class="section-label">预览（{{ osdPreviewSize.width }}×{{ osdPreviewSize.height }}，本地默认布局）</div>
          <div class="osd-preview" :style="{ aspectRatio: `${osdPreviewSize.width} / ${osdPreviewSize.height}` }">
            <div v-if="osdDraft.watermark.enabled && osdDraft.watermark.text.trim()" class="osd-watermark">
              <span v-for="tile in osdWatermarkTiles" :key="tile" :style="{ ...osdTextStyle(osdDraft.watermark), opacity: 0.28 }">{{ osdDraft.watermark.text }}</span>
            </div>
            <span v-if="osdDraft.timestamp.enabled" class="osd-time" :style="osdTextStyle(osdDraft.timestamp)">{{ formatOsdTime(osdNow) }}</span>
            <span v-if="osdDraft.channel_name.enabled" class="osd-channel" :style="osdTextStyle(osdDraft.channel_name)">{{ osdChannelName }}</span>
          </div>

          <div v-for="layer in osdLayers" :key="layer.key" class="osd-layer">
            <div class="osd-layer-head">
              <label class="osd-switch">
                <input v-model="osdDraft[layer.key].enabled" type="checkbox" :disabled="locked" />
                <strong>{{ layer.label }}</strong>
              </label>
              <small>{{ layer.hint }}</small>
            </div>
            <div class="field-grid osd-fields">
              <label v-if="layer.key === 'watermark'" class="field osd-text">
                <span>水印文字（≤{{ OSD_TEXT_MAX_CHARS }} 字）</span>
                <input v-model="osdDraft.watermark.text" :maxlength="OSD_TEXT_MAX_CHARS" :disabled="locked" placeholder="例如：内部测试" />
              </label>
              <label class="field">
                <span>字号</span>
                <select v-model="osdDraft[layer.key].size" :disabled="locked">
                  <option v-for="option in OSD_SIZE_OPTIONS" :key="option.value" :value="option.value">
                    {{ option.label }}（{{ osdPixelSize(option.value, osdPreviewSize.height) }}px）
                  </option>
                </select>
              </label>
              <label class="field">
                <span>填充色</span>
                <input v-model="osdDraft[layer.key].fill_color" type="color" :disabled="locked" />
              </label>
              <label class="field">
                <span>描边色</span>
                <input v-model="osdDraft[layer.key].outline_color" type="color" :disabled="locked" />
              </label>
            </div>
          </div>
          <div class="osd-footnote">{{ osdDirty ? "有未保存修改" : "已保存" }} · 修改在下次启动设备时生效</div>
        </section>

        <section v-else-if="activeKey === 'network' && networkDraft" class="settings-section">
          <div class="section-label">网络绑定</div>
          <div class="network-options">
            <button class="network-option" :class="{ selected: networkDraft.bind_mode === 'auto' }" :disabled="locked" @click="setBindMode('auto')"><span>⌁</span><b>自动绑定</b></button>
            <button class="network-option" :class="{ selected: networkDraft.bind_mode === 'specific' }" :disabled="locked" @click="setBindMode('specific')"><span>◎</span><b>指定本机 IP</b></button>
          </div>
          <div v-if="networkDraft.bind_mode === 'specific'" class="network-address-row">
            <label class="field network-address">
              <span>本机绑定地址</span>
              <select v-model="networkDraft.bind_address" :disabled="locked || localIpLoading || localIpOptions.length === 0">
                <option v-if="networkDraft.bind_address && !localIpOptions.some((item) => item.address === networkDraft!.bind_address)" :value="networkDraft.bind_address">
                  {{ networkDraft.bind_address }}（已保存，当前未扫描到）
                </option>
                <option v-if="localIpOptions.length === 0" value="">{{ localIpLoading ? "正在扫描本机 IP…" : "未扫描到可绑定 IP" }}</option>
                <option v-for="item in localIpOptions" :key="`${item.interface_name}-${item.address}`" :value="item.address">
                  {{ item.address }} · {{ item.interface_name }} · {{ item.address_family }}{{ item.loopback ? " · 回环" : "" }}
                </option>
              </select>
            </label>
            <button class="btn ghost refresh-ip" type="button" :disabled="locked || localIpLoading" @click="refreshLocalIpOptions">
              {{ localIpLoading ? "扫描中…" : "重新扫描" }}
            </button>
          </div>
          <div v-if="localIpError" class="feedback error">扫描本机 IP 失败：{{ localIpError }}</div>
          <label class="switch-line"><span><b>SIP 信令追踪</b><small>关闭后下一次启动不再安装 tracer</small></span><input v-model="networkDraft.sip_trace" type="checkbox" :disabled="locked" /></label>
          <div class="runtime-card"><span>Rust 运行回读</span><strong>{{ runtimeSummary }}</strong></div>
          <div class="info-note">指定地址不可绑定时启动会明确失败，不会静默回退自动模式。</div>
        </section>

        <section v-else-if="activeKey === 'about'" class="settings-section about-section">
          <div class="about-hero"><div class="about-logo">UVP</div><div><h2>GB28181 Sim</h2><p>国标设备模拟 · 压力测试</p></div></div>
          <div class="about-grid"><div><span>版本</span><strong>v0.1.2</strong></div><div><span>协议</span><strong>GB/T 28181-2016 / 2022</strong></div><div><span>桌面框架</span><strong>Tauri 2 + Vue 3</strong></div><div><span>配置真相源</span><strong>Rust ConfigStore</strong></div></div>
        </section>
        <section v-else class="settings-section loading-section">
          <strong>配置正在加载</strong>
          <span>正在等待桌面端读取 Rust ConfigStore，请稍候。</span>
        </section>
    </main>
  </div>
</template>

<style scoped>
/* 与 theme.css 的 .page-wrap 同口径：父级 .content 是 overflow:hidden，
   页面自身必须建立垂直滚动上下文，否则参数区超出视口会被直接裁掉且没有滚动条。
   底部留白保证滚到底时提示条与「保存音视频设置」按钮不贴边。 */
.settings-page { max-width: 1420px; flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; padding-bottom: 16px; }
.settings-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 20px; margin-bottom: 18px; }
.settings-header-meta { display: flex; align-items: center; gap: 10px; font-size: 12px; }
.platform-summary { color: var(--text-secondary); }
.lock-badge { padding: 5px 10px; border: 1px solid color-mix(in srgb, var(--success) 35%, transparent); border-radius: 999px; background: color-mix(in srgb, var(--success) 10%, transparent); color: var(--success); }
.lock-badge.running { border-color: color-mix(in srgb, var(--warning) 35%, transparent); background: color-mix(in srgb, var(--warning) 10%, transparent); color: var(--warning); }
.settings-main { padding: 16px; }
.section-label { color: var(--text-tertiary); font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
.content-heading { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 2px 2px 17px; border-bottom: 1px solid var(--border-default); }
.content-context { display: flex; align-items: center; gap: 10px; color: var(--text-secondary); font-size: 13px; font-weight: 600; }
.content-icon { display: grid; width: 30px; height: 30px; place-items: center; border-radius: 8px; background: var(--accent-pale); color: var(--accent); font-size: 16px; }
.content-actions { display: flex; flex: 0 0 auto; gap: 8px; }
.btn { padding: 7px 14px; border: 1px solid var(--border-default); border-radius: 8px; cursor: pointer; font-size: 12px; }
.btn.primary { border-color: var(--accent); background: var(--accent); color: #fff; }
.btn.ghost { background: rgba(255,255,255,.45); color: var(--text-secondary); }
.btn:disabled, input:disabled, button:disabled { cursor: not-allowed; opacity: .55; }
.feedback { margin-top: 14px; padding: 9px 12px; border-radius: 8px; background: color-mix(in srgb, var(--success) 12%, transparent); color: var(--success); font-size: 12px; }
.feedback.error { background: color-mix(in srgb, var(--error) 12%, transparent); color: var(--error); }
.settings-section { display: flex; flex-direction: column; gap: 11px; padding-top: 18px; }
.section-label { margin-top: 5px; }
.segmented { display: flex; gap: 4px; padding: 3px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.42); }
.segmented button { flex: 1; padding: 8px 12px; border: 0; border-radius: 7px; background: transparent; color: var(--text-secondary); cursor: pointer; }
.segmented button.on { background: var(--accent); color: #fff; }
/* 字符集按版本禁用不可选项：灰掉但保留可见，让「这版没有这个编码」这层约束看得见。 */
.segmented button:disabled { cursor: not-allowed; opacity: .42; }
.segmented button.on:disabled { opacity: .55; }
.charset-hint { margin: 8px 0 0; padding: 0 2px; color: var(--text-tertiary); font-size: 11px; line-height: 1.5; }
.field-grid { display: grid; gap: 10px; }
.field-grid.two { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.field { display: flex; flex-direction: column; gap: 6px; color: var(--text-secondary); font-size: 12px; }
.field input, .field select { width: 100%; min-height: 34px; padding: 7px 10px; border: 1px solid var(--border-default); border-radius: 8px; outline: none; background: rgba(255,255,255,.58); color: var(--text-primary); }
.field input:focus, .field select:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-dim); }
.future-field { min-height: 57px; border: 1px dashed var(--border-default); border-radius: 8px; background: rgba(255,255,255,.3); color: var(--text-tertiary); }
.info-note, .capability-card { padding: 12px; border-radius: 9px; background: var(--accent-dim); color: var(--text-secondary); font-size: 11px; line-height: 1.6; }
.capability-card { display: flex; flex-direction: column; gap: 14px; }
.capability-card strong { color: var(--text-primary); font-size: 14px; }
.media-section { gap: 10px; }
.media-summary-card { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px 16px; border: 1px solid color-mix(in srgb, var(--accent) 24%, var(--border-default)); border-radius: 11px; background: linear-gradient(135deg, color-mix(in srgb, var(--accent-pale) 72%, #fff), rgba(255,255,255,.62)); }
.media-summary-card > div { display: flex; min-width: 0; flex-direction: column; gap: 4px; }
.media-summary-caption { color: var(--text-tertiary); font-size: 11px; }
.media-summary-card strong { color: var(--text-primary); font-size: 17px; }
.media-summary-card small { overflow: hidden; color: var(--text-secondary); font-family: "SF Mono", Menlo, monospace; font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.media-dirty { flex: 0 0 auto; padding: 4px 9px; border-radius: 999px; background: color-mix(in srgb, var(--warning) 13%, transparent); color: var(--warning); font-size: 11px; }
.media-dirty.saved { background: color-mix(in srgb, var(--success) 11%, transparent); color: var(--success); }
.encoder-badge { margin-left: 6px; padding: 1px 7px; border-radius: 999px; font-style: normal; font-size: 11px; background: color-mix(in srgb, var(--text-secondary, #888) 12%, transparent); }
.encoder-badge.hardware { background: color-mix(in srgb, var(--success) 13%, transparent); color: var(--success); }
.encoder-badge.software { background: color-mix(in srgb, var(--warning) 13%, transparent); color: var(--warning); }
.encoder-warning { display: block; margin-top: 4px; color: var(--warning); font-size: 11px; line-height: 1.4; }
.media-presets { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; }
.media-preset { display: flex; min-width: 0; min-height: 82px; flex-direction: column; align-items: flex-start; gap: 5px; padding: 12px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.44); color: var(--text-secondary); cursor: pointer; text-align: left; }
.media-preset:hover:not(:disabled) { border-color: color-mix(in srgb, var(--accent) 60%, var(--border-default)); background: var(--accent-pale); }
.media-preset.selected { border-color: var(--accent); background: var(--accent-dim); box-shadow: 0 0 0 2px color-mix(in srgb, var(--accent) 13%, transparent); }
.media-preset:disabled { cursor: not-allowed; opacity: .58; }
.media-preset strong { color: var(--text-primary); font-size: 13px; }
.media-preset span, .media-preset small { overflow: hidden; max-width: 100%; text-overflow: ellipsis; white-space: nowrap; }
.media-preset span { color: var(--text-secondary); font-size: 11px; }
.media-preset small { color: var(--text-tertiary); font-size: 10px; }
.media-codec-grid { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.media-advanced-toggle { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; align-items: center; gap: 10px; padding: 11px 12px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.42); color: var(--text-primary); cursor: pointer; text-align: left; }
.media-advanced-toggle:hover { border-color: color-mix(in srgb, var(--accent) 50%, var(--border-default)); }
.media-advanced-toggle span { font-size: 13px; font-weight: 650; }
.media-advanced-toggle small { overflow: hidden; color: var(--text-tertiary); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.media-advanced-toggle b { color: var(--accent); font-size: 16px; }
.media-advanced-fields { padding: 12px; border: 1px solid var(--border-subtle); border-radius: 9px; background: rgba(255,255,255,.28); }
.media-save-row { display: flex; justify-content: flex-end; padding-top: 2px; }
.network-options { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 10px; }
.network-option { display: flex; align-items: center; gap: 10px; padding: 14px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.4); color: var(--text-primary); cursor: pointer; }
.network-option.selected { border-color: var(--accent); background: var(--accent-dim); }
.network-address { max-width: 420px; }
.network-address-row { display: flex; align-items: flex-end; gap: 8px; }
.network-address-row .network-address { flex: 1; }
.refresh-ip { min-height: 34px; white-space: nowrap; }
.switch-line { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 13px; border: 1px solid var(--border-default); border-radius: 9px; }
.switch-line span { display: flex; flex-direction: column; gap: 4px; }
.switch-line small { color: var(--text-tertiary); }
.runtime-card { display: flex; flex-direction: column; gap: 7px; padding: 14px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.35); }
.runtime-card span { color: var(--text-tertiary); font-size: 11px; }
.about-hero { display: flex; align-items: center; gap: 14px; padding: 20px; border-radius: 12px; background: linear-gradient(135deg, var(--accent-pale), rgba(255,255,255,.45)); }
.about-logo { display: grid; width: 54px; height: 54px; place-items: center; border-radius: 14px; background: linear-gradient(135deg, var(--accent), var(--accent-deep)); color: #fff; font-weight: 750; }
.about-hero h2 { margin: 0; }
.about-hero p { margin: 5px 0 0; color: var(--text-secondary); }
.about-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; }
.about-grid div { display: flex; flex-direction: column; gap: 6px; padding: 13px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.35); }
.about-grid span { color: var(--text-tertiary); font-size: 11px; }
.loading-section { min-height: 180px; align-items: center; justify-content: center; color: var(--text-tertiary); }
.loading-section strong { color: var(--text-primary); font-size: 15px; }
@media (max-width: 980px) { .settings-header { align-items: flex-start; flex-direction: column; } }
@media (max-width: 900px) { .media-presets { grid-template-columns: repeat(2, minmax(0, 1fr)); } .media-codec-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
@media (max-width: 680px) { .field-grid.two, .network-options, .about-grid, .media-codec-grid { grid-template-columns: 1fr; } .media-presets { grid-template-columns: 1fr 1fr; } .content-heading { align-items: flex-start; flex-direction: column; } .media-summary-card { align-items: flex-start; flex-direction: column; gap: 9px; } }
.osd-scope { padding: 10px 12px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.55); color: var(--text-secondary); font-size: 12px; line-height: 1.6; }
.osd-scope.warn { border-color: #e6a23c; background: #fdf6ec; color: #a8661c; }
.osd-preview { position: relative; width: 100%; max-width: 640px; overflow: hidden; container-type: size; border-radius: 10px; background: linear-gradient(135deg, #3a4a5c, #1e2833); font-family: "Microsoft YaHei", "PingFang SC", sans-serif; }
.osd-time { position: absolute; top: 0.6cqh; left: 0.4cqw; white-space: nowrap; }
.osd-channel { position: absolute; top: 0.6cqh; right: 0.4cqw; white-space: nowrap; }
.osd-watermark { position: absolute; inset: -40%; display: flex; flex-wrap: wrap; align-content: space-around; justify-content: space-around; gap: 8cqh 6cqw; transform: rotate(-30deg); pointer-events: none; }
.osd-watermark span { white-space: nowrap; }
.osd-layer { display: flex; flex-direction: column; gap: 8px; padding: 12px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.45); }
.osd-layer-head { display: flex; align-items: center; gap: 12px; }
.osd-layer-head small { color: var(--text-tertiary); font-size: 11px; }
.osd-switch { display: flex; align-items: center; gap: 6px; color: var(--text-primary); font-size: 13px; }
.osd-fields { grid-template-columns: repeat(3, minmax(0, 1fr)); }
.osd-fields .osd-text { grid-column: 1 / -1; }
.field input[type="color"] { padding: 2px 4px; cursor: pointer; }
.osd-footnote { color: var(--text-tertiary); font-size: 11px; }
</style>
