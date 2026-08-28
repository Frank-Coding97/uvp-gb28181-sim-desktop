<script setup lang="ts">
// 单设备联调控制台(UC-1)。本文件只做布局与生命周期编排:
//   · 视频预览 / 云台控制        → components/DevicePreview.vue、PtzConsole.vue
//   · 设备身份 + 媒体源配置      → components/DeviceIdentityPanel.vue(内含 MediaSourcePanel)
//   · 平台实时状态 / 命令时间线  → components/PlatformRuntimePanel.vue、CommandTimeline.vue
//   · SIP 信令追踪               → components/SipTracePanel.vue
// 状态与事件订阅集中在 composables/ 下的模块级单例(与 device.ts 的 store 模式一致)。
//
// 平台连接参数(server_host/port/domain/transport)来自顶栏全局平台档案,SIP 密码存 OS
// 钥匙串、只由 Rust 侧读取;注册状态/在线时长/传输模式也在顶栏,本页不重复展示。
import { onActivated, onMounted, onUnmounted } from "vue";
import CommandTimeline from "../components/CommandTimeline.vue";
import DeviceIdentityPanel from "../components/DeviceIdentityPanel.vue";
import DevicePreview from "../components/DevicePreview.vue";
import PlatformRuntimePanel from "../components/PlatformRuntimePanel.vue";
import PtzConsole from "../components/PtzConsole.vue";
import SipTracePanel from "../components/SipTracePanel.vue";
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";
import { useMediaPreview } from "../composables/useMediaPreview";
import { usePlatformRuntime } from "../composables/usePlatformRuntime";
import { useSipTrace } from "../composables/useSipTrace";

const { reconcile } = useDevice();
const { refreshLiveSources } = useLiveSources();
const { refreshVideoPreview, stopPreviewStream } = useMediaPreview();
// 命令时间线与平台实时状态由两个组件共用同一份状态,订阅生命周期放在页面统一管理,
// 避免任一组件卸载时把另一个组件的数据源一起摘掉。
const { startPlatformRuntime, stopPlatformRuntime } = usePlatformRuntime();
const { startSipTrace, stopSipTrace } = useSipTrace();

onMounted(async () => {
  await Promise.all([startPlatformRuntime(), startSipTrace()]);
  // 首次进入静默枚举采集设备(不弹提示),再拉起本地预览。
  await refreshLiveSources();
  refreshVideoPreview(true);
});

onUnmounted(() => {
  stopPlatformRuntime();
  stopSipTrace();
  stopPreviewStream();
});

// keep-alive 激活时与引擎对账(store 内统一实现),避免切页后按钮态错乱。
onActivated(reconcile);
</script>

<template>
  <div class="page">
    <div class="page-header">
      <h1 class="page-title">单设备联调</h1>
      <div class="page-sub">把本机模拟成一台国标下级设备,注册到上级平台并实时观察平台交互</div>
    </div>

    <!-- 1. 顶部:视频预览与云台控制左右各占一半 -->
    <section class="top-media-ptz-grid">
      <DevicePreview />
      <PtzConsole />
    </section>

    <!-- 2. 中间:设备身份与国标参数(全宽) -->
    <section class="device-identity-section">
      <DeviceIdentityPanel />
    </section>

    <!-- 3. 下方工作区:平台实时状态与命令时间线并排,最下方是 SIP 信令追踪 -->
    <div class="workspace">
      <div class="runtime-timeline-grid">
        <PlatformRuntimePanel />
        <CommandTimeline />
      </div>
      <SipTracePanel />
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1480px; padding-bottom: 16px; }
.page-header { padding: 0 2px; }

.top-media-ptz-grid {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 10px; align-items: stretch; margin-bottom: 10px;
}
.device-identity-section { margin-bottom: 10px; }
.workspace { display: flex; flex-direction: column; gap: 10px; margin-top: 10px; }
.runtime-timeline-grid { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr); gap: 10px; }

@media (max-width: 1080px) {
  .top-media-ptz-grid, .runtime-timeline-grid { grid-template-columns: 1fr; }
}
</style>
