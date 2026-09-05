<script setup lang="ts">
// 本地媒体预览卡片:文件/摄像头/屏幕三种源的画面回显 + 适应/铺满切换。
// 只呈现本地回显,推给平台的仍是后端 PS/RTP 流(状态行有明确说明)。
import { useMessage } from "naive-ui";
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";
import { useMediaPreview } from "../composables/useMediaPreview";

const message = useMessage();
const { startDisabled } = useDevice();
const { mediaMode, pickVideoSource } = useLiveSources();
const {
  videoPreviewEl,
  previewUrl,
  previewState,
  videoFitMode,
  previewTitle,
  previewMeta,
  toggleVideoFitMode,
  refreshVideoPreview,
} = useMediaPreview();

async function onPickVideoSource() {
  const picked = await pickVideoSource();
  if (!picked.ok) {
    message.error(picked.error);
    return;
  }
  if (picked.path) refreshVideoPreview(true);
}

// 预览用的 <video> 元素归 useMediaPreview 持有(它负责挂 srcObject / 停轨),
// 这里用函数式 ref 把 DOM 节点回填过去。
function bindPreviewEl(el: unknown) {
  videoPreviewEl.value = (el as HTMLVideoElement | null) ?? null;
}
</script>

<template>
  <aside class="glass-card console-panel preview-panel">
    <div class="preview-head">
      <div>
        <div class="panel-title">视频预览</div>
        <div class="preview-sub">{{ previewTitle }}</div>
      </div>
      <div class="preview-head-right">
        <button
          type="button"
          class="file-btn compact toggle-fit-btn"
          :title="videoFitMode === 'contain' ? '当前：保持比例适应；点击切换为完全铺满' : '当前：铺满裁剪；点击切换为保持比例'"
          @click="toggleVideoFitMode"
        >
          {{ videoFitMode === 'cover' ? '模式：铺满' : '模式：适应' }}
        </button>
        <span class="preview-badge" :class="previewState.tone">
          {{ previewState.tone === 'live' ? 'PREVIEW' : previewState.tone === 'warn' ? 'CHECK' : 'IDLE' }}
        </span>
        <div class="preview-actions">
          <button
            type="button"
            class="file-btn compact primary"
            :disabled="mediaMode === 'none'"
            @click="refreshVideoPreview()"
          >
            刷新
          </button>
          <button
            type="button"
            class="file-btn compact"
            :disabled="mediaMode !== 'file' || startDisabled"
            @click="onPickVideoSource"
          >
            选文件
          </button>
        </div>
      </div>
    </div>
    <div class="video-frame" :class="[previewState.tone, `fit-${videoFitMode}`]">
      <video
        :ref="bindPreviewEl"
        :src="previewUrl || undefined"
        autoplay
        muted
        playsinline
        controls
        :class="`video-${videoFitMode}`"
      />
      <div v-if="!previewUrl && previewState.tone !== 'live'" class="preview-empty">
        <div class="scan-mark">GB</div>
        <strong>{{ previewState.text }}</strong>
        <span>{{ previewMeta }}</span>
      </div>
    </div>
    <div class="preview-status-line">
      <div class="preview-status" :class="previewState.tone">
        <i />
        <span>{{ previewState.text }}</span>
      </div>
      <span class="preview-note">本地预览画面（支持适应/铺满），推送到平台为标准 PS/RTP 流</span>
    </div>
  </aside>
</template>

<style scoped>
.preview-panel {
  box-sizing: border-box;
  display: flex;
  flex-direction: column;
}
.preview-head {
  display: flex; align-items: flex-start; justify-content: space-between;
  gap: 8px; margin-bottom: 6px; flex: 0 0 auto;
}
.preview-head .panel-title { margin-bottom: 1px; }
.preview-head-right { display: flex; align-items: center; gap: 6px; }
.preview-sub { color: var(--text-secondary); font-size: var(--fs-sm); }
.preview-badge {
  padding: 2px 6px; border-radius: 999px; font: 700 9px/1 "SF Mono", Menlo, monospace; letter-spacing: .5px;
  color: var(--text-tertiary); background: var(--bg-surface); border: 1px solid var(--border-default);
}
.preview-badge.live { color: var(--success); background: color-mix(in srgb, var(--success) 10%, transparent); border-color: color-mix(in srgb, var(--success) 22%, transparent); }
.preview-badge.warn { color: var(--warning); background: color-mix(in srgb, var(--warning) 10%, transparent); border-color: color-mix(in srgb, var(--warning) 22%, transparent); }

/* 视频框:严格 16:9 比例展示,支持 Cover 与 Contain 切换 */
.video-frame {
  position: relative; width: 100%; aspect-ratio: 16 / 9;
  display: flex; align-items: center; justify-content: center; overflow: hidden; border-radius: 8px;
  background:
    radial-gradient(circle at 50% 35%, rgba(56,132,255,.18), transparent 32%),
    linear-gradient(145deg, #0f1b2e, #14243a 58%, #0d1727);
  border: 1px solid var(--border-subtle); box-shadow: inset 0 1px 0 rgba(255,255,255,.15), 0 6px 16px rgba(15,23,42,.08);
}
.video-frame::after {
  content: ""; position: absolute; inset: 0; pointer-events: none; opacity: .18;
  background-image: linear-gradient(rgba(255,255,255,.12) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,.08) 1px, transparent 1px);
  background-size: 20px 20px; mask-image: linear-gradient(to bottom, #000, transparent 88%);
}
.video-frame video { width: 100%; height: 100%; background: #050914; z-index: 1; }
.video-frame video.video-cover { object-fit: cover; }
.video-frame video.video-contain { object-fit: contain; }

.preview-empty {
  position: absolute; inset: 0; z-index: 2; display: grid; place-items: center; align-content: center;
  gap: 4px; padding: 12px; text-align: center; color: rgba(231,241,255,.92);
}
.preview-empty strong { font-size: var(--fs-sm); }
.preview-empty span { max-width: 240px; color: rgba(210,226,246,.68); font-size: var(--fs-xs); line-height: 1.4; }
.scan-mark {
  display: grid; place-items: center; width: 34px; height: 34px; border-radius: 50%;
  border: 1px solid rgba(122,181,255,.45); color: #9ec8ff; font: 800 11px/1 "SF Mono", Menlo, monospace;
  box-shadow: 0 0 0 6px rgba(56,132,255,.06), inset 0 0 14px rgba(56,132,255,.14);
}
.preview-status-line {
  display: flex; align-items: center; justify-content: space-between; gap: 8px;
  margin-top: 5px; flex: 0 0 auto;
}
.preview-status { display: flex; align-items: center; gap: 5px; color: var(--text-secondary); font-size: var(--fs-sm); line-height: 1.35; }
.preview-status i { width: 5px; height: 5px; border-radius: 50%; background: var(--text-tertiary); flex: 0 0 auto; }
.preview-status.live i { background: var(--success); box-shadow: 0 0 0 2px color-mix(in srgb, var(--success) 12%, transparent); }
.preview-status.warn i { background: var(--warning); box-shadow: 0 0 0 2px color-mix(in srgb, var(--warning) 12%, transparent); }
.preview-actions { display: flex; gap: 4px; }
.toggle-fit-btn { background: var(--accent-dim); border-color: var(--border-accent); color: var(--accent); }
.preview-note { color: var(--text-tertiary); font-size: var(--fs-xs); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

@media (max-width: 1080px) {
  .preview-panel { min-height: 240px; }
}
</style>
