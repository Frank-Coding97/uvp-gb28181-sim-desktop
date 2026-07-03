<script setup lang="ts">
// 仪表盘:欢迎区 + 引擎状态 + 功能入口卡片(frost-blue 风,突出压测)。
import { ref, onMounted } from "vue";
import { useRouter } from "vue-router";
import { NIcon } from "naive-ui";
import {
  HardwareChipOutline, LayersOutline, ServerOutline, PulseOutline,
} from "@vicons/ionicons5";
import { invoke } from "@tauri-apps/api/core";

const router = useRouter();
const engineInfo = ref("检查中…");
const online = ref(false);

onMounted(async () => {
  try {
    engineInfo.value = await invoke<string>("engine_version");
    online.value = true;
  } catch {
    engineInfo.value = "引擎离线";
  }
});

const features = [
  { key: "/device", icon: HardwareChipOutline, color: "#3884ff",
    title: "单设备联调", desc: "模拟一台国标 IPC,注册到上级平台,联调点播/报警/PTZ" },
  { key: "/scenario", icon: LayersOutline, color: "#7a6cff",
    title: "压力测试", desc: "海量虚拟设备并发注册/心跳/推流,量化平台承载", highlight: true },
  { key: "/config", icon: ServerOutline, color: "#00b9d6",
    title: "平台配置", desc: "配置上级平台(WVP/LiveGBS/EasyGBS)连接参数" },
  { key: "/monitor", icon: PulseOutline, color: "#08b388",
    title: "运行监控", desc: "实时曲线:注册成功率、心跳、推流带宽、失败归因" },
];
</script>

<template>
  <div class="page">
    <div class="hero">
      <div class="hero-title">UVP GB28181 桌面端</div>
      <div class="hero-sub">多系统国标设备模拟器 + 压力测试工具 · 无需真实摄像头即可联调上级平台</div>
    </div>

    <div class="glass-card status" :class="{ off: !online }">
      <span class="sdot" :style="{ background: online ? 'var(--success)' : 'var(--text-tertiary)' }" />
      <span>{{ engineInfo }}</span>
    </div>

    <div class="section-title">功能入口</div>
    <div class="feature-grid">
      <div v-for="f in features" :key="f.key"
           class="glass-card feature" :class="{ highlight: f.highlight }"
           @click="router.push(f.key)">
        <div class="f-icon" :style="{ background: f.color + '1a', color: f.color }">
          <n-icon :size="26"><component :is="f.icon" /></n-icon>
        </div>
        <div class="f-body">
          <div class="f-name">
            {{ f.title }}
            <span v-if="f.highlight" class="tag-core">核心</span>
          </div>
          <div class="f-desc">{{ f.desc }}</div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 960px; }
.hero { margin-bottom: 18px; }
.hero-title { font-size: 26px; font-weight: 700; color: var(--text-primary); }
.hero-sub { font-size: 13px; color: var(--text-tertiary); margin-top: 8px; line-height: 1.6; }
.status {
  display: flex; align-items: center; gap: 10px; padding: 14px 18px;
  font-size: 13px; color: var(--text-secondary); margin-bottom: 26px;
}
.sdot { width: 9px; height: 9px; border-radius: 50%; }
.section-title { font-size: 14px; font-weight: 600; color: var(--text-secondary); margin: 0 0 14px 2px; }
.feature-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }
.feature {
  display: flex; gap: 14px; align-items: center; padding: 20px;
  cursor: pointer; transition: transform var(--transition), box-shadow var(--transition);
}
.feature:hover { transform: translateY(-2px); box-shadow: var(--shadow-elevated); }
.feature.highlight { border-color: rgba(122, 108, 255, 0.4); }
.f-icon {
  width: 52px; height: 52px; border-radius: 13px; flex-shrink: 0;
  display: flex; align-items: center; justify-content: center;
}
.f-name {
  font-size: 16px; font-weight: 600; color: var(--text-primary);
  display: flex; align-items: center; gap: 8px;
}
.tag-core {
  font-size: 11px; font-weight: 600; color: #fff; background: var(--violet);
  padding: 1px 8px; border-radius: 999px;
}
.f-desc { font-size: 12.5px; color: var(--text-tertiary); margin-top: 5px; line-height: 1.55; }
</style>
