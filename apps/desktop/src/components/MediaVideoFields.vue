<script setup lang="ts">
import { useId } from "vue";
// 视频编码参数列(摄像头/屏幕两种模式共用):分辨率、帧率、码率、编码格式、GOP。
// 采集设备本身的选择由父组件通过 #device 插槽提供(摄像头列表 vs 显示器列表)。
//
// 分辨率/码率/编码/GOP 进 live: URI(引擎按它配置编码器);帧率写在 DeviceConfig 上,
// 因为它对文件源同样生效。两条路径都真实下发到后端,不是摆设。
import { useDevice } from "../device";
import { useLiveSources } from "../composables/useLiveSources";

defineProps<{ title: string; disabled: boolean }>();

const { form } = useDevice();
const {
  selectedScreenResolution,
  selectedScreenBitrate,
  selectedScreenCodec,
  selectedGop,
  screenResolutionOptions,
  applyScreenResolution,
  syncLiveSourceUri,
} = useLiveSources();

// 表单控件与 label 的关联用 `useId()` 生成:本组件在同一页里会被实例化两次
// (屏幕采集列 / 摄像头列),写死 id 会重复,而重复的 id 会让屏幕阅读器
// 把两列的字段名读串。
const idResolution = useId();
const idFps = useId();
const idBitrate = useId();
const idCodec = useId();
const idGop = useId();
</script>

<template>
  <div class="param-col">
    <div class="col-title">🎥 {{ title }}</div>
    <slot name="device" />
    <div class="fg">
      <label :for="idResolution">输出分辨率</label>
      <select
        :id="idResolution"
        v-model="selectedScreenResolution"
        :disabled="disabled"
        class="inp"
        @change="applyScreenResolution"
      >
        <option v-for="resolution in screenResolutionOptions" :key="resolution.value" :value="resolution.value">
          {{ resolution.label }}
        </option>
      </select>
    </div>
    <div class="fg">
      <label :for="idFps">视频帧率 (FPS)</label>
      <select :id="idFps" v-model.number="form.video_fps" :disabled="disabled" class="inp">
        <option :value="15">15 fps（低开销）</option>
        <option :value="25">25 fps（国标推荐）</option>
        <option :value="30">30 fps（标准）</option>
        <option :value="60">60 fps（高流畅）</option>
      </select>
    </div>
    <div class="fg">
      <label :for="idBitrate">视频码率</label>
      <div class="bitrate-input">
        <input
          :id="idBitrate"
          v-model.number="selectedScreenBitrate"
          :disabled="disabled"
          class="inp"
          type="number"
          min="128"
          max="20000"
          step="50"
          aria-label="视频码率（kbps）"
          @change="syncLiveSourceUri"
        />
        <span>kbps</span>
      </div>
    </div>
    <div class="fg">
      <label :for="idCodec">视频编码格式</label>
      <select :id="idCodec" v-model="selectedScreenCodec" :disabled="disabled" class="inp" @change="syncLiveSourceUri">
        <option value="h264">H.264 / AVC（兼容性优先）</option>
        <option value="h265">H.265 / HEVC（更高压缩率、更低带宽）</option>
      </select>
    </div>
    <div class="fg">
      <label :for="idGop">GOP 关键帧间隔</label>
      <select :id="idGop" v-model.number="selectedGop" :disabled="disabled" class="inp" @change="syncLiveSourceUri">
        <option :value="25">25 帧（约 1 秒 GOP）</option>
        <option :value="50">50 帧（约 2 秒 GOP · 推荐）</option>
        <option :value="100">100 帧（约 4 秒 GOP）</option>
      </select>
    </div>
  </div>
</template>

<style scoped>
.bitrate-input { display: flex; align-items: center; gap: 4px; }
.bitrate-input .inp { flex: 1 1 auto; min-width: 0; }
.bitrate-input span { flex: 0 0 auto; color: var(--text-tertiary); font-size: var(--fs-xs); }
</style>
