<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { NButton, useMessage } from "naive-ui";
import { useDevice } from "../device";
import { usePreviewSession } from "../composables/preview/session";

const message = useMessage();
const { form, deviceState } = useDevice();
const frameUrl = ref("");
const previewLatency = ref<number | null>(null);
const previewFps = ref(0);
const previewSequence = ref(0);
let fpsWindowStart = 0;
let fpsWindowFrames = 0;
const state = ref<"idle" | "starting" | "playing" | "stopped" | "error">("idle");
const error = ref("");
const canvasRef = ref<HTMLCanvasElement | null>(null);
const binarySessionActive = ref(false);
let binarySession: ReturnType<typeof usePreviewSession> | null = null;
let unlistenFrame: UnlistenFn | null = null;
let unlistenState: UnlistenFn | null = null;
let unlistenError: UnlistenFn | null = null;

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
  if (!binarySession) binarySession = usePreviewSession(canvasRef.value);
  if (binarySession && await binarySession.start()) {
    binarySessionActive.value = true;
    return;
  }
  try {
    message.success(await invoke<string>("start_preview", { source: source.value }));
  } catch (e) {
    state.value = "error";
    error.value = String(e);
  }
}

async function stop() {
  try {
    await binarySession?.stop();
    binarySessionActive.value = false;
    await invoke<string>("stop_preview");
    state.value = "stopped";
    frameUrl.value = "";
    previewLatency.value = null;
  } catch (e) {
    error.value = String(e);
    state.value = "error";
  }
}

onMounted(async () => {
  unlistenFrame = await listen<{ data: string; captured_at_ms: number; sequence?: number }>("preview_frame", (event) => {
    frameUrl.value = `data:image/jpeg;base64,${event.payload.data}`;
    previewLatency.value = Math.max(0, Date.now() - event.payload.captured_at_ms);
    previewSequence.value = event.payload.sequence ?? previewSequence.value + 1;
    const now = performance.now();
    if (!fpsWindowStart) fpsWindowStart = now;
    fpsWindowFrames += 1;
    if (now - fpsWindowStart >= 1000) {
      previewFps.value = Math.round(fpsWindowFrames * 1000 / (now - fpsWindowStart));
      fpsWindowFrames = 0;
      fpsWindowStart = now;
    }
    state.value = "playing";
  });
  unlistenState = await listen<string>("preview_state", (event) => {
    if (event.payload === "starting" || event.payload === "playing" || event.payload === "stopped") {
      state.value = event.payload;
    }
  });
  await start();
  unlistenError = await listen<string>("preview_error", (event) => {
    error.value = event.payload;
    state.value = "error";
  });
});
onUnmounted(() => { unlistenFrame?.(); unlistenState?.(); unlistenError?.(); void binarySession?.stop(); void invoke("stop_preview"); });
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
        <div v-if="frameUrl || binarySessionActive" class="video-wrap">
          <canvas v-show="binarySessionActive" ref="canvasRef" aria-label="推流预览画面" />
          <img v-show="!binarySessionActive" :src="frameUrl" alt="推流预览画面" />
        </div>
        <div v-else class="empty-stage">
          <div class="empty-icon">◉</div>
          <strong>{{ state === 'error' ? '预览无法启动' : '等待预览画面' }}</strong>
          <span>{{ error || '选择媒体源后点击“开始预览”' }}</span>
        </div>
        <div class="stage-foot"><span>{{ sourceType }}</span><span>状态：{{ stateText }}<template v-if="previewLatency !== null"> · {{ previewFps }} FPS · 同帧处理 {{ previewLatency }} ms</template></span></div>
      </section>

      <aside class="glass-card panel control-panel">
        <div class="panel-title">预览控制</div>
        <div class="info-row"><span>媒体源</span><code>{{ source || '未配置' }}</code></div>
        <div class="info-row"><span>设备状态</span><b>{{ deviceState === 'InCall' ? '平台推流中' : deviceState }}</b></div>
        <div class="hint">注册成功后采集源会持续运行，本地预览直接订阅这一路编码帧；平台点播时复用同一批帧，不会再次打开摄像头或屏幕。</div>
        <div class="actions">
          <n-button type="primary" :disabled="!canStart" @click="start">订阅预览</n-button>
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
