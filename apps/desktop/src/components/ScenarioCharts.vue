<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import { LineChart } from "echarts/charts";
import {
  DataZoomComponent,
  GridComponent,
  LegendComponent,
  TooltipComponent,
} from "echarts/components";
import { init, use as useECharts, type ECharts } from "echarts/core";
import { CanvasRenderer } from "echarts/renderers";
import { useTheme } from "../theme";

useECharts([
  LineChart,
  GridComponent,
  LegendComponent,
  TooltipComponent,
  DataZoomComponent,
  CanvasRenderer,
]);

export interface LatencySnapshot {
  count: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  p99_ms: number;
  max_ms: number;
}

export interface TransportStats {
  received: number;
  parse_errors: number;
  rejected_source: number;
  rate_limited: number;
  rate_limited_bytes: number;
  anti_amplification_blocked: number;
  dropped_queue_full: number;
  unrouted: number;
  sent: number;
  send_errors: number;
  oversized_sent: number;
}

export interface MetricsSnapshot {
  register_attempted: number;
  register_first_attempt: number;
  register_retry: number;
  register_succeeded: number;
  register_failed: number;
  register_success_rate: number;
  device_success_rate: number;
  heartbeat_succeeded: number;
  heartbeat_failed: number;
  active_streams: number;
  devices_online: number;
  devices_online_peak: number;
  fail_timeout: number;
  fail_rejected: number;
  fail_rejected_auth: number;
  fail_rejected_overload: number;
  fail_rejected_other: number;
  fail_local: number;
  fail_other: number;
  first_failure_online: number | null;
  ramp_throttled: number;
  failure_samples: Array<{
    device_id: string;
    kind: string;
    status?: number;
    online_at_failure: number;
  }>;
  position_reported: number;
  alarm_reported: number;
  register_latency: LatencySnapshot;
  heartbeat_latency: LatencySnapshot;
  transport?: TransportStats;
  local_pressure: number;
  results_trustworthy: boolean;
}

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
  updateChart();
}

function resetSeries() {
  times.length = 0;
  regRates.length = 0;
  hbFails.length = 0;
  onlineCounts.length = 0;
  p95Latency.length = 0;
  updateChart();
}

function resize() {
  chart.value?.resize();
}

watch(isDark, () => {
  chart.value?.setOption(paletteOption());
});

onMounted(() => {
  initChart();
});

onUnmounted(() => {
  resizeObserver?.disconnect();
  chart.value?.dispose();
});

defineExpose({
  pushPoint,
  resetSeries,
  resize,
});
</script>

<template>
  <section class="glass-card panel chart-panel">
    <div class="panel-head">
      <div>
        <div class="panel-title">实时运行走势</div>
        <div class="panel-sub">双 Y 轴监控成功率、并发规模、拐点延迟与心跳质量</div>
      </div>
      <span class="chart-tip">滚轮或下方滑块缩放</span>
    </div>
    <div ref="chartEl" class="echart-box" />
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
.chart-tip {
  font-size: 11px;
  color: var(--text-color-3);
}
.echart-box {
  width: 100%;
  height: 280px;
}
</style>
