<script setup lang="ts">
// 仪表盘:欢迎区 + 引擎状态 + 功能入口卡片(突出压测差异化能力)。
import { ref, onMounted, h } from "vue";
import { useRouter } from "vue-router";
import {
  NCard, NSpace, NText, NGrid, NGi, NIcon, NTag,
} from "naive-ui";
import {
  HardwareChipOutline, LayersOutline, ServerOutline, PulseOutline,
  FlashOutline, CheckmarkCircle, CloseCircle,
} from "@vicons/ionicons5";
import { invoke } from "@tauri-apps/api/core";

const router = useRouter();
const engineInfo = ref("检查中…");
const online = ref(false);
const loading = ref(true);

onMounted(async () => {
  try {
    engineInfo.value = await invoke<string>("engine_version");
    online.value = true;
  } catch {
    engineInfo.value = "引擎离线";
    online.value = false;
  } finally {
    loading.value = false;
  }
});

// 功能入口(突出单设备联调 + 压测两大主线)。
const features = [
  {
    key: "/device", icon: HardwareChipOutline, color: "#1890ff",
    title: "单设备联调", desc: "把电脑模拟成一台国标 IPC,注册到上级平台,联调点播/报警",
  },
  {
    key: "/scenario", icon: LayersOutline, color: "#722ed1",
    title: "压力测试", desc: "海量虚拟设备并发注册/心跳/推流,量化平台承载(差异化能力)",
    highlight: true,
  },
  {
    key: "/config", icon: ServerOutline, color: "#13c2c2",
    title: "平台配置", desc: "配置上级平台(WVP/LiveGBS/EasyGBS)连接参数",
  },
  {
    key: "/monitor", icon: PulseOutline, color: "#52c41a",
    title: "运行监控", desc: "实时曲线:注册成功率、心跳、推流带宽、失败归因",
  },
];

function renderIcon(comp: any, color: string) {
  return h(NIcon, { size: 26, color }, { default: () => h(comp) });
}
</script>

<template>
  <div class="page">
    <!-- 欢迎区 -->
    <div class="hero">
      <div>
        <div class="hero-title">UVP GB28181 桌面端</div>
        <div class="hero-sub">
          多系统国标设备模拟器 + 压力测试工具 · 无需真实摄像头即可联调上级平台
        </div>
      </div>
      <n-tag v-if="!loading" :type="online ? 'success' : 'error'" round size="large">
        <template #icon>
          <n-icon :component="online ? CheckmarkCircle : CloseCircle" />
        </template>
        {{ online ? "引擎就绪" : "引擎离线" }}
      </n-tag>
    </div>

    <!-- 引擎状态条 -->
    <n-card size="small" :bordered="false" class="status-bar">
      <n-space align="center" :size="10">
        <n-icon :component="FlashOutline" :color="online ? '#52c41a' : '#bbb'" size="18" />
        <n-text depth="2">{{ engineInfo }}</n-text>
      </n-space>
    </n-card>

    <!-- 功能入口卡片 -->
    <div class="section-title">功能入口</div>
    <n-grid :cols="2" :x-gap="16" :y-gap="16" responsive="screen">
      <n-gi v-for="f in features" :key="f.key">
        <n-card
          hoverable
          class="feature-card"
          :class="{ highlight: f.highlight }"
          @click="router.push(f.key)"
        >
          <div class="feature-body">
            <div class="feature-icon" :style="{ background: f.color + '18' }">
              <component :is="() => renderIcon(f.icon, f.color)" />
            </div>
            <div class="feature-text">
              <div class="feature-name">
                {{ f.title }}
                <n-tag v-if="f.highlight" type="warning" size="small" round>核心</n-tag>
              </div>
              <div class="feature-desc">{{ f.desc }}</div>
            </div>
          </div>
        </n-card>
      </n-gi>
    </n-grid>
  </div>
</template>

<style scoped>
.page { max-width: 960px; }
.hero {
  display: flex; justify-content: space-between; align-items: flex-start;
  margin-bottom: 18px;
}
.hero-title { font-size: 24px; font-weight: 700; color: #1f2937; }
.hero-sub { font-size: 13px; color: #8a93a3; margin-top: 6px; max-width: 620px; line-height: 1.6; }
.status-bar { background: #fff; border-radius: 10px; margin-bottom: 24px; }
.section-title {
  font-size: 14px; font-weight: 600; color: #4b5563; margin: 0 0 14px 2px;
}
.feature-card { border-radius: 12px; cursor: pointer; transition: all .2s; }
.feature-card.highlight { border: 1px solid #ffd591; background: linear-gradient(180deg,#fffdf7,#fff); }
.feature-body { display: flex; gap: 14px; align-items: center; }
.feature-icon {
  width: 52px; height: 52px; border-radius: 12px;
  display: flex; align-items: center; justify-content: center; flex-shrink: 0;
}
.feature-name {
  font-size: 16px; font-weight: 600; color: #1f2937;
  display: flex; align-items: center; gap: 8px;
}
.feature-desc { font-size: 12.5px; color: #8a93a3; margin-top: 5px; line-height: 1.55; }
</style>
