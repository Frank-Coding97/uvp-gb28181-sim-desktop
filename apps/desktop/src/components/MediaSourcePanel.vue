<script setup lang="ts">
// 媒体源配置区:媒体模式选择 + 摄像头/屏幕刷新测试 + 三种源(文件/摄像头/屏幕)的参数面板 + 采集自检结果。
// 状态集中在 useLiveSources,这里只负责渲染与提示。
import { useMessage } from "naive-ui";
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";
import { useMediaPreview } from "../composables/useMediaPreview";
import MediaAudioFields from "./MediaAudioFields.vue";
import MediaVideoFields from "./MediaVideoFields.vue";

const message = useMessage();
const { form, startDisabled } = useDevice();
const {
  mediaMode,
  liveCatalog,
  sourceLoading,
  probeState,
  selectedCameraIndex,
  selectedMicrophone,
  selectedCameraAudioCodec,
  selectedDisplayId,
  shareTargetPinned,
  shareTargetText,
  sharePickerBusy,
  selectedScreenAudio,
  selectedScreenAudioCodec,
  screenProfileValidationError,
  canProbeSource,
  syncLiveSourceUri,
  pickShareTarget,
  useDisplayTarget,
  setMediaMode,
  pickVideoSource,
  refreshLiveSources,
  probeLiveSource,
} = useLiveSources();
const { refreshVideoPreview } = useMediaPreview();

async function onPickVideoSource() {
  const picked = await pickVideoSource();
  if (!picked.ok) {
    message.error(picked.error);
    return;
  }
  if (picked.path) refreshVideoPreview(true);
}

async function onRefreshLiveSources() {
  const result = await refreshLiveSources();
  if (result.ok) message.success("采集设备列表已刷新");
  else message.error(result.error ?? "设备枚举失败");
}

async function onPickShareTarget() {
  const result = await pickShareTarget();
  if (!result.ok) {
    message.error(result.error ?? "系统选择器失败");
    return;
  }
  if (result.label) {
    message.success(`采集目标已设为「${result.label}」，重启设备后生效`);
  } else {
    message.info("已取消选择，采集目标未改变");
  }
}

function onUseDisplayTarget() {
  useDisplayTarget();
  message.info("采集目标已切回整块显示器");
}

async function onProbeLiveSource() {
  const loadingMsg = message.loading("正在探测硬件采集通道与编码器…", { duration: 0 });
  try {
    const outcome = await probeLiveSource();
    if (!outcome) return;
    if (outcome.ready) {
      message.success(outcome.message);
      refreshVideoPreview(true); // 自检通过后联动刷新画面预览
    } else {
      message.warning(outcome.message);
    }
  } finally {
    loadingMsg.destroy();
  }
}
</script>

<template>
  <!-- 媒体源类型与操作行:下拉框 + 刷新/测试操作 -->
  <div class="media-control-bar">
    <div class="media-select-wrap">
      <label for="media-mode-select">媒体源类型</label>
      <select
        id="media-mode-select"
        v-model="mediaMode"
        :disabled="startDisabled"
        class="inp media-mode-select"
        @change="setMediaMode(mediaMode)"
      >
        <option value="file">视频文件（循环推送本地视频）</option>
        <option value="screen">电脑屏幕实时画面（ScreenCaptureKit 原生采集）</option>
        <option value="camera">电脑摄像头（含 iPhone 连续互通）</option>
        <option value="none">无媒体（仅信令联调）</option>
      </select>
    </div>
    <div class="media-actions-wrap">
      <button
        v-if="mediaMode === 'camera' || mediaMode === 'screen'"
        type="button"
        class="file-btn compact"
        :disabled="startDisabled || sourceLoading"
        @click="onRefreshLiveSources"
      >
        {{ sourceLoading ? '刷新中…' : '刷新设备' }}
      </button>
      <button
        v-if="mediaMode === 'camera' || mediaMode === 'screen'"
        type="button"
        class="file-btn compact primary"
        :disabled="!canProbeSource || probeState?.kind === 'loading'"
        @click="onProbeLiveSource"
      >
        {{ probeState?.kind === 'loading' ? '测试中…' : '测试采集' }}
      </button>
    </div>
  </div>

  <!-- 1. 视频文件模式:仅显示文件选择行,音视频参数隐藏 -->
  <div v-if="mediaMode === 'file'" class="source-config file-config-box">
    <div class="file-row">
      <input
        v-model="form.video_source"
        :disabled="startDisabled"
        class="inp"
        aria-label="视频文件路径"
        placeholder="选择 H.264/H.265 裸流或 MP4、MOV 等视频文件"
      />
      <button type="button" class="file-btn" :disabled="startDisabled" @click="onPickVideoSource">
        选择文件
      </button>
    </div>
    <div class="fg-hint">容器文件会在设备启动前转封装；有音频轨时自动转为 G.711A 后与视频复用。</div>
  </div>

  <!-- 2. 摄像头模式 -->
  <div v-else-if="mediaMode === 'camera'" class="source-config">
    <div class="source-capability">
      <span class="cap-badge" :class="liveCatalog.ffmpeg_available ? 'ok' : 'warn'">
        FFmpeg {{ liveCatalog.ffmpeg_available ? '可用' : '未找到' }}
      </span>
      <span>AVFoundation 摄像头与音频采集</span>
    </div>
    <div class="media-param-columns">
      <MediaVideoFields title="摄像头视频参数" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="camera-device-select">视频采集设备</label>
            <select
              id="camera-device-select"
              v-model.number="selectedCameraIndex"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option v-if="!liveCatalog.cameras.length && selectedCameraIndex === null" :value="null" disabled>
                未发现摄像头
              </option>
              <option
                v-if="selectedCameraIndex !== null && !liveCatalog.cameras.some(item => item.index === selectedCameraIndex)"
                :value="selectedCameraIndex"
                disabled
              >
                [{{ selectedCameraIndex }}] 当前摄像头不可用
              </option>
              <option v-for="camera in liveCatalog.cameras" :key="camera.index" :value="camera.index">
                [{{ camera.index }}] {{ camera.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaVideoFields>

      <MediaAudioFields v-model:codec="selectedCameraAudioCodec" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="camera-mic-select">音频采集设备</label>
            <select
              id="camera-mic-select"
              v-model="selectedMicrophone"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option value="none">不采集音频（画面将没有声音）</option>
              <option
                v-if="selectedMicrophone !== 'none' && !liveCatalog.microphones.some(item => String(item.index) === selectedMicrophone)"
                :value="selectedMicrophone"
                disabled
              >
                [{{ selectedMicrophone }}] 当前音频设备不可用
              </option>
              <option v-for="mic in liveCatalog.microphones" :key="mic.index" :value="String(mic.index)">
                [{{ mic.index }}] {{ mic.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaAudioFields>
    </div>

    <div v-if="screenProfileValidationError" class="source-notice error">
      {{ screenProfileValidationError }}
    </div>
    <div v-if="liveCatalog.avfoundation_error" class="source-notice error">
      {{ liveCatalog.avfoundation_error }}
    </div>
  </div>

  <!-- 3. 屏幕录制模式 -->
  <div v-else-if="mediaMode === 'screen'" class="source-config">
    <div class="source-capability">
      <span class="cap-badge ok">ScreenCaptureKit</span>
      <span>原生屏幕与系统音频采集</span>
    </div>
    <div class="capture-target">
      <div class="capture-target-head">
        <span class="capture-target-label">采集目标</span>
        <span class="capture-target-value" :class="{ pinned: shareTargetPinned }">
          {{ shareTargetText }}
        </span>
      </div>
      <p class="capture-target-hint">
        用 macOS 系统选择器挑「显示器 / 窗口 / 应用」——选中的是什么,推给平台的就是什么。
        注意:右下角本地预览走的是 WebView 自己的屏幕共享授权,与这里的目标无关。
      </p>
      <div class="capture-target-actions">
        <button
          type="button"
          class="btn btn-ghost"
          :disabled="startDisabled || sourceLoading || sharePickerBusy"
          @click="onPickShareTarget"
        >
          {{ sharePickerBusy ? "等待系统选择器…" : "用系统选择器选择窗口 / 应用…" }}
        </button>
        <button
          type="button"
          class="btn btn-ghost"
          :disabled="startDisabled || sourceLoading || !shareTargetPinned"
          @click="onUseDisplayTarget"
        >
          改为整屏(显示器)
        </button>
      </div>
    </div>
    <div class="media-param-columns">
      <MediaVideoFields title="屏幕画面参数" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="screen-device-select">显示器选择</label>
            <select
              id="screen-device-select"
              v-model.number="selectedDisplayId"
              :disabled="startDisabled || sourceLoading || shareTargetPinned"
              class="inp"
              @change="onUseDisplayTarget"
            >
              <option v-if="!liveCatalog.screens.length && selectedDisplayId === null" :value="null" disabled>
                未发现可用显示器
              </option>
              <option
                v-if="selectedDisplayId !== null && !liveCatalog.screens.some(item => item.display_id === selectedDisplayId)"
                :value="selectedDisplayId"
                disabled
              >
                Display {{ selectedDisplayId }}（当前不可用）
              </option>
              <option v-for="screen in liveCatalog.screens" :key="screen.display_id" :value="screen.display_id">
                {{ screen.name }}
              </option>
            </select>
          </div>
        </template>
      </MediaVideoFields>

      <MediaAudioFields v-model:codec="selectedScreenAudioCodec" :disabled="startDisabled || sourceLoading">
        <template #device>
          <div class="fg">
            <label for="screen-audio-select">声音采集源</label>
            <select
              id="screen-audio-select"
              v-model="selectedScreenAudio"
              :disabled="startDisabled || sourceLoading"
              class="inp"
              @change="syncLiveSourceUri"
            >
              <option value="system">系统播放声音</option>
              <option
                v-if="selectedScreenAudio.startsWith('microphone:') && !liveCatalog.microphones.some(item => `microphone:${item.index}` === selectedScreenAudio)"
                :value="selectedScreenAudio"
                disabled
              >
                麦克风 {{ selectedScreenAudio.slice('microphone:'.length) }}（当前不可用）
              </option>
              <option
                v-for="mic in liveCatalog.microphones"
                :key="`screen-mic-${mic.index}`"
                :value="`microphone:${mic.index}`"
              >
                麦克风 · [{{ mic.index }}] {{ mic.name }}
              </option>
              <option value="none">不采集声音</option>
            </select>
          </div>
        </template>
      </MediaAudioFields>
    </div>

    <div v-if="screenProfileValidationError" class="source-notice error">
      {{ screenProfileValidationError }}
    </div>
    <div v-if="liveCatalog.screen_error" class="source-notice error">
      {{ liveCatalog.screen_error }}
      <span>请在“系统设置 → 隐私与安全性 → 屏幕与系统音频录制”中允许本应用。</span>
    </div>
  </div>

  <div v-if="probeState" class="probe-result" :class="probeState.kind">{{ probeState.message }}</div>
</template>

<style scoped>
.media-control-bar {
  display: flex; align-items: flex-end; justify-content: space-between;
  gap: 10px; margin-top: 8px; padding-top: 8px;
  border-top: 1px solid var(--border-default);
}
.media-select-wrap { display: flex; flex-direction: column; gap: 2px; flex: 1 1 260px; max-width: 360px; }
.media-select-wrap label { font-size: var(--fs-sm); color: var(--text-secondary); font-weight: 600; }
.media-mode-select { font-weight: 600; color: var(--text-primary); }
.media-actions-wrap { display: flex; align-items: center; gap: 6px; flex-wrap: wrap; flex: 0 0 auto; }
.file-config-box { margin-top: 6px; }

.source-config {
  margin-top: 6px; padding: 7px 8px; border-radius: var(--radius-sm);
  border: 1px solid var(--border-default); background: var(--bg-card);
}
.media-param-columns { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; margin-top: 4px; }
.capture-target {
  margin-bottom: 6px; padding: 6px 8px; border-radius: var(--radius-sm);
  border: 1px solid var(--border-default); background: var(--bg-elevated, var(--bg-card));
}
.capture-target-head { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
.capture-target-label { font-size: var(--fs-sm); color: var(--text-secondary); font-weight: 600; }
.capture-target-value { font-size: var(--fs-sm); color: var(--text-primary); font-weight: 600; }
.capture-target-value.pinned { color: var(--accent); }
.capture-target-hint { margin: 4px 0 6px; font-size: var(--fs-xs); line-height: 1.35; color: var(--text-secondary); }
.capture-target-actions { display: flex; gap: 6px; flex-wrap: wrap; }
.source-capability {
  display: flex; align-items: center; gap: 6px; margin-bottom: 5px;
  color: var(--text-secondary); font-size: var(--fs-xs);
}
.cap-badge { padding: 1px 5px; border-radius: 6px; font-size: var(--fs-xs); font-weight: 650; }
.cap-badge.ok { color: var(--success); background: color-mix(in srgb, var(--success) 12%, transparent); }
.cap-badge.warn { color: var(--warning); background: color-mix(in srgb, var(--warning) 12%, transparent); }
.file-row { display: flex; gap: 6px; align-items: stretch; }
.file-row .inp { flex: 1 1 auto; }
.probe-result { margin-top: 5px; padding: 4px 6px; border-radius: 4px; font-size: var(--fs-xs); line-height: 1.3; }
.probe-result.loading { color: var(--accent); background: var(--accent-dim); }
.probe-result.success { color: var(--success); background: color-mix(in srgb, var(--success) 10%, transparent); }
.probe-result.warning { color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); }
.probe-result.error { color: var(--error); background: color-mix(in srgb, var(--error) 9%, transparent); }

@media (max-width: 760px) {
  .media-param-columns { grid-template-columns: 1fr; }
}
</style>
