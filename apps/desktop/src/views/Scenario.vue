<script setup lang="ts">
// 压力测试工作台：场景编排、生命周期控制、实时指标和报告导出。
import {
  computed,
  nextTick,
  onActivated,
  onMounted,
  onUnmounted,
  ref,
  shallowRef,
} from "vue";
import {
  NAlert,
  NButton,
  NForm,
  NFormItem,
  NInput,
  NInputGroup,
  NInputNumber,
  NSelect,
  NSwitch,
  NTag,
  useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { LineChart } from "echarts/charts";
import { GridComponent, LegendComponent, TooltipComponent } from "echarts/components";
import { init, use as useECharts, type ECharts } from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { usePlatform } from "../platform";

useECharts([LineChart, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer]);

const message = useMessage();
const { active: activePlatform, passwordFor } = usePlatform();

const form = ref({
  base_device_id: "34020000001320000001",
  count: 10,
  heartbeat_interval: 60,
  channels_per_device: 1,
  gb_version: "V2022",
  media_profile: "A",
  bitrate_kbps: 512,
  ramp_per_second: 50,
  active_ratio: 100,
  video_source: "",
  position_enabled: false,
  position_interval: 10,
  alarm_enabled: false,
  alarm_interval: 30,
});

const mediaOptions = [
  { label: "A · 仅信令（最低资源）", value: "A" },
  { label: "B · 轻量 RTP 伪流", value: "B" },
  { label: "C · 真实视频循环推流", value: "C" },
];
const versionOptions = [
  { label: "GB/T 28181-2022", value: "V2022" },
  { label: "GB/T 28181-2016", value: "V2016" },
];

const running = ref(false);
const starting = ref(false);
const stopping = ref(false);
const stopPending = ref(false);
const currentRunId = ref<number | null>(null);
let lastSeenRunId = 0;
let reconcileSequence = 0;
const hasReport = ref(false);
const statusText = ref("就绪");
const statusTone = computed(() => {
  if (starting.value || stopping.value || stopPending.value) return "busy";
  return running.value ? "running" : "idle";
});
const platformLabel = computed(() => {
  const p = activePlatform.value;
  return p ? `${p.name} · ${p.server_host}:${p.server_port} · ${p.transport}` : "未配置目标平台";
});

interface MetricsSnapshot {
  register_attempted: number;
  register_succeeded: number;
  register_failed: number;
  register_success_rate: number;
  heartbeat_succeeded: number;
  heartbeat_failed: number;
  active_streams: number;
  fail_timeout: number;
  fail_rejected: number;
  fail_other: number;
  position_reported: number;
  alarm_reported: number;
}

interface StressState {
  run_id: number;
  running: boolean;
  stopping: boolean;
  reason?: "completed" | "failed" | "stopped" | "stopping";
  error?: string;
  device_count?: number;
  metrics?: MetricsSnapshot;
}

interface MetricsTick {
  run_id: number;
  metrics: MetricsSnapshot;
}

interface StressStatus {
  running: boolean;
  stopping: boolean;
  run_id: number | null;
  device_count: number;
  has_report: boolean;
}

const latest = ref<MetricsSnapshot | null>(null);
const successRate = computed(() =>
  latest.value ? `${(latest.value.register_success_rate * 100).toFixed(1)}%` : "—",
);

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function validateForm(): string | null {
  const p = activePlatform.value;
  if (!p) return "请先在顶栏配置目标平台";
  if (p.transport.toUpperCase() !== "UDP") return "当前 SIP 信令仅支持 UDP，请修改目标平台传输模式";
  if (!/^\d{20}$/.test(form.value.base_device_id)) return "起始设备 ID 必须是 20 位数字";
  if (form.value.count < 1 || form.value.count > 10_000) return "设备数量必须在 1 到 10000 之间";
  if (form.value.media_profile === "C" && !form.value.video_source.trim()) {
    return "C 档真实媒体压测必须选择视频源";
  }
  return null;
}

function buildToml(): string {
  const p = activePlatform.value!;
  const lines = [
    `base_device_id = ${tomlString(form.value.base_device_id)}`,
    `password = ${tomlString(passwordFor(p.id))}`,
    `server_host = ${tomlString(p.server_host.trim())}`,
    `server_port = ${p.server_port}`,
    `server_id = ${tomlString(p.server_id.trim())}`,
    `server_domain = ${tomlString(p.server_domain.trim())}`,
    `transport = ${tomlString(p.transport.toUpperCase())}`,
    `heartbeat_interval_secs = ${form.value.heartbeat_interval}`,
    `channels_per_device = ${form.value.channels_per_device}`,
    `gb_version = ${tomlString(form.value.gb_version)}`,
    `media_profile = ${tomlString(form.value.media_profile)}`,
    `bitrate_kbps = ${form.value.bitrate_kbps}`,
    `ramp_per_second = ${form.value.ramp_per_second}`,
    `active_ratio = ${(form.value.active_ratio / 100).toFixed(2)}`,
    "video_fps = 25",
  ];
  if (form.value.media_profile === "C") {
    lines.push(`video_source = ${tomlString(form.value.video_source.trim())}`);
  }
  lines.push(
    "[device_info]",
    `device_name = ${tomlString("UVP-Sim")}`,
    `manufacturer = ${tomlString("UVP")}`,
    `model = ${tomlString("Desktop-Sim")}`,
    `firmware = ${tomlString("0.1.2")}`,
  );
  return lines.join("\n");
}

async function pickVideoSource() {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) {
    message.error("当前是浏览器调试页面，请切换到 Tauri 桌面窗口后选择视频文件");
    return;
  }
  try {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] },
      ],
    });
    if (typeof picked === "string") form.value.video_source = picked;
  } catch (error) {
    message.error(`选择文件失败：${String(error)}`);
  }
}

function resetSeries() {
  times.length = 0;
  regRates.length = 0;
  hbFails.length = 0;
  latest.value = null;
  hasReport.value = false;
  updateChart();
}

async function startStress() {
  const validation = validateForm();
  if (validation) {
    message.warning(validation);
    return;
  }

  starting.value = true;
  statusText.value = "正在启动";
  try {
    const toml = buildToml();
    await invoke("validate_scenario", { toml });
    resetSeries();
    const result = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
      positionInterval: form.value.position_enabled ? form.value.position_interval : 0,
      alarmInterval: form.value.alarm_enabled ? form.value.alarm_interval : 0,
    });
    // IPC 成功只表示启动请求已提交；以后台真实状态为准，避免覆盖瞬时失败事件。
    const state = await reconcile();
    if (state?.running) {
      message.success(result);
    } else {
      message.warning("启动请求已提交，但任务已结束，请查看运行日志");
    }
  } catch (error) {
    await reconcile();
    if (!running.value) statusText.value = "启动失败";
    message.error(String(error));
  } finally {
    starting.value = false;
  }
}

async function stopStress() {
  stopPending.value = true;
  statusText.value = "正在停止";
  try {
    const result = await invoke<string>("stop_stress");
    await reconcile();
    message.info(result);
  } catch (error) {
    await reconcile();
    message.error(String(error));
  } finally {
    stopPending.value = false;
  }
}

async function exportReport() {
  try {
    const path = await invoke<string>("export_report");
    message.success(`报告已导出：${path}`, { duration: 6000 });
  } catch (error) {
    message.error(String(error));
  }
}

async function reconcile(): Promise<StressStatus | null> {
  const requestId = ++reconcileSequence;
  try {
    const state = await invoke<StressStatus>("get_stress_status");
    if (requestId !== reconcileSequence) return null;
    if (state.run_id !== null) {
      if (state.run_id < lastSeenRunId) return null;
      lastSeenRunId = Math.max(lastSeenRunId, state.run_id);
    }

    running.value = state.running;
    stopping.value = state.stopping;
    currentRunId.value = state.run_id;
    hasReport.value = state.has_report;
    if (state.running) {
      statusText.value = state.stopping
        ? `正在停止 · ${state.device_count} 台`
        : `运行中 · ${state.device_count} 台`;
      try {
        const snapshot = JSON.parse(await invoke<string>("get_metrics")) as MetricsSnapshot;
        if (requestId !== reconcileSequence || currentRunId.value !== state.run_id) return null;
        latest.value = snapshot;
      } catch {
        // 指标任务可能刚创建或刚退出，等待事件/下一次状态对账即可。
      }
    } else if (statusText.value !== "异常结束") {
      statusText.value = state.has_report ? "已停止" : "就绪";
    }
    return requestId === reconcileSequence ? state : null;
  } catch (error) {
    if (requestId !== reconcileSequence) return null;
    statusText.value = "引擎不可用";
    console.error("压测状态对账失败", error);
    return null;
  }
}

const MAX_POINTS = 60;
const times: string[] = [];
const regRates: number[] = [];
const hbFails: number[] = [];
const chartEl = ref<HTMLElement | null>(null);
const chart = shallowRef<ECharts | null>(null);
let resizeObserver: ResizeObserver | null = null;

function pushPoint(snapshot: MetricsSnapshot) {
  times.push(new Date().toLocaleTimeString("zh-CN", { hour12: false }));
  regRates.push(Number((snapshot.register_success_rate * 100).toFixed(1)));
  hbFails.push(snapshot.heartbeat_failed);
  if (times.length > MAX_POINTS) {
    times.shift();
    regRates.shift();
    hbFails.shift();
  }
}

function updateChart() {
  chart.value?.setOption({
    xAxis: { data: [...times] },
    series: [{ data: [...regRates] }, { data: [...hbFails] }],
  });
}

function initChart() {
  if (!chartEl.value || chart.value) return;
  const instance = init(chartEl.value, undefined, { renderer: "canvas" });
  instance.setOption({
    animationDuration: 300,
    color: ["#3884ff", "#e89320"],
    tooltip: { trigger: "axis", backgroundColor: "rgba(14,39,71,.92)", borderWidth: 0, textStyle: { color: "#fff" } },
    legend: { right: 4, top: 0, textStyle: { color: "#627797" } },
    grid: { left: 44, right: 48, top: 46, bottom: 28 },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: [],
      axisLine: { lineStyle: { color: "rgba(76,99,133,.2)" } },
      axisLabel: { color: "#8fa1bd" },
    },
    yAxis: [
      {
        type: "value",
        min: 0,
        max: 100,
        axisLabel: { formatter: "{value}%", color: "#8fa1bd" },
        splitLine: { lineStyle: { color: "rgba(76,99,133,.08)" } },
      },
      {
        type: "value",
        min: 0,
        axisLabel: { color: "#8fa1bd" },
        splitLine: { show: false },
      },
    ],
    series: [
      {
        name: "注册成功率",
        type: "line",
        yAxisIndex: 0,
        smooth: true,
        showSymbol: false,
        areaStyle: { color: "rgba(56,132,255,.10)" },
        data: [],
      },
      {
        name: "心跳失败累计",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
        data: [],
      },
    ],
  });
  chart.value = instance;
  resizeObserver = new ResizeObserver(() => instance.resize());
  resizeObserver.observe(chartEl.value);
}

let unlistenMetrics: UnlistenFn | null = null;
let unlistenState: UnlistenFn | null = null;
onMounted(async () => {
  initChart();
  unlistenMetrics = await listen<MetricsTick>("metrics_tick", (event) => {
    const tick = event.payload;
    if (currentRunId.value !== tick.run_id) return;
    latest.value = tick.metrics;
    hasReport.value = true;
    pushPoint(tick.metrics);
    updateChart();
  });
  unlistenState = await listen<StressState>("stress_state", (event) => {
    const state = event.payload;
    // run_id 单调递增：即使当前空闲，也拒绝旧运行的迟到 running 事件。
    if (state.run_id < lastSeenRunId) return;
    if (state.running && currentRunId.value === null && state.run_id === lastSeenRunId) return;
    if (currentRunId.value !== null && currentRunId.value !== state.run_id) {
      if (state.run_id < currentRunId.value) return;
    }

    // 已接受的生命周期事件使所有在途 reconcile 响应失效。
    reconcileSequence += 1;
    if (state.run_id > lastSeenRunId) lastSeenRunId = state.run_id;
    if (state.running) currentRunId.value = state.run_id;

    running.value = state.running;
    stopping.value = state.stopping;
    if (state.metrics) {
      latest.value = state.metrics;
      hasReport.value = true;
    }

    if (state.running) {
      statusText.value = state.stopping
        ? `正在停止 · ${state.device_count ?? form.value.count} 台`
        : `运行中 · ${state.device_count ?? form.value.count} 台`;
      return;
    }

    currentRunId.value = null;
    if (state.reason === "failed") {
      statusText.value = "异常结束";
      message.error(`压测异常结束：${state.error ?? "未知错误"}`);
    } else {
      statusText.value = state.reason === "completed" ? "已完成" : "已停止";
    }
  });
  await reconcile();
});

onActivated(async () => {
  await reconcile();
  await nextTick();
  chart.value?.resize();
});

onUnmounted(() => {
  unlistenMetrics?.();
  unlistenState?.();
  resizeObserver?.disconnect();
  chart.value?.dispose();
});
</script>

<template>
  <div class="page">
    <header class="page-header">
      <div>
        <div class="eyebrow">LOAD TESTING</div>
        <h1>压力测试工作台</h1>
        <p>批量模拟注册、心跳、媒体与主动上报，实时观察上级平台承载能力。</p>
      </div>
      <div class="header-actions">
        <div class="status-chip" :class="statusTone">
          <span class="status-dot" />
          {{ statusText }}
        </div>
        <n-button size="small" secondary :disabled="!hasReport" @click="exportReport">导出脱敏报告</n-button>
      </div>
    </header>

    <n-alert v-if="activePlatform?.transport === 'Tcp'" type="warning" :show-icon="false" class="notice">
      当前 SIP 信令层仅支持 UDP。请在顶栏将目标平台传输模式改为 UDP 后再启动。
    </n-alert>

    <section class="glass-card panel config-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title">场景配置</div>
          <div class="panel-sub">{{ platformLabel }}</div>
        </div>
        <n-tag size="small" round :bordered="false" type="info">上限 10,000 台</n-tag>
      </div>

      <n-form :model="form" label-placement="top" :disabled="running || starting">
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
              <n-button type="primary" ghost @click="pickVideoSource">选择文件</n-button>
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
        <n-button type="primary" size="large" :loading="starting" :disabled="running || stopping || stopPending" @click="startStress">
          启动压测
        </n-button>
        <n-button type="error" secondary size="large" :loading="stopPending" :disabled="!running || starting || stopping" @click="stopStress">
          停止压测
        </n-button>
        <span>配置会先经过后端语义校验，报告自动隐藏认证密码。</span>
      </div>
    </section>

    <section class="metric-grid">
      <div class="glass-card metric primary">
        <span>注册成功率</span>
        <strong>{{ successRate }}</strong>
        <small>{{ latest?.register_succeeded ?? 0 }} / {{ latest?.register_attempted ?? 0 }} 次</small>
      </div>
      <div class="glass-card metric">
        <span>当前活跃流</span>
        <strong>{{ latest?.active_streams ?? 0 }}</strong>
        <small>实时 RTP 会话</small>
      </div>
      <div class="glass-card metric">
        <span>心跳成功</span>
        <strong>{{ latest?.heartbeat_succeeded ?? 0 }}</strong>
        <small>失败 {{ latest?.heartbeat_failed ?? 0 }} 次</small>
      </div>
      <div class="glass-card metric">
        <span>主动上报</span>
        <strong>{{ (latest?.position_reported ?? 0) + (latest?.alarm_reported ?? 0) }}</strong>
        <small>位置 {{ latest?.position_reported ?? 0 }} · 报警 {{ latest?.alarm_reported ?? 0 }}</small>
      </div>
    </section>

    <section class="monitor-grid">
      <div class="glass-card panel chart-panel">
        <div class="panel-head compact">
          <div>
            <div class="panel-title">实时趋势</div>
            <div class="panel-sub">最近 60 个采样点</div>
          </div>
          <span class="live-indicator" :class="{ on: running }">{{ running ? "LIVE" : "PAUSED" }}</span>
        </div>
        <div ref="chartEl" class="chart" />
      </div>

      <div class="glass-card panel failure-panel">
        <div class="panel-title">失败归因</div>
        <div class="failure-total">{{ latest?.register_failed ?? 0 }}</div>
        <div class="failure-caption">注册失败总数</div>
        <div class="failure-list">
          <div><span><i class="timeout" />事务超时</span><b>{{ latest?.fail_timeout ?? 0 }}</b></div>
          <div><span><i class="rejected" />平台拒绝</span><b>{{ latest?.fail_rejected ?? 0 }}</b></div>
          <div><span><i class="other" />网络或其它</span><b>{{ latest?.fail_other ?? 0 }}</b></div>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.page { width: min(1320px, 100%); margin: 0 auto; }
.page-header { display: flex; justify-content: space-between; align-items: flex-start; gap: 24px; margin: 18px 0 22px; }
.eyebrow { color: var(--accent); font-size: 11px; font-weight: 800; letter-spacing: 1.8px; }
h1 { margin: 5px 0 0; color: var(--text-primary); font-size: clamp(24px, 2.5vw, 32px); letter-spacing: -0.6px; }
.page-header p { margin: 8px 0 0; color: var(--text-tertiary); font-size: 13px; line-height: 1.6; }
.header-actions { display: flex; align-items: center; gap: 10px; flex: 0 0 auto; }
.status-chip { display: inline-flex; align-items: center; gap: 8px; min-height: 30px; padding: 0 13px; border: 1px solid var(--border-default); border-radius: 999px; background: rgba(255,255,255,.55); color: var(--text-secondary); font-size: 12px; font-weight: 650; }
.status-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--text-tertiary); }
.status-chip.running { color: var(--success); border-color: rgba(8,179,136,.25); background: rgba(8,179,136,.08); }
.status-chip.running .status-dot { background: var(--success); box-shadow: 0 0 0 4px rgba(8,179,136,.12); animation: pulse 1.8s infinite; }
.status-chip.busy { color: var(--warning); }
.status-chip.busy .status-dot { background: var(--warning); }
.notice { margin-bottom: 16px; border-radius: var(--radius-md); }
.panel { padding: 22px 24px; }
.config-panel { margin-bottom: 18px; }
.panel-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 18px; }
.panel-head.compact { margin-bottom: 4px; }
.panel-title { color: var(--text-primary); font-size: 14px; font-weight: 750; }
.panel-sub { margin-top: 4px; color: var(--text-tertiary); font-size: 11.5px; }
.form-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); column-gap: 18px; }
.span-2 { grid-column: span 2; }
.section-divider { display: flex; align-items: baseline; gap: 12px; margin: 3px 0 12px; padding-top: 15px; border-top: 1px solid var(--border-default); }
.section-divider span { color: var(--text-secondary); font-size: 12.5px; font-weight: 700; }
.section-divider small { color: var(--text-tertiary); font-size: 11.5px; }
.report-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
.report-option { display: grid; grid-template-columns: minmax(120px, 1fr) auto 100px auto; align-items: center; gap: 10px; padding: 12px 14px; border: 1px solid var(--border-default); border-radius: var(--radius-md); background: rgba(255,255,255,.32); }
.report-option div { display: flex; flex-direction: column; gap: 2px; }
.report-option b { color: var(--text-primary); font-size: 12.5px; }
.report-option span, .report-option em { color: var(--text-tertiary); font-size: 11px; font-style: normal; }
.action-row { display: flex; align-items: center; gap: 10px; margin-top: 20px; }
.action-row span { margin-left: auto; color: var(--text-tertiary); font-size: 11.5px; }
.metric-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 14px; margin-bottom: 18px; }
.metric { position: relative; overflow: hidden; padding: 18px 20px; }
.metric::after { content: ""; position: absolute; width: 84px; height: 84px; right: -30px; top: -35px; border-radius: 50%; background: var(--accent-dim); }
.metric.primary { border-color: rgba(56,132,255,.28); background: linear-gradient(145deg, rgba(255,255,255,.72), rgba(217,231,255,.46)); }
.metric > span { display: block; color: var(--text-tertiary); font-size: 11.5px; }
.metric strong { display: block; margin-top: 7px; color: var(--text-primary); font-size: 27px; line-height: 1; font-variant-numeric: tabular-nums; }
.metric.primary strong { color: var(--accent-deep); }
.metric small { display: block; margin-top: 8px; color: var(--text-tertiary); font-size: 11px; }
.monitor-grid { display: grid; grid-template-columns: minmax(0, 1fr) 260px; gap: 18px; }
.chart-panel { min-width: 0; }
.chart { width: 100%; height: 300px; }
.live-indicator { padding: 3px 8px; border-radius: 5px; background: rgba(143,161,189,.12); color: var(--text-tertiary); font: 700 10px/1.4 "SF Mono", Menlo, monospace; letter-spacing: .7px; }
.live-indicator.on { background: rgba(8,179,136,.10); color: var(--success); }
.failure-panel { display: flex; flex-direction: column; }
.failure-total { margin-top: 24px; color: var(--text-primary); font-size: 40px; font-weight: 750; line-height: 1; }
.failure-caption { margin-top: 5px; color: var(--text-tertiary); font-size: 11.5px; }
.failure-list { display: flex; flex-direction: column; gap: 13px; margin-top: auto; padding-top: 30px; }
.failure-list > div { display: flex; justify-content: space-between; align-items: center; color: var(--text-secondary); font-size: 12px; }
.failure-list span { display: inline-flex; align-items: center; gap: 8px; }
.failure-list i { width: 7px; height: 7px; border-radius: 50%; background: var(--text-tertiary); }
.failure-list i.timeout { background: var(--warning); }
.failure-list i.rejected { background: var(--error); }
.failure-list i.other { background: var(--violet); }
.failure-list b { color: var(--text-primary); font-size: 14px; font-variant-numeric: tabular-nums; }
@keyframes pulse { 50% { box-shadow: 0 0 0 7px rgba(8,179,136,0); } }
@media (max-width: 1120px) {
  .form-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .metric-grid { grid-template-columns: repeat(2, 1fr); }
}
@media (max-width: 820px) {
  .page-header { flex-direction: column; }
  .header-actions { width: 100%; justify-content: space-between; }
  .report-grid, .monitor-grid { grid-template-columns: 1fr; }
  .failure-list { margin-top: 0; }
}
@media (max-width: 620px) {
  .panel { padding: 18px; }
  .form-grid, .metric-grid { grid-template-columns: 1fr; }
  .span-2 { grid-column: span 1; }
  .report-option { grid-template-columns: 1fr auto; }
  .report-option :deep(.n-input-number), .report-option em { display: none; }
  .action-row { align-items: stretch; flex-direction: column; }
  .action-row span { margin-left: 0; }
}
</style>
