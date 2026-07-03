<script setup lang="ts">
// 运行监控大盘：订阅 metrics_tick 事件，用 ECharts 绘制实时曲线（FR-42）。
import { ref, onMounted, onUnmounted, shallowRef } from "vue";
import { NCard, NGrid, NGi, NStatistic, NSpace, NText, NButton, useMessage } from "naive-ui";
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
  <n-space vertical size="large">
    <!-- 关键指标卡 -->
    <n-grid :cols="3" :x-gap="12" :y-gap="12">
      <n-gi>
        <n-card>
          <n-statistic
            label="注册成功率"
            :value="latest ? (latest.register_success_rate * 100).toFixed(1) + '%' : '—'"
          />
        </n-card>
      </n-gi>
      <n-gi>
        <n-card>
          <n-statistic
            label="注册成功"
            :value="latest?.register_succeeded ?? '—'"
          />
        </n-card>
      </n-gi>
      <n-gi>
        <n-card>
          <n-statistic
            label="活跃推流"
            :value="latest?.active_streams ?? '—'"
          />
        </n-card>
      </n-gi>
    </n-grid>

    <!-- ECharts 实时曲线 -->
    <n-card title="实时指标曲线">
      <div ref="chartEl" style="height: 320px;" />
    </n-card>

    <!-- 失败归因 + 报告导出 -->
    <n-card title="失败归因" v-if="latest">
      <n-space vertical>
        <n-space>
          <n-text>超时: {{ latest.fail_timeout }}</n-text>
          <n-text>被拒: {{ latest.fail_rejected }}</n-text>
          <n-text>其它: {{ latest.fail_other }}</n-text>
        </n-space>
        <n-button size="small" @click="exportReport">导出报告</n-button>
      </n-space>
    </n-card>
  </n-space>
</template>
