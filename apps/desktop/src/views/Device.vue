<script setup lang="ts">
// 单设备联调控制台(UC-1):填参数 → 注册 → 实时状态灯 → 报警/点播。
// 规格见 docs/10-functional/desktop-device-console.md。
import { ref, onMounted, onUnmounted, computed } from "vue";
import {
  NCard, NForm, NFormItem, NInput, NInputNumber, NSelect,
  NButton, NSpace, NTag, NText, useMessage,
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
    case "Registering": return { text: "注册中",  type: "warning" as const };
    case "Registered":  return { text: "已注册",  type: "success" as const };
    case "InCall":      return { text: "推流中",  type: "info" as const };
    case "Failed":      return { text: "注册失败", type: "error" as const };
    default:            return { text: "未连接",  type: "default" as const };
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
  <n-space vertical size="large">
    <n-card title="单设备联调">
      <template #header-extra>
        <n-tag :type="stateMeta.type" round>{{ stateMeta.text }}</n-tag>
      </template>

      <n-form :model="form" label-placement="left" label-width="110">
        <n-form-item label="平台地址">
          <n-input v-model:value="form.server_host" placeholder="WVP 主机 IP" />
        </n-form-item>
        <n-form-item label="SIP 端口">
          <n-input-number v-model:value="form.server_port" :min="1" :max="65535" />
        </n-form-item>
        <n-form-item label="SIP 域">
          <n-input v-model:value="form.server_domain" placeholder="如 3502000000" />
        </n-form-item>
        <n-form-item label="设备 ID">
          <n-input v-model:value="form.device_id" placeholder="20 位国标 ID" />
        </n-form-item>
        <n-form-item label="密码">
          <n-input v-model:value="form.password" type="password" show-password-on="click" />
        </n-form-item>
        <n-form-item label="传输">
          <n-select v-model:value="form.transport" :options="transportOptions" />
        </n-form-item>
        <n-form-item label="国标版本">
          <n-select v-model:value="form.gb_version" :options="versionOptions" />
        </n-form-item>
        <n-form-item label="通道名">
          <n-input v-model:value="form.channel_name" />
        </n-form-item>
        <n-form-item label="视频源文件">
          <n-input v-model:value="form.video_source"
                   placeholder="可空;填 H.264 文件路径则点播可出画面" />
        </n-form-item>
      </n-form>

      <n-space style="margin-top:8px">
        <n-button type="primary" :disabled="running" @click="startDevice">注册上线</n-button>
        <n-button type="error" :disabled="!running" @click="stopDevice">注销</n-button>
        <n-button :disabled="deviceState !== 'Registered' && deviceState !== 'InCall'"
                  @click="fireAlarm">上报报警</n-button>
      </n-space>
    </n-card>

    <n-card title="说明">
      <n-text depth="3">
        填入上级平台(WVP/LiveGBS/EasyGBS)的 SIP 参数,点「注册上线」→ 状态变「已注册」后
        平台即可见设备在线。配置视频源文件后,平台点播可出画面(状态转「推流中」)。
      </n-text>
    </n-card>
  </n-space>
</template>
