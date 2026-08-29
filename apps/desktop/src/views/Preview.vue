<script setup lang="ts">
import { computed, onActivated, onDeactivated, onUnmounted, ref } from "vue";
import { NButton } from "naive-ui";
import { useDevice } from "../device";
import { usePreviewSession } from "../composables/preview/session";

const { form, deviceState } = useDevice();
const previewLatency = ref<number | null>(null);
const previewFps = ref(0);
const previewSkipped = ref(0);
const state = ref<"idle" | "starting" | "playing" | "stopped" | "error">("idle");
const error = ref("");
const canvasRef = ref<HTMLCanvasElement | null>(null);
const hasFrame = ref(false);
let session: ReturnType<typeof usePreviewSession> | null = null;

const source = computed(() => form.value.video_source.trim());
const stateText = computed(() => ({ idle: "未启动", starting: "启动中", playing: "播放中", stopped: "已停止", error: "异常" }[state.value]));
const canStart = computed(() => state.value !== "starting" && state.value !== "playing");
const sourceType = computed(() => {
  if (!source.value) return "未配置媒体源";
  if (source.value.startsWith("live:camera:")) return "电脑摄像头";
  if (source.value.startsWith("live:screen:")) return "电脑屏幕";
  return "视频文件";
});

async function start() {
  error.value = "";
  state.value = "starting";
  if (!session) session = usePreviewSession(canvasRef.value, {
    onState: (next) => { state.value = next; },
    onError: (reason) => { error.value = reason; },
    onFrame: (stats) => {
      hasFrame.value = true;
      previewLatency.value = stats.latencyMs;
      previewFps.value = stats.fps;
      previewSkipped.value = stats.skipped;
    },
  });
  await session.start();
}

async function stop() {
  await session?.stop();
  hasFrame.value = false;
  previewLatency.value = null;
}

async function retry() {
  error.value = "";
  try {
    await session?.retry();
  } catch (reason) {
    state.value = "error";
    error.value = `预览重试失败：${String(reason)}`;
  }
}

onActivated(() => { void start(); });
onDeactivated(() => { void stop(); });
onUnmounted(() => { void stop(); });
</script>

<template>
  <div class="page preview-page">
    <div class="page-header">
      <div>
        <div class="page-title">推流预览</div>
        <div class="page-sub">设备注册成功后自动预览，平台点播时复用同一批编码帧</div>
      </div>
      <div class="status-pill" :class="state"><i />{{ stateText }}</div>
    </div>

    <div class="preview-grid">
      <section class="glass-card panel preview-stage">
        <div v-show="hasFrame" class="video-wrap">
          <canvas ref="canvasRef" aria-label="推流预览画面" />
        </div>
        <div v-if="!hasFrame" class="empty-stage">
          <div class="empty-icon">◉</div>
          <strong>{{ state === 'error' ? '预览无法启动' : '等待预览画面' }}</strong>
          <span>{{ error || '选择媒体源后点击“开始预览”' }}</span>
        </div>
        <div class="stage-foot"><span>{{ sourceType }}</span><span>状态：{{ stateText }}<template v-if="previewLatency !== null"> · {{ previewFps }} FPS · {{ previewLatency }} ms · 跳 {{ previewSkipped }}</template></span></div>
      </section>

      <aside class="glass-card panel control-panel">
        <div class="panel-title">预览控制</div>
        <div class="info-row"><span>媒体源</span><code>{{ source || '未配置' }}</code></div>
        <div class="info-row"><span>设备状态</span><b>{{ deviceState === 'InCall' ? '平台推流中' : deviceState }}</b></div>
        <div class="hint">预览复用唯一 H.264 采集流，由不打开摄像头的隔离 worker 转成 MJPEG。页面切换只更换 Canvas，不会重启摄像头或平台推流。</div>
        <div class="actions">
          <n-button type="primary" :disabled="!canStart" @click="start">订阅预览</n-button>
          <n-button v-if="state === 'error'" type="warning" secondary @click="retry">重试 worker</n-button>
          <n-button :disabled="state === 'idle' || state === 'stopped'" @click="stop">停止预览</n-button>
        </div>
        <div v-if="error" class="error-box">{{ error }}</div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.preview-page { max-width: 1480px; padding-bottom: 28px; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; margin: 18px 2px 20px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.status-pill { display: inline-flex; align-items: center; gap: 8px; padding: 7px 13px; border: 1px solid var(--border-default); border-radius: 999px; color: var(--text-secondary); background: rgba(255,255,255,.58); font-size: 12px; }
.status-pill i { width: 8px; height: 8px; border-radius: 50%; background: var(--text-tertiary); }
.status-pill.playing i { background: var(--success); box-shadow: 0 0 0 4px color-mix(in srgb, var(--success) 14%, transparent); }
.status-pill.starting i { background: var(--warning); }
.status-pill.error i { background: var(--error); }
.preview-grid { display: grid; grid-template-columns: minmax(0, 1fr) 320px; gap: 18px; }
.panel { position: relative; overflow: hidden; border: 1px solid color-mix(in srgb, var(--border-default) 82%, white); box-shadow: 0 12px 34px rgba(32,51,79,.07); }
.preview-stage { min-height: 520px; padding: 16px; background: rgba(8,23,44,.86); }
.video-wrap { display: grid; place-items: center; height: 450px; overflow: hidden; border-radius: 12px; background: #071424; }
.video-wrap img, .video-wrap canvas { width: 100%; height: 100%; object-fit: contain; }
.empty-stage { display: grid; place-items: center; align-content: center; gap: 10px; height: 450px; border: 1px dashed rgba(160,190,230,.28); border-radius: 12px; color: rgba(220,232,250,.75); text-align: center; }
.empty-stage strong { color: #e6efff; font-size: 16px; }
.empty-stage span { max-width: 520px; color: rgba(220,232,250,.6); font-size: 12px; }
.empty-icon { color: #69a5ff; font-size: 42px; }
.stage-foot { display: flex; justify-content: space-between; gap: 10px; margin-top: 12px; color: rgba(220,232,250,.62); font-size: 11px; }
.control-panel { align-self: start; padding: 20px; }
.panel-title { margin-bottom: 18px; color: var(--text-primary); font-size: 16px; font-weight: 700; }
.info-row { display: grid; gap: 5px; margin-bottom: 16px; color: var(--text-tertiary); font-size: 11px; }
.info-row b, .info-row code { overflow-wrap: anywhere; color: var(--text-secondary); font-size: 12px; }
.hint { margin: 20px 0; padding: 11px 12px; border-radius: 9px; background: var(--accent-dim); color: var(--text-secondary); font-size: 11px; line-height: 1.6; }
.actions { display: flex; gap: 8px; }
.error-box { margin-top: 14px; padding: 10px 12px; border: 1px solid color-mix(in srgb, var(--error) 35%, transparent); border-radius: 9px; background: color-mix(in srgb, var(--error) 10%, transparent); color: var(--error); font-size: 11px; line-height: 1.5; }
@media (max-width: 900px) { .preview-grid { grid-template-columns: 1fr; } .preview-stage { min-height: 420px; } .video-wrap, .empty-stage { height: 350px; } }
</style>
