<script setup lang="ts">
import { computed, onActivated, onBeforeUnmount, onDeactivated, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { NButton, NIcon } from "naive-ui";
import CameraOutline from "@vicons/ionicons5/es/CameraOutline.js";
import StopCircleOutline from "@vicons/ionicons5/es/StopCircleOutline.js";
import VideocamOutline from "@vicons/ionicons5/es/VideocamOutline.js";
import {
  CameraPreviewCancelledError,
  startLeasedCameraPreview,
  stopLeasedCameraPreview,
  type CameraLeaseClient,
  type LeasedCameraPreview,
} from "../composables/camera-demo/session";

type CaptureState = "idle" | "starting" | "active" | "error";

const videoRef = ref<HTMLVideoElement | null>(null);
const state = ref<CaptureState>("idle");
const error = ref("");
const resolution = ref("");
let leasedPreview: LeasedCameraPreview | null = null;
let pageActive = true;
let requestGeneration = 0;

const leaseClient: CameraLeaseClient = {
  async acquire() {
    const grant = await invoke<{ token: number }>("acquire_camera_demo");
    return grant.token;
  },
  async release(token) {
    await invoke("release_camera_demo", { token });
  },
};

const supported = computed(() => Boolean(navigator.mediaDevices?.getUserMedia));
const statusText = computed(() => {
  if (!supported.value) return "当前运行环境不支持摄像头采集";
  if (state.value === "starting") return "正在请求摄像头";
  if (state.value === "active") return resolution.value || "采集中";
  if (state.value === "error") return "采集失败";
  return "未启动";
});

function errorMessage(value: unknown): string {
  if (value instanceof Error && value.message === "camera request timed out") {
    return "摄像头请求超时，请检查系统权限后重试";
  }
  if (value instanceof DOMException) {
    if (value.name === "NotAllowedError") {
      return "摄像头权限未授权，请前往“系统设置 → 隐私与安全性 → 摄像头”允许 UVP 后重试";
    }
    if (value.name === "NotFoundError") return "未检测到可用摄像头";
    if (value.name === "NotReadableError") return "摄像头正被其他应用占用";
    if (value.name === "OverconstrainedError") return "摄像头不支持请求的画面参数";
    return value.message || value.name;
  }
  if (typeof value === "object" && value && "message" in value) {
    return String((value as { message: unknown }).message);
  }
  return String(value);
}

function updateResolution() {
  const video = videoRef.value;
  resolution.value = video?.videoWidth && video.videoHeight
    ? `${video.videoWidth} × ${video.videoHeight}`
    : "采集中";
}

function handleTrackEnded() {
  if (state.value !== "active") return;
  void stopCapture().then(() => {
    error.value = "摄像头连接已断开";
    state.value = "error";
  });
}

async function startCapture() {
  const video = videoRef.value;
  if (!video || !supported.value || state.value === "starting") return;
  await stopCapture();
  const request = ++requestGeneration;
  state.value = "starting";
  error.value = "";
  try {
    leasedPreview = await startLeasedCameraPreview(
      video,
      navigator.mediaDevices,
      leaseClient,
      10_000,
      () => pageActive && request === requestGeneration,
    );
    leasedPreview.stream
      .getVideoTracks()[0]
      ?.addEventListener("ended", handleTrackEnded, { once: true });
    updateResolution();
    state.value = "active";
  } catch (value) {
    leasedPreview = null;
    if (value instanceof CameraPreviewCancelledError) return;
    state.value = "error";
    error.value = errorMessage(value);
  }
}

async function stopCapture() {
  requestGeneration += 1;
  const current = leasedPreview;
  leasedPreview = null;
  await stopLeasedCameraPreview(videoRef.value, current, leaseClient);
  resolution.value = "";
  if (state.value !== "idle") state.value = "idle";
}

onActivated(() => { pageActive = true; });
onDeactivated(() => { pageActive = false; void stopCapture(); });
onBeforeUnmount(() => { pageActive = false; void stopCapture(); });
</script>

<template>
  <div class="camera-demo-page">
    <header class="page-header">
      <div>
        <h1>视频采集demo</h1>
        <div class="capture-status" :class="state">
          <span class="status-dot" />
          <span>{{ statusText }}</span>
        </div>
      </div>
      <div class="actions">
        <n-button
          type="primary"
          :disabled="!supported || state === 'starting' || state === 'active'"
          :loading="state === 'starting'"
          @click="startCapture"
        >
          <template #icon><n-icon :component="CameraOutline" /></template>
          开始采集
        </n-button>
        <n-button :disabled="state !== 'active'" @click="stopCapture">
          <template #icon><n-icon :component="StopCircleOutline" /></template>
          停止采集
        </n-button>
      </div>
    </header>

    <section class="preview-surface" :class="{ active: state === 'active' }">
      <video
        ref="videoRef"
        autoplay
        muted
        playsinline
        @loadedmetadata="updateResolution"
      />
      <div v-if="state !== 'active'" class="empty-state">
        <n-icon :component="VideocamOutline" />
        <span>{{ state === 'starting' ? "正在启动" : "等待采集" }}</span>
      </div>
    </section>

    <div v-if="error" class="error-message" role="alert">{{ error }}</div>
  </div>
</template>

<style scoped>
.camera-demo-page {
  width: min(1120px, 100%);
  margin: 0 auto;
  padding: 24px;
}

.page-header {
  display: flex;
  align-items: flex-end;
  justify-content: space-between;
  gap: 20px;
  margin-bottom: 18px;
}

h1 {
  margin: 0 0 8px;
  color: var(--text-primary);
  font-size: 24px;
  font-weight: 700;
  letter-spacing: 0;
}

.capture-status {
  display: flex;
  align-items: center;
  gap: 7px;
  min-height: 20px;
  color: var(--text-secondary);
  font-size: 13px;
}

.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--text-tertiary);
}

.capture-status.starting .status-dot { background: var(--warning); }
.capture-status.active .status-dot { background: var(--success); }
.capture-status.error .status-dot { background: var(--error); }

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
}

.preview-surface {
  position: relative;
  width: 100%;
  aspect-ratio: 16 / 9;
  overflow: hidden;
  border: 1px solid rgba(120, 160, 210, 0.24);
  border-radius: 8px;
  background: #101722;
  box-shadow: var(--shadow-card);
}

video {
  display: block;
  width: 100%;
  height: 100%;
  object-fit: contain;
  background: #101722;
}

.empty-state {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 12px;
  color: #9cabbd;
  background: #101722;
}

.empty-state :deep(svg) {
  width: 44px;
  height: 44px;
}

.error-message {
  margin-top: 12px;
  padding: 10px 12px;
  border: 1px solid rgba(229, 72, 92, 0.25);
  border-radius: 6px;
  color: #b92d42;
  background: rgba(229, 72, 92, 0.08);
  font-size: 13px;
}

@media (max-width: 720px) {
  .camera-demo-page { padding: 18px; }
  .page-header { align-items: flex-start; flex-direction: column; }
  .actions { width: 100%; }
}
</style>
