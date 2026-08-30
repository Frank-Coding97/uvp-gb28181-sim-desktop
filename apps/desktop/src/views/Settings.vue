<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { useDevice } from "../device";
import {
  usePlatform,
  type BindMode,
  type DeviceSettings,
  type GbVersion,
  type NetworkSettings,
} from "../platform";

type SettingKey = "device" | "channels" | "media" | "osd" | "network" | "about";

const settings = [
  { key: "device" as const, icon: "◈", title: "设备", description: "设备身份、注册周期与出厂信息" },
  { key: "channels" as const, icon: "▦", title: "设备通道", description: "当前通道与后续目录能力" },
  { key: "media" as const, icon: "◉", title: "音视频", description: "有效帧率与后续编码能力" },
  { key: "osd" as const, icon: "◇", title: "OSD 水印", description: "后续阶段能力预告" },
  { key: "network" as const, icon: "⌁", title: "网络", description: "本机绑定与 SIP 追踪" },
  { key: "about" as const, icon: "ⓘ", title: "关于", description: "版本、协议与开源信息" },
];

const activeKey = ref<SettingKey>("device");
const saving = ref(false);
const feedback = ref<{ ok: boolean; text: string } | null>(null);
const { deviceLive, effectiveConfig } = useDevice();
const {
  config,
  active: activePlatform,
  saveDesktopConfig,
  resetDesktopConfig,
} = usePlatform();
const locked = computed(() => deviceLive.value || saving.value);
const editableSection = computed(() => ["device", "channels", "media", "network"].includes(activeKey.value));

const deviceDraft = ref<DeviceSettings | null>(null);
const networkDraft = ref<NetworkSettings | null>(null);
const profileVersion = ref<GbVersion>("V2022");

function replaceDrafts() {
  if (!config.value) return;
  deviceDraft.value = { ...config.value.device };
  networkDraft.value = { ...config.value.network };
  profileVersion.value = activePlatform.value?.gb_version ?? "V2022";
}

watch(config, replaceDrafts, { immediate: true });
watch(activePlatform, replaceDrafts);

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
const deviceIdPrefix = computed(() => {
  const id = (deviceDraft.value?.device_id ?? "").replace(/\D/g, "");
  return id.length >= 17 ? id.slice(0, 17) : id.padEnd(17, "0");
});

function setActive(key: SettingKey) {
  activeKey.value = key;
  feedback.value = null;
}

function validateDraft(): string | null {
  const device = deviceDraft.value;
  const network = networkDraft.value;
  if (!device || !network) return "配置尚未加载";
  if (!/^\d{20}$/.test(device.device_id)) return "设备 ID 必须是 20 位数字";
  if (device.register_expires_secs < 3600) return "注册有效期不得短于 3600 秒";
  if (device.heartbeat_interval_secs < 1) return "心跳间隔必须大于 0 秒";
  if (device.heartbeat_fail_threshold < 1) return "连续心跳失败阈值必须大于 0";
  if (device.video_fps < 1 || device.video_fps > 120) return "视频帧率必须在 1 到 120 之间";
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
    const activeId = config.value.active_profile_id;
    await saveDesktopConfig({
      ...config.value,
      profiles: config.value.profiles.map((profile) => profile.id === activeId
        ? { ...profile, gb_version: profileVersion.value }
        : profile),
      device: { ...deviceDraft.value },
      network: { ...networkDraft.value },
    });
    replaceDrafts();
    feedback.value = { ok: true, text: "有效设置已由 Rust 保存并回读" };
  } catch (cause) {
    feedback.value = { ok: false, text: String(cause) };
  } finally {
    saving.value = false;
  }
}

async function resetCurrent() {
  if (locked.value) return;
  saving.value = true;
  try {
    await resetDesktopConfig();
    replaceDrafts();
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
        <div class="page-title">设置</div>
        <div class="page-sub">只保存模拟器当前真正支持并能从运行态回读的参数</div>
      </div>
      <div class="settings-header-meta">
        <span class="platform-summary">{{ platformSummary }}</span>
        <span class="lock-badge" :class="{ running: locked }">{{ locked ? "设备运行中 · 配置已锁定" : "设备未运行 · 可编辑" }}</span>
      </div>
    </div>

    <div class="settings-layout">
      <aside class="settings-nav glass-card">
        <div class="nav-caption">设置项</div>
        <button v-for="item in settings" :key="item.key" class="settings-nav-item" :class="{ active: activeKey === item.key }" type="button" @click="setActive(item.key)">
          <span class="nav-icon">{{ item.icon }}</span>
          <span class="nav-copy"><b>{{ item.title }}</b><small>{{ item.description }}</small></span>
          <span class="nav-chevron">›</span>
        </button>
      </aside>

      <main class="settings-main glass-card">
        <div class="content-heading">
          <div>
            <div class="content-title"><span class="content-icon">{{ activeSetting.icon }}</span>{{ activeSetting.title }}</div>
            <div class="content-sub">{{ activeSetting.description }}</div>
          </div>
          <div v-if="editableSection" class="content-actions">
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

        <section v-else-if="activeKey === 'channels' && deviceDraft" class="settings-section">
          <div class="section-label">当前有效通道</div>
          <div class="field-grid two">
            <label class="field"><span>当前通道名称</span><input v-model="deviceDraft.channel_name" :disabled="locked" /></label>
            <label class="field"><span>通道 ID（运行时生成）</span><input :value="`${deviceIdPrefix}13200000001`" disabled /></label>
            <label class="field"><span>额外前置通道</span><input value="后续阶段" disabled /></label>
            <label class="field"><span>报警通道</span><input value="后续阶段" disabled /></label>
          </div>
          <div class="info-note">本阶段仅当前通道名称真实生效；多通道目录与报警通道将在后续阶段接入。</div>
        </section>

        <section v-else-if="activeKey === 'media' && deviceDraft" class="settings-section">
          <div class="section-label">当前有效媒体参数</div>
          <div class="field-grid two">
            <label class="field"><span>视频 FPS</span><input v-model.number="deviceDraft.video_fps" type="number" min="1" max="120" :disabled="locked" /></label>
            <label class="field"><span>分辨率</span><input value="后续阶段" disabled /></label>
            <label class="field"><span>码率 / GOP</span><input value="后续阶段" disabled /></label>
            <label class="field"><span>视频 / 音频编解码器</span><input value="后续阶段" disabled /></label>
            <label class="field"><span>音频采样率</span><input value="后续阶段" disabled /></label>
          </div>
          <div class="info-note">只有 FPS 会保存并影响下一次启动；其余控件显示但禁用，避免制造“保存成功”的假象。</div>
        </section>

        <section v-else-if="activeKey === 'osd'" class="settings-section">
          <div class="capability-card">
            <strong>OSD 尚未接入媒体引擎</strong>
            <span>时间戳、通道名、自定义水印及字号均属于后续阶段，本页不保存任何占位值。</span>
            <div class="field-grid two">
              <label class="field"><span>时间戳</span><input value="后续阶段" disabled /></label>
              <label class="field"><span>通道名</span><input value="后续阶段" disabled /></label>
              <label class="field"><span>自定义水印</span><input value="后续阶段" disabled /></label>
              <label class="field"><span>字号</span><input value="后续阶段" disabled /></label>
            </div>
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

        <section v-else class="settings-section about-section">
          <div class="about-hero"><div class="about-logo">UVP</div><div><h2>GB28181 Sim</h2><p>国标设备模拟 · 压力测试</p></div></div>
          <div class="about-grid"><div><span>版本</span><strong>v0.1.2</strong></div><div><span>协议</span><strong>GB/T 28181-2016 / 2022</strong></div><div><span>桌面框架</span><strong>Tauri 2 + Vue 3</strong></div><div><span>配置真相源</span><strong>Rust ConfigStore</strong></div></div>
        </section>
      </main>
    </div>
  </div>
</template>

<style scoped>
.settings-page { max-width: 1420px; min-height: 100%; }
.settings-header { display: flex; align-items: flex-end; justify-content: space-between; gap: 20px; margin-bottom: 18px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub, .content-sub { margin-top: 5px; color: var(--text-tertiary); font-size: 13px; }
.settings-header-meta { display: flex; align-items: center; gap: 10px; font-size: 12px; }
.platform-summary { color: var(--text-secondary); }
.lock-badge { padding: 5px 10px; border: 1px solid color-mix(in srgb, var(--success) 35%, transparent); border-radius: 999px; background: color-mix(in srgb, var(--success) 10%, transparent); color: var(--success); }
.lock-badge.running { border-color: color-mix(in srgb, var(--warning) 35%, transparent); background: color-mix(in srgb, var(--warning) 10%, transparent); color: var(--warning); }
.settings-layout { display: grid; grid-template-columns: 270px minmax(0, 1fr); align-items: start; gap: 16px; }
.settings-nav, .settings-main { padding: 16px; }
.nav-caption, .section-label { color: var(--text-tertiary); font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
.nav-caption { padding: 3px 10px 10px; }
.settings-nav-item { display: flex; width: 100%; align-items: center; gap: 10px; padding: 12px 10px; border: 1px solid transparent; border-radius: 10px; background: transparent; color: var(--text-primary); text-align: left; cursor: pointer; }
.settings-nav-item:hover { background: var(--bg-hover); }
.settings-nav-item.active { border-color: var(--border-accent); background: var(--accent-dim); }
.nav-icon, .content-icon { display: grid; width: 28px; height: 28px; flex: 0 0 auto; place-items: center; border-radius: 8px; background: var(--accent-pale); color: var(--accent); font-size: 20px; }
.nav-copy { display: flex; min-width: 0; flex: 1; flex-direction: column; gap: 3px; }
.nav-copy b { font-size: 13px; }
.nav-copy small { overflow: hidden; color: var(--text-tertiary); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }
.nav-chevron { color: var(--text-tertiary); font-size: 22px; }
.content-heading { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 2px 2px 17px; border-bottom: 1px solid var(--border-default); }
.content-title { display: flex; align-items: center; gap: 10px; font-size: 18px; font-weight: 700; }
.content-icon { width: 30px; height: 30px; font-size: 16px; }
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
@media (max-width: 980px) { .settings-layout { grid-template-columns: 1fr; } .settings-nav { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; } .nav-caption { grid-column: 1 / -1; } .settings-header { align-items: flex-start; flex-direction: column; } }
@media (max-width: 680px) { .field-grid.two, .network-options, .about-grid { grid-template-columns: 1fr; } .settings-nav { grid-template-columns: 1fr; } .content-heading { align-items: flex-start; flex-direction: column; } }
</style>
