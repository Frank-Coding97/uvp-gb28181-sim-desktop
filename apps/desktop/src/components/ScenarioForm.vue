<script setup lang="ts">
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

const mediaOptions = [
  { label: "A · 仅信令（最低资源）", value: "A" },
  { label: "B · 轻量 RTP 伪流", value: "B" },
  { label: "C · 真实视频循环推流", value: "C" },
];

const versionOptions = [
  { label: "GB/T 28181-2022", value: "V2022" },
  { label: "GB/T 28181-2016", value: "V2016" },
];
</script>

<template>
  <section class="glass-card panel config-panel">
    <div class="panel-head">
      <div>
        <div class="panel-title">压测场景配置</div>
        <div class="panel-sub" :title="platformLabel">{{ platformLabel }}</div>
      </div>
      <n-tag size="tiny" round :bordered="false" type="info">并发上限 10,000</n-tag>
    </div>

    <!-- 运行中状态卡片：紧凑展示锁定参数与运行态 -->
    <div v-if="running || starting || stopping || stopPending" class="running-card">
      <div class="rc-status-row">
        <span class="rc-pulse" :class="{ stopping: stopping || stopPending }" />
        <span class="rc-status-title">
          {{ stopping || stopPending ? '压测正在平滑停止…' : starting ? '压测正在并发启动…' : '压测场景全力运行中' }}
        </span>
      </div>
      <div class="rc-grid">
        <div class="rc-item">
          <span class="rc-label">目标平台</span>
          <b class="rc-val mono" :title="platformLabel">{{ platformLabel }}</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">场景概要</span>
          <b class="rc-val mono">{{ scenarioSummary }}</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">并发规模</span>
          <b class="rc-val">{{ form.count }} 台 · {{ form.ramp_per_second }} 台/秒</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">国标与媒体</span>
          <b class="rc-val">{{ form.gb_version }} · 档位 {{ form.media_profile }} ({{ form.active_ratio }}% 推流)</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">心跳上报</span>
          <b class="rc-val">{{ form.heartbeat_interval }}s 周期 · {{ form.channels_per_device }} 通道/台</b>
        </div>
        <div class="rc-item">
          <span class="rc-label">主动施压</span>
          <b class="rc-val">
            {{ form.position_enabled ? `GPS(${form.position_interval}s)` : '无GPS' }}
            ·
            {{ form.alarm_enabled ? `报警(${form.alarm_interval}s)` : '无报警' }}
          </b>
        </div>
      </div>
    </div>

    <!-- 编辑表单：高密度双列紧凑排布 -->
    <n-form
      v-show="!(running || starting || stopping || stopPending)"
      size="small"
      :model="form"
      label-placement="top"
      :show-feedback="false"
      class="compact-form"
    >
      <div class="form-grid">
        <n-form-item label="起始国标 ID" class="span-2 fg-item">
          <n-input
            v-model:value="form.base_device_id"
            size="small"
            maxlength="20"
            placeholder="20 位国标设备编号"
          />
        </n-form-item>

        <n-form-item label="并发设备数 (台)" class="fg-item">
          <n-input-number
            v-model:value="form.count"
            size="small"
            :min="1"
            :max="10000"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="爬坡速率 (台/秒)" class="fg-item">
          <n-input-number
            v-model:value="form.ramp_per_second"
            size="small"
            :min="0"
            :max="1000"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="国标协议版本" class="fg-item">
          <n-select
            v-model:value="form.gb_version"
            size="small"
            :options="versionOptions"
          />
        </n-form-item>

        <n-form-item label="媒体档位" class="fg-item">
          <n-select
            v-model:value="form.media_profile"
            size="small"
            :options="mediaOptions"
          />
        </n-form-item>

        <n-form-item label="心跳间隔 (秒)" class="fg-item">
          <n-input-number
            v-model:value="form.heartbeat_interval"
            size="small"
            :min="1"
            :max="600"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="每台通道数" class="fg-item">
          <n-input-number
            v-model:value="form.channels_per_device"
            size="small"
            :min="1"
            :max="8"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item label="活跃推流占比" class="fg-item">
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
          label="伪流码率 (kbps)"
          class="fg-item"
        >
          <n-input-number
            v-model:value="form.bitrate_kbps"
            size="small"
            :min="64"
            :max="8192"
            style="width: 100%"
          />
        </n-form-item>

        <n-form-item
          v-if="form.media_profile === 'C'"
          label="视频源文件"
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

      <div class="section-divider">
        <span>主动施压上报</span>
        <small>周期向平台上报</small>
      </div>

      <div class="report-list">
        <div class="report-row">
          <div class="rr-title">
            <b>GPS 移动位置</b>
            <span>MobilePosition</span>
          </div>
          <n-switch v-model:value="form.position_enabled" size="small" />
          <n-input-number
            v-model:value="form.position_interval"
            :min="1"
            :max="600"
            :disabled="!form.position_enabled"
            size="small"
            class="rr-num"
          />
          <span class="rr-unit">秒/次</span>
        </div>

        <div class="report-row">
          <div class="rr-title">
            <b>国标报警事件</b>
            <span>MANSCDP Alarm</span>
          </div>
          <n-switch v-model:value="form.alarm_enabled" size="small" />
          <n-input-number
            v-model:value="form.alarm_interval"
            :min="1"
            :max="600"
            :disabled="!form.alarm_enabled"
            size="small"
            class="rr-num"
          />
          <span class="rr-unit">秒/次</span>
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
      <div class="action-hint">配置经严格语义校验，运行报告自动脱敏</div>
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
  gap: 8px;
  padding: 10px 12px;
  background: var(--bg-surface);
  border: 1px solid var(--border-default);
  border-radius: var(--radius-sm);
  margin-bottom: 8px;
}
.rc-status-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding-bottom: 6px;
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
  gap: 5px;
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

.section-divider {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin: 10px 0 6px;
  padding-top: 6px;
  border-top: 1px dashed var(--border-default);
  font-size: var(--fs-xs);
  font-weight: 700;
  color: var(--text-primary);
}
.section-divider small {
  font-weight: 400;
  color: var(--text-tertiary);
}

.report-list {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.report-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 5px 8px;
  background: var(--bg-surface);
  border: 1px solid var(--border-default);
  border-radius: var(--radius-xs);
}
.rr-title {
  flex: 1;
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.rr-title b {
  font-size: var(--fs-xs);
  color: var(--text-primary);
}
.rr-title span {
  font-size: 10px;
  color: var(--text-tertiary);
}
.rr-num {
  width: 70px !important;
}
.rr-unit {
  font-size: 10px;
  color: var(--text-tertiary);
  flex-shrink: 0;
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
