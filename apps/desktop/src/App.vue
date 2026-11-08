<script setup lang="ts">
// 主应用壳(frost-blue 磨砂风,对齐参考原型):
// 顶栏右侧状态胶囊 + 磨砂侧边栏 + 内容区路由视图。
// 必须保留 message/dialog/notification provider,否则子页 useMessage() 抛错致空白。
import { computed, h, onMounted, onUnmounted, provide, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NMenu, NIcon, NPopover, NInput, NInputNumber, NButton, NSelect, zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import HomeOutline from "@vicons/ionicons5/es/HomeOutline.js";
import HardwareChipOutline from "@vicons/ionicons5/es/HardwareChipOutline.js";
import ServerOutline from "@vicons/ionicons5/es/ServerOutline.js";
import PulseOutline from "@vicons/ionicons5/es/PulseOutline.js";
import GitNetworkOutline from "@vicons/ionicons5/es/GitNetworkOutline.js";
import TerminalOutline from "@vicons/ionicons5/es/TerminalOutline.js";
import SettingsOutline from "@vicons/ionicons5/es/SettingsOutline.js";
import VideocamOutline from "@vicons/ionicons5/es/VideocamOutline.js";
import TextOutline from "@vicons/ionicons5/es/TextOutline.js";
import FilmOutline from "@vicons/ionicons5/es/FilmOutline.js";
import WifiOutline from "@vicons/ionicons5/es/WifiOutline.js";
import TimeOutline from "@vicons/ionicons5/es/TimeOutline.js";
import CameraOutline from "@vicons/ionicons5/es/CameraOutline.js";
import WarningOutline from "@vicons/ionicons5/es/WarningOutline.js";
import CalendarOutline from "@vicons/ionicons5/es/CalendarOutline.js";
import InformationCircleOutline from "@vicons/ionicons5/es/InformationCircleOutline.js";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { usePlatform } from "./platform";
import type { GbVersion, SignalingEncoding } from "./platform";
// 应用图标与应用包图标同源(app-icon.svg → tauri icon),侧边栏 logo 直接复用矢量图。
import appIconUrl from "./assets/app-icon.svg?url";
import { effective, normalizeForVersion, optionsFor, parseEncoding } from "./signaling-charset.mjs";
import { useDevice } from "./device";
import NotificationBell from "./components/NotificationBell.vue";
import { startNotificationCenter, stopNotificationCenter } from "./notification-center";

const route = useRoute();
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const router = useRouter();

function icon(comp: any) {
  return () => h(NIcon, null, { default: () => h(comp) });
}

const menuOptions: MenuOption[] = [
  { label: "首页",       key: "/dashboard",        icon: icon(HomeOutline) },
  { label: "模拟器中心", key: "/simulator",        icon: icon(HardwareChipOutline) },
  { label: "目录管理",   key: "/channels",         icon: icon(GitNetworkOutline) },
  { label: "录像中心",   key: "/recordings",       icon: icon(FilmOutline) },
  { label: "抓拍图库",   key: "/snapshots",        icon: icon(CameraOutline) },
  { label: "报警管理",   key: "/alarms",           icon: icon(WarningOutline) },
  { label: "录像计划",   key: "/record-plan",      icon: icon(CalendarOutline) },
  { label: "压力测试",   key: "/scenario",         icon: icon(PulseOutline) },
  { label: "设备配置",   key: "/device-settings",  icon: icon(SettingsOutline) },
  { label: "音视频配置", key: "/media-settings",   icon: icon(VideocamOutline) },
  { label: "OSD 水印",   key: "/osd-settings",     icon: icon(TextOutline) },
  { label: "网络配置",   key: "/network-settings", icon: icon(WifiOutline) },
  { label: "设备校时",   key: "/clock",            icon: icon(TimeOutline) },
  { label: "日志",       key: "/logs",             icon: icon(TerminalOutline) },
  { label: "关于",       key: "/about",            icon: icon(InformationCircleOutline) },
];

// 平台档案(全局):顶栏切换,单设备/压测共用同一份平台连接参数。
const {
  profiles, activeId, active, error: configError,
  loadDesktopConfig, resetDesktopConfig,
  setActive, addProfile, updateProfile, removeProfile,
  passwordFor,
} = usePlatform();
provide("platform", { profiles, activeId, active, setActive, addProfile, updateProfile, removeProfile });
const profileOptions = computed(() =>
  profiles.value.map((p) => ({ label: `${p.name}  (${p.server_host}:${p.server_port})`, value: p.id }))
);
// 顶栏编辑弹层用的临时表单。
const editForm = ref({
  name: "",
  server_host: "",
  server_port: 5060,
  server_id: "",
  server_domain: "",
  password: "",
  transport: "UDP" as "UDP" | "TCP",
  gb_version: "V2022" as GbVersion,
  signaling_encoding: "Gb18030" as SignalingEncoding,
});
function openEdit() {
  if (active.value) {
    Object.assign(editForm.value, active.value);
    editForm.value.password = passwordFor(active.value.id);
    // 存量配置可能存着与版本不匹配的编码：编辑框里显示实际会出站的那个值。
    editForm.value.signaling_encoding = effective(
      parseEncoding(active.value.signaling_encoding),
      active.value.gb_version,
    );
  }
}
async function saveEdit() {
  if (!active.value || deviceLive.value) return;
  try {
    await updateProfile(activeId.value, {
      name: editForm.value.name.trim(),
      server_host: editForm.value.server_host.trim(),
      server_port: editForm.value.server_port,
      server_id: editForm.value.server_id.trim(),
      server_domain: editForm.value.server_domain.trim(),
      password: editForm.value.password,
      transport: editForm.value.transport,
      gb_version: editForm.value.gb_version,
      signaling_encoding: editForm.value.signaling_encoding,
    });
    topMessage.value = { text: "平台配置已保存", ok: true };
  } catch (cause) {
    lastError.value = { scope: "config", message: String(cause), ts_ms: Date.now() };
  }
}
async function addNew() {
  if (deviceLive.value) return;
  try {
    await addProfile({ name: "新平台", server_host: "127.0.0.1", server_port: 5060,
    server_id: "34020000002000000001", server_domain: "3402000000",
      password: "",
      transport: "UDP", audio_transport: "TCP_ACTIVE", gb_version: "V2022", signaling_encoding: "Gb18030" });
    openEdit();
  } catch (cause) {
    lastError.value = { scope: "config", message: String(cause), ts_ms: Date.now() };
  }
}
const transportOptions = [
  { label: "UDP（共享 socket）", value: "UDP" },
  { label: "TCP（共享连接池）", value: "TCP" },
];
const versionOptions = [
  { label: "GB/T 28181-2022", value: "V2022" },
  { label: "GB/T 28181-2016", value: "V2016" },
];
const encodingOptions = computed(() => optionsFor(editForm.value.gb_version).map((item) => ({
  label: item.label,
  value: item.value,
  disabled: item.disabled,
})));
/** 切版本时把不适用的编码纠到新版本默认，避免把必定被兜底的值存进档案。 */
function pickEditVersion(version: GbVersion) {
  editForm.value.gb_version = version;
  editForm.value.signaling_encoding = normalizeForVersion(editForm.value.signaling_encoding, version);
}

const activeKey = computed(() => route.path);
const showTopbar = computed(() => route.path !== "/dashboard");
function onMenuSelect(key: string) {
  router.push(key);
}

// 全局设备状态 + 注册/注销:来自 device store(顶栏与单设备页共用同一份)。
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const {
  deviceState, startedAt, statusMeta, deviceLive,
  startDevice, stopDevice, reconcile,
} = useDevice();
const registrationBusy = ref(false);
const registrationActionLabel = computed(() =>
  deviceState.value === "Failed" && deviceLive.value ? "停止重试" : deviceLive.value ? "注销" : "注册",
);

const uptime = ref("--:--:--");
let uptimeTimer: number | undefined;
function tickUptime() {
  if (!startedAt.value) { uptime.value = "--:--:--"; return; }
  const s = Math.floor((Date.now() - startedAt.value) / 1000);
  const h = String(Math.floor(s / 3600)).padStart(2, "0");
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  uptime.value = `${h}:${m}:${ss}`;
}

// 全局顶栏状态芯片:连接状态 + 在线时长 + 传输模式(TCP/UDP)。
const statusChips = computed(() => [
  { key: "state", label: "连接状态", value: statusMeta.value.text, color: statusMeta.value.color, dot: true },
  { key: "uptime", label: "在线时长", value: uptime.value, mono: true },
  { key: "transport", label: "传输模式", value: active.value?.transport.toUpperCase() ?? "—", mono: true },
]);

// 顶栏注册/注销:任意页面可操作同一台设备。成功用 message,失败落全局错误条。
async function toggleRegistration() {
  if (registrationBusy.value) return;
  registrationBusy.value = true;
  try {
    const r = deviceLive.value
      ? await stopDevice()
      : await startDevice(active.value, active.value ? passwordFor(active.value.id) : "");
    if (r.ok) topMessage.value = { text: r.msg, ok: true };
    else lastError.value = { scope: "register", message: r.msg, ts_ms: Date.now() };
  } finally {
    registrationBusy.value = false;
  }
}
// 顶栏轻量提示(2.5s 自动消失),避免依赖 message provider(其在本组件树更外层)。
const topMessage = ref<{ text: string; ok: boolean } | null>(null);
let topMsgTimer: number | undefined;
watch(topMessage, (v) => {
  if (v) {
    if (topMsgTimer) clearTimeout(topMsgTimer);
    topMsgTimer = window.setTimeout(() => { topMessage.value = null; }, 2500);
  }
});

// 全局错误条:订阅后端运行时错误(如点播采集失败),所有页面可见。
interface DeviceError { scope: string; message: string; ts_ms: number; }
const lastError = ref<DeviceError | null>(null);
function dismissError() { lastError.value = null; }
const errorScopeLabel: Record<string, string> = {
  capture: "视频源采集", invite: "点播", stream: "推流", register: "注册", config: "配置",
};

async function resetBrokenConfig() {
  try {
    await resetDesktopConfig();
    lastError.value = null;
    topMessage.value = { text: "配置已恢复默认", ok: true };
  } catch (cause) {
    lastError.value = { scope: "config", message: String(cause), ts_ms: Date.now() };
  }
}

// 媒体运行时自检:ffmpeg 缺失则主干媒体能力不可用;ffprobe 缺失只影响录像音轨校验。
// 两者都在收尾/采集路径深处才报错,首屏先提示能省掉用户一次"点了没反应"的排查。
interface MediaToolStatus { path: string | null; version: string | null; source: string; }
interface MediaRuntimeStatus {
  ffmpeg: MediaToolStatus;
  ffprobe: MediaToolStatus;
  ready: boolean;
  verification_available: boolean;
}
const mediaRuntime = ref<MediaRuntimeStatus | null>(null);
const mediaRuntimeDismissed = ref(false);
const mediaRuntimeNotice = computed(() => {
  const status = mediaRuntime.value;
  if (!status) return null;
  const detail = `ffmpeg: ${status.ffmpeg.path ?? "未找到"}${status.ffmpeg.version ? ` (${status.ffmpeg.version})` : ""}\n` +
    `ffprobe: ${status.ffprobe.path ?? "未找到"}${status.ffprobe.version ? ` (${status.ffprobe.version})` : ""}`;
  if (!status.ready) {
    return {
      level: "error" as const,
      message: "未找到 ffmpeg：文件源转封装、摄像头/屏幕采集、抓拍与录像收尾均不可用。请安装 FFmpeg，或改用内嵌 ffmpeg 的安装包。",
      detail,
    };
  }
  if (!status.verification_available) {
    return {
      level: "warning" as const,
      message: "未找到 ffprobe：录像仍可进行，但音轨编码与采样率不做校验。",
      detail,
    };
  }
  return null;
});

async function checkMediaRuntime() {
  try {
    mediaRuntime.value = await invoke<MediaRuntimeStatus>("media_runtime_status");
  } catch {
    // 自检是增强项:该命令不存在或调用失败时保持静默,不能影响应用启动。
    mediaRuntime.value = null;
  }
}

let unlisten: UnlistenFn | null = null;
let unlistenErr: UnlistenFn | null = null;
onMounted(async () => {
  if (isTauri) {
    void checkMediaRuntime();
    try {
      await loadDesktopConfig();
    } catch {
      lastError.value = { scope: "config", message: configError.value, ts_ms: Date.now() };
    }
  }
  if (!isTauri) {
    uptimeTimer = window.setInterval(tickUptime, 1000);
    return;
  }
  unlisten = await listen<string>("device_state", (e) => {
    const s = e.payload as DState;
    deviceState.value = s;
    if (s === "Registered" && !startedAt.value) startedAt.value = Date.now();
    if (s === "Disconnected" || s === "Failed") startedAt.value = null;
  });
  unlistenErr = await listen<DeviceError>("device_error", (e) => {
    lastError.value = e.payload;
  });
  uptimeTimer = window.setInterval(tickUptime, 1000);
  // 通知中心自带事件订阅：与当前停在哪个页面无关，切页不丢历史。
  await startNotificationCenter();
  // app 启动/刷新后与引擎对账,反映后台是否已有设备在跑。
  reconcile();
});
onUnmounted(() => {
  unlisten?.(); unlistenErr?.();
  if (uptimeTimer) clearInterval(uptimeTimer);
  stopNotificationCenter();
});

const themeOverrides = {
  common: {
    primaryColor: "#3884ff",
    primaryColorHover: "#5a9bff",
    primaryColorPressed: "#1e63dc",
    primaryColorSuppl: "#3884ff",
    borderRadius: "8px",
    borderRadiusSmall: "6px",
    fontSize: "13px",
    fontSizeMedium: "13px",
    fontSizeSmall: "12px",
    heightMedium: "34px",
    heightSmall: "28px",
    lineHeight: "1.5",
  },
  Button: {
    borderRadiusMedium: "8px",
    borderRadiusSmall: "6px",
    fontSizeMedium: "13px",
    fontSizeSmall: "12px",
    heightMedium: "34px",
    heightSmall: "28px",
    paddingMedium: "0 16px",
    paddingSmall: "0 12px",
  },
  Input: {
    borderRadius: "8px",
    heightMedium: "34px",
    heightSmall: "28px",
    fontSizeMedium: "13px",
    fontSizeSmall: "12px",
    color: "rgba(255,255,255,0.65)",
    colorFocus: "rgba(255,255,255,0.85)",
    border: "1px solid rgba(120,160,210,0.3)",
    borderFocus: "1px solid #3884ff",
    borderHover: "1px solid rgba(56,132,255,0.5)",
    boxShadowFocus: "0 0 0 2px rgba(56,132,255,0.15)",
  },
  InputNumber: {
    borderRadius: "8px",
    heightMedium: "34px",
    heightSmall: "28px",
    fontSizeMedium: "13px",
    fontSizeSmall: "12px",
    color: "rgba(255,255,255,0.65)",
    border: "1px solid rgba(120,160,210,0.3)",
  },
  Select: {
    borderRadius: "8px",
    heightMedium: "34px",
    heightSmall: "28px",
    fontSizeMedium: "13px",
    fontSizeSmall: "12px",
    peers: {
      InternalSelectMenu: { borderRadius: "10px" },
    },
  },
  Switch: {
    railColorActive: "#3884ff",
  },
  Tag: {
    borderRadius: "6px",
  },
  Card: { color: "rgba(255,255,255,0.5)", borderRadius: "16px" },
  Form: {
    labelFontSizeTopMedium: "11.5px",
    labelTextColor: "#8fa1bd",
    feedbackFontSizeMedium: "11px",
    blankHeightMedium: "34px",
  },
};
</script>

<template>
  <n-config-provider
    :locale="zhCN"
    :date-locale="dateZhCN"
    :theme-overrides="themeOverrides"
  >
    <n-message-provider>
      <n-dialog-provider>
        <n-notification-provider>
          <div class="shell">
            <!-- 侧边栏 -->
            <aside class="sidebar">
              <div class="brand">
                <div class="brand-logo">
                  <img :src="appIconUrl" alt="UVP" />
                </div>
                <div>
                  <div class="brand-title">GB28181 Sim</div>
                  <div class="brand-sub">国标设备模拟 · 压测</div>
                </div>
              </div>
              <n-menu
                class="sidebar-menu"
                :value="activeKey"
                :options="menuOptions"
                :indent="18"
                @update:value="onMenuSelect"
              />
              <div class="sidebar-footer">v0.1.2 · UVP</div>
            </aside>

            <!-- 主区 -->
            <main class="main">
              <header v-if="showTopbar" class="topbar glass-card">
                <!-- 全局目标平台:单设备/压测共用,下拉切换 + 编辑 -->
                <div class="platform-bar">
                  <n-icon :component="ServerOutline" class="plat-icon" />
                  <span class="plat-label">目标平台</span>
                  <n-select
                    size="small"
                    :value="activeId"
                    :options="profileOptions"
                    class="plat-select"
                    @update:value="setActive"
                  />
                  <n-popover trigger="click" placement="bottom-start" @update:show="(s: boolean) => s && openEdit()">
                    <template #trigger>
                      <n-button size="small" tertiary :disabled="deviceLive">编辑</n-button>
                    </template>
                    <div class="plat-edit">
                      <div class="pe-title">平台档案</div>
                      <n-input v-model:value="editForm.name" size="small" placeholder="档案名称" />
                      <n-input v-model:value="editForm.server_host" size="small" placeholder="平台 IP" />
                      <n-input-number v-model:value="editForm.server_port" size="small" :min="1" :max="65535" style="width:100%" placeholder="SIP 端口" />
                      <n-input v-model:value="editForm.server_id" size="small" maxlength="20" placeholder="服务器 ID（20 位）" />
                      <n-input v-model:value="editForm.server_domain" size="small" maxlength="10" placeholder="服务器域（10 位）" />
                      <n-input v-model:value="editForm.password" size="small" type="password" show-password-on="click" placeholder="SIP 密码" />
                      <n-select v-model:value="editForm.transport" size="small" :options="transportOptions" />
                      <n-select :value="editForm.gb_version" size="small" :options="versionOptions" @update:value="pickEditVersion" />
                      <n-select v-model:value="editForm.signaling_encoding" size="small" :options="encodingOptions" />
                      <div class="pe-actions">
                        <n-button size="small" type="primary" :disabled="deviceLive" @click="saveEdit">保存</n-button>
                        <n-button size="small" :disabled="deviceLive" @click="addNew">+ 新增</n-button>
                        <n-button size="small" type="error" :disabled="deviceLive || profiles.length <= 1" @click="removeProfile(activeId)">删除</n-button>
                      </div>
                    </div>
                  </n-popover>
                </div>
                <!-- 全局状态芯片 + 注册/注销:所有菜单页可见,任意页面均可注册/注销设备。 -->
                <div class="status-area">
                  <div class="status-chips">
                    <div
                      v-for="c in statusChips"
                      :key="c.key"
                      class="chip"
                      :title="c.label + '：' + c.value"
                    >
                      <span v-if="c.dot" class="chip-dot" :style="{ background: c.color }" />
                      <span class="chip-val" :class="{ mono: c.mono }" :style="{ color: c.color }">{{ c.value }}</span>
                    </div>
                  </div>
                  <div class="reg-btns">
                    <NotificationBell />
                    <n-button size="small" :type="deviceLive ? 'error' : 'primary'" :loading="registrationBusy" :disabled="registrationBusy" @click="toggleRegistration">
                      {{ registrationActionLabel }}
                    </n-button>
                  </div>
                </div>
              </header>
              <!-- 顶栏轻量提示(注册/注销结果),2.5s 自动消失。 -->
              <transition name="fade">
                <div v-if="topMessage" class="top-toast" :class="{ ok: topMessage.ok, err: !topMessage.ok }">
                  {{ topMessage.text }}
                </div>
              </transition>
              <!-- 媒体运行时自检:首屏就告诉用户 ffmpeg/ffprobe 是否可用,
                   而不是等点录像时才在收尾阶段报"未找到 ffprobe"。 -->
              <div
                v-if="mediaRuntimeNotice && !mediaRuntimeDismissed"
                class="runtime-bar"
                :class="mediaRuntimeNotice.level"
              >
                <span class="eb-icon">⚠</span>
                <span class="eb-scope">媒体运行时</span>
                <span class="eb-msg" :title="mediaRuntimeNotice.detail">{{ mediaRuntimeNotice.message }}</span>
                <button class="eb-close" :class="mediaRuntimeNotice.level" @click="mediaRuntimeDismissed = true" title="关闭">✕</button>
              </div>
              <!-- 全局错误条:后端运行时错误(如点播采集失败),所有页面可见,可关闭。 -->
              <div v-if="lastError" class="error-bar">
                <span class="eb-icon">⚠</span>
                <span class="eb-scope">{{ errorScopeLabel[lastError.scope] ?? lastError.scope }}失败</span>
                <span class="eb-msg" :title="lastError.message">{{ lastError.message }}</span>
                <n-button v-if="lastError.scope === 'config'" size="tiny" type="warning" @click="resetBrokenConfig">显式恢复默认</n-button>
                <button class="eb-close" @click="dismissError" title="关闭">✕</button>
              </div>
              <section class="content">
                <!-- keep-alive:切换标签页不销毁组件,保留各页表单/运行/曲线状态。 -->
                <router-view v-slot="{ Component }">
                  <keep-alive>
                    <component :is="Component" />
                  </keep-alive>
                </router-view>
              </section>
            </main>
          </div>
        </n-notification-provider>
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>

<style scoped>
.shell { display: flex; height: 100vh; }
.sidebar {
  width: 216px; flex-shrink: 0;
  background: rgba(255, 255, 255, 0.45);
  border-right: 1px solid var(--border-default);
  backdrop-filter: var(--blur-light);
  display: flex; flex-direction: column;
}
.brand { display: flex; align-items: center; gap: 12px; padding: 20px 20px 18px; }
.brand-logo {
  width: 38px; height: 38px; border-radius: 10px; flex: 0 0 auto;
  overflow: hidden;
  /* 渐变底色作 img 加载前的兜底，避免闪白 */
  background: linear-gradient(135deg, #3884ff, #1e63dc);
  display: flex; align-items: center; justify-content: center;
  box-shadow: 0 4px 12px var(--accent-glow);
}
/* 图标 SVG 画布四周有 3.9% 透明留白(40/1024)，放大 1/0.9219 使可见圆角方块
   正好 38px，与改动前的尺寸、位置完全一致，不影响 .brand 布局。 */
.brand-logo img { width: 41.22px; height: 41.22px; display: block; }
.brand-title { font-size: 15px; font-weight: 700; color: var(--text-primary); }
.brand-sub { font-size: 11px; color: var(--text-tertiary); margin-top: 2px; }
.sidebar-footer {
  margin-top: auto; padding: 14px 20px; font-size: 11px;
  color: var(--text-tertiary); border-top: 1px solid var(--border-default);
}
.sidebar-menu { flex: 1; min-height: 0; overflow-y: auto; }
.main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
/* 顶栏复用 .glass-card 做成独立容器块：内缩留白 + 圆角 + 描边 + 阴影，
   与下方 .content 的卡片区分开，不再像直接贴在窗口背景上的一条裸控件。 */
.topbar {
  height: 48px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: space-between;
  margin: 8px 12px 6px; padding: 0 14px; gap: 12px;
  border-radius: var(--radius-md);
}
.platform-bar { display: inline-flex; align-items: center; gap: 8px; min-width: 0; }
.plat-select { width: 280px; min-width: 160px; }
.plat-icon { color: var(--accent); font-size: 16px; }
.plat-label { font-size: 12.5px; color: var(--text-secondary); }
.plat-edit { display: flex; flex-direction: column; gap: 8px; width: 240px; }
.pe-title { font-size: 13px; font-weight: 600; color: var(--text-primary); }
.pe-actions { display: flex; gap: 8px; margin-top: 4px; }
/* 顶栏右侧:状态芯片 + 注册/注销按钮。 */
.status-area { display: inline-flex; align-items: center; gap: 12px; min-width: 0; }
.reg-btns { display: inline-flex; align-items: center; gap: 6px; flex: 0 0 auto; }
/* 顶栏轻量提示条:居中浮在顶栏下方,不占布局。 */
.top-toast {
  position: absolute; top: 64px; left: 50%; transform: translateX(-50%);
  z-index: 50; padding: 7px 18px; border-radius: 999px; font-size: 13px;
  box-shadow: 0 4px 16px rgba(0,0,0,0.12); backdrop-filter: var(--blur-light);
}
.top-toast.ok { background: color-mix(in srgb, var(--success) 16%, #fff); color: var(--success); border: 1px solid color-mix(in srgb, var(--success) 40%, transparent); }
.top-toast.err { background: color-mix(in srgb, var(--error) 16%, #fff); color: var(--error); border: 1px solid color-mix(in srgb, var(--error) 40%, transparent); }
.fade-enter-active, .fade-leave-active { transition: opacity .2s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
/* 全局状态芯片组:一行贴在顶栏右侧,只显示值。内容超长省略,不挤占顶栏。 */
.status-chips {
  display: inline-flex; align-items: center; gap: 6px;
  min-width: 0; overflow: hidden;
}
.chip {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 5px 12px; border-radius: 999px;
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid var(--border-subtle);
  backdrop-filter: var(--blur-light);
  max-width: 200px; min-width: 0;
}
.chip-dot { width: 8px; height: 8px; border-radius: 50%; flex: 0 0 auto; transition: background var(--transition); }
.chip-val {
  font-size: 12.5px; font-weight: 600; color: var(--text-secondary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0;
}
.chip-val.mono { font-family: "SF Mono", Menlo, monospace; font-size: 12px; letter-spacing: .2px; }

/* 全局错误条:顶栏与内容之间,醒目但不遮挡,可手动关闭。 */
.error-bar {
  flex-shrink: 0;
  display: flex; align-items: center; gap: 10px;
  margin: 8px 12px 0; padding: 9px 14px; border-radius: 10px;
  background: color-mix(in srgb, var(--error) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--error) 40%, transparent);
  color: var(--error); font-size: 13px;
}
.eb-icon { flex: 0 0 auto; font-size: 15px; }
.eb-scope { flex: 0 0 auto; font-weight: 700; }
.eb-msg {
  flex: 1 1 auto; min-width: 0; color: var(--text-primary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.eb-close {
  flex: 0 0 auto; background: none; border: none; cursor: pointer;
  color: var(--error); font-size: 13px; padding: 2px 6px; border-radius: 6px;
}
.eb-close:hover { background: color-mix(in srgb, var(--error) 20%, transparent); }

/* 媒体运行时自检条:默认告警色(缺 ffprobe),缺 ffmpeg 时为错误色。 */
.runtime-bar {
  flex-shrink: 0;
  display: flex; align-items: center; gap: 10px;
  margin: 8px 12px 0; padding: 9px 14px; border-radius: 10px;
  font-size: 13px;
  background: color-mix(in srgb, var(--warning) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--warning) 40%, transparent);
  color: var(--warning);
}
.runtime-bar.error {
  background: color-mix(in srgb, var(--error) 12%, transparent);
  border-color: color-mix(in srgb, var(--error) 40%, transparent);
  color: var(--error);
}
.runtime-bar .eb-close.warning { color: var(--warning); }
.runtime-bar .eb-close.warning:hover { background: color-mix(in srgb, var(--warning) 20%, transparent); }

.content { flex: 1; overflow: hidden; padding: 0 12px; min-height: 0; display: flex; flex-direction: column; }

@media (max-width: 1120px) {
  .topbar { padding: 0 12px; }
  .plat-select { width: 220px; }
  .chip:nth-child(2) { display: none; }
  .status-area { gap: 7px; }
}
@media (max-width: 980px) {
  .sidebar { width: 188px; }
  .brand { padding-inline: 16px; }
  .plat-label, .chip:nth-child(3) { display: none; }
  .content { padding-inline: 10px; }
  .topbar, .error-bar, .runtime-bar { margin-inline: 10px; }
}
@media (prefers-reduced-motion: reduce) {
  .fade-enter-active, .fade-leave-active { transition: none; }
}
</style>
