<script setup lang="ts">
// 仪表盘：显示引擎状态 + 快速跳转入口。
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { NCard, NSpace, NButton, NText, NGrid, NGi } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";

const router = useRouter();
const engineInfo = ref("检查中…");
const loading = ref(true);

onMounted(async () => {
  try {
    engineInfo.value = await invoke<string>("engine_version");
  } catch {
    engineInfo.value = "引擎离线";
  } finally {
    loading.value = false;
  }
});
</script>

<template>
  <n-space vertical size="large">
    <n-card title="引擎状态">
      <n-text :type="loading ? 'default' : 'success'">{{ engineInfo }}</n-text>
    </n-card>

    <n-card title="快速操作">
      <n-grid :cols="2" :x-gap="12" :y-gap="12">
        <n-gi>
          <n-button block type="primary" @click="router.push('/device')">
            单设备联调
          </n-button>
        </n-gi>
        <n-gi>
          <n-button block @click="router.push('/scenario')">
            压测编排
          </n-button>
        </n-gi>
        <n-gi>
          <n-button block @click="router.push('/monitor')">
            查看监控
          </n-button>
        </n-gi>
      </n-grid>
    </n-card>
  </n-space>
</template>
