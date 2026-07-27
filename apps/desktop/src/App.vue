<script setup lang="ts">
// 主应用壳(frost-blue 磨砂风):侧边栏 + 顶栏状态胶囊 + 内容区路由视图。
// 必须保留 message/dialog/notification provider,否则子页 useMessage() 抛错致空白。
import { computed, h, onMounted, onUnmounted, provide, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NMenu, NIcon, zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import { VideocamOutline } from "@vicons/ionicons5";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const route = useRoute();
const router = useRouter();

function icon(comp: any) {
  return () => h(NIcon, null, { default: () => h(comp) });
}

const menuOptions: MenuOption[] = [
  { label: "设备模拟", key: "/simulator", icon: icon(VideocamOutline) },
];

const activeKey = computed(() => route.path);
// 设备模拟页自带 SIP 配置与状态,不显示全局顶栏(避免两处配置打架)。
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

// 作为唯一状态源下发给子页(避免各存一份导致状态分叉)。
provide("deviceState", deviceState);
provide("deviceStartedAt", startedAt);

// 浏览器直连 vite 时 __TAURI_INTERNALS__ 不存在,直接调 listen/invoke 会 throw。
// 兜底一下,页面不至于白屏;老板正确用法是 `npm run tauri dev` 起桌面窗口。
const inTauri = typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
let unlisten: UnlistenFn | null = null;
onMounted(async () => {
  if (!inTauri) {
    console.warn("[UVP] 未检测到 Tauri 环境。请用 `npm run tauri dev` 启动桌面应用,而不是直接访问 vite dev URL。");
    return;
  }
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
                  <div class="brand-sub">国标设备模拟</div>
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
              <!-- 设备模拟页自带 SIP 配置与状态,不显示全局顶栏。 -->
              <header v-if="!hideTopbar" class="topbar">
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
  display: flex; align-items: center; justify-content: flex-end;
  padding: 0 24px; gap: 12px;
}
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
