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
import { LineChart } from "echarts/charts";
import {
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from "echarts/components";
import { init, use as useECharts, type ECharts } from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { usePlatform } from "../platform";
import { useTheme } from "../theme";
import { buildScenarioToml } from "../lib/scenarioToml";
import { formatError } from "../lib/ipcError";

useECharts([
  LineChart,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  DataZoomComponent,
  CanvasRenderer,
]);

const message = useMessage();
const { active: activePlatform, activeHasSecret, refreshSecretStatus } = usePlatform();

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

interface LatencySnapshot {
  count: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  max_ms: number;
}

interface TransportStats {
  received: number;
  parse_errors: number;
  rejected_source: number;
  dropped_queue_full: number;
  unrouted: number;
  sent: number;
  send_errors: number;
  oversized_sent: number;
}

interface MetricsSnapshot {
  register_attempted: number;
  register_first_attempt: number;
  register_retry: number;
  register_succeeded: number;
  register_failed: number;
  register_success_rate: number;
  /** 设备维度成功率:不被退避重试稀释,更接近"多少台真的上线了"。 */
  device_success_rate: number;
  heartbeat_succeeded: number;
  heartbeat_failed: number;
  active_streams: number;
  devices_online: number;
  devices_online_peak: number;
  fail_timeout: number;
  fail_rejected: number;
  /** 拒绝里的凭据/权限档(401/403/407):加并发不会变好,去核对账号配置。 */
  fail_rejected_auth: number;
  /** 拒绝里的忙/过载档(480/486/500/503/504):真正的承载力信号。 */
  fail_rejected_overload: number;
  /** 拒绝里的其它状态码(常见 404:编号不被平台接受)。 */
  fail_rejected_other: number;
  /** 本地瓶颈导致的失败(不是平台的问题)。 */
  fail_local: number;
  fail_other: number;
  /** 首次失败时的在线设备数(失败水位);全程无失败为 null。 */
  first_failure_online: number | null;
  /** 爬坡因本机背压减速的次数。非 0 说明本机跟不上设定速率。 */
  ramp_throttled: number;
  /** 失败设备样本(最多 20 条),供挑一台复现。 */
  failure_samples: FailureSample[];
  position_reported: number;
  alarm_reported: number;
  register_latency: LatencySnapshot;
  heartbeat_latency: LatencySnapshot;
  transport?: TransportStats;
  local_pressure: number;
  /** false 表示出现了本机瓶颈,失败不能直接归因给平台。 */
  results_trustworthy: boolean;
}

/** 一条失败样本(对应 Rust 侧 stress_engine::FailureSample)。 */
interface FailureSample {
  device_id: string;
  /** timeout / rejected / local / other。 */
  kind: string;
  /** 平台状态码(仅拒绝类有)。 */
  status?: number;
  /** 出现该失败时的在线设备数。 */
  online_at_failure: number;
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
// 展示**设备维度**成功率:尝试维度会被退避重试稀释,长跑后失真。
const successRate = computed(() =>
  latest.value ? `${(latest.value.device_success_rate * 100).toFixed(1)}%` : "—",
);
const successHealth = computed(() => {
  if (!latest.value || latest.value.register_first_attempt === 0) return "等待采样";
  const rate = latest.value.device_success_rate;
  if (rate >= 0.98) return "健康";
  if (rate >= 0.9) return "需观察";
  return "异常";
});
const onlineText = computed(() => {
  const m = latest.value;
  if (!m) return "—";
  return `${m.devices_online}`;
});
const onlineNote = computed(() => {
  const m = latest.value;
  if (!m) return "等待上线";
  return `峰值 ${m.devices_online_peak} · 目标 ${form.value.count}`;
});
const latencyText = computed(() => {
  const l = latest.value?.register_latency;
  if (!l || l.count === 0) return "—";
  return `${l.p95_ms} ms`;
});
const latencyNote = computed(() => {
  const l = latest.value?.register_latency;
  if (!l || l.count === 0) return "等待采样";
  return `p50 ${l.p50_ms} · p99 ${l.p99_ms} · max ${l.max_ms} ms`;
});
const heartbeatLatencyNote = computed(() => {
  const l = latest.value?.heartbeat_latency;
  if (!l || l.count === 0) return "";
  return `心跳 p95 ${l.p95_ms} ms`;
});
// 本机瓶颈提示:一旦出现,失败数就不能直接当成平台承载上限。
const trustWarning = computed(() => {
  const m = latest.value;
  if (!m || m.results_trustworthy) return "";
  const t = m.transport;
  const parts: string[] = [];
  if (m.fail_local) parts.push(`本地发送失败 ${m.fail_local}`);
  if (t?.dropped_queue_full) parts.push(`入站队列丢弃 ${t.dropped_queue_full}`);
  if (t?.send_errors) parts.push(`socket 发送错误 ${t.send_errors}`);
  if (t?.parse_errors) parts.push(`报文解析失败 ${t.parse_errors}`);
  if (t?.oversized_sent) parts.push(`超 MTU 报文 ${t.oversized_sent}`);
  return `检测到本机瓶颈(${parts.join("、") || "本地压力 " + m.local_pressure}),失败数不能直接归因给平台;请降低设备数或放宽爬坡后重测。`;
});
const streamNote = computed(() => {
  if (!latest.value) return "等待点播";
  if (latest.value.active_streams > 0) return "平台正在收流";
  return form.value.media_profile === "A" ? "仅信令场景" : "等待 INVITE 点播";
});
const heartbeatNote = computed(() => {
  const failed = latest.value?.heartbeat_failed ?? 0;
  return failed > 0 ? "检查网络抖动" : "心跳稳定";
});
const reportNote = computed(() => {
  const total = (latest.value?.position_reported ?? 0) + (latest.value?.alarm_reported ?? 0);
  return total > 0 ? "主动上报已生效" : "未触发主动上报";
});
/**
 * 排障建议。按**处置动作**给,而不是按数字大小罗列。
 *
 * 拒绝类现在细分到档:401/403 是配置问题(再加并发也没用),486/503 才是承载力信号。
 * 之前这两种给的是同一句"检查设备是否已添加、认证密码……",而 503 的正确动作
 * 恰恰是降并发。
 */
const failureAdvice = computed(() => {
  const m = latest.value;
  if (!m || m.register_failed === 0) return "暂无失败。若平台无设备上线，优先检查平台地址、域 ID 与设备 ID 是否匹配。";
  if (m.fail_local > 0 && m.fail_local >= m.fail_timeout) return "本机瓶颈为主:本地 socket 发送失败或队列溢出。先减少设备数、放宽爬坡速率,再判断平台承载。";
  if (m.fail_rejected_auth > 0 && m.fail_rejected_auth >= m.fail_rejected_overload) {
    return "平台以 401/403 拒绝:这是凭据或权限问题,增减并发不会改变结果。核对 SIP 密码、平台域 ID、设备编号是否已在平台注册。";
  }
  if (m.fail_rejected_overload > 0 && m.fail_rejected_overload >= m.fail_timeout) {
    return "平台以 486/503 拒绝:这是平台侧过载信号,已经触到承载上限。降低设备数或爬坡速率再看拐点在哪。";
  }
  if (m.fail_rejected_other > 0 && m.fail_rejected_other >= m.fail_timeout) {
    return "平台以其它 4xx/5xx 拒绝(常见 404):设备编号可能不被平台接受。核对编号规则与平台侧的设备清单。";
  }
  if (m.fail_timeout >= m.fail_other) return "事务超时较多：检查平台 IP/端口、防火墙、SIP 监听地址和本机网络出口。";
  return "网络或其它错误较多：查看系统日志页，确认本机 socket 权限、媒体源和运行时依赖。";
});

/** 失败水位文案:"撑到多少台开始出问题"。 */
const failureWatermark = computed(() => {
  const n = latest.value?.first_failure_online;
  if (n === null || n === undefined) return "";
  return `首次失败发生在在线 ${n} 台时`;
});

/** 爬坡是否被本机背压减速过。 */
const rampThrottled = computed(() => (latest.value?.ramp_throttled ?? 0) > 0);

/** 前几条失败样本(挑一台复现)。 */
const failureSamples = computed(() => (latest.value?.failure_samples ?? []).slice(0, 5));
const scenarioSummary = computed(() => {
  const media = mediaOptions.find((item) => item.value === form.value.media_profile)?.label ?? form.value.media_profile;
  return [
    `${form.value.count} 台设备`,
    form.value.ramp_per_second ? `爬坡 ${form.value.ramp_per_second} 台/秒` : "一次性拉起",
    `${form.value.channels_per_device} 通道/设备`,
    media,
  ].join(" · ");
});

function validateForm(): string | null {
  const p = activePlatform.value;
  if (!p) return "请先在顶栏配置目标平台";
  if (!activeHasSecret.value) return "当前平台档案尚未配置 SIP 密码，请在顶栏「编辑」中填写并保存";
  if (!/^\d{20}$/.test(form.value.base_device_id)) return "起始设备 ID 必须是 20 位数字";
  if (form.value.count < 1 || form.value.count > 10_000) return "设备数量必须在 1 到 10000 之间";
  if (form.value.media_profile === "C" && !form.value.video_source.trim()) {
    return "C 档真实媒体压测必须选择视频源";
  }
  return null;
}



// 与单设备页同走 Rust 侧对话框(命令 pick_video_source):选择与 asset 预览授权
// 在同一次调用里完成,前端拿不到"凭空放行任意路径"的能力。
async function pickVideoSource() {
  try {
    const picked = await invoke<string | null>("pick_video_source");
    if (typeof picked === "string") form.value.video_source = picked;
  } catch (error) {
    message.error(formatError(error, "选择文件失败"), { duration: 0, closable: true });
  }
}

function resetSeries() {
  times.length = 0;
  regRates.length = 0;
  hbFails.length = 0;
  onlineCounts.length = 0;
  p95Latency.length = 0;
  latest.value = null;
  hasReport.value = false;
  updateChart();
}

async function startStress() {
  await refreshSecretStatus();
  const validation = validateForm();
  if (validation) {
    message.warning(validation);
    return;
  }

  starting.value = true;
  statusText.value = "正在启动";
  try {
    const toml = buildScenarioToml(form.value, activePlatform.value!);
    await invoke("validate_scenario", { toml });
    resetSeries();
    const result = await invoke<string>("start_stress", {
      toml,
      count: form.value.count,
      profileId: activePlatform.value!.id,
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
    message.error(formatError(error, "压测启动失败"), { duration: 0, closable: true });
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
    message.error(formatError(error, "压测停止失败"), { duration: 0, closable: true });
  } finally {
    stopPending.value = false;
  }
}

async function exportReport() {
  try {
    const path = await invoke<string>("export_report");
    message.success(`报告已导出：${path}`, { duration: 6000 });
  } catch (error) {
    message.error(formatError(error), { duration: 0, closable: true });
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

// 保留**全量**采样点(用 dataZoom 缩放),而不是只留最后 60 个。
// 之前 MAX_POINTS=60 意味着跑 10 分钟只剩最后一分钟曲线,长跑等于没有历史。
// 上限只作为内存兜底(24 小时 @1s)。
const MAX_POINTS = 86_400;
const times: string[] = [];
const regRates: number[] = [];
const hbFails: number[] = [];
const onlineCounts: number[] = [];
const p95Latency: number[] = [];
const { isDark } = useTheme();
const chartEl = ref<HTMLElement | null>(null);
const chart = shallowRef<ECharts | null>(null);
let resizeObserver: ResizeObserver | null = null;

function pushPoint(snapshot: MetricsSnapshot) {
  times.push(new Date().toLocaleTimeString("zh-CN", { hour12: false }));
  regRates.push(Number((snapshot.device_success_rate * 100).toFixed(1)));
  hbFails.push(snapshot.heartbeat_failed);
  onlineCounts.push(snapshot.devices_online);
  p95Latency.push(snapshot.register_latency?.p95_ms ?? 0);
  if (times.length > MAX_POINTS) {
    times.shift();
    regRates.shift();
    hbFails.shift();
    onlineCounts.shift();
    p95Latency.shift();
  }
}

function updateChart() {
  chart.value?.setOption({
    xAxis: { data: [...times] },
    series: [
      { data: [...regRates] },
      { data: [...onlineCounts] },
      { data: [...p95Latency] },
      { data: [...hbFails] },
    ],
  });
}

/// ECharts 画在 canvas 上,拿不到 CSS 变量,配色只能显式给。
/// 暗色下沿用浅色的轴文字(#8fa1bd 对深底对比度不足)会让坐标轴几乎看不见,
/// 所以按主题切一套。
const chartPalette = computed(() => {
  const dark = isDark.value;
  return {
    series: dark
      ? ["#5a9bff", "#2ed3a5", "#9b8fff", "#f0a93c"]
      : ["#3884ff", "#08b388", "#8b5cf6", "#e89320"],
    axisLabel: dark ? "#a9bcd6" : "#8fa1bd",
    legend: dark ? "#a9bcd6" : "#627797",
    axisLine: dark ? "rgba(169,188,214,.28)" : "rgba(76,99,133,.2)",
    splitLine: dark ? "rgba(169,188,214,.12)" : "rgba(76,99,133,.08)",
    tooltipBg: dark ? "rgba(10,18,30,.94)" : "rgba(14,39,71,.92)",
    zoomFiller: dark ? "rgba(90,155,255,.18)" : "rgba(56,132,255,.12)",
  };
});

/// 只含配色的 option 片段。初始化与主题切换共用一份,避免两处写法漂移。
function paletteOption() {
  const c = chartPalette.value;
  return {
    color: c.series,
    tooltip: {
      trigger: "axis" as const,
      backgroundColor: c.tooltipBg,
      borderWidth: 0,
      textStyle: { color: "#fff" },
    },
    legend: { right: 4, top: 0, textStyle: { color: c.legend } },
    dataZoom: [
      { type: "inside" as const, throttle: 60 },
      {
        type: "slider" as const,
        height: 14,
        bottom: 6,
        borderColor: "transparent",
        fillerColor: c.zoomFiller,
        handleSize: "70%",
        textStyle: { color: c.axisLabel, fontSize: 9 },
      },
    ],
    xAxis: {
      axisLine: { lineStyle: { color: c.axisLine } },
      axisLabel: { color: c.axisLabel },
    },
    yAxis: [
      {
        axisLabel: { formatter: "{value}%", color: c.axisLabel },
        splitLine: { lineStyle: { color: c.splitLine } },
      },
      { axisLabel: { color: c.axisLabel }, splitLine: { show: false } },
    ],
  };
}

function initChart() {
  if (!chartEl.value || chart.value) return;
  const instance = init(chartEl.value, undefined, { renderer: "canvas" });
  instance.setOption({
    animationDuration: 300,
    ...paletteOption(),
    grid: { left: 44, right: 52, top: 46, bottom: 44 },
    xAxis: {
      type: "category",
      boundaryGap: false,
      data: [],
      ...paletteOption().xAxis,
    },
    yAxis: [
      {
        type: "value",
        min: 0,
        max: 100,
        ...paletteOption().yAxis[0],
      },
      {
        type: "value",
        min: 0,
        ...paletteOption().yAxis[1],
      },
    ],
    series: [
      {
        name: "上线成功率",
        type: "line",
        yAxisIndex: 0,
        smooth: true,
        showSymbol: false,
        areaStyle: { color: "rgba(56,132,255,.10)" },
        data: [],
      },
      {
        name: "在线设备数",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
        data: [],
      },
      {
        // 拐点通常先体现在延迟上,再体现在失败上 —— 这条线是提前预警。
        name: "注册延迟 p95(ms)",
        type: "line",
        yAxisIndex: 1,
        smooth: true,
        showSymbol: false,
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

// 主题切换时把配色重刷到图表上。ECharts 画在 canvas 里,不会跟着 CSS 变量走,
// 少了这一步暗色下坐标轴文字几乎看不见。
watch(isDark, () => {
  chart.value?.setOption(paletteOption());
});
</script>

<template>
  <div class="page" :class="{ 'is-running': running || starting || stopping || stopPending }">
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

    <n-alert v-if="activePlatform?.transport === 'TCP'" type="info" :show-icon="false" class="notice">
      当前使用 SIP over TCP:目录树很大(单条报文超 MTU)或网络丢包较多时更稳,
      代价是每台设备一条 TCP 连接,万级压测请优先用 UDP。
    </n-alert>

    <div class="stress-sections" :class="{ running: running || starting || stopping || stopPending }">
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

    <div v-if="trustWarning" class="trust-warning" role="status" aria-live="polite">
      <span aria-hidden="true">⚠</span>
      <span>{{ trustWarning }}</span>
    </div>

    <section class="metric-grid">
      <div class="glass-card metric primary">
        <span>设备上线成功率</span>
        <strong>{{ successRate }}</strong>
        <small>
          {{ latest?.register_succeeded ?? 0 }} 成功 / {{ latest?.register_first_attempt ?? 0 }} 台
          · 重试 {{ latest?.register_retry ?? 0 }} 次 · {{ successHealth }}
        </small>
      </div>
      <div class="glass-card metric">
        <span>当前在线设备</span>
        <strong>{{ onlineText }}</strong>
        <small>{{ onlineNote }}</small>
      </div>
      <div class="glass-card metric">
        <span>注册延迟 p95</span>
        <strong>{{ latencyText }}</strong>
        <small>{{ latencyNote }}</small>
      </div>
      <div class="glass-card metric">
        <span>当前活跃流</span>
        <strong>{{ latest?.active_streams ?? 0 }}</strong>
        <small>{{ streamNote }}</small>
      </div>
      <div class="glass-card metric">
        <span>心跳成功</span>
        <strong>{{ latest?.heartbeat_succeeded ?? 0 }}</strong>
        <small>
          失败 {{ latest?.heartbeat_failed ?? 0 }} 次 · {{ heartbeatNote }}
          <template v-if="heartbeatLatencyNote"> · {{ heartbeatLatencyNote }}</template>
        </small>
      </div>
      <div class="glass-card metric">
        <span>主动上报</span>
        <strong>{{ (latest?.position_reported ?? 0) + (latest?.alarm_reported ?? 0) }}</strong>
        <small>位置 {{ latest?.position_reported ?? 0 }} · 报警 {{ latest?.alarm_reported ?? 0 }} · {{ reportNote }}</small>
      </div>
    </section>

    <section class="monitor-grid">
      <div class="glass-card panel chart-panel">
        <div class="panel-head compact">
          <div>
            <div class="panel-title">实时趋势</div>
            <div class="panel-sub">全量采样点(每秒一次)· 可滚轮/拖动缩放</div>
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
          <div class="sub"><span>· 凭据/权限 401·403</span><b>{{ latest?.fail_rejected_auth ?? 0 }}</b></div>
          <div class="sub"><span>· 平台忙/过载 486·503</span><b>{{ latest?.fail_rejected_overload ?? 0 }}</b></div>
          <div class="sub"><span>· 其它拒绝</span><b>{{ latest?.fail_rejected_other ?? 0 }}</b></div>
          <div><span><i class="local" />本机瓶颈</span><b>{{ latest?.fail_local ?? 0 }}</b></div>
          <div><span><i class="other" />网络或其它</span><b>{{ latest?.fail_other ?? 0 }}</b></div>
        </div>
        <div class="failure-advice">{{ failureAdvice }}</div>
        <!-- 失败水位与样本:回答"撑到多少台开始出问题"与"挑哪台复现" -->
        <div v-if="failureWatermark" class="failure-meta">{{ failureWatermark }}</div>
        <div v-if="rampThrottled" class="failure-meta warn">
          爬坡曾因本机背压减速 {{ latest?.ramp_throttled }} 次 —— 本机跟不上设定速率,
          这段时间的失败不能算作平台承载上限。
        </div>
        <details v-if="failureSamples.length" class="failure-samples">
          <summary>失败样本({{ failureSamples.length }} 条,可挑一台复现)</summary>
          <ul>
            <li v-for="sample in failureSamples" :key="sample.device_id + sample.online_at_failure">
              <code>{{ sample.device_id }}</code>
              <span>{{ sample.kind }}<template v-if="sample.status"> · {{ sample.status }}</template></span>
              <span class="dim">在线 {{ sample.online_at_failure }} 台时</span>
            </li>
          </ul>
        </details>
      </div>
    </section>
    </div>
  </div>
</template>

<style scoped>
.page { width: min(1320px, 100%); margin: 0 auto; }
.page-header { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; margin: 8px 0 12px; }
.eyebrow { color: var(--accent); font-size: var(--fs-xs); font-weight: 800; letter-spacing: 1.5px; }
h1 { margin: 3px 0 0; color: var(--text-primary); font-size: clamp(18px, 2vw, 22px); letter-spacing: -0.4px; }
.page-header p { margin: 4px 0 0; color: var(--text-tertiary); font-size: var(--fs-sm); line-height: 1.45; }
.header-actions { display: flex; align-items: center; gap: 8px; flex: 0 0 auto; }
.status-chip { display: inline-flex; align-items: center; gap: 6px; min-height: 24px; padding: 0 10px; border: 1px solid var(--border-default); border-radius: 999px; background: var(--bg-surface); color: var(--text-secondary); font-size: var(--fs-sm); font-weight: 650; }
.status-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--text-tertiary); }
.status-chip.running { color: var(--success); border-color: rgba(8,179,136,.25); background: rgba(8,179,136,.08); }
.status-chip.running .status-dot { background: var(--success); box-shadow: 0 0 0 3px rgba(8,179,136,.12); animation: pulse 1.8s infinite; }
.status-chip.busy { color: var(--warning); }
.status-chip.busy .status-dot { background: var(--warning); }
.notice { margin-bottom: 12px; border-radius: var(--radius-md); }
.stress-sections { display: flex; flex-direction: column; gap: 12px; }
.stress-sections.running .metric-grid { order: 1; }
.stress-sections.running .monitor-grid { order: 2; }
.stress-sections.running .config-panel { order: 3; }
.panel { padding: 12px 16px; border-radius: var(--radius-md); }
.config-panel { margin-bottom: 0; transition: opacity var(--transition), transform var(--transition); }
.stress-sections.running .config-panel { opacity: .86; }
.scenario-summary { display: grid; grid-template-columns: minmax(0, 1.4fr) minmax(0, 1fr); gap: 8px; margin-bottom: 8px; }
.scenario-summary div { padding: 8px 10px; border-radius: var(--radius-sm); background: var(--bg-card); border: 1px solid var(--border-default); min-width: 0; }
.scenario-summary span { display: block; color: var(--text-tertiary); font-size: var(--fs-xs); margin-bottom: 2px; }
.scenario-summary b { display: block; color: var(--text-primary); font-size: var(--fs-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.panel-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 12px; margin-bottom: 10px; }
.panel-head.compact { margin-bottom: 2px; }
.panel-title { color: var(--text-primary); font-size: var(--fs-md); font-weight: 700; }
.panel-sub { margin-top: 2px; color: var(--text-tertiary); font-size: var(--fs-sm); }
.form-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); column-gap: 12px; row-gap: 0; }
.span-2 { grid-column: span 2; }
.section-divider { display: flex; align-items: baseline; gap: 8px; margin: 2px 0 8px; padding-top: 10px; border-top: 1px solid var(--border-default); }
.section-divider span { color: var(--text-secondary); font-size: var(--fs-sm); font-weight: 700; }
.section-divider small { color: var(--text-tertiary); font-size: var(--fs-sm); }
.report-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
.report-option { display: grid; grid-template-columns: minmax(100px, 1fr) auto 90px auto; align-items: center; gap: 8px; padding: 8px 10px; border: 1px solid var(--border-default); border-radius: var(--radius-sm); background: var(--bg-card); }
.report-option div { display: flex; flex-direction: column; gap: 1px; }
.report-option b { color: var(--text-primary); font-size: var(--fs-sm); }
.report-option span, .report-option em { color: var(--text-tertiary); font-size: var(--fs-xs); font-style: normal; }
.action-row { display: flex; align-items: center; gap: 8px; margin-top: 12px; }
.action-row span { margin-left: auto; color: var(--text-tertiary); font-size: var(--fs-sm); }
.metric-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 10px; margin-bottom: 0; }
.metric { position: relative; overflow: hidden; padding: 10px 14px; border-radius: var(--radius-md); }
.metric::after { content: ""; position: absolute; width: 64px; height: 64px; right: -20px; top: -25px; border-radius: 50%; background: var(--accent-dim); }
.metric.primary { border-color: var(--border-accent); background: linear-gradient(145deg, var(--bg-elevated), color-mix(in srgb, var(--accent) 10%, transparent)); }
.metric > span { display: block; color: var(--text-tertiary); font-size: var(--fs-sm); }
.metric strong { display: block; margin-top: 4px; color: var(--text-primary); font-size: var(--fs-xl); line-height: 1; font-variant-numeric: tabular-nums; }
.metric.primary strong { color: var(--accent-deep); }
.metric small { display: block; margin-top: 4px; color: var(--text-tertiary); font-size: var(--fs-xs); }
.monitor-grid { display: grid; grid-template-columns: minmax(0, 1fr) 240px; gap: 10px; }
.chart-panel { min-width: 0; }
.chart { width: 100%; height: 230px; }
.live-indicator { padding: 2px 6px; border-radius: 4px; background: rgba(143,161,189,.12); color: var(--text-tertiary); font: 700 9px/1.4 "SF Mono", Menlo, monospace; letter-spacing: .6px; }
.live-indicator.on { background: rgba(8,179,136,.10); color: var(--success); }
.failure-panel { display: flex; flex-direction: column; }
.failure-total { margin-top: 12px; color: var(--text-primary); font-size: var(--fs-2xl); font-weight: 750; line-height: 1; }
.failure-caption { margin-top: 3px; color: var(--text-tertiary); font-size: var(--fs-sm); }
.failure-list { display: flex; flex-direction: column; gap: 8px; margin-top: auto; padding-top: 14px; }
.failure-list > div { display: flex; justify-content: space-between; align-items: center; color: var(--text-secondary); font-size: var(--fs-sm); }
.failure-list span { display: inline-flex; align-items: center; gap: 6px; }
.failure-list i { width: 6px; height: 6px; border-radius: 50%; background: var(--text-tertiary); }
.failure-list i.timeout { background: var(--warning); }
.failure-list i.rejected { background: var(--error); }
.failure-list i.local { background: var(--accent-deep, #1e63dc); }
.failure-list i.other { background: var(--violet); }
.trust-warning {
  display: flex; align-items: flex-start; gap: 8px; margin-bottom: 10px;
  padding: 8px 10px; border-radius: var(--radius-sm); font-size: var(--fs-sm); line-height: 1.5;
  background: color-mix(in srgb, var(--warning) 12%, transparent);
  border: 1px solid color-mix(in srgb, var(--warning) 32%, transparent);
  color: var(--text-primary);
}
.failure-list b { color: var(--text-primary); font-size: var(--fs-lg); font-variant-numeric: tabular-nums; }
.failure-advice { margin-top: 18px; padding: 10px 12px; border-radius: var(--radius-sm); background: rgba(56,132,255,.07); color: var(--text-secondary); font-size: var(--fs-sm); line-height: 1.55; }
@keyframes pulse { 50% { box-shadow: 0 0 0 7px rgba(8,179,136,0); } }
@media (max-width: 1120px) {
  .form-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
  .metric-grid { grid-template-columns: repeat(2, 1fr); }
}
@media (max-width: 820px) {
  .page-header { flex-direction: column; }
  .header-actions { width: 100%; justify-content: space-between; }
  .report-grid, .monitor-grid, .scenario-summary { grid-template-columns: 1fr; }
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
.failure-meta {
  margin-top: 6px; font-size: var(--fs-sm); line-height: 1.55; color: var(--text-tertiary);
}
.failure-meta.warn { color: var(--warning); }
.failure-samples { margin-top: 6px; font-size: var(--fs-sm); color: var(--text-tertiary); }
.failure-samples summary { cursor: pointer; }
.failure-samples ul { margin: 4px 0 0; padding-left: 16px; }
.failure-samples li { display: flex; gap: 8px; align-items: baseline; padding: 1px 0; }
.failure-samples code {
  font-family: ui-monospace, "SF Mono", Menlo, monospace; color: var(--text-secondary);
}
.failure-samples .dim { color: var(--text-tertiary); margin-left: auto; }
.failure-list .sub span { padding-left: 10px; color: var(--text-tertiary); }
</style>
