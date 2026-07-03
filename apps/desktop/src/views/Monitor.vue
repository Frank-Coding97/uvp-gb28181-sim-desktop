<script setup lang="ts">
// 运行监控大盘：订阅 metrics_tick 事件，用 ECharts 绘制实时曲线（FR-42）。
import { ref, onMounted, onUnmounted, shallowRef } from "vue";
import { NButton, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import * as echarts from "echarts";

const message = useMessage();

// 导出报告(FR-28)。
async function exportReport() {
  try {
    const path = await invoke<string>("export_report");
    message.success(`报告已导出:${path}`);
  } catch (e) {
    message.error(String(e));
  }
}

// ── 数据模型（对应 MetricsSnapshot）──────────────────────
interface MetricsSnapshot {
  register_attempted:    number;
  register_succeeded:    number;
  register_success_rate: number;
  heartbeat_succeeded:   number;
  heartbeat_failed:      number;
  active_streams:        number;
  fail_timeout:          number;
  fail_rejected:         number;
  fail_other:            number;
}

const latest    = ref<MetricsSnapshot | null>(null);
const MAX_POINTS = 60; // 最近 60 秒

// 时间序列数据（供 ECharts）。
const times:    string[] = [];
const regRates: number[] = [];
const hbFails:  number[] = [];

// ECharts 实例（用 shallowRef 避免 Vue 深度 reactive 代理导致的性能问题）。
const chartEl = ref<HTMLElement | null>(null);
const chart   = shallowRef<echarts.ECharts | null>(null);

// 格式化时间 HH:mm:ss。
function fmtTime(): string {
  return new Date().toTimeString().slice(0, 8);
}

function pushPoint(snap: MetricsSnapshot) {
  times.push(fmtTime());
  regRates.push(+(snap.register_success_rate * 100).toFixed(1));
  hbFails.push(snap.heartbeat_failed);
  if (times.length > MAX_POINTS) {
    times.shift(); regRates.shift(); hbFails.shift();
  }
}

function updateChart() {
  chart.value?.setOption({
    xAxis: { data: [...times] },
    series: [
      { name: "注册成功率(%)", data: [...regRates] },
      { name: "心跳失败次数",  data: [...hbFails] },
    ],
  });
}

function initChart() {
  if (!chartEl.value) return;
  const c = echarts.init(chartEl.value);
  c.setOption({
    tooltip: { trigger: "axis" },
    legend: { data: ["注册成功率(%)", "心跳失败次数"] },
    xAxis: { type: "category", data: [] },
    yAxis: { type: "value" },
    series: [
      { name: "注册成功率(%)", type: "line", smooth: true, data: [] },
      { name: "心跳失败次数",  type: "line", smooth: true, data: [] },
    ],
  });
  chart.value = c;
}

// ── 事件监听 ─────────────────────────────────────────────
let unlisten: UnlistenFn | null = null;

onMounted(async () => {
  initChart();
  // 订阅引擎推送的 metrics_tick 事件（每秒一次）。
  unlisten = await listen<string>("metrics_tick", (event) => {
    try {
      const snap: MetricsSnapshot = JSON.parse(event.payload);
      latest.value = snap;
      pushPoint(snap);
      updateChart();
    } catch {}
  });
});

onUnmounted(() => {
  unlisten?.();
  chart.value?.dispose();
});
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">运行监控</div>
        <div class="page-sub">压测实时指标 · 每秒刷新</div>
      </div>
      <n-button size="small" tertiary @click="exportReport">导出报告</n-button>
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

    <!-- ECharts 实时曲线 -->
    <div class="glass-card panel">
      <div class="panel-title">实时指标曲线</div>
      <div ref="chartEl" style="height: 320px;" />
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
.metrics { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 18px; }
.metric { padding: 16px 18px; }
.m-label { font-size: 12px; color: var(--text-tertiary); }
.m-value { font-size: 22px; font-weight: 700; margin-top: 8px; color: var(--text-primary); }
.panel { padding: 20px 22px; margin-bottom: 18px; }
.panel-title { font-size: 13px; font-weight: 600; color: var(--text-secondary); margin-bottom: 14px; }
.fails { display: flex; gap: 28px; }
.fail-item { display: flex; flex-direction: column; gap: 4px; }
.fail-item span { font-size: 12px; color: var(--text-tertiary); }
.fail-item b { font-size: 20px; color: var(--text-primary); }
</style>
