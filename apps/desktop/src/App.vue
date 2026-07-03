<script setup lang="ts">
// 主应用壳(frost-blue 磨砂风,对齐参考原型):
// 顶栏右侧状态胶囊 + 磨砂侧边栏 + 内容区路由视图。
// 必须保留 message/dialog/notification provider,否则子页 useMessage() 抛错致空白。
import { computed, h, onMounted, onUnmounted, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NMenu, NIcon, zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import {
  SpeedometerOutline, HardwareChipOutline, ServerOutline,
  LayersOutline, PulseOutline,
} from "@vicons/ionicons5";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const route = useRoute();
const router = useRouter();

function icon(comp: any) {
  return () => h(NIcon, null, { default: () => h(comp) });
}

const menuOptions: MenuOption[] = [
  { label: "仪表盘",    key: "/dashboard", icon: icon(SpeedometerOutline) },
  { label: "单设备联调", key: "/device",    icon: icon(HardwareChipOutline) },
  { label: "平台配置",   key: "/config",    icon: icon(ServerOutline) },
  { label: "压测场景",   key: "/scenario",  icon: icon(LayersOutline) },
  { label: "运行监控",   key: "/monitor",   icon: icon(PulseOutline) },
];

const activeKey = computed(() => route.path);
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
let unlisten: UnlistenFn | null = null;
onMounted(async () => {
  unlisten = await listen<string>("device_state", (e) => {
    deviceState.value = e.payload as DState;
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
                <div class="brand-logo">GB</div>
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
              <div class="sidebar-footer">v0.1.0 · UVP</div>
            </aside>

            <!-- 主区 -->
            <main class="main">
              <header class="topbar">
                <div class="status-pill">
                  <span class="pill-dot" :style="{ background: statusMeta.color }" />
                  <span class="pill-text">{{ statusMeta.text }}</span>
                </div>
              </header>
              <section class="content">
                <router-view />
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
  color: #fff; font-weight: 700; font-size: 15px;
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
  padding: 0 24px;
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
.content { flex: 1; overflow-y: auto; padding: 4px 28px 28px; }
</style>
