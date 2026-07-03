<script setup lang="ts">
// 压测场景编排页：生成 TOML 场景内容并启动/停止压测（FR-41）。
import { ref } from "vue";
import {
  NCard, NForm, NFormItem, NInput, NInputNumber,
  NButton, NSpace, NSelect, NText, useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { useRouter } from "vue-router";

const message = useMessage();
const router  = useRouter();

const STORAGE_KEY = "uvp_platform_config";

// 场景参数。
const form = ref({
  base_device_id:       "34020000001320000001",
  count:                10,
  heartbeat_interval:   60,
  channels_per_device:  1,
  media_profile:        "A",
  ramp_per_second:      50,
  active_ratio:         100,
});

const mediaOptions = [
  { label: "A - 空媒体（仅信令）",   value: "A" },
  { label: "B - 轻量 RTP（伪流）",  value: "B" },
  { label: "C - 真实 H.264 文件",  value: "C" },
];

const running    = ref(false);
const statusText = ref("就绪");

// 读取平台配置（Config.vue 存的）。
function loadPlatformConfig() {
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw ? JSON.parse(raw) : {
    server_host:   "127.0.0.1",
    server_port:   5060,
    server_domain: "34020000002000000001",
    password:      "12345678",
    transport:     "UDP",
  };
}

// 从表单生成 TOML 字符串。
function buildToml(): string {
  const p = loadPlatformConfig();
  return [
    `base_device_id = "${form.value.base_device_id}"`,
    `password = "${p.password}"`,
    `server_host = "${p.server_host}"`,
    `server_port = ${p.server_port}`,
    `server_domain = "${p.server_domain}"`,
    `transport = "${p.transport}"`,
    `heartbeat_interval_secs = ${form.value.heartbeat_interval}`,
    `channels_per_device = ${form.value.channels_per_device}`,
    `media_profile = "${form.value.media_profile}"`,
    `ramp_per_second = ${form.value.ramp_per_second}`,
    `video_fps = 25`,
    `[device_info]`,
    `device_name = "UVP-Sim"`,
    `manufacturer = "UVP"`,
    `model = "Desktop-Sim"`,
    `firmware = "0.1.0"`,
  ].join("\n");
}

async function startStress() {
  try {
    const toml = buildToml();
    const msg = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
    });
    running.value    = true;
    statusText.value = msg;
    message.success(msg);
    // 跳到监控页。
    router.push("/monitor");
  } catch (e) {
    message.error(String(e));
  }
}

async function stopStress() {
  try {
    const msg = await invoke<string>("stop_stress");
    running.value    = false;
    statusText.value = "已停止";
    message.info(msg);
  } catch (e) {
    message.error(String(e));
  }
}
</script>

<template>
  <n-space vertical size="large">
    <n-card title="压测场景编排">
      <n-form :model="form" label-placement="left" label-width="120">
        <n-form-item label="起始设备 ID">
          <n-input v-model:value="form.base_device_id" placeholder="20位国标设备ID" />
        </n-form-item>
        <n-form-item label="设备数量">
          <n-input-number v-model:value="form.count" :min="1" :max="10000" />
        </n-form-item>
        <n-form-item label="心跳间隔(秒)">
          <n-input-number v-model:value="form.heartbeat_interval" :min="10" :max="600" />
        </n-form-item>
        <n-form-item label="每设备通道数">
          <n-input-number v-model:value="form.channels_per_device" :min="1" :max="8" />
        </n-form-item>
        <n-form-item label="爬坡(台/秒)">
          <n-input-number v-model:value="form.ramp_per_second" :min="0" :max="1000" />
        </n-form-item>
        <n-form-item label="媒体档">
          <n-select v-model:value="form.media_profile" :options="mediaOptions" />
        </n-form-item>
      </n-form>

      <n-space style="margin-top:16px">
        <n-button type="primary" :disabled="running" @click="startStress">
          启动压测
        </n-button>
        <n-button type="error" :disabled="!running" @click="stopStress">
          停止压测
        </n-button>
      </n-space>

      <n-text depth="3" style="display:block; margin-top:12px;">
        状态：{{ statusText }}
      </n-text>
    </n-card>
  </n-space>
</template>
