<script setup lang="ts">
import { computed, ref } from "vue";
import {
  NButton,
  NForm,
  NFormItem,
  NInput,
  NInputGroup,
  NInputNumber,
  NSelect,
  NSwitch,
  NTag,
} from "naive-ui";

export interface ScenarioFormModel {
  base_device_id: string;
  count: number;
  heartbeat_interval: number;
  channels_per_device: number;
  gb_version: string;
  media_profile: string;
  bitrate_kbps: number;
  ramp_per_second: number;
  active_ratio: number;
  video_source: string;
  position_enabled: boolean;
  position_interval: number;
  alarm_enabled: boolean;
  alarm_interval: number;
}

const form = defineModel<ScenarioFormModel>("form", { required: true });

defineProps<{
  running: boolean;
  starting: boolean;
  stopping: boolean;
  stopPending: boolean;
  platformLabel: string;
  scenarioSummary: string;
}>();

const emit = defineEmits<{
  (e: "start"): void;
  (e: "stop"): void;
  (e: "pick-video"): void;
}>();

const videoCollapsed = ref(true);

const mediaOptions = [
  { label: "A · 仅信令", value: "A" },
  { label: "B · 轻量伪流", value: "B" },
  { label: "C · 真实视频", value: "C" },
];

const versionOptions = [
  { label: "GB/T 28181-2022", value: "V2022" },
  { label: "GB/T 28181-2016", value: "V2016" },
];

const mediaTagText = computed(() => {
  if (form.value.media_profile === "A") return "A · 仅信令";
  if (form.value.media_profile === "B") return `B · 伪流 (${form.value.bitrate_kbps}k)`;
  return `C · 真实视频 (${form.value.active_ratio}%)`;
});
</script>

<template>
  <section class="glass-card panel config-panel">
    <div class="panel-head">
      <div>
        <div class="panel-title">压测场景配置</div>
        <div class="panel-sub" :title="platformLabel">{{ platformLabel }}</div>
      </div>
      <n-tag size="tiny" round :bordered="false" type="info">上限 10,000</n-tag>
    </div>

    <!-- 运行中状态卡片：紧凑展示锁定参数与运行态 -->
    <div v-if="running || starting || stopping || stopPending" class="running-card">
      <div class="rc-status-row">
        <span class="rc-pulse" :class="{ stopping: stopping || stopPending }" />
        <span class="rc-status-title">
          {{ stopping || stopPending ? '压测正在停止…' : starting ? '正在并发拉起…' : '压测全力运行中' }}
        </span>
      </div>
      <div class="rc-grid">
        <div class="rc-item">
          <span class="rc-label">平台</span>
          <b class="rc-val mono" :title="platformLabel">{{ platformLabel }}</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">概要</span>
          <b class="rc-val mono">{{ scenarioSummary }}</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">并发</span>
          <b class="rc-val">{{ form.count }} 台 · {{ form.ramp_per_second }} 台/s</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">国标/媒体</span>
          <b class="rc-val">{{ form.gb_version }} · 档位 {{ form.media_profile }} ({{ form.active_ratio }}%)</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">心跳/通道</span>
          <b class="rc-val">{{ form.heartbeat_interval }}s · {{ form.channels_per_device }} 通道</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">主动上报</span>
          <b class="rc-val">
            {{ form.position_enabled ? `GPS ${form.position_interval}s` : '无GPS' }}
            ·
            {{ form.alarm_enabled ? `报警 ${form.alarm_interval}s` : '无报警' }}
          </b>
        </div>
      </div>
    </div>

    <!-- 编辑表单：设备身份、国标参数与折叠视频设置 -->
    <n-form
      v-show="!(running || starting || stopping || stopPending)"
      size="small"
      :model="form"
      label-placement="top"
      :show-feedback="false"
      class="compact-form"
    >
      <!-- 设备身份与国标核心参数（含主动上报） -->
      <div class="form-grid">
        <n-form-item label="起始设备 ID" class="span-2 fg-item">
          <n-input
            v-model:value="form.base_device_id"
            size="small"
            maxlength="20"
            placeholder="20 位国标设备编号"
          />
        </n-form-item>

        <n-form-item label="并发数 (台)" class="fg-item">
          <n-input-number
            v-model:value="form.count"
            size="small"
            :min="1"
            :max="10000"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="爬坡 (台/s)" class="fg-item">
          <n-input-number
            v-model:value="form.ramp_per_second"
            size="small"
            :min="0"
            :max="1000"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="心跳 (s)" class="fg-item">
          <n-input-number
            v-model:value="form.heartbeat_interval"
            size="small"
            :min="1"
            :max="600"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="单台通道" class="fg-item">
          <n-input-number
            v-model:value="form.channels_per_device"
            size="small"
            :min="1"
            :max="8"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="国标版本" class="span-2 fg-item">
          <n-select
            v-model:value="form.gb_version"
            size="small"
            :options="versionOptions"
          />
        </n-form-item>

        <!-- 上报 GPS 与 报警事件（整合到主卡片中并排两列） -->
        <div class="report-cell fg-item">
          <div class="rc-hd">
            <span class="rc-tit">GPS 上报</span>
            <n-switch v-model:value="form.position_enabled" size="small" />
          </div>
          <n-input-number
            v-model:value="form.position_interval"
            size="small"
            :min="1"
            :max="600"
            :disabled="!form.position_enabled"
            placeholder="间隔"
            style="width: 100%"
          >
            <template #suffix>s</template>
          </n-input-number>
        </div>

        <div class="report-cell fg-item">
          <div class="rc-hd">
            <span class="rc-tit">报警上报</span>
            <n-switch v-model:value="form.alarm_enabled" size="small" />
          </div>
          <n-input-number
            v-model:value="form.alarm_interval"
            size="small"
            :min="1"
            :max="600"
            :disabled="!form.alarm_enabled"
            placeholder="间隔"
            style="width: 100%"
          >
            <template #suffix>s</template>
          </n-input-number>
        </div>
      </div>

      <!-- 视频参数设置（可折叠） -->
      <div class="video-collapse-box">
        <button
          type="button"
          class="video-collapse-header"
          :aria-expanded="!videoCollapsed"
          @click="videoCollapsed = !videoCollapsed"
        >
          <div class="vch-left">
            <span class="vch-arrow" :class="{ rotated: !videoCollapsed }">▶</span>
            <span class="vch-title">视频参数设置</span>
            <span class="vch-tag">{{ mediaTagText }}</span>
          </div>
          <span class="vch-toggle">{{ videoCollapsed ? '展开' : '收起' }}</span>
        </button>

        <div v-show="!videoCollapsed" class="video-collapse-body">
          <div class="form-grid">
            <n-form-item label="媒体档位" class="fg-item">
              <n-select
                v-model:value="form.media_profile"
                size="small"
                :options="mediaOptions"
              />
            </n-form-item>

            <n-form-item label="推流占比" class="fg-item">
              <n-input-number
                v-model:value="form.active_ratio"
                size="small"
                :min="0"
                :max="100"
                :format="(v: number | null) => `${v ?? 0}%`"
                style="width: 100%"
              />
            </n-form-item>

            <n-form-item
              v-if="form.media_profile === 'B'"
              label="伪流码率"
              class="span-2 fg-item"
            >
              <n-input-number
                v-model:value="form.bitrate_kbps"
                size="small"
                :min="64"
                :max="8192"
                style="width: 100%"
              >
                <template #suffix>kbps</template>
              </n-input-number>
            </n-form-item>

            <n-form-item
              v-if="form.media_profile === 'C'"
              label="视频源"
              class="span-2 fg-item"
            >
              <n-input-group size="small">
                <n-input
                  v-model:value="form.video_source"
                  size="small"
                  placeholder="H.264/H.265 或 MP4"
                />
                <n-button type="primary" ghost size="small" @click="emit('pick-video')">选择</n-button>
              </n-input-group>
            </n-form-item>
          </div>
        </div>
      </div>
    </n-form>

    <!-- 底部操作按钮 -->
    <div class="action-section">
      <div class="action-buttons">
        <n-button
          type="primary"
          class="action-btn"
          :loading="starting"
          :disabled="running || stopping || stopPending"
          @click="emit('start')"
        >
          启动压测
        </n-button>
        <n-button
          type="error"
          secondary
          class="action-btn"
          :loading="stopPending"
          :disabled="!running || starting || stopping"
          @click="emit('stop')"
        >
          停止压测
        </n-button>
      </div>
      <div class="action-hint">配置语义已校验 · 报告自动脱敏</div>
    </div>
  </section>
</template>

<style scoped>
.config-panel {
  display: flex;
  flex-direction: column;
  box-sizing: border-box;
}
.panel-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 8px;
  flex: 0 0 auto;
}
.panel-title {
  font-size: var(--fs-md);
  font-weight: 700;
  color: var(--text-primary);
}
.panel-sub {
  font-size: var(--fs-xs);
  color: var(--text-tertiary);
  margin-top: 1px;
  max-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 运行中锁定卡片 */
.running-card {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 8px 10px;
  background: var(--bg-surface);
  border: 1px solid var(--border-default);
  border-radius: var(--radius-sm);
  margin-bottom: 8px;
}
.rc-status-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-bottom: 5px;
  border-bottom: 1px solid var(--border-default);
}
.rc-pulse {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--success);
  box-shadow: 0 0 6px var(--success);
  animation: pulse 1.6s infinite;
}
.rc-pulse.stopping {
  background: var(--warning);
  box-shadow: 0 0 6px var(--warning);
}
.rc-status-title {
  font-size: var(--fs-sm);
  font-weight: 700;
  color: var(--text-primary);
}
.rc-grid {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.rc-item {
  display: flex;
  justify-content: space-between;
  align-items: baseline;
  font-size: var(--fs-xs);
  gap: 8px;
}
.rc-label {
  color: var(--text-tertiary);
  flex-shrink: 0;
}
.rc-val {
  color: var(--text-secondary);
  text-align: right;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 紧凑表单 */
.compact-form {
  display: flex;
  flex-direction: column;
}
.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 4px 8px;
}
.span-2 {
  grid-column: span 2 / span 2;
}
.fg-item {
  margin-bottom: 0 !important;
}
:deep(.n-form-item .n-form-item-label) {
  font-size: var(--fs-xs);
  color: var(--text-secondary);
  font-weight: 600;
  padding-bottom: 2px !important;
  min-height: auto !important;
}

/* 整合进主卡片的上报项 */
.report-cell {
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.rc-hd {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding-bottom: 1px;
}
.rc-tit {
  font-size: var(--fs-xs);
  color: var(--text-secondary);
  font-weight: 600;
}

/* 视频参数折叠组件 */
.video-collapse-box {
  margin-top: 8px;
  border: 1px solid var(--border-default);
  border-radius: var(--radius-xs);
  background: var(--bg-surface);
  overflow: hidden;
}
.video-collapse-header {
  width: 100%;
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 6px 8px;
  background: transparent;
  border: none;
  cursor: pointer;
  text-align: left;
  transition: background 0.15s;
}
.video-collapse-header:hover {
  background: rgba(120, 140, 170, 0.08);
}
.vch-left {
  display: flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}
.vch-arrow {
  font-size: 9px;
  color: var(--text-tertiary);
  transition: transform 0.2s;
  display: inline-block;
}
.vch-arrow.rotated {
  transform: rotate(90deg);
}
.vch-title {
  font-size: 11.5px;
  font-weight: 700;
  color: var(--text-primary);
}
.vch-tag {
  font-size: 10px;
  padding: 1px 5px;
  border-radius: 999px;
  background: rgba(56, 132, 255, 0.1);
  color: var(--accent-deep, #1e63dc);
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 120px;
}
.vch-toggle {
  font-size: 11px;
  color: var(--accent);
  font-weight: 600;
  flex-shrink: 0;
}
.video-collapse-body {
  padding: 6px 8px 8px;
  border-top: 1px dashed var(--border-default);
  background: var(--bg-elevated);
}

/* 操作区域 */
.action-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-top: 10px;
  padding-top: 8px;
  border-top: 1px solid var(--border-default);
}
.action-buttons {
  display: flex;
  gap: 8px;
}
.action-btn {
  flex: 1;
  font-weight: 650;
}
.action-hint {
  font-size: 10px;
  color: var(--text-tertiary);
  text-align: center;
}

@keyframes pulse {
  50% {
    opacity: 0.4;
  }
}
</style>
