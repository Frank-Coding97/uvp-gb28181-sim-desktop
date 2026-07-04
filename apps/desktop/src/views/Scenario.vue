<script setup lang="ts">
// 压力测试页(FR-41/42 合并):场景编排 + 启动/停止 + 实时监控大盘同页。
// 批量虚拟设备并发注册/心跳/推流,并可批量主动上报定位/报警,压上级平台。
import { ref, onMounted, onUnmounted, onActivated, nextTick, shallowRef } from "vue";
import {
  NForm, NFormItem, NInput, NInputNumber, NInputGroup,
  NButton, NSpace, NSelect, NSwitch, useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import * as echarts from "echarts";

const message = useMessage();

const STORAGE_KEY = "uvp_platform_config";

// 场景参数。
const form = ref({
  base_device_id:       "34020000001320000001",
  count:                10,
  heartbeat_interval:   60,
  channels_per_device:  1,
  media_profile:        "A",
  ramp_per_second:      50,
  active_ratio:         100,
  video_source:         "",
  // 主动上报(压测施压):按周期批量触发定位/报警。
  position_enabled:     false,
  position_interval:    10,
  alarm_enabled:        false,
  alarm_interval:       30,
});

const mediaOptions = [
  { label: "A - 空媒体(仅信令)",   value: "A" },
  { label: "B - 轻量 RTP(伪流)",  value: "B" },
  { label: "C - 真实 H.264 文件",  value: "C" },
];

const running    = ref(false);
const statusText = ref("就绪");

// 选择 C 档视频源文件。
async function pickVideoSource() {
  try {
    const picked = await openDialog({
      multiple: false, directory: false,
      filters: [
        { name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] },
      ],
    });
    if (typeof picked === "string") form.value.video_source = picked;
  } catch (e) { message.error("选择文件失败:" + String(e)); }
}

// 每次进入本页(含 keep-alive 激活)与引擎真实状态对账。
onActivated(async () => {
  await reconcile();
  await nextTick();
  chart.value?.resize();
});
async function reconcile() {
  try {
    const st = await invoke<{ running: boolean }>("get_stress_status");
    running.value = st.running;
    if (st.running && statusText.value === "就绪") statusText.value = "运行中(已在后台)";
    if (!st.running && statusText.value.startsWith("运行中")) statusText.value = "就绪";
  } catch { /* 忽略 */ }
}

function loadPlatformConfig() {
  const raw = localStorage.getItem(STORAGE_KEY);
  return raw ? JSON.parse(raw) : {
    server_host: "127.0.0.1", server_port: 5060,
    server_domain: "34020000002000000001", password: "12345678", transport: "UDP",
  };
}

function buildToml(): string {
  const p = loadPlatformConfig();
  const lines = [
    `base_device_id = "${form.value.base_device_id}"`,
    `password = "${p.password}"`,
    `server_host = "${p.server_host}"`,
    `server_port = ${p.server_port}`,
    `server_domain = "${p.server_domain}"`,
    `transport = "${p.transport}"`,
    `heartbeat_interval_secs = ${form.value.heartbeat_interval}`,
    `channels_per_device = ${form.value.channels_per_device}`,
    `media_profile = "${form.value.media_profile}"`,
    `ramp_per_second = ${form.value.ramp_per_second}`,
    `active_ratio = ${(form.value.active_ratio / 100).toFixed(2)}`,
    `video_fps = 25`,
  ];
  // C 档且选了文件才带 video_source。
  if (form.value.media_profile === "C" && form.value.video_source.trim()) {
    lines.push(`video_source = "${form.value.video_source.trim().replace(/\\/g, "\\\\")}"`);
  }
  lines.push(
    `[device_info]`,
    `device_name = "UVP-Sim"`, `manufacturer = "UVP"`,
    `model = "Desktop-Sim"`, `firmware = "0.1.0"`,
  );
  return lines.join("\n");
}

async function startStress() {
  try {
    const toml = buildToml();
    const msg = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
      // 主动上报参数(0=关闭)。
      positionInterval: form.value.position_enabled ? form.value.position_interval : 0,
      alarmInterval: form.value.alarm_enabled ? form.value.alarm_interval : 0,
    });
    running.value = true;
    statusText.value = msg;
    message.success(msg);
  } catch (e) { message.error(String(e)); }
}

async function stopStress() {
  try {
    const msg = await invoke<string>("stop_stress");
    running.value = false;
    statusText.value = "已停止";
    message.info(msg);
  } catch (e) { message.error(String(e)); }
}

async function exportReport() {
  try {
    const path = await invoke<string>("export_report");
    message.success(`报告已导出:${path}`);
  } catch (e) { message.error(String(e)); }
}

// ── 实时监控 ──────────────────────────────────────────────
interface MetricsSnapshot {
  register_attempted: number; register_succeeded: number; register_success_rate: number;
  heartbeat_succeeded: number; heartbeat_failed: number; active_streams: number;
  fail_timeout: number; fail_rejected: number; fail_other: number;
}
const latest = ref<MetricsSnapshot | null>(null);
const MAX_POINTS = 60;
const times: string[] = [];
const regRates: number[] = [];
const hbFails: number[] = [];
const chartEl = ref<HTMLElement | null>(null);
const chart = shallowRef<echarts.ECharts | null>(null);

function pushPoint(s: MetricsSnapshot) {
  times.push(new Date().toTimeString().slice(0, 8));
  regRates.push(+(s.register_success_rate * 100).toFixed(1));
  hbFails.push(s.heartbeat_failed);
  if (times.length > MAX_POINTS) { times.shift(); regRates.shift(); hbFails.shift(); }
}
function updateChart() {
  chart.value?.setOption({
    xAxis: { data: [...times] },
    series: [{ data: [...regRates] }, { data: [...hbFails] }],
  });
}
function initChart() {
  if (!chartEl.value || chart.value) return;
  const c = echarts.init(chartEl.value);
  c.setOption({
    tooltip: { trigger: "axis" },
    legend: { data: ["注册成功率(%)", "心跳失败次数"] },
    xAxis: { type: "category", data: [] },
    yAxis: { type: "value" },
    series: [
      { name: "注册成功率(%)", type: "line", smooth: true, data: [] },
      { name: "心跳失败次数", type: "line", smooth: true, data: [] },
    ],
  });
  chart.value = c;
}

let unlisten: UnlistenFn | null = null;
onMounted(async () => {
  initChart();
  unlisten = await listen<string>("metrics_tick", (event) => {
    try {
      const snap: MetricsSnapshot = JSON.parse(event.payload);
      latest.value = snap;
      pushPoint(snap);
      updateChart();
    } catch { /* 忽略 */ }
  });
});
onUnmounted(() => { unlisten?.(); chart.value?.dispose(); });
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">压力测试</div>
        <div class="page-sub">批量虚拟设备并发注册/心跳/推流/主动上报 · 实时监控上级平台承载</div>
      </div>
      <n-space>
        <span class="status-chip">{{ statusText }}</span>
        <n-button size="small" tertiary @click="exportReport">导出报告</n-button>
      </n-space>
    </div>

    <!-- 场景配置 -->
    <div class="glass-card panel">
      <div class="panel-title">场景配置</div>
      <n-form :model="form" label-placement="top">
        <div class="grid3">
          <n-form-item label="起始设备 ID">
            <n-input v-model:value="form.base_device_id" placeholder="20 位国标 ID" />
          </n-form-item>
          <n-form-item label="设备数量">
            <n-input-number v-model:value="form.count" :min="1" :max="10000" style="width:100%" />
          </n-form-item>
          <n-form-item label="爬坡(台/秒)">
            <n-input-number v-model:value="form.ramp_per_second" :min="0" :max="1000" style="width:100%" />
          </n-form-item>
          <n-form-item label="心跳间隔(秒)">
            <n-input-number v-model:value="form.heartbeat_interval" :min="10" :max="600" style="width:100%" />
          </n-form-item>
          <n-form-item label="每设备通道数">
            <n-input-number v-model:value="form.channels_per_device" :min="1" :max="8" style="width:100%" />
          </n-form-item>
          <n-form-item label="推流占比(%)">
            <n-input-number v-model:value="form.active_ratio" :min="0" :max="100" style="width:100%" />
          </n-form-item>
          <n-form-item label="媒体档">
            <n-select v-model:value="form.media_profile" :options="mediaOptions" />
          </n-form-item>
          <n-form-item v-if="form.media_profile === 'C'" label="视频源文件">
            <n-input-group>
              <n-input v-model:value="form.video_source" placeholder="H.264 裸流路径" />
              <n-button type="primary" ghost @click="pickVideoSource">选择</n-button>
            </n-input-group>
          </n-form-item>
        </div>

        <!-- 主动上报施压 -->
        <div class="sub-title">主动上报施压(设备 → 平台)</div>
        <div class="grid3">
          <n-form-item label="批量定位上报">
            <n-space align="center">
              <n-switch v-model:value="form.position_enabled" />
              <n-input-number v-model:value="form.position_interval" :min="1" :max="600"
                :disabled="!form.position_enabled" style="width:120px" />
              <span class="unit">秒/次</span>
            </n-space>
          </n-form-item>
          <n-form-item label="批量报警上报">
            <n-space align="center">
              <n-switch v-model:value="form.alarm_enabled" />
              <n-input-number v-model:value="form.alarm_interval" :min="1" :max="600"
                :disabled="!form.alarm_enabled" style="width:120px" />
              <span class="unit">秒/次</span>
            </n-space>
          </n-form-item>
        </div>
      </n-form>

      <n-space style="margin-top:8px">
        <n-button type="primary" :disabled="running" @click="startStress">启动压测</n-button>
        <n-button type="error" secondary :disabled="!running" @click="stopStress">停止压测</n-button>
      </n-space>
    </div>

    <!-- 关键指标卡 -->
    <div class="metrics">
      <div class="glass-card metric">
        <div class="m-label">注册成功率</div>
        <div class="m-value" style="color: var(--success)">
          {{ latest ? (latest.register_success_rate * 100).toFixed(1) + "%" : "—" }}
        </div>
      </div>
      <div class="glass-card metric">
        <div class="m-label">注册成功</div>
        <div class="m-value">{{ latest?.register_succeeded ?? "—" }}</div>
      </div>
      <div class="glass-card metric">
        <div class="m-label">活跃推流</div>
        <div class="m-value" style="color: var(--accent)">{{ latest?.active_streams ?? "—" }}</div>
      </div>
      <div class="glass-card metric">
        <div class="m-label">心跳失败</div>
        <div class="m-value" style="color: var(--warning)">{{ latest?.heartbeat_failed ?? "—" }}</div>
      </div>
    </div>

    <!-- 实时曲线 -->
    <div class="glass-card panel">
      <div class="panel-title">实时指标曲线</div>
      <div ref="chartEl" style="height: 300px;" />
    </div>

    <!-- 失败归因 -->
    <div v-if="latest" class="glass-card panel">
      <div class="panel-title">失败归因</div>
      <div class="fails">
        <div class="fail-item"><span>超时</span><b>{{ latest.fail_timeout }}</b></div>
        <div class="fail-item"><span>被拒</span><b>{{ latest.fail_rejected }}</b></div>
        <div class="fail-item"><span>其它</span><b>{{ latest.fail_other }}</b></div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1000px; }
.page-header { display: flex; justify-content: space-between; align-items: flex-start; margin-bottom: 20px; }
.page-title { font-size: 24px; font-weight: 700; color: var(--text-primary); }
.page-sub { font-size: 13px; color: var(--text-tertiary); margin-top: 6px; }
.status-chip {
  padding: 5px 14px; border-radius: 999px; font-size: 12.5px; color: var(--text-secondary);
  background: rgba(255,255,255,0.6); border: 1px solid var(--border-subtle);
}
.panel { padding: 22px 24px; margin-bottom: 18px; }
.panel-title { font-size: 13px; font-weight: 600; color: var(--text-secondary); margin-bottom: 14px; }
.sub-title { font-size: 12.5px; font-weight: 600; color: var(--text-tertiary); margin: 6px 0 12px; }
.grid3 { display: grid; grid-template-columns: repeat(3, 1fr); gap: 0 18px; }
.unit { font-size: 12px; color: var(--text-tertiary); }
.metrics { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 18px; }
.metric { padding: 16px 18px; }
.m-label { font-size: 12px; color: var(--text-tertiary); }
.m-value { font-size: 22px; font-weight: 700; margin-top: 8px; color: var(--text-primary); }
.fails { display: flex; gap: 28px; }
.fail-item { display: flex; flex-direction: column; gap: 4px; }
.fail-item span { font-size: 12px; color: var(--text-tertiary); }
.fail-item b { font-size: 20px; color: var(--text-primary); }
</style>
