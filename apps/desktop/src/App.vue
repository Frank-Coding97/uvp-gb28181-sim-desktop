<script setup lang="ts">
// 主应用壳(frost-blue 磨砂风,对齐参考原型):
// 顶栏右侧状态胶囊 + 磨砂侧边栏 + 内容区路由视图。
// 必须保留 message/dialog/notification provider,否则子页 useMessage() 抛错致空白。
import { computed, h, onMounted, onUnmounted, provide, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NMenu, NIcon, NPopover, NInput, NInputNumber, NButton, NSelect, zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import {
  SpeedometerOutline, HardwareChipOutline, ServerOutline,
  PulseOutline, GitNetworkOutline, VideocamOutline,
} from "@vicons/ionicons5";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { usePlatform } from "./platform";

const route = useRoute();
const router = useRouter();

function icon(comp: any) {
  return () => h(NIcon, null, { default: () => h(comp) });
}

const menuOptions: MenuOption[] = [
  { label: "设备模拟",   key: "/simulator", icon: icon(VideocamOutline) },
  { label: "单设备联调", key: "/device",    icon: icon(HardwareChipOutline) },
  { label: "多通道目录", key: "/channels",  icon: icon(GitNetworkOutline) },
  { label: "压力测试",   key: "/scenario",  icon: icon(PulseOutline) },
  { label: "仪表盘",    key: "/dashboard", icon: icon(SpeedometerOutline) },
];

// 平台档案(全局):顶栏切换,单设备/压测共用同一份平台连接参数。
const { profiles, activeId, active, setActive, addProfile, updateProfile, removeProfile } = usePlatform();
provide("platform", { profiles, activeId, active, setActive, addProfile, updateProfile, removeProfile });
const profileOptions = computed(() =>
  profiles.value.map((p) => ({ label: `${p.name}  (${p.server_host}:${p.server_port})`, value: p.id }))
);
// 顶栏编辑弹层用的临时表单。
const editForm = ref({ name: "", server_host: "", server_port: 5060, server_domain: "", password: "", transport: "UDP", signaling_encoding: "GB18030" });
function openEdit() {
  if (active.value) Object.assign(editForm.value, active.value);
}
function saveEdit() {
  updateProfile(activeId.value, { ...editForm.value });
}
function addNew() {
  addProfile({ name: "新平台", server_host: "127.0.0.1", server_port: 5060,
    server_domain: "34020000002000000001", password: "12345678", transport: "UDP", signaling_encoding: "GB18030" });
  openEdit();
}
const transportOptions = [{ label: "UDP", value: "UDP" }, { label: "TCP", value: "TCP" }];
const encodingOptions = [
  { label: "GB18030(国标默认)", value: "GB18030" },
  { label: "UTF-8", value: "UTF-8" },
];

const activeKey = computed(() => route.path);
// 设备模拟页隐藏全局顶栏(目标平台下拉 + 状态胶囊),该页自己管配置与状态。
const hideTopbar = computed(() => route.path === "/simulator");
function onMenuSelect(key: string) {
  router.push(key);
}

// 全局设备状态(顶栏胶囊),订阅 device_state 事件。
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const deviceState = ref<DState>("Disconnected");
const statusMeta = computed(() => {
  switch (deviceState.value) {
    case "Registering": return { text: "注册中", color: "var(--warning)" };
    case "Registered":  return { text: "已注册", color: "var(--success)" };
    case "InCall":      return { text: "推流中", color: "var(--accent)" };
    case "Failed":      return { text: "注册失败", color: "var(--error)" };
    default:            return { text: "未连接", color: "var(--text-tertiary)" };
  }
});
// 注册起始时刻(在线时长基准)。放在常驻的 App 里,切换路由不丢失。
const startedAt = ref<number | null>(null);

// 作为唯一状态源下发给子页(Device 页 inject,避免各存一份导致状态分叉)。
provide("deviceState", deviceState);
provide("deviceStartedAt", startedAt);

let unlisten: UnlistenFn | null = null;
onMounted(async () => {
  unlisten = await listen<string>("device_state", (e) => {
    const s = e.payload as DState;
    deviceState.value = s;
    if (s === "Registered" && !startedAt.value) startedAt.value = Date.now();
    if (s === "Disconnected" || s === "Failed") startedAt.value = null;
  });
});
onUnmounted(() => unlisten?.());

const themeOverrides = {
  common: {
    primaryColor: "#3884ff",
    primaryColorHover: "#5a9bff",
    primaryColorPressed: "#1e63dc",
    borderRadius: "8px",
    fontSize: "13px",
  },
  Card: { color: "rgba(255,255,255,0.5)", borderRadius: "16px" },
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
                <div class="brand-logo">UVP</div>
                <div>
                  <div class="brand-title">GB28181 Sim</div>
                  <div class="brand-sub">国标设备模拟 · 压测</div>
                </div>
              </div>
              <n-menu
                :value="activeKey"
                :options="menuOptions"
                :indent="18"
                @update:value="onMenuSelect"
              />
              <div class="sidebar-footer">v0.1.2 · UVP</div>
            </aside>

            <!-- 主区 -->
            <main class="main">
              <!-- 设备模拟页自带 SIP 配置与状态,不显示全局顶栏(避免两处配置打架)。
                   顶栏暂留给压测页用,待 SIP 配置提为唯一真相源后整体撤掉。 -->
              <header v-if="!hideTopbar" class="topbar">
                <!-- 全局目标平台:单设备/压测共用,下拉切换 + 编辑 -->
                <div class="platform-bar">
                  <n-icon :component="ServerOutline" class="plat-icon" />
                  <span class="plat-label">目标平台</span>
                  <n-select
                    size="small"
                    :value="activeId"
                    :options="profileOptions"
                    style="width: 280px"
                    @update:value="setActive"
                  />
                  <n-popover trigger="click" placement="bottom-start" @update:show="(s: boolean) => s && openEdit()">
                    <template #trigger>
                      <n-button size="small" tertiary>编辑</n-button>
                    </template>
                    <div class="plat-edit">
                      <div class="pe-title">平台档案</div>
                      <n-input v-model:value="editForm.name" size="small" placeholder="档案名称" />
                      <n-input v-model:value="editForm.server_host" size="small" placeholder="平台 IP" />
                      <n-input-number v-model:value="editForm.server_port" size="small" :min="1" :max="65535" style="width:100%" placeholder="SIP 端口" />
                      <n-input v-model:value="editForm.server_domain" size="small" placeholder="平台域 ID" />
                      <n-input v-model:value="editForm.password" size="small" type="password" show-password-on="click" placeholder="SIP 密码" />
                      <n-select v-model:value="editForm.transport" size="small" :options="transportOptions" />
                      <n-select v-model:value="editForm.signaling_encoding" size="small" :options="encodingOptions" />
                      <div class="pe-actions">
                        <n-button size="small" type="primary" @click="saveEdit">保存</n-button>
                        <n-button size="small" @click="addNew">+ 新增</n-button>
                        <n-button size="small" type="error" :disabled="profiles.length <= 1" @click="removeProfile(activeId)">删除</n-button>
                      </div>
                    </div>
                  </n-popover>
                </div>
                <div class="status-pill">
                  <span class="pill-dot" :style="{ background: statusMeta.color }" />
                  <span class="pill-text">{{ statusMeta.text }}</span>
                </div>
              </header>
              <section class="content" :class="{ 'no-topbar': hideTopbar }">
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
  width: 38px; height: 38px; border-radius: 10px;
  background: linear-gradient(135deg, #3884ff, #1e63dc);
  color: #fff; font-weight: 700; font-size: 13px; letter-spacing: 0.5px;
  display: flex; align-items: center; justify-content: center;
  box-shadow: 0 4px 12px var(--accent-glow);
}
.brand-title { font-size: 15px; font-weight: 700; color: var(--text-primary); }
.brand-sub { font-size: 11px; color: var(--text-tertiary); margin-top: 2px; }
.sidebar-footer {
  margin-top: auto; padding: 14px 20px; font-size: 11px;
  color: var(--text-tertiary); border-top: 1px solid var(--border-default);
}
.main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
.topbar {
  height: 48px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: space-between;
  padding: 0 24px; gap: 12px;
}
.platform-bar { display: inline-flex; align-items: center; gap: 8px; }
.plat-icon { color: var(--accent); font-size: 16px; }
.plat-label { font-size: 12.5px; color: var(--text-secondary); }
.plat-edit { display: flex; flex-direction: column; gap: 8px; width: 240px; }
.pe-title { font-size: 13px; font-weight: 600; color: var(--text-primary); }
.pe-actions { display: flex; gap: 8px; margin-top: 4px; }
.status-pill {
  display: inline-flex; align-items: center; gap: 8px;
  padding: 5px 14px; border-radius: 999px;
  background: rgba(255, 255, 255, 0.6);
  border: 1px solid var(--border-subtle);
  backdrop-filter: var(--blur-light);
  font-size: 12.5px; color: var(--text-secondary);
}
.pill-dot { width: 8px; height: 8px; border-radius: 50%; transition: background var(--transition); }
.content { flex: 1; overflow-y: auto; padding: 4px 28px 28px; min-height: 0; }
/* 无顶栏时补回顶部呼吸;并让子页能用 height:100% 撑满(设备模拟页靠它填满高度) */
.content.no-topbar { padding-top: 24px; display: flex; flex-direction: column; overflow: hidden; }
.content.no-topbar > * { min-height: 0; }
</style>
