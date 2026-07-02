<script setup lang="ts">
// M0 骨架首页:验证前端 ↔ Tauri(Rust)IPC 通路。
// M4 起替换为真正的配置向导 / 场景编排 / 监控大盘。
import { ref } from "vue";
import {
  NConfigProvider,
  NLayout,
  NLayoutSider,
  NLayoutContent,
  NMenu,
  NButton,
  NCard,
  NSpace,
  NText,
  zhCN,
  dateZhCN,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import type { MenuOption } from "naive-ui";
import { h } from "vue";

// 侧边栏导航项(对应架构里的 UI 页面规划)。
const menuOptions: MenuOption[] = [
  { label: "仪表盘", key: "dashboard" },
  { label: "平台配置", key: "platform" },
  { label: "设备模板", key: "devices" },
  { label: "压测场景", key: "scenario" },
  { label: "运行监控", key: "monitor" },
  { label: "日志中心", key: "logs" },
];
const activeKey = ref("dashboard");

// 调用后端 Tauri 命令,验证引擎版本可达。
const engineInfo = ref("尚未连接引擎");
async function pingEngine() {
  try {
    engineInfo.value = await invoke<string>("engine_version");
  } catch (e) {
    engineInfo.value = `调用失败: ${e}`;
  }
}

function renderTitle() {
  return h(NText, { strong: true, style: "font-size:16px" }, () => "GB28181 Sim");
}
</script>

<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
    <n-layout has-sider style="height: 100vh">
      <n-layout-sider bordered :width="220" content-style="padding-top:16px">
        <div style="padding: 0 20px 16px">
          <component :is="renderTitle" />
        </div>
        <n-menu v-model:value="activeKey" :options="menuOptions" />
      </n-layout-sider>
      <n-layout-content content-style="padding: 24px">
        <n-space vertical size="large">
          <n-card title="欢迎使用 UVP GB28181 桌面端">
            <n-text depth="3">
              多系统桌面端 GB28181 设备模拟器 + 压力测试工具（M0 骨架）。
            </n-text>
          </n-card>
          <n-card title="引擎连通性自检">
            <n-space vertical>
              <n-text>{{ engineInfo }}</n-text>
              <n-button type="primary" @click="pingEngine">Ping 引擎</n-button>
            </n-space>
          </n-card>
        </n-space>
      </n-layout-content>
    </n-layout>
  </n-config-provider>
</template>
