<script setup lang="ts">
// 主应用壳：侧边栏导航 + 路由视图。
import { computed } from "vue";
import { useRoute, useRouter } from "vue-router";
import {
  NConfigProvider, NLayout, NLayoutSider, NLayoutContent,
  NMenu, zhCN, dateZhCN,
} from "naive-ui";
import type { MenuOption } from "naive-ui";

const route  = useRoute();
const router = useRouter();

// 侧边栏菜单项（与 routes 对应）。
const menuOptions: MenuOption[] = [
  { label: "仪表盘",  key: "/dashboard" },
  { label: "平台配置", key: "/config" },
  { label: "压测场景", key: "/scenario" },
  { label: "运行监控", key: "/monitor" },
];

const activeKey = computed(() => route.path);

function onMenuSelect(key: string) {
  router.push(key);
}
</script>

<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
    <n-layout has-sider style="height: 100vh">
      <n-layout-sider bordered :width="200" content-style="padding-top:16px">
        <div style="padding: 12px 20px 16px; font-size:15px; font-weight:600;">
          GB28181 Sim
        </div>
        <n-menu
          :value="activeKey"
          :options="menuOptions"
          @update:value="onMenuSelect"
        />
      </n-layout-sider>

      <n-layout-content content-style="padding: 24px; overflow-y: auto;">
        <router-view />
      </n-layout-content>
    </n-layout>
  </n-config-provider>
</template>
