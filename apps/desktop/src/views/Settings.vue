<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useDevice } from "../device";
import {
  MEDIA_AUDIO_CODECS,
  MEDIA_BITRATE_OPTIONS,
  MEDIA_FPS_OPTIONS,
  MEDIA_GOP_OPTIONS,
  MEDIA_PRESETS,
  MEDIA_RESOLUTIONS,
  MEDIA_SAMPLE_RATES,
  MEDIA_VIDEO_CODECS,
  applyMediaPreset,
  matchedPreset,
  mediaProfileEqual,
  mediaProfileSummary,
  validateMediaProfile,
  type MediaProfile,
  type MediaQualityPresetId,
} from "../media-profile";
import {
  usePlatform,
  type BindMode,
  type DeviceSettings,
  type GbVersion,
  type NetworkSettings,
} from "../platform";

type SettingKey = "device" | "media" | "network" | "about";

const props = defineProps<{ section: SettingKey }>();

const settings = [
  { key: "device" as const, icon: "◈", title: "设备配置", description: "设备身份、注册周期与出厂信息" },
  { key: "media" as const, icon: "◉", title: "音视频配置", description: "画质预设、视频编码与音频参数" },
  { key: "network" as const, icon: "⌁", title: "网络配置", description: "本机绑定与 SIP 追踪" },
  { key: "about" as const, icon: "ⓘ", title: "关于", description: "版本、协议与开源信息" },
];

const activeKey = computed(() => props.section);
const saving = ref(false);
const feedback = ref<{ ok: boolean; text: string } | null>(null);
const { deviceLive, deviceState, effectiveConfig } = useDevice();
const {
  config,
  active: activePlatform,
  saveDesktopConfig,
  saveMediaConfig,
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
const profileVersion = ref<GbVersion>("V2022");
const mediaAdvancedOpen = ref(false);

function sameDeviceDraft(left: DeviceSettings, right: DeviceSettings): boolean {
  return left.device_id === right.device_id
    && left.device_name === right.device_name
    && left.manufacturer === right.manufacturer
    && left.model === right.model
    && left.firmware === right.firmware
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
  if (!preserveDevice) deviceDraft.value = { ...config.value.device };
  if (force || !mediaDraft.value || !mediaDraftBase.value
    || mediaProfileEqual(mediaDraft.value, mediaDraftBase.value)) {
    mediaDraft.value = { ...config.value.media };
    mediaDraftBase.value = { ...config.value.media };
    mediaDraftRevision.value = config.value.config_revision;
  }
  if (!preserveNetwork) networkDraft.value = { ...config.value.network };
  if (!preserveVersion) profileVersion.value = activePlatform.value?.gb_version ?? "V2022";
}

watch(config, () => replaceDrafts(), { immediate: true });
watch(activePlatform, () => replaceDrafts());

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

function selectMediaPreset(id: MediaQualityPresetId) {
  if (!mediaDraft.value || locked.value) return;
  mediaDraft.value = applyMediaPreset(mediaDraft.value, id);
}

function setResolution(event: Event) {
  if (!mediaDraft.value) return;
  const width = Number((event.target as HTMLSelectElement).value);
  const resolution = MEDIA_RESOLUTIONS.find((item) => item.width === width);
  if (!resolution) return;
  mediaDraft.value.width = resolution.width;
  mediaDraft.value.height = resolution.height;
}

function toggleAudioCodec(codec: MediaProfile["audio_codec"]) {
  if (!mediaDraft.value || locked.value) return;
  mediaDraft.value.audio_codec = codec;
}

function isCommonFps(value: number): boolean {
  return (MEDIA_FPS_OPTIONS as readonly number[]).includes(value);
}

function validateDraft(): string | null {
  const device = deviceDraft.value;
  const network = networkDraft.value;
  if (activeKey.value === "media") {
    if (!mediaDraft.value) return "配置尚未加载";
    return validateMediaProfile(mediaDraft.value);
  }
  if (!device || !network) return "配置尚未加载";
  if (!/^\d{20}$/.test(device.device_id)) return "设备 ID 必须是 20 位数字";
  if (device.register_expires_secs < 3600) return "注册有效期不得短于 3600 秒";
  if (device.heartbeat_interval_secs < 1) return "心跳间隔必须大于 0 秒";
  if (device.heartbeat_fail_threshold < 1) return "连续心跳失败阈值必须大于 0";
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
  saving.value = true;
  try {
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
        ? { ...profile, gb_version: profileVersion.value }
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
  if (networkDraft.value) networkDraft.value.bind_mode = mode;
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
            <button class="btn ghost" type="button" :disabled="locked" @click="resetCurrent">恢复全部默认</button>
            <button class="btn primary" type="button" :disabled="locked || !config" @click="saveCurrent">保存有效设置</button>
          </div>
        </div>

        <div v-if="feedback" class="feedback" :class="{ error: !feedback.ok }">{{ feedback.text }}</div>

        <section v-if="activeKey === 'device' && deviceDraft" class="settings-section">
          <div class="section-label">协议版本</div>
          <div class="segmented wide">
            <button type="button" :class="{ on: profileVersion === 'V2016' }" :disabled="locked" @click="profileVersion = 'V2016'">GB/T 28181-2016</button>
            <button type="button" :class="{ on: profileVersion === 'V2022' }" :disabled="locked" @click="profileVersion = 'V2022'">GB/T 28181-2022</button>
          </div>

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
            <button class="future-field" disabled>硬件版本（后续阶段）</button>
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
              :disabled="locked"
              :aria-pressed="mediaPreset === preset.id"
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
              <span>视频编码</span>
              <select v-model="mediaDraft.video_codec" :disabled="locked">
                <option v-for="codec in MEDIA_VIDEO_CODECS" :key="codec.value" :value="codec.value">{{ codec.label }}</option>
              </select>
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
                <option v-for="resolution in MEDIA_RESOLUTIONS" :key="resolution.width" :value="resolution.width">{{ resolution.label }}</option>
              </select>
            </label>
            <label class="field">
              <span>视频帧率</span>
              <select v-model.number="mediaDraft.video_fps" :disabled="locked">
                <option v-if="!isCommonFps(mediaDraft.video_fps)" :value="mediaDraft.video_fps">{{ mediaDraft.video_fps }} FPS（自定义）</option>
                <option v-for="fps in MEDIA_FPS_OPTIONS" :key="fps" :value="fps">{{ fps }} FPS</option>
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
          <div class="info-note">保存后，下次启动设备时生效；运行中请先注销后修改。</div>
          <div class="media-save-row">
            <button class="btn primary" type="button" :disabled="locked || !config || !mediaDirty" @click="saveCurrent">保存音视频设置</button>
          </div>
        </section>

        <section v-else-if="activeKey === 'network' && networkDraft" class="settings-section">
          <div class="section-label">网络绑定</div>
          <div class="network-options">
            <button class="network-option" :class="{ selected: networkDraft.bind_mode === 'auto' }" :disabled="locked" @click="setBindMode('auto')"><span>⌁</span><b>自动绑定</b></button>
            <button class="network-option" :class="{ selected: networkDraft.bind_mode === 'specific' }" :disabled="locked" @click="setBindMode('specific')"><span>◎</span><b>指定本机 IP</b></button>
          </div>
          <label v-if="networkDraft.bind_mode === 'specific'" class="field network-address"><span>本机绑定地址</span><input v-model="networkDraft.bind_address" placeholder="例如 192.168.1.20" :disabled="locked" /></label>
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
.settings-page { max-width: 1420px; min-height: 100%; }
.settings-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 20px; margin-bottom: 18px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 5px; color: var(--text-tertiary); font-size: 13px; }
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
.field-grid { display: grid; gap: 10px; }
.field-grid.two { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.field { display: flex; flex-direction: column; gap: 6px; color: var(--text-secondary); font-size: 12px; }
.field input { width: 100%; min-height: 34px; padding: 7px 10px; border: 1px solid var(--border-default); border-radius: 8px; outline: none; background: rgba(255,255,255,.58); color: var(--text-primary); }
.field input:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-dim); }
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
</style>
