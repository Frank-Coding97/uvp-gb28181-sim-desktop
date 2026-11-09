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
  watch,
} from "vue";
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
  useMessage,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { LineChart } from "echarts/charts";
import { GridComponent, LegendComponent, TooltipComponent } from "echarts/components";
import { init, use as useECharts, type ECharts } from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { usePlatform, type PlatformProfile } from "../platform";

useECharts([LineChart, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer]);

const message = useMessage();
const { active: activePlatform, passwordFor } = usePlatform();

interface ScenarioValues {
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

const form = ref<ScenarioValues>({
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

// 注册成功率百分比数字，用于进度环
const successRateNum = computed(() =>
  latest.value ? latest.value.register_success_rate * 100 : 0,
);
// SVG 进度环描边长
const RING_R = 32;
const RING_C = 2 * Math.PI * RING_R;
const ringDash = computed(() => {
  const pct = Math.min(successRateNum.value, 100) / 100;
  return `${(RING_C * pct).toFixed(1)} ${RING_C}`;
});
const ringColor = computed(() => {
  const v = successRateNum.value;
  if (v >= 90) return "var(--success)";
  if (v >= 60) return "var(--warning)";
  return "var(--error)";
});

// ── 快速预设 ──────────────────────────────────────────────
const presets = [
  { label: "10台", count: 10,   ramp: 0,   media: "A" },
  { label: "100台", count: 100,  ramp: 20,  media: "A" },
  { label: "500台", count: 500,  ramp: 50,  media: "B" },
  { label: "1000台", count: 1000, ramp: 100, media: "B" },
  { label: "10000台", count: 10000, ramp: 200, media: "B" },
] as const;
function applyPreset(p: (typeof presets)[number]) {
  form.value.count = p.count;
  form.value.ramp_per_second = p.ramp;
  form.value.media_profile = p.media;
}
// 选中态按「当前配置是否与该预设完全一致」推导：用户手动改了数量/爬坡/档位后
// 高亮会自动消失，避免出现「配置已经不是预设了却还亮着」的假选中。
const activePreset = computed(() => {
  const { count, ramp_per_second, media_profile } = form.value;
  const hit = presets.find(
    (p) => p.count === count && p.ramp === ramp_per_second && p.media === media_profile,
  );
  return hit ? hit.label : null;
});

// 配置面板折叠（运行时自动折叠，可手动展开）
const configCollapsed = ref(false);
watch(running, (v) => { if (v) configCollapsed.value = true; });

// ── 运行计时 ──────────────────────────────────────────────
const elapsedMs = ref(0);
let elapsedStart = 0;
let elapsedTimer: number | undefined;
const elapsedStr = computed(() => {
  const s = Math.floor(elapsedMs.value / 1000);
  const h = String(Math.floor(s / 3600)).padStart(2, "0");
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  return `${h}:${m}:${ss}`;
});
watch(running, (v) => {
  if (v) {
    elapsedStart = Date.now() - elapsedMs.value;
    elapsedTimer = window.setInterval(() => {
      elapsedMs.value = Date.now() - elapsedStart;
    }, 1000);
  } else {
    if (elapsedTimer) { clearInterval(elapsedTimer); elapsedTimer = undefined; }
  }
});

// ── TPS（10 秒滑动窗口）──────────────────────────────────
let tpsSamples: { t: number; n: number }[] = [];
const tps = ref(0);
function updateTps(succeeded: number) {
  const now = Date.now();
  tpsSamples.push({ t: now, n: succeeded });
  tpsSamples = tpsSamples.filter((s) => now - s.t < 10_000);
  if (tpsSamples.length >= 2) {
    // 不用 Array.prototype.at：tsconfig 的 lib 是 ES2021，声明里没有 at()。
    const first = tpsSamples[0];
    const last = tpsSamples[tpsSamples.length - 1];
    const dt = (last.t - first.t) / 1000;
    const dn = last.n - first.n;
    tps.value = dt > 0 ? Math.round(dn / dt) : 0;
  }
}

// ── 设备点阵 ─────────────────────────────────────────────
const MAX_DOTS = 300;
type DotState = "idle" | "ok" | "failed";
const dotGrid = computed((): DotState[] => {
  const total = Math.max(form.value.count, 1);
  const succeeded = latest.value?.register_succeeded ?? 0;
  const failed    = latest.value?.register_failed    ?? 0;
  const n = Math.min(total, MAX_DOTS);
  return Array.from({ length: n }, (_, i) => {
    const idx = Math.floor((i / n) * total);
    if (idx < failed)    return "failed";
    if (idx < succeeded) return "ok";
    return "idle";
  });
});

function tomlString(value: string): string {
  return JSON.stringify(value);
}

function validateForm(): string | null {
  const p = activePlatform.value;
  if (!p) return "请先在顶栏配置目标平台";
  if (!passwordFor(p.id).trim()) return "请输入本次注册密码";
  if (!/^\d{20}$/.test(form.value.base_device_id)) return "起始设备 ID 必须是 20 位数字";
  if (form.value.count < 1 || form.value.count > 10_000) return "设备数量必须在 1 到 10000 之间";
  if (form.value.media_profile === "C" && !form.value.video_source.trim()) {
    return "C 档真实媒体压测必须选择视频源";
  }
  return null;
}

function buildScenarioToml(
  profile: PlatformProfile,
  password: string,
  values: ScenarioValues,
): string {
  const lines = [
    `base_device_id = ${tomlString(values.base_device_id)}`,
    `password = ${tomlString(password)}`,
    `server_host = ${tomlString(profile.server_host.trim())}`,
    `server_port = ${profile.server_port}`,
    `server_id = ${tomlString(profile.server_id.trim())}`,
    `server_domain = ${tomlString(profile.server_domain.trim())}`,
    `transport = ${tomlString(profile.transport.toUpperCase())}`,
    `heartbeat_interval_secs = ${values.heartbeat_interval}`,
    `channels_per_device = ${values.channels_per_device}`,
    `gb_version = ${tomlString(values.gb_version)}`,
    `media_profile = ${tomlString(values.media_profile)}`,
    `bitrate_kbps = ${values.bitrate_kbps}`,
    `ramp_per_second = ${values.ramp_per_second}`,
    `active_ratio = ${(values.active_ratio / 100).toFixed(2)}`,
    "video_fps = 25",
  ];
  if (values.media_profile === "C") {
    lines.push(`video_source = ${tomlString(values.video_source.trim())}`);
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
  activeStreamsHistory.length = 0;
  tpsSamples = [];
  tps.value = 0;
  elapsedMs.value = 0;
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
    const profile = activePlatform.value!;
    const toml = buildScenarioToml(profile, passwordFor(profile.id), form.value);
    await invoke("validate_scenario", { toml });
    resetSeries();
    const result = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
      positionInterval: form.value.position_enabled ? form.value.position_interval : 0,
      alarmInterval: form.value.alarm_enabled ? form.value.alarm_interval : 0,
    });
    const state = await reconcile();
    if (state?.running) {
      message.success(result);
    } else {
      message.warning("启动请求已提交，但任务已结束，请查看日志");
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

// 导出压测报告：后台渲染一份自包含 HTML 报表(带图表)，浏览器打开即可看，
// 也可直接打印成 PDF。同目录会再落一份同名 .json 作为机器可读的原始数据。
async function exportReport() {
  try {
    let targetPath: string | undefined;
    if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
      const now = new Date();
      const pad = (n: number) => String(n).padStart(2, "0");
      // 平台名可能带 / : 等非法字符，做一次替换，避免对话框默认名不合法。
      const platform = (activePlatform.value?.name ?? "压测").replace(/[\\/:*?"<>|]/g, "-");
      const defaultPath =
        `压测报告-${platform}-${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}` +
        `-${pad(now.getHours())}${pad(now.getMinutes())}.html`;
      const picked = await saveDialog({
        defaultPath,
        filters: [{ name: "压测报告（网页，可打印为 PDF）", extensions: ["html"] }],
      });
      // 用户取消对话框时不导出，也不弹错误。
      if (typeof picked !== "string") return;
      targetPath = picked;
    }
    // 显式传 null 而不是省掉键：Tauri 对缺失参数比 null 更严格。
    const path = await invoke<string>("export_report", { targetPath: targetPath ?? null });
    message.success(`报告已导出：${path}`, { duration: 8000 });
  } catch (error) {
    message.error(`导出失败：${String(error)}`);
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
const activeStreamsHistory: number[] = [];
const chartEl = ref<HTMLElement | null>(null);
const chart = shallowRef<ECharts | null>(null);
let resizeObserver: ResizeObserver | null = null;

function pushPoint(snapshot: MetricsSnapshot) {
  times.push(new Date().toLocaleTimeString("zh-CN", { hour12: false }));
  regRates.push(Number((snapshot.register_success_rate * 100).toFixed(1)));
  hbFails.push(snapshot.heartbeat_failed);
  activeStreamsHistory.push(snapshot.active_streams);
  if (times.length > MAX_POINTS) {
    times.shift();
    regRates.shift();
    hbFails.shift();
    activeStreamsHistory.shift();
  }
  updateTps(snapshot.register_succeeded);
}

function updateChart() {
  chart.value?.setOption({
    xAxis: { data: [...times] },
    series: [
      { data: [...regRates] },
      { data: [...hbFails] },
      { data: [...activeStreamsHistory] },
    ],
  });
}

function initChart() {
  if (!chartEl.value || chart.value) return;
  const instance = init(chartEl.value, undefined, { renderer: "canvas" });
  instance.setOption({
    animationDuration: 300,
    color: ["#3884ff", "#e89320", "#08b388"],
    tooltip: {
      trigger: "axis",
      backgroundColor: "rgba(14,39,71,.94)",
      borderWidth: 0,
      borderRadius: 10,
      textStyle: { color: "#e8f0fc", fontSize: 12 },
    },
    legend: { right: 8, top: 0, textStyle: { color: "#627797", fontSize: 12 } },
    grid: { left: 46, right: 54, top: 44, bottom: 28 },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: [],
      axisLine: { lineStyle: { color: "rgba(76,99,133,.15)" } },
      axisLabel: { color: "#8fa1bd", fontSize: 11 },
    },
    yAxis: [
      {
        type: "value",
        min: 0,
        max: 100,
        axisLabel: { formatter: "{value}%", color: "#8fa1bd", fontSize: 11 },
        splitLine: { lineStyle: { color: "rgba(76,99,133,.07)" } },
      },
      {
        type: "value",
        min: 0,
        axisLabel: { color: "#8fa1bd", fontSize: 11 },
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
        lineStyle: { width: 2 },
        areaStyle: {
          color: {
            type: "linear",
            x: 0, y: 0, x2: 0, y2: 1,
            colorStops: [
              { offset: 0, color: "rgba(56,132,255,.20)" },
              { offset: 1, color: "rgba(56,132,255,.01)" },
            ],
          },
        },
        data: [],
      },
      {
        name: "心跳失败累计",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
        lineStyle: { width: 2, type: "dashed" },
        data: [],
      },
      {
        name: "活跃流",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
        lineStyle: { width: 2 },
        areaStyle: {
          color: {
            type: "linear",
            x: 0, y: 0, x2: 0, y2: 1,
            colorStops: [
              { offset: 0, color: "rgba(8,179,136,.18)" },
              { offset: 1, color: "rgba(8,179,136,.01)" },
            ],
          },
        },
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
    if (state.run_id < lastSeenRunId) return;
    if (state.running && currentRunId.value === null && state.run_id === lastSeenRunId) return;
    if (currentRunId.value !== null && currentRunId.value !== state.run_id) {
      if (state.run_id < currentRunId.value) return;
    }

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
  if (elapsedTimer) clearInterval(elapsedTimer);
});
</script>

<template>
  <div class="page page-wrap">

    <!-- ── 状态条：常驻单行，承载状态、运行统计与报告导出 ── -->
    <div class="status-bar" :class="statusTone">
      <span class="sb-dot" />
      <span class="sb-text">{{ statusText }}</span>
      <template v-if="running">
        <span class="sb-sep">·</span>
        <span class="sb-stat">运行 <b>{{ elapsedStr }}</b></span>
        <span class="sb-sep">·</span>
        <span class="sb-stat">TPS <b>{{ tps }}</b> reg/s</span>
      </template>
      <span class="sb-platform">{{ platformLabel }}</span>
      <n-button
        size="small"
        secondary
        :disabled="!hasReport"
        :title="hasReport ? '导出一份带图表的网页报告，可直接打印为 PDF' : '先跑一次压测才能导出报告'"
        @click="exportReport"
      >导出报告</n-button>
    </div>

    <!-- ── 主体：左配置 / 右监控 ── -->
    <div class="workspace">

      <!-- 左：配置面板 -->
      <aside class="config-col">
        <div class="glass-card config-card" :class="{ 'is-collapsed': running && configCollapsed }">

          <!-- 卡片头 + 折叠切换 -->
          <div class="card-head" @click="running && (configCollapsed = !configCollapsed)" :class="{ clickable: running }">
            <div>
              <div class="card-title">场景配置</div>
              <div class="card-sub">{{ platformLabel }}</div>
            </div>
            <div class="head-right">
              <n-tag size="small" round :bordered="false" type="info">上限 10,000 台</n-tag>
              <span v-if="running" class="collapse-toggle">{{ configCollapsed ? '▸ 展开' : '▾ 收起' }}</span>
            </div>
          </div>

          <!-- 运行时折叠摘要 -->
          <transition name="fade">
            <div v-if="running && configCollapsed" class="config-summary">
              <span><b>{{ form.count }}</b> 台</span>
              <span>爬坡 <b>{{ form.ramp_per_second }}</b>/s</span>
              <span>档位 <b>{{ form.media_profile }}</b></span>
              <span>{{ form.gb_version }}</span>
            </div>
          </transition>

          <!-- 完整配置表单（卡片内独立滚动，操作条始终可见） -->
          <div class="config-scroll">
            <transition name="expand">
              <div v-show="!running || !configCollapsed">
              <!-- 快速预设 -->
              <div class="preset-row">
                <span class="preset-label">快速预设</span>
                <button
                  v-for="p in presets"
                  :key="p.label"
                  class="preset-btn"
                  :class="{ active: activePreset === p.label }"
                  :aria-pressed="activePreset === p.label"
                  :disabled="running || starting"
                  @click="applyPreset(p)"
                >{{ p.label }}</button>
              </div>

              <n-form :model="form" label-placement="top" :disabled="running || starting">
                <!-- 基础参数 -->
                <div class="section-label">基础参数</div>
                <div class="form-grid">
                  <n-form-item label="起始设备 ID" class="span-2">
                    <n-input v-model:value="form.base_device_id" maxlength="20" placeholder="20 位国标设备编号" />
                  </n-form-item>
                  <n-form-item label="设备数量">
                    <n-input-number v-model:value="form.count" :min="1" :max="10000" style="width:100%" />
                  </n-form-item>
                  <n-form-item label="爬坡速率（台/秒）">
                    <n-input-number v-model:value="form.ramp_per_second" :min="0" :max="1000" style="width:100%" />
                  </n-form-item>
                  <n-form-item label="协议版本">
                    <n-select v-model:value="form.gb_version" :options="versionOptions" />
                  </n-form-item>
                  <n-form-item label="心跳间隔（秒）">
                    <n-input-number v-model:value="form.heartbeat_interval" :min="1" :max="600" style="width:100%" />
                  </n-form-item>
                  <n-form-item label="每设备通道数">
                    <n-input-number v-model:value="form.channels_per_device" :min="1" :max="8" style="width:100%" />
                  </n-form-item>
                  <n-form-item label="推流占比（%）">
                    <n-input-number
                      v-model:value="form.active_ratio" :min="0" :max="100"
                      :format="(v: number | null) => `${v ?? 0}%`"
                      style="width:100%"
                    />
                  </n-form-item>
                </div>

                <!-- 媒体档位 -->
                <div class="section-label">媒体档位</div>
                <div class="form-grid">
                  <n-form-item label="档位" class="span-2">
                    <n-select v-model:value="form.media_profile" :options="mediaOptions" />
                  </n-form-item>
                  <n-form-item v-if="form.media_profile === 'B'" label="伪流码率（kbps）" class="span-2">
                    <n-input-number v-model:value="form.bitrate_kbps" :min="64" :max="8192" style="width:100%" />
                  </n-form-item>
                  <n-form-item v-if="form.media_profile === 'C'" label="视频源文件" class="span-2">
                    <n-input-group>
                      <n-input v-model:value="form.video_source" placeholder="H.264/H.265 裸流或 MP4 容器" />
                      <n-button type="primary" ghost @click="pickVideoSource">选择</n-button>
                    </n-input-group>
                  </n-form-item>
                </div>

                <!-- 主动上报 -->
                <div class="section-label">主动上报施压</div>
                <div class="report-grid">
                  <div class="report-row">
                    <div class="rr-info">
                      <b>移动位置</b><small>MobilePosition</small>
                    </div>
                    <n-switch v-model:value="form.position_enabled" size="small" />
                    <div class="rr-interval" :class="{ disabled: !form.position_enabled }">
                      <n-input-number v-model:value="form.position_interval" :min="1" :max="600" :disabled="!form.position_enabled" size="small" style="width:72px" />
                      <span>秒/次</span>
                    </div>
                  </div>
                  <div class="report-row">
                    <div class="rr-info">
                      <b>报警事件</b><small>Alarm</small>
                    </div>
                    <n-switch v-model:value="form.alarm_enabled" size="small" />
                    <div class="rr-interval" :class="{ disabled: !form.alarm_enabled }">
                      <n-input-number v-model:value="form.alarm_interval" :min="1" :max="600" :disabled="!form.alarm_enabled" size="small" style="width:72px" />
                      <span>秒/次</span>
                    </div>
                  </div>
                </div>
              </n-form>
              </div>
            </transition>
          </div>

          <!-- 常驻操作条：贴在卡片底部，运行中收起配置也不消失 -->
          <div class="action-bar">
            <div class="action-row">
              <n-button type="primary" size="large" style="flex:1" :loading="starting" :disabled="running || stopping || stopPending" @click="startStress">
                启动压测
              </n-button>
              <n-button type="error" secondary size="large" style="flex:1" :loading="stopPending" :disabled="!running || starting || stopping" @click="stopStress">
                停止压测
              </n-button>
            </div>
            <p class="action-hint">配置经后端语义校验 · 报告自动隐藏认证密码</p>
          </div>
        </div>
      </aside>

      <!-- 右：监控面板 -->
      <div class="monitor-col">

        <!-- 核心指标行 -->
        <div class="metric-row">
          <!-- 注册成功率进度环 -->
          <div class="glass-card kpi-ring">
            <div class="ring-wrap">
              <svg class="ring-svg" viewBox="0 0 80 80" aria-hidden="true">
                <circle cx="40" cy="40" r="32" class="ring-track" />
                <circle cx="40" cy="40" r="32" class="ring-fill" :stroke="ringColor" :stroke-dasharray="ringDash" />
              </svg>
              <div class="ring-center">
                <strong>{{ successRate }}</strong>
                <span>注册成功率</span>
              </div>
            </div>
            <div class="kpi-sub">{{ latest?.register_succeeded ?? 0 }} / {{ latest?.register_attempted ?? 0 }}</div>
          </div>

          <!-- 活跃流 -->
          <div class="glass-card kpi-tile">
            <span class="kpi-label">活跃流</span>
            <strong class="kpi-val" :class="{ positive: (latest?.active_streams ?? 0) > 0 }">
              {{ latest?.active_streams ?? 0 }}
            </strong>
            <span class="kpi-hint">RTP 推流会话</span>
          </div>

          <!-- 心跳成功 -->
          <div class="glass-card kpi-tile">
            <span class="kpi-label">心跳成功</span>
            <strong class="kpi-val">{{ latest?.heartbeat_succeeded ?? 0 }}</strong>
            <span class="kpi-hint" :class="{ warn: (latest?.heartbeat_failed ?? 0) > 0 }">
              失败 {{ latest?.heartbeat_failed ?? 0 }}
            </span>
          </div>

          <!-- TPS + 运行时长 -->
          <div class="glass-card kpi-tile tps-tile">
            <span class="kpi-label">注册速率</span>
            <strong class="kpi-val">{{ tps }}<small> /s</small></strong>
            <span class="kpi-hint mono">{{ elapsedStr }}</span>
          </div>
        </div>

        <!-- 设备点阵 -->
        <div class="glass-card dot-card">
          <div class="dot-head">
            <span class="card-title">设备注册进度</span>
            <div class="dot-legend">
              <span><i class="dot-i ok"/>已注册</span>
              <span><i class="dot-i failed"/>失败</span>
              <span><i class="dot-i idle"/>待注册</span>
              <span class="dot-count-label">{{ latest?.register_succeeded ?? 0 }} / {{ form.count }} 台</span>
            </div>
          </div>
          <div class="dot-grid" :class="{ active: running }">
            <span
              v-for="(state, i) in dotGrid"
              :key="i"
              class="dot-cell"
              :class="state"
            />
          </div>
          <!-- 整体进度条 -->
          <div class="reg-progress-bar">
            <div
              class="reg-progress-fill"
              :style="{ width: `${Math.min((latest?.register_succeeded ?? 0) / Math.max(form.count, 1) * 100, 100)}%`, background: ringColor }"
            />
          </div>
        </div>

        <!-- 趋势图 -->
        <div class="glass-card chart-card">
          <div class="chart-head">
            <div>
              <div class="card-title">实时趋势</div>
              <div class="card-sub">最近 60 个采样点</div>
            </div>
            <span class="live-badge" :class="{ on: running }">
              <span v-if="running" class="live-pulse" />
              {{ running ? "LIVE" : "PAUSED" }}
            </span>
          </div>
          <div ref="chartEl" class="chart" />
        </div>

        <!-- 失败归因 -->
        <div class="glass-card failure-card">
          <div class="failure-header">
            <span class="card-title">失败归因</span>
            <span class="failure-total-num">{{ latest?.register_failed ?? 0 }} 次注册失败</span>
          </div>
          <div class="stacked-bar">
            <div class="sb-seg timeout" :style="{ flex: latest?.fail_timeout ?? 0 }" :title="`超时 ${latest?.fail_timeout ?? 0}`" />
            <div class="sb-seg rejected" :style="{ flex: latest?.fail_rejected ?? 0 }" :title="`拒绝 ${latest?.fail_rejected ?? 0}`" />
            <div class="sb-seg other" :style="{ flex: latest?.fail_other ?? 0 }" :title="`其它 ${latest?.fail_other ?? 0}`" />
            <div v-if="!latest?.register_failed" class="sb-empty" />
          </div>
          <div class="failure-legend">
            <div><i class="dot timeout"/>事务超时<b>{{ latest?.fail_timeout ?? 0 }}</b></div>
            <div><i class="dot rejected"/>平台拒绝<b>{{ latest?.fail_rejected ?? 0 }}</b></div>
            <div><i class="dot other"/>其它<b>{{ latest?.fail_other ?? 0 }}</b></div>
            <div class="fl-extra"><i class="dot-i ok"/>上报 {{ (latest?.position_reported ?? 0) + (latest?.alarm_reported ?? 0) }} 次</div>
          </div>
        </div>

      </div><!-- /monitor-col -->
    </div><!-- /workspace -->
  </div>
</template>

<style scoped>
/* ── 页面骨架 ── */
.page { width: min(1440px, 100%); margin: 0 auto; display: flex; flex-direction: column; overflow: hidden; }

/* ── 顶部状态条（替代原页头，单行常驻） ── */
.status-bar {
  display: flex; align-items: center; gap: 10px; flex-wrap: wrap;
  padding: 6px 12px 6px 14px; border-radius: var(--radius-sm);
  border: 1px solid var(--border-default);
  background: rgba(255,255,255,.55);
  margin-bottom: 8px; font-size: 12px; flex-shrink: 0;
}
.sb-dot { width: 8px; height: 8px; border-radius: 50%; flex: 0 0 auto; background: var(--text-tertiary); }
.sb-text { font-weight: 700; color: var(--text-secondary); }
.sb-sep { color: var(--text-tertiary); }
.sb-stat { color: var(--text-secondary); }
.sb-stat b { color: var(--text-primary); font-weight: 700; font-variant-numeric: tabular-nums; }
.sb-platform { margin-left: auto; color: var(--text-tertiary); font-size: 11.5px; }

.status-bar.running { background: rgba(8,179,136,.07); border-color: rgba(8,179,136,.2); }
.status-bar.running .sb-dot { background: var(--success); animation: pulse 1.8s infinite; box-shadow: 0 0 0 4px rgba(8,179,136,.12); }
.status-bar.running .sb-text { color: var(--success); }
.status-bar.busy { border-color: rgba(240,160,32,.22); background: rgba(240,160,32,.07); }
.status-bar.busy .sb-dot { background: var(--warning); animation: pulse-warn 1.8s infinite; box-shadow: 0 0 0 4px rgba(240,160,32,.12); }
.status-bar.busy .sb-text { color: var(--warning); }

.fade-enter-active, .fade-leave-active { transition: opacity .2s; }
.fade-enter-from, .fade-leave-to { opacity: 0; }

/* ── 主体布局 ── */
.workspace { display: grid; grid-template-columns: 320px minmax(0,1fr); gap: 12px; align-items: start; flex: 1; min-height: 0; overflow: hidden; }

/* ── 配置面板 ── */
.config-col { display: flex; flex-direction: column; height: 100%; min-height: 0; overflow: hidden; }
.config-card { display: flex; flex-direction: column; flex: 1; min-height: 0; padding: 0; overflow: hidden; }
.config-card.is-collapsed { flex: 0 0 auto; }
/* 表单区独立滚动：卡片头与操作条固定在两端，启动按钮永远在视野内 */
.config-scroll { flex: 1; min-height: 0; overflow-y: auto; overflow-x: hidden; padding: 0 16px 12px; }
.card-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; margin-bottom: 14px; padding: 14px 16px 0; }
.card-head.clickable { cursor: pointer; user-select: none; }
.card-head.clickable:hover .card-title { color: var(--accent); }
.card-title { font-size: 13.5px; font-weight: 750; color: var(--text-primary); transition: color .15s; }
.card-sub { margin-top: 3px; font-size: 11.5px; color: var(--text-tertiary); max-width: 180px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.head-right { display: flex; flex-direction: column; align-items: flex-end; gap: 4px; }
.collapse-toggle { font-size: 11px; color: var(--accent); cursor: pointer; }

/* 折叠摘要 */
.config-summary {
  display: flex; gap: 16px; flex-wrap: wrap;
  margin: 0 16px 12px; padding: 8px 12px;
  background: rgba(56,132,255,.05); border-radius: var(--radius-sm);
  border: 1px solid rgba(56,132,255,.12);
  font-size: 12px; color: var(--text-secondary);
}
.config-summary b { color: var(--text-primary); font-weight: 700; }

/* 快速预设：5 档等宽排列，列宽收窄时也不换行 */
.preset-row { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 6px; margin-bottom: 12px; }
.preset-label { grid-column: 1 / -1; font-size: 11px; color: var(--text-tertiary); margin-bottom: 1px; }
.preset-btn {
  padding: 3px 0; border-radius: 999px; border: 1px solid var(--border-default);
  background: rgba(255,255,255,.5); color: var(--text-secondary);
  font-size: 11.5px; font-weight: 600; text-align: center; white-space: nowrap;
  cursor: pointer;
  transition: all .15s;
}
.preset-btn:hover:not(:disabled) { background: var(--accent); border-color: var(--accent); color: #fff; }
.preset-btn:disabled { opacity: .4; cursor: not-allowed; }
/* 选中态：点击后常驻高亮（hover 只是临时变色，真正"选中"看这条） */
.preset-btn.active {
  background: var(--accent); border-color: var(--accent); color: #fff;
  box-shadow: 0 0 0 2px var(--accent-glow);
}
.preset-btn.active:hover:not(:disabled) { background: var(--accent-deep); border-color: var(--accent-deep); }
/* 压测运行中按钮整体禁用，但选中项仍要能看清是哪一档 */
.preset-btn.active:disabled { opacity: .75; }

.section-label {
  font-size: 10.5px; font-weight: 700; color: var(--accent);
  letter-spacing: .9px; text-transform: uppercase;
  margin: 12px 0 2px; padding-bottom: 5px;
  border-bottom: 1px solid var(--border-default);
}
.form-grid { display: grid; grid-template-columns: 1fr 1fr; column-gap: 12px; }
.span-2 { grid-column: span 2; }

/* 主动上报 */
.report-grid { display: flex; flex-direction: column; gap: 6px; margin-bottom: 4px; }
.report-row { display: flex; align-items: center; gap: 8px; padding: 9px 11px; border-radius: var(--radius-sm); background: rgba(255,255,255,.32); border: 1px solid var(--border-default); }
.rr-info { flex: 1; display: flex; flex-direction: column; gap: 1px; }
.rr-info b { font-size: 12.5px; color: var(--text-primary); font-weight: 600; }
.rr-info small { font-size: 10.5px; color: var(--text-tertiary); }
.rr-interval { display: flex; align-items: center; gap: 5px; transition: opacity .2s; }
.rr-interval.disabled { opacity: .3; pointer-events: none; }
.rr-interval span { font-size: 11px; color: var(--text-tertiary); }

/* 操作行（常驻条） */
.action-bar { flex: 0 0 auto; padding: 10px 16px 12px; border-top: 1px solid var(--border-default); background: rgba(255,255,255,.42); }
.action-row { display: flex; gap: 10px; }
.action-hint { margin: 7px 0 0; font-size: 10.5px; color: var(--text-tertiary); text-align: center; }

/* ── 监控列 ── */
.monitor-col { display: flex; flex-direction: column; gap: 8px; height: 100%; overflow-y: auto; overflow-x: hidden; }

/* 指标行 */
.metric-row { display: grid; grid-template-columns: 120px repeat(3, 1fr); gap: 8px; flex-shrink: 0; }

/* 进度环 */
.kpi-ring { padding: 10px 10px; display: flex; flex-direction: column; align-items: center; gap: 5px; }
.ring-wrap { position: relative; width: 80px; height: 80px; flex: 0 0 auto; }
.ring-svg { width: 80px; height: 80px; transform: rotate(-90deg); display: block; }
.ring-track { fill: none; stroke: rgba(56,132,255,.1); stroke-width: 6; }
.ring-fill { fill: none; stroke-width: 6; stroke-linecap: round; transition: stroke-dasharray .5s, stroke .4s; }
.ring-center { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 1px; }
.ring-center strong { font-size: 14px; font-weight: 800; color: var(--text-primary); line-height: 1; }
.ring-center span { font-size: 8.5px; color: var(--text-tertiary); white-space: nowrap; }
.kpi-sub { font-size: 10px; color: var(--text-tertiary); }

/* KPI 卡片 */
.kpi-tile { padding: 10px 14px; display: flex; flex-direction: column; position: relative; overflow: hidden; }
.kpi-tile::after { content: ""; position: absolute; width: 56px; height: 56px; right: -18px; top: -22px; border-radius: 50%; background: var(--accent-dim); }
.kpi-label { font-size: 10.5px; color: var(--text-tertiary); }
.kpi-val { font-size: 24px; font-weight: 800; color: var(--text-primary); line-height: 1.1; margin: 4px 0 4px; font-variant-numeric: tabular-nums; transition: color .3s; }
.kpi-val.positive { color: var(--success); }
.kpi-val small { font-size: 13px; font-weight: 600; color: var(--text-secondary); }
.kpi-hint { font-size: 10px; color: var(--text-tertiary); }
.kpi-hint.warn { color: var(--warning); }
.kpi-hint.mono { font-family: "SF Mono", Menlo, monospace; font-size: 11px; letter-spacing: .3px; color: var(--text-secondary); }

/* ── 设备点阵 ── */
.dot-card { padding: 10px 14px; }
.dot-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 7px; flex-wrap: wrap; gap: 5px; }
.dot-legend { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.dot-legend span { display: flex; align-items: center; gap: 4px; font-size: 11px; color: var(--text-tertiary); }
.dot-count-label { font-size: 11.5px; font-weight: 600; color: var(--text-secondary); margin-left: 4px; font-variant-numeric: tabular-nums; }
.dot-i { width: 8px; height: 8px; border-radius: 50%; flex: 0 0 auto; }
.dot-i.ok { background: var(--success); }
.dot-i.failed { background: var(--error); }
.dot-i.idle { background: rgba(143,161,189,.3); }

.dot-grid {
  display: flex; flex-wrap: wrap; gap: 3px;
  margin-bottom: 10px;
}
.dot-cell {
  width: 8px; height: 8px; border-radius: 2px; flex: 0 0 auto;
  transition: background .4s;
}
.dot-cell.idle    { background: rgba(143,161,189,.25); }
.dot-cell.ok      { background: var(--success); }
.dot-cell.failed  { background: var(--error); }
.dot-grid.active .dot-cell.ok { animation: dot-appear .3s ease-out; }

/* 注册进度条 */
.reg-progress-bar { height: 4px; border-radius: 999px; background: rgba(76,99,133,.1); overflow: hidden; }
.reg-progress-fill { height: 100%; border-radius: 999px; transition: width .6s cubic-bezier(.4,0,.2,1), background .4s; min-width: 0; }

/* 趋势图 */
.chart-card { padding: 10px 14px; flex: 1; min-height: 0; display: flex; flex-direction: column; }
.chart-head { display: flex; align-items: flex-start; justify-content: space-between; margin-bottom: 4px; flex-shrink: 0; }
.chart { width: 100%; flex: 1; min-height: 160px; }
.live-badge { display: inline-flex; align-items: center; gap: 5px; padding: 3px 9px; border-radius: 6px; background: rgba(143,161,189,.1); color: var(--text-tertiary); font: 700 10px/1.5 "SF Mono", Menlo, monospace; letter-spacing: .7px; }
.live-badge.on { background: rgba(8,179,136,.1); color: var(--success); }
.live-pulse { width: 6px; height: 6px; border-radius: 50%; flex: 0 0 auto; background: currentColor; animation: pulse 1.5s infinite; }

/* 失败归因 */
.failure-card { padding: 10px 14px; flex-shrink: 0; }
.failure-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 10px; }
.failure-total-num { font-size: 12px; color: var(--text-tertiary); font-variant-numeric: tabular-nums; }
.stacked-bar { display: flex; height: 9px; border-radius: 999px; overflow: hidden; background: rgba(76,99,133,.08); margin-bottom: 10px; }
.sb-seg { min-width: 0; transition: flex .4s; }
.sb-seg.timeout  { background: var(--warning); }
.sb-seg.rejected { background: var(--error); }
.sb-seg.other    { background: var(--violet); }
.sb-empty { flex: 1; }
.failure-legend { display: flex; gap: 16px; flex-wrap: wrap; align-items: center; }
.failure-legend > div { display: flex; align-items: center; gap: 5px; font-size: 11.5px; color: var(--text-secondary); }
.failure-legend b { color: var(--text-primary); font-weight: 700; margin-left: 3px; font-variant-numeric: tabular-nums; }
.dot { width: 8px; height: 8px; border-radius: 50%; flex: 0 0 auto; }
.dot.timeout  { background: var(--warning); }
.dot.rejected { background: var(--error); }
.dot.other    { background: var(--violet); }
.fl-extra { margin-left: auto; font-size: 11px; color: var(--text-tertiary); }

/* 动画 */
@keyframes pulse { 50% { box-shadow: 0 0 0 7px rgba(8,179,136,0); } }
@keyframes pulse-warn { 50% { box-shadow: 0 0 0 7px rgba(240,160,32,0); } }
@keyframes dot-appear { from { opacity: 0; transform: scale(.5); } to { opacity: 1; transform: scale(1); } }

/* 响应式 */
@media (max-width: 1220px) {
  .workspace { grid-template-columns: 280px minmax(0,1fr); }
  /* 列宽 280px 时 5 档一行放不下「10000台」，改 3 列两行 */
  .preset-row { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  .metric-row { grid-template-columns: 110px repeat(3,1fr); }
}
@media (max-width: 980px) {
  .workspace { grid-template-columns: 1fr; overflow: visible; }
  .config-col, .monitor-col { height: auto; overflow: visible; }
  /* 单列时表单不再内滚，操作条改为吸底，避免被长表单挤出屏幕 */
  .config-scroll { overflow: visible; }
  .config-card { overflow: visible; }
  .action-bar {
    position: sticky; bottom: 0; z-index: 3;
    background: rgba(244, 249, 255, .95);
    backdrop-filter: blur(8px); -webkit-backdrop-filter: blur(8px);
    border-radius: 0 0 var(--radius-lg) var(--radius-lg);
  }
  .metric-row { grid-template-columns: repeat(2,1fr); }
  .kpi-ring { grid-column: span 2; flex-direction: row; padding: 10px 14px; gap: 14px; justify-content: flex-start; align-items: center; }
  .kpi-sub { margin-top: 0; }
}
@media (max-width: 640px) {
  .metric-row { grid-template-columns: 1fr 1fr; }
  .kpi-ring { grid-column: span 2; }
  .form-grid { grid-template-columns: 1fr; }
  .span-2 { grid-column: span 1; }
  .failure-legend { flex-direction: column; gap: 8px; }
  .fl-extra { margin-left: 0; }
}
@media (prefers-reduced-motion: reduce) {
  .fade-enter-active, .fade-leave-active { transition: none; }
  .status-bar.running .sb-dot, .status-bar.busy .sb-dot, .live-pulse { animation: none; }
  .dot-grid.active .dot-cell.ok { animation: none; }
}
</style>
