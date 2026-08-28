<script setup lang="ts">
// 音频参数列(摄像头/屏幕两种模式共用):只有"编码格式"是可选项。
// 采样率/声道/比特率由编码格式决定(G.711 固定 8 kHz 单声道、AAC/Opus 固定 48 kHz 单声道),
// 因此这里做只读展示 —— 之前那三个下拉框后端根本不读,属于"界面在撒谎"。
import { computed } from "vue";
import { audioSpecText, useLiveSources } from "../composables/useLiveSources";
import type { AudioCodec } from "../lib/liveSource";

defineProps<{ disabled: boolean }>();
const codec = defineModel<AudioCodec>("codec", { required: true });

const { liveCatalog, syncLiveSourceUri } = useLiveSources();
const specText = computed(() => audioSpecText(codec.value));
</script>

<template>
  <div class="param-col">
    <div class="col-title">🎙 音频流参数</div>
    <slot name="device" />
    <div class="fg">
      <label>音频编码格式</label>
      <select v-model="codec" :disabled="disabled" class="inp" aria-label="实时音频格式" @change="syncLiveSourceUri">
        <option value="g711a">G.711A / PCMA · 8 kHz · 单声道</option>
        <option value="g711u">G.711U / PCMU · 8 kHz · 单声道</option>
        <option value="aac" :disabled="!liveCatalog.aac_available">
          AAC-LC / ADTS · 48 kHz · 单声道{{ liveCatalog.aac_available ? '' : '（不可用）' }}
        </option>
        <option value="opus" :disabled="!liveCatalog.opus_available">
          Opus · 48 kHz · 20 ms（扩展{{ liveCatalog.opus_available ? '' : '，不可用' }}）
        </option>
      </select>
    </div>
    <div class="fg">
      <label>音频规格（由编码格式决定）</label>
      <output class="audio-spec">{{ specText }}</output>
    </div>
    <div class="fg-hint">
      国标只要求 G.711A/G.711U;AAC 与 Opus 属扩展能力,是否可用取决于内嵌 FFmpeg 的编译选项。
    </div>
  </div>
</template>

<style scoped>
.audio-spec {
  display: block;
  padding: 4px 8px;
  border: 1px dashed var(--border-default);
  border-radius: var(--radius-xs);
  background: var(--bg-card);
  color: var(--text-secondary);
  font-size: 10.5px;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
}
</style>
