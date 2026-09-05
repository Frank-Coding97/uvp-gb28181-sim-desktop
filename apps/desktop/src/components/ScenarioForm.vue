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
        <div class="panel-title">场景配置</div>
        <div class="panel-sub">{{ platformLabel }}</div>
      </div>
      <n-tag size="small" round :bordered="false" type="info">上限 10,000 台</n-tag>
    </div>

    <div v-if="running || starting || stopping || stopPending" class="scenario-summary">
      <div><span>当前场景</span><b>{{ scenarioSummary }}</b></div>
      <div><span>目标平台</span><b>{{ platformLabel }}</b></div>
    </div>

    <n-form v-show="!(running || starting || stopping || stopPending)" :model="form" label-placement="top" :disabled="running || starting">
      <div class="form-grid">
        <n-form-item label="起始设备 ID" class="span-2">
          <n-input v-model:value="form.base_device_id" maxlength="20" placeholder="20 位国标设备编号" />
        </n-form-item>
        <n-form-item label="设备数量">
          <n-input-number v-model:value="form.count" :min="1" :max="10000" style="width: 100%" />
        </n-form-item>
        <n-form-item label="爬坡速率（台/秒）">
          <n-input-number v-model:value="form.ramp_per_second" :min="0" :max="1000" style="width: 100%" />
        </n-form-item>
        <n-form-item label="协议版本">
          <n-select v-model:value="form.gb_version" :options="versionOptions" />
        </n-form-item>
        <n-form-item label="心跳间隔（秒）">
          <n-input-number v-model:value="form.heartbeat_interval" :min="1" :max="600" style="width: 100%" />
        </n-form-item>
        <n-form-item label="每设备通道数">
          <n-input-number v-model:value="form.channels_per_device" :min="1" :max="8" style="width: 100%" />
        </n-form-item>
        <n-form-item label="推流设备占比">
          <n-input-number v-model:value="form.active_ratio" :min="0" :max="100" :format="(v: number | null) => `${v ?? 0}%`" style="width: 100%" />
        </n-form-item>
        <n-form-item label="媒体档位">
          <n-select v-model:value="form.media_profile" :options="mediaOptions" />
        </n-form-item>
        <n-form-item v-if="form.media_profile === 'B'" label="伪流码率（kbps）">
          <n-input-number v-model:value="form.bitrate_kbps" :min="64" :max="8192" style="width: 100%" />
        </n-form-item>
        <n-form-item v-if="form.media_profile === 'C'" label="视频源文件" class="span-2">
          <n-input-group>
            <n-input v-model:value="form.video_source" placeholder="H.264/H.265 裸流或 MP4 等容器" />
            <n-button type="primary" ghost @click="emit('pick-video')">选择文件</n-button>
          </n-input-group>
        </n-form-item>
      </div>

      <div class="section-divider">
        <span>主动上报施压</span>
        <small>0 表示关闭；启用后每台设备按周期向平台上报</small>
      </div>
      <div class="report-grid">
        <div class="report-option">
          <div>
            <b>移动位置</b>
            <span>MobilePosition</span>
          </div>
          <n-switch v-model:value="form.position_enabled" />
          <n-input-number v-model:value="form.position_interval" :min="1" :max="600" :disabled="!form.position_enabled" size="small" />
          <em>秒/次</em>
        </div>
        <div class="report-option">
          <div>
            <b>报警事件</b>
            <span>Alarm</span>
          </div>
          <n-switch v-model:value="form.alarm_enabled" />
          <n-input-number v-model:value="form.alarm_interval" :min="1" :max="600" :disabled="!form.alarm_enabled" size="small" />
          <em>秒/次</em>
        </div>
      </div>
    </n-form>

    <div class="action-row">
      <n-button type="primary" size="large" :loading="starting" :disabled="running || stopping || stopPending" @click="emit('start')">
        启动压测
      </n-button>
      <n-button type="error" secondary size="large" :loading="stopPending" :disabled="!running || starting || stopping" @click="emit('stop')">
        停止压测
      </n-button>
      <span>配置会先经过后端语义校验，报告自动隐藏认证密码。</span>
    </div>
  </section>
</template>

<style scoped>
.panel-head {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: 12px;
}
.panel-title {
  font-size: 14px;
  font-weight: 700;
  color: var(--text-color);
}
.panel-sub {
  font-size: 11px;
  color: var(--text-color-3);
  margin-top: 2px;
}
.form-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 8px 12px;
}
.span-2 {
  grid-column: span 2 / span 2;
}
.section-divider {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 12px 0 8px;
  padding-top: 10px;
  border-top: 1px dashed var(--border-color);
  font-size: 11px;
  font-weight: 600;
  color: var(--text-color-2);
}
.section-divider small {
  font-weight: 400;
  color: var(--text-color-3);
}
.report-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 10px;
}
.report-option {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  background: var(--card-color);
  border: 1px solid var(--border-color);
  border-radius: var(--border-radius);
}
.report-option div {
  flex: 1;
  display: flex;
  flex-direction: column;
}
.report-option b {
  font-size: 12px;
  color: var(--text-color);
}
.report-option span {
  font-size: 10px;
  color: var(--text-color-3);
}
.report-option em {
  font-style: normal;
  font-size: 11px;
  color: var(--text-color-3);
}
.scenario-summary {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 14px;
  background: var(--card-color);
  border: 1px solid var(--border-color);
  border-radius: var(--border-radius);
  margin-bottom: 12px;
}
.scenario-summary div {
  display: flex;
  justify-content: space-between;
  font-size: 12px;
}
.scenario-summary span {
  color: var(--text-color-3);
}
.scenario-summary b {
  color: var(--text-color);
}
.action-row {
  display: flex;
  align-items: center;
  gap: 12px;
  margin-top: 16px;
  padding-top: 14px;
  border-top: 1px solid var(--border-color);
}
.action-row span {
  font-size: 11px;
  color: var(--text-color-3);
}
</style>
