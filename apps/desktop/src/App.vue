<script setup lang="ts">
// 主应用壳:品牌侧边栏导航 + 路由视图。
// 关键:必须用 n-message-provider / n-dialog-provider / n-notification-provider 包裹,
// 否则子页面 useMessage() 会抛错导致整页不渲染(务必保留)。
import { computed, h } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NMessageProvider, NDialogProvider, NNotificationProvider,
  NLayout, NLayoutSider, NLayoutContent, NMenu, NIcon,
  zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";
import {
  SpeedometerOutline, HardwareChipOutline, ServerOutline,
  LayersOutline, PulseOutline,
} from "@vicons/ionicons5";

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

// 品牌主色主题(企业蓝)。
const themeOverrides = {
  common: {
    primaryColor: "#1890ff",
    primaryColorHover: "#40a9ff",
    primaryColorPressed: "#096dd9",
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
          <n-layout has-sider style="height: 100vh">
            <n-layout-sider
              bordered
              :width="216"
              :native-scrollbar="false"
              content-style="display:flex; flex-direction:column; height:100%;"
            >
              <!-- 品牌区 -->
              <div class="brand">
                <div class="brand-logo">GB</div>
                <div class="brand-text">
                  <div class="brand-title">GB28181 Sim</div>
                  <div class="brand-sub">国标设备模拟 · 压测</div>
                </div>
              </div>

              <n-menu
                :value="activeKey"
                :options="menuOptions"
                :indent="20"
                @update:value="onMenuSelect"
              />

              <div class="sidebar-footer">v0.1.0 · UVP</div>
            </n-layout-sider>

            <n-layout-content
              content-style="padding: 24px 28px; overflow-y: auto; background:#f5f7fa;"
            >
              <router-view />
            </n-layout-content>
          </n-layout>
        </n-notification-provider>
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>

<style scoped>
.brand {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 18px 20px 16px;
}
.brand-logo {
  width: 36px;
  height: 36px;
  border-radius: 9px;
  background: linear-gradient(135deg, #1890ff, #096dd9);
  color: #fff;
  font-weight: 700;
  font-size: 15px;
  display: flex;
  align-items: center;
  justify-content: center;
  box-shadow: 0 2px 8px rgba(24, 144, 255, 0.35);
}
.brand-title {
  font-size: 15px;
  font-weight: 600;
  color: #1f2937;
  line-height: 1.2;
}
.brand-sub {
  font-size: 11px;
  color: #9ca3af;
  margin-top: 2px;
}
.sidebar-footer {
  margin-top: auto;
  padding: 14px 20px;
  font-size: 11px;
  color: #b0b7c3;
  border-top: 1px solid #f0f0f0;
}
</style>
