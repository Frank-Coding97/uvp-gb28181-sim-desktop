<script setup lang="ts">
// 单设备联调控制台(UC-1):填参数 → 注册 → 实时状态灯 → 报警/点播。
// 规格见 docs/10-functional/desktop-device-console.md。
import { ref, onMounted, onUnmounted, computed } from "vue";
import {
  NCard, NForm, NFormItem, NInput, NInputNumber, NSelect,
  NButton, NSpace, NText, NGrid, NGi, NDivider, useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const message = useMessage();

// 表单(默认填实测 WVP 参数,方便直接联调)。
const form = ref({
  server_host:   "192.168.10.222",
  server_port:   8160,
  server_domain: "3502000000",
  device_id:     "35020000001310000001",
  password:      "wvp_sip_password",
  transport:     "UDP",
  gb_version:    "2022",
  channel_name:  "Camera-1",
  video_source:  "",
});

const transportOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP", value: "TCP" },
];
const versionOptions = [
  { label: "GB/T 28181-2022", value: "2022" },
  { label: "GB/T 28181-2016", value: "2016" },
];

// 设备状态(由 device_state 事件驱动)。
const deviceState = ref<"Disconnected" | "Registering" | "Registered" | "InCall" | "Failed">(
  "Disconnected",
);

const stateMeta = computed(() => {
  switch (deviceState.value) {
    case "Registering": return { text: "注册中",   color: "#fa8c16", type: "warning" as const };
    case "Registered":  return { text: "已注册",   color: "#52c41a", type: "success" as const };
    case "InCall":      return { text: "推流中",   color: "#1890ff", type: "info" as const };
    case "Failed":      return { text: "注册失败",  color: "#ff4d4f", type: "error" as const };
    default:            return { text: "未连接",   color: "#c0c4cc", type: "default" as const };
  }
});

const running = computed(() => deviceState.value !== "Disconnected" && deviceState.value !== "Failed");

async function startDevice() {
  try {
    const config = {
      ...form.value,
      video_source: form.value.video_source.trim() || null,
    };
    const msg = await invoke<string>("start_device", { config });
    message.success(msg);
  } catch (e) {
    message.error(String(e));
    deviceState.value = "Failed";
  }
}

async function stopDevice() {
  try {
    await invoke<string>("stop_device");
    deviceState.value = "Disconnected";
    message.info("设备已停止");
  } catch (e) {
    message.error(String(e));
  }
}

async function fireAlarm() {
  try {
    const msg = await invoke<string>("fire_alarm", { description: "移动侦测报警" });
    message.success(msg);
  } catch (e) {
    message.error(String(e));
  }
}

// 订阅后端推送的状态变化。
let unlisten: UnlistenFn | null = null;
onMounted(async () => {
  unlisten = await listen<string>("device_state", (e) => {
    deviceState.value = e.payload as typeof deviceState.value;
  });
});
onUnmounted(() => unlisten?.());
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">单设备联调</div>
        <div class="page-sub">把本机模拟成一台国标 IPC,注册到上级平台联调点播与报警</div>
      </div>
    </div>

    <n-grid :cols="3" :x-gap="18" responsive="screen" item-responsive>
      <!-- 左:配置表单(占 2 列) -->
      <n-gi :span="2">
        <n-card title="连接配置" class="card">
          <n-form :model="form" label-placement="left" label-width="92" size="small">
            <n-divider title-placement="left" class="grp">上级平台</n-divider>
            <n-grid :cols="2" :x-gap="16">
              <n-gi>
                <n-form-item label="平台地址">
                  <n-input v-model:value="form.server_host" placeholder="WVP 主机 IP" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="SIP 端口">
                  <n-input-number v-model:value="form.server_port" :min="1" :max="65535" style="width:100%" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="SIP 域">
                  <n-input v-model:value="form.server_domain" placeholder="如 3502000000" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="传输">
                  <n-select v-model:value="form.transport" :options="transportOptions" />
                </n-form-item>
              </n-gi>
            </n-grid>

            <n-divider title-placement="left" class="grp">设备</n-divider>
            <n-grid :cols="2" :x-gap="16">
              <n-gi>
                <n-form-item label="设备 ID">
                  <n-input v-model:value="form.device_id" placeholder="20 位国标 ID" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="密码">
                  <n-input v-model:value="form.password" type="password" show-password-on="click" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="国标版本">
                  <n-select v-model:value="form.gb_version" :options="versionOptions" />
                </n-form-item>
              </n-gi>
              <n-gi>
                <n-form-item label="通道名">
                  <n-input v-model:value="form.channel_name" />
                </n-form-item>
              </n-gi>
            </n-grid>

            <n-divider title-placement="left" class="grp">媒体(可选)</n-divider>
            <n-form-item label="视频源">
              <n-input v-model:value="form.video_source"
                       placeholder="填 H.264 文件路径则平台点播可出画面;留空仅信令" />
            </n-form-item>
          </n-form>
        </n-card>
      </n-gi>

      <!-- 右:状态面板 -->
      <n-gi :span="1">
        <n-card title="设备状态" class="card status-card">
          <div class="status-light">
            <span class="dot" :style="{ background: stateMeta.color, boxShadow: `0 0 0 6px ${stateMeta.color}22` }" />
            <div class="status-text" :style="{ color: stateMeta.color }">{{ stateMeta.text }}</div>
          </div>

          <n-space vertical size="medium" style="margin-top: 20px">
            <n-button type="primary" block :disabled="running" @click="startDevice">
              注册上线
            </n-button>
            <n-button type="error" secondary block :disabled="!running" @click="stopDevice">
              注销
            </n-button>
            <n-button
              block
              :disabled="deviceState !== 'Registered' && deviceState !== 'InCall'"
              @click="fireAlarm"
            >
              上报报警
            </n-button>
          </n-space>

          <n-divider style="margin: 18px 0 12px" />
          <n-text depth="3" style="font-size:12px; line-height:1.6">
            状态变「已注册」后平台即见设备在线;配视频源后点播出画面(转「推流中」)。
          </n-text>
        </n-card>
      </n-gi>
    </n-grid>
  </div>
</template>

<style scoped>
.page { max-width: 1000px; }
.page-header { margin-bottom: 18px; }
.page-title { font-size: 20px; font-weight: 700; color: #1f2937; }
.page-sub { font-size: 12.5px; color: #8a93a3; margin-top: 5px; }
.card { border-radius: 12px; }
.grp :deep(.n-divider__title) { font-size: 12px; color: #8a93a3; font-weight: 600; }
.status-card { text-align: center; }
.status-light { padding: 18px 0 4px; }
.dot {
  display: inline-block; width: 20px; height: 20px; border-radius: 50%;
  transition: all .3s;
}
.status-text { font-size: 22px; font-weight: 700; margin-top: 14px; }
</style>
