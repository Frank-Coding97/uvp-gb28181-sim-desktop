<script setup lang="ts">
// 主应用壳(frost-blue 磨砂风,对齐参考原型):
// 顶栏右侧状态胶囊 + 磨砂侧边栏 + 内容区路由视图。
// 必须保留 message/dialog/notification provider,否则子页 useMessage() 抛错致空白。
import { computed, h, onMounted, onUnmounted, provide, ref, watch } from "vue";
// 起别名避免与模板里 <router-view> 的插槽变量 `Component` 撞名。
import type { Component as VueComponent } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NMenu, NIcon, NModal, NInput, NInputNumber, NButton, NSelect, zhCN, dateZhCN,
  createDiscreteApi,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import SpeedometerOutline from "@vicons/ionicons5/es/SpeedometerOutline.js";
import HardwareChipOutline from "@vicons/ionicons5/es/HardwareChipOutline.js";
import ServerOutline from "@vicons/ionicons5/es/ServerOutline.js";
import PulseOutline from "@vicons/ionicons5/es/PulseOutline.js";
import GitNetworkOutline from "@vicons/ionicons5/es/GitNetworkOutline.js";
import TerminalOutline from "@vicons/ionicons5/es/TerminalOutline.js";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlatform } from "./platform";
import { useDevice } from "./device";
import { useTheme } from "./theme";

const route = useRoute();
const router = useRouter();

/** 读一个 CSS 设计令牌的当前值。取不到时回落到给定默认色。 */
function readToken(name: string, fallback: string): string {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

// 主题:浅色 / 暗色 / 跟随系统。切 <html data-theme> + Naive UI 主题。
const { isDark, naiveTheme, label: themeLabel, toggle: toggleTheme } = useTheme();

// 侧边栏菜单图标工厂。参数是 @vicons 的组件,用 `Component` 而不是 `any`。
function icon(comp: VueComponent) {
  return () => h(NIcon, null, { default: () => h(comp) });
}

const menuOptions: MenuOption[] = [
  { label: "仪表盘",    key: "/dashboard", icon: icon(SpeedometerOutline) },
  { label: "单设备联调", key: "/device",    icon: icon(HardwareChipOutline) },
  { label: "多通道目录", key: "/channels",  icon: icon(GitNetworkOutline) },
  { label: "压力测试",   key: "/scenario",  icon: icon(PulseOutline) },
  { label: "系统信息",   key: "/system",    icon: icon(TerminalOutline) },
];

// 平台档案(全局):顶栏切换,单设备/压测共用同一份平台连接参数。
// 密码不在档案里(存 OS 钥匙串),这里只维护「是否已配置」与保存动作。
const {
  profiles, activeId, active, secretStatus, activeHasSecret,
  setActive, addProfile, updateProfile, removeProfile,
  savePassword, refreshSecretStatus, migrateLegacyPasswords,
} = usePlatform();
provide("platform", {
  profiles, activeId, active, secretStatus, activeHasSecret,
  setActive, addProfile, updateProfile, removeProfile,
  savePassword, refreshSecretStatus,
});
const profileOptions = computed(() =>
  profiles.value.map((p) => ({ label: `${p.name}  (${p.server_host}:${p.server_port})`, value: p.id }))
);
// 平台档案编辑弹窗(稳定模态框:点击遮罩/ESC 均不关闭,防误触丢失输入)。
// 表单只在编辑目标切换时重新装载;关闭后重开保留未保存的草稿。
const editOpen = ref(false);
let editTargetId: string | null = null;
// password 是「待写入钥匙串」的一次性输入,留空表示不改动已保存的密码。
const editForm = ref({ name: "", server_host: "", server_port: 5060, server_domain: "", password: "", transport: "UDP", signaling_encoding: "GB18030" });
const secretHint = computed(() =>
  activeHasSecret.value ? "已保存到系统钥匙串,留空则不改动" : "尚未配置,注册前必须填写"
);
function openEdit() {
  if (active.value && editTargetId !== activeId.value) {
    Object.assign(editForm.value, active.value, { password: "" });
    editTargetId = activeId.value;
  }
}
function openEditModal() {
  openEdit();
  refreshSecretStatus();
  editOpen.value = true;
}
async function saveEdit() {
  const { password, ...profile } = editForm.value;
  updateProfile(activeId.value, profile);
  if (password) {
    try {
      const msg = await savePassword(activeId.value, password);
      topMessage.value = { text: msg, ok: true };
    } catch (e) {
      topMessage.value = { text: `密码保存失败:${String(e)}`, ok: false };
      return; // 密码没存上就别关窗,让用户重试
    }
  }
  editForm.value.password = "";
  editTargetId = activeId.value;
  editOpen.value = false;
}
function addNew() {
  addProfile({ name: "新平台", server_host: "127.0.0.1", server_port: 5060,
    server_domain: "34020000002000000001", transport: "UDP", signaling_encoding: "GB18030" });
  openEdit();
}
// App.vue **本身**渲染 n-dialog-provider,所以它自己不能用 useDialog()
// (provider 必须是祖先)。createDiscreteApi 就是为这种场景提供的脱离上下文的 API。
const { dialog: discreteDialog } = createDiscreteApi(["dialog"]);

async function onDeleteProfile() {
  // 删档案会连带删掉钥匙串里的密码,且不可撤销 —— 必须二次确认。
  // 之前点一下即删,连提示都没有。
  const target = profiles.value.find((p) => p.id === activeId.value);
  discreteDialog.warning({
    title: "删除平台档案",
    content: `确定删除「${target?.name ?? activeId.value}」吗?该档案在系统钥匙串里保存的 SIP 密码也会一并删除,此操作无法撤销。`,
    positiveText: "删除",
    negativeText: "取消",
    async onPositiveClick() {
      await removeProfile(activeId.value);
      openEdit(); // 删除后 activeId 可能切换,重新装载当前档案到表单
    },
  });
}
const transportOptions = [
  { label: "UDP(国标最常用)", value: "UDP" },
  { label: "TCP(大目录/丢包网络更稳)", value: "TCP" },
];
const encodingOptions = [
  { label: "GB18030(国标默认)", value: "GB18030" },
  { label: "UTF-8", value: "UTF-8" },
];

const activeKey = computed(() => route.path);
function onMenuSelect(key: string) {
  router.push(key);
}

// 全局设备状态 + 注册/注销:来自 device store(顶栏与单设备页共用同一份)。
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const {
  deviceState, startedAt, statusMeta, startDisabled, stopDisabled, startingUp,
  startDevice, stopDevice, reconcile,
} = useDevice();

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

// 顶栏注册/注销:任意页面可操作同一台设备。成功用 message,失败落全局错误条。
async function onRegister() {
  const r = await startDevice(active.value);
  if (r.ok) topMessage.value = { text: r.msg, ok: true };
  else lastError.value = { scope: "register", message: r.msg, ts_ms: Date.now() };
}
async function onLogout() {
  const r = await stopDevice();
  topMessage.value = { text: r.msg, ok: r.ok };
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
  capture: "视频源采集", invite: "点播", stream: "推流", register: "注册",
};

let unlisten: UnlistenFn | null = null;
let unlistenErr: UnlistenFn | null = null;
onMounted(async () => {
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
  // app 启动/刷新后与引擎对账,反映后台是否已有设备在跑。
  reconcile();
  // 旧版本可能把密码明文存在 localStorage:搬进钥匙串并抹掉明文。
  const migrated = await migrateLegacyPasswords();
  if (migrated.length) {
    topMessage.value = {
      text: `已将 ${migrated.length} 个平台档案的密码迁移到系统钥匙串并清除本地明文`,
      ok: true,
    };
  }
  await refreshSecretStatus();
});
onUnmounted(() => { unlisten?.(); unlistenErr?.(); if (uptimeTimer) clearInterval(uptimeTimer); });

const themeOverrides = {
  common: {
    // 与 theme.css 的 --accent 同源。naive-ui 的 themeOverrides 只接受具体色值,
    // 不能写 var(),所以这里从 CSS 令牌读一次而不是再抄一份 ——
    // 抄一份就意味着改主色要改两处,漏一处就是"按钮和其它控件不同色"。
    primaryColor: readToken("--accent", "#3884ff"),
    primaryColorHover: readToken("--accent-hover", "#5a9bff"),
    primaryColorPressed: readToken("--accent-deep", "#1e63dc"),
    borderRadius: "6px",
    fontSize: "12px",
    fontSizeSmall: "11.5px",
    fontSizeMini: "10.5px",
    heightSmall: "26px",
    heightMedium: "30px",
  },
  Card: { borderRadius: "10px" },
  Button: { borderRadiusSmall: "5px", borderRadiusMedium: "6px", fontSizeSmall: "11.5px", heightSmall: "26px" },
  Input: { borderRadius: "6px", fontSizeSmall: "12px", heightSmall: "26px" },
  Select: { borderRadius: "6px", fontSizeSmall: "12px" },
  Menu: { fontSize: "12px", itemHeight: "34px", borderRadius: "6px" },
};
</script>

<template>
  <n-config-provider
    :locale="zhCN"
    :date-locale="dateZhCN"
    :theme="naiveTheme"
    :theme-overrides="themeOverrides"
  >
    <n-message-provider>
      <n-dialog-provider>
        <n-notification-provider>
          <div class="shell">
            <!-- 侧边栏 -->
            <aside class="sidebar" aria-label="主导航">
              <div class="brand">
                <div class="brand-logo">UVP</div>
                <div>
                  <div class="brand-title">GB28181 Sim</div>
                  <div class="brand-sub">国标设备模拟 · 压测</div>
                </div>
              </div>
              <n-menu
                :value="activeKey"
                :options="menuOptions"
                :indent="14"
                @update:value="onMenuSelect"
              />
              <div class="sidebar-footer">v0.1.2 · UVP</div>
            </aside>

            <!-- 主区 -->
            <main class="main">
              <header class="topbar">
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
                  <n-button size="small" tertiary @click="openEditModal">编辑</n-button>
                  <span
                    class="plat-secret"
                    :class="{ missing: !activeHasSecret }"
                    :title="activeHasSecret ? 'SIP 密码已存入系统钥匙串' : '尚未配置 SIP 密码,无法注册'"
                  >
                    <span aria-hidden="true">{{ activeHasSecret ? "🔒" : "⚠" }}</span>
                    {{ activeHasSecret ? "密码已配置" : "缺密码" }}
                  </span>
                  <!-- 平台档案编辑:稳定模态框,点击遮罩/ESC 不关闭,误触不丢失输入。 -->
                  <n-modal
                    v-model:show="editOpen"
                    preset="card"
                    title="平台档案"
                    :mask-closable="false"
                    :close-on-esc="false"
                    style="width: 330px"
                  >
                    <div class="plat-edit">
                      <n-input v-model:value="editForm.name" size="small" placeholder="档案名称" />
                      <n-input v-model:value="editForm.server_host" size="small" placeholder="平台 IP" />
                      <n-input-number v-model:value="editForm.server_port" size="small" :min="1" :max="65535" style="width:100%" placeholder="SIP 端口" />
                      <n-input v-model:value="editForm.server_domain" size="small" placeholder="平台域 ID" />
                      <div class="pe-secret">
                        <n-input
                          v-model:value="editForm.password"
                          size="small"
                          type="password"
                          show-password-on="click"
                          :placeholder="activeHasSecret ? 'SIP 密码(留空不改)' : 'SIP 密码'"
                        />
                        <p class="pe-hint" :class="{ warn: !activeHasSecret }">
                          <span aria-hidden="true">{{ activeHasSecret ? "🔒" : "⚠" }}</span>
                          {{ secretHint }}
                        </p>
                      </div>
                      <n-select v-model:value="editForm.transport" size="small" :options="transportOptions" />
                      <n-select v-model:value="editForm.signaling_encoding" size="small" :options="encodingOptions" />
                      <div class="pe-actions">
                        <n-button size="small" type="primary" @click="saveEdit">保存</n-button>
                        <n-button size="small" @click="addNew">+ 新增</n-button>
                        <n-button size="small" type="error" :disabled="profiles.length <= 1" @click="onDeleteProfile">删除</n-button>
                      </div>
                    </div>
                  </n-modal>
                </div>
                <!-- 全局连接状态与注册/注销：单行极简设计，与左侧等高 -->
                <div class="connection-card" role="status" aria-live="polite">
                  <span class="conn-dot" :style="{ background: statusMeta.color }" aria-hidden="true" />
                  <span class="conn-status-text">
                    {{ statusMeta.text }}
                    <template v-if="deviceState === 'Registered' || deviceState === 'InCall'"> · 在线 {{ uptime }}</template>
                  </span>
                  <div class="reg-btns">
                    <!-- :loading 与 :disabled 同时挂 startingUp:注册要几百毫秒到几秒,
                         这段窗口不给反馈的话用户会连点,重复下发 start_device。 -->
                    <n-button
                      size="small"
                      type="primary"
                      :disabled="startDisabled"
                      :loading="startingUp"
                      @click="onRegister"
                    >注册</n-button>
                    <n-button
                      size="small"
                      secondary
                      type="error"
                      :disabled="stopDisabled"
                      :loading="startingUp"
                      @click="onLogout"
                    >注销</n-button>
                    <button
                      class="theme-toggle"
                      type="button"
                      :title="`外观:${themeLabel}(点击切换)`"
                      :aria-label="`切换外观,当前为${themeLabel}`"
                      @click="toggleTheme"
                    >
                      <span aria-hidden="true">{{ isDark ? "🌙" : "☀️" }}</span>
                    </button>
                  </div>
                </div>
              </header>
              <!-- 顶栏轻量提示(注册/注销结果),2.5s 自动消失。 -->
              <transition name="fade">
                <div
                  v-if="topMessage"
                  class="top-toast"
                  role="status"
                  aria-live="polite"
                  :class="{ ok: topMessage.ok, err: !topMessage.ok }"
                >
                  {{ topMessage.text }}
                </div>
              </transition>
              <!-- 全局错误条:后端运行时错误(如点播采集失败),所有页面可见,可关闭。 -->
              <div v-if="lastError" class="error-bar" role="alert" aria-live="assertive">
                <span class="eb-icon">⚠</span>
                <span class="eb-scope">{{ errorScopeLabel[lastError.scope] ?? lastError.scope }}失败</span>
                <span class="eb-msg" :title="lastError.message">{{ lastError.message }}</span>
                <button class="eb-close" type="button" aria-label="关闭错误提示" @click="dismissError">✕</button>
              </div>
              <section class="content" role="main" aria-label="页面内容">
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
  width: 180px; flex-shrink: 0;
  background: var(--bg-surface);
  border-right: 1px solid var(--border-default);
  backdrop-filter: var(--blur-light);
  display: flex; flex-direction: column;
}
.brand { display: flex; align-items: center; gap: 9px; padding: 13px 14px 10px; }
.brand-logo {
  width: 28px; height: 28px; border-radius: 7px;
  background: linear-gradient(135deg, var(--accent), var(--accent-deep));
  color: #fff; font-weight: 700; font-size: var(--fs-sm); letter-spacing: 0.5px;
  display: flex; align-items: center; justify-content: center;
  box-shadow: 0 2px 8px var(--accent-glow);
}
.brand-title { font-size: var(--fs-md); font-weight: 700; color: var(--text-primary); }
.brand-sub { font-size: var(--fs-xs); color: var(--text-tertiary); margin-top: 1px; }
.sidebar-footer {
  margin-top: auto; padding: 10px 14px; font-size: var(--fs-xs);
  color: var(--text-tertiary); border-top: 1px solid var(--border-default);
}
.main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
.topbar {
  min-height: 46px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: space-between;
  padding: 4px 16px; gap: 10px;
  border-bottom: 1px solid var(--border-default);
}
.platform-bar { display: inline-flex; align-items: center; gap: 6px; min-width: 0; flex: 1 1 auto; }
.plat-select { width: clamp(180px, 20vw, 260px); min-width: 140px; }
.plat-icon { color: var(--accent); font-size: var(--fs-lg); }
.plat-label { font-size: var(--fs-sm); color: var(--text-secondary); }
.plat-edit { display: flex; flex-direction: column; gap: 6px; width: 100%; }
.pe-actions { display: flex; gap: 6px; margin-top: 4px; }
.pe-secret { display: flex; flex-direction: column; gap: 3px; }
.pe-hint { margin: 0; font-size: var(--fs-sm); color: var(--text-tertiary); line-height: 1.35; }
.pe-hint.warn { color: var(--warning); }
/* 顶栏密码状态徽标:一眼看出当前档案能否注册 */
.plat-secret {
  display: inline-flex; align-items: center; gap: 3px; flex: 0 0 auto;
  padding: 1px 7px; border-radius: 999px; font-size: var(--fs-sm); white-space: nowrap;
  background: color-mix(in srgb, var(--success) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--success) 32%, transparent);
  color: var(--success);
}
.plat-secret.missing {
  background: color-mix(in srgb, var(--warning) 14%, transparent);
  border-color: color-mix(in srgb, var(--warning) 36%, transparent);
  color: var(--warning);
}
/* 顶栏右侧连接状态条：单行，与左侧目标平台等高 */
.connection-card {
  display: inline-flex; align-items: center; gap: 8px;
  padding: 0 4px 0 10px; border-radius: var(--radius-xs);
  background: var(--bg-surface); border: 1px solid var(--border-subtle);
  box-shadow: var(--shadow-card);
  backdrop-filter: var(--blur-light); -webkit-backdrop-filter: var(--blur-light);
  height: 28px; box-sizing: border-box;
}
.conn-dot { width: 7px; height: 7px; border-radius: 50%; flex: 0 0 auto; box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 10%, transparent); }
.conn-status-text { color: var(--text-primary); font-size: var(--fs-sm); font-weight: 600; white-space: nowrap; }
.reg-btns { display: inline-flex; align-items: center; gap: 4px; flex: 0 0 auto; margin-left: 2px; }
/* 外观切换:轻量图标按钮,与注册/注销同一行 */
.theme-toggle {
  display: inline-flex; align-items: center; justify-content: center;
  width: 26px; height: 26px; padding: 0; cursor: pointer;
  border: 1px solid var(--border-default); border-radius: var(--radius-xs);
  background: var(--bg-surface); color: var(--text-secondary); font-size: var(--fs-md);
  transition: border-color var(--transition), background var(--transition);
}
.theme-toggle:hover { border-color: var(--accent); background: var(--bg-hover); }
/* 顶栏轻量提示条:居中浮在顶栏下方,不占布局。 */
.top-toast {
  position: absolute; top: 50px; left: 50%; transform: translateX(-50%);
  z-index: 50; padding: 5px 14px; border-radius: 999px; font-size: var(--fs-sm);
  box-shadow: var(--shadow-elevated); backdrop-filter: var(--blur-light);
}
.top-toast.ok { background: color-mix(in srgb, var(--success) 16%, var(--bg-base)); color: var(--success); border: 1px solid color-mix(in srgb, var(--success) 40%, transparent); }
.top-toast.err { background: color-mix(in srgb, var(--error) 16%, var(--bg-base)); color: var(--error); border: 1px solid color-mix(in srgb, var(--error) 40%, transparent); }
.fade-enter-active, .fade-leave-active { transition: opacity .18s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }
/* 全局错误条:顶栏与内容之间,醒目但不遮挡,可手动关闭。 */
.error-bar {
  flex-shrink: 0;
  display: flex; align-items: center; gap: 8px;
  margin: 3px 16px 0; padding: 6px 10px; border-radius: 6px;
  background: color-mix(in srgb, var(--error) 10%, transparent);
  border: 1px solid color-mix(in srgb, var(--error) 30%, transparent);
  color: var(--error); font-size: var(--fs-sm);
}
.eb-icon { flex: 0 0 auto; font-size: var(--fs-md); }
.eb-scope { flex: 0 0 auto; font-weight: 700; }
.eb-msg {
  flex: 1 1 auto; min-width: 0; color: var(--text-primary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.eb-close {
  flex: 0 0 auto; background: none; border: none; cursor: pointer;
  color: var(--error); font-size: var(--fs-sm); padding: 1px 4px; border-radius: 4px;
}
.eb-close:hover { background: color-mix(in srgb, var(--error) 20%, transparent); }

.content { flex: 1; overflow-y: auto; padding: 8px 16px 16px; }

@media (max-width: 1120px) {
  .topbar { padding: 4px 12px; }
  .plat-select { width: 180px; }
  .connection-card { flex-basis: 380px; min-width: 240px; }
}
@media (max-width: 900px) {
  .topbar { align-items: stretch; flex-direction: column; height: auto; padding: 6px 12px; }
  .platform-bar, .connection-card { width: 100%; flex-basis: auto; }
}
@media (max-width: 980px) {
  .sidebar { width: 160px; }
  .brand { padding-inline: 12px; }
  .plat-label, .chip:nth-child(3) { display: none; }
  .content { padding-inline: 12px; }
}
@media (prefers-reduced-motion: reduce) {
  .fade-enter-active, .fade-leave-active { transition: none; }
}
</style>
