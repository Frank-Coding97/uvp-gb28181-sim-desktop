<script setup lang="ts">
// 平台配置页：填写上级平台连接参数，保存到 localStorage。
import { ref, onMounted } from "vue";
import { NCard, NForm, NFormItem, NInput, NInputNumber, NButton, NSpace, NSelect, useMessage } from "naive-ui";

const message = useMessage();

// 表单模型（与 Rust DeviceConfig / LinearScenario 字段对应）。
const form = ref({
  server_host: "127.0.0.1",
  server_port: 5060,
  server_domain: "34020000002000000001",
  password: "12345678",
  transport: "UDP",
});

const transportOptions = [
  { label: "UDP（默认）", value: "UDP" },
  { label: "TCP", value: "TCP" },
];

// 持久化到 localStorage。
const STORAGE_KEY = "uvp_platform_config";

onMounted(() => {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved) {
    try { Object.assign(form.value, JSON.parse(saved)); } catch {}
  }
});

function save() {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(form.value));
  message.success("平台配置已保存");
}
</script>

<template>
  <n-card title="平台连接配置">
    <n-form :model="form" label-placement="left" label-width="100">
      <n-form-item label="平台地址">
        <n-input v-model:value="form.server_host" placeholder="如 192.168.1.100" />
      </n-form-item>
      <n-form-item label="SIP 端口">
        <n-input-number v-model:value="form.server_port" :min="1" :max="65535" />
      </n-form-item>
      <n-form-item label="平台域 ID">
        <n-input v-model:value="form.server_domain" placeholder="20位国标ID" />
      </n-form-item>
      <n-form-item label="认证密码">
        <n-input v-model:value="form.password" type="password" show-password-on="click" />
      </n-form-item>
      <n-form-item label="传输方式">
        <n-select v-model:value="form.transport" :options="transportOptions" />
      </n-form-item>
    </n-form>
    <n-space style="margin-top:16px">
      <n-button type="primary" @click="save">保存配置</n-button>
    </n-space>
  </n-card>
</template>
