<script setup lang="ts">
// 历史运行面板:SQLite 历史索引(list_stress_runs)的列表 + 任选两条做关键指标对比。
//
// 定位(见 data-model.md):JSONL/报告文件仍是事实源,这里只是"紧凑二次索引" ——
// 回答"这次比上次好还是差、过载拐点在不在同一水位",不用翻报告文件。
// 对比是摘要级的;逐秒曲线叠加是后续项(样本已入 metric_sample 表)。
import { computed, h, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { NDataTable, NTag } from "naive-ui";
import type { DataTableRowKey } from "naive-ui";
import type { StressRunSummaryDto } from "../types/generated/StressRunSummaryDto";

const runs = ref<StressRunSummaryDto[]>([]);
const loading = ref(false);
const loadError = ref<string | null>(null);
/** 对比选择:最多两条,按勾选顺序为 A、B。 */
const compareKeys = ref<DataTableRowKey[]>([]);

async function refresh() {
  loading.value = true;
  loadError.value = null;
  try {
    runs.value = await invoke<StressRunSummaryDto[]>("list_stress_runs", { limit: 20 });
  } catch (e) {
    loadError.value = String(e);
  } finally {
    loading.value = false;
  }
}
onMounted(refresh);

const columns = computed(() => [
  { type: "selection" as const },
  {
    title: "时间",
    key: "started_at_ms",
    render: (row: StressRunSummaryDto) => new Date(row.started_at_ms).toLocaleString("zh-CN", { hour12: false }),
    width: 150,
  },
  { title: "设备", key: "device_count", width: 60 },
  {
    title: "成功率",
    key: "device_success_rate",
    render: (row: StressRunSummaryDto) => `${(row.device_success_rate * 100).toFixed(1)}%`,
  },
  { title: "p95", key: "register_latency_p95_ms", render: (row: StressRunSummaryDto) => `${row.register_latency_p95_ms} ms` },
  { title: "峰值在线", key: "devices_online_peak" },
  { title: "过载拒绝", key: "fail_rejected_overload" },
  {
    title: "结论",
    key: "results_trustworthy",
    render: (row: StressRunSummaryDto) =>
      row.results_trustworthy
        ? h(NTag, { size: "small", type: "success", bordered: false }, { default: () => "可信" })
        : h(NTag, { size: "small", type: "warning", bordered: false }, { default: () => "含本地压力" }),
  },
  {
    title: "结果",
    key: "outcome",
    render: (row: StressRunSummaryDto) =>
      h(
        NTag,
        {
          size: "small",
          bordered: false,
          type: row.outcome === "completed" ? "success" : row.outcome === "stopped" ? "default" : "error",
        },
        { default: () => row.outcome },
      ),
  },
]);

const comparePair = computed(() => {
  const [a, b] = compareKeys.value;
  const pick = (k: DataTableRowKey | undefined) =>
    runs.value.find((r) => r.run_id === k);
  return { a: pick(a), b: pick(b) };
});

/** 差值展示:数值类给 ±,比率类给百分点。 */
interface DiffRow {
  label: string;
  a: string;
  b: string;
  delta: string;
  goodWhenBHigher: boolean | null;
}
const diffRows = computed<DiffRow[]>(() => {
  const { a, b } = comparePair.value;
  if (!a || !b) return [];
  const fmt = (v: number | null | undefined, suffix = "") =>
    v === null || v === undefined ? "—" : `${v}${suffix}`;
  const int = (x: number | null | undefined, y: number | null | undefined) =>
    x === null || x === undefined || y === null || y === undefined ? "—" : `${y - x >= 0 ? "+" : ""}${y - x}`;
  const pct = (x: number, y: number) => `${((y - x) * 100).toFixed(1)} pp`;
  return [
    {
      label: "设备成功率",
      a: `${(a.device_success_rate * 100).toFixed(1)}%`,
      b: `${(b.device_success_rate * 100).toFixed(1)}%`,
      delta: pct(a.device_success_rate, b.device_success_rate),
      goodWhenBHigher: true,
    },
    {
      label: "注册成功数",
      a: fmt(a.register_succeeded),
      b: fmt(b.register_succeeded),
      delta: int(a.register_succeeded, b.register_succeeded),
      goodWhenBHigher: true,
    },
    {
      label: "注册延迟 p95",
      a: `${a.register_latency_p95_ms} ms`,
      b: `${b.register_latency_p95_ms} ms`,
      delta: `${b.register_latency_p95_ms - a.register_latency_p95_ms >= 0 ? "+" : ""}${b.register_latency_p95_ms - a.register_latency_p95_ms} ms`,
      goodWhenBHigher: false,
    },
    {
      label: "峰值在线",
      a: fmt(a.devices_online_peak),
      b: fmt(b.devices_online_peak),
      delta: int(a.devices_online_peak, b.devices_online_peak),
      goodWhenBHigher: true,
    },
    {
      label: "失败 · 超时",
      a: fmt(a.fail_timeout),
      b: fmt(b.fail_timeout),
      delta: int(a.fail_timeout, b.fail_timeout),
      goodWhenBHigher: false,
    },
    {
      label: "失败 · 凭据/权限",
      a: fmt(a.fail_rejected_auth),
      b: fmt(b.fail_rejected_auth),
      delta: int(a.fail_rejected_auth, b.fail_rejected_auth),
      goodWhenBHigher: false,
    },
    {
      label: "失败 · 平台过载",
      a: fmt(a.fail_rejected_overload),
      b: fmt(b.fail_rejected_overload),
      delta: int(a.fail_rejected_overload, b.fail_rejected_overload),
      goodWhenBHigher: false,
    },
    {
      label: "失败 · 本地压力",
      a: fmt(a.fail_local),
      b: fmt(b.fail_local),
      delta: int(a.fail_local, b.fail_local),
      goodWhenBHigher: false,
    },
    {
      label: "爬坡被节流",
      a: fmt(a.ramp_throttled),
      b: fmt(b.ramp_throttled),
      delta: int(a.ramp_throttled, b.ramp_throttled),
      goodWhenBHigher: false,
    },
    {
      label: "时长",
      a: `${Math.max(0, Math.round((a.ended_at_ms - a.started_at_ms) / 1000))} s`,
      b: `${Math.max(0, Math.round((b.ended_at_ms - b.started_at_ms) / 1000))} s`,
      delta: "—",
      goodWhenBHigher: null,
    },
  ];
});

function deltaClass(row: DiffRow): string {
  if (row.goodWhenBHigher === null || row.delta === "—") return "";
  const up = row.delta.startsWith("+") && row.delta !== "+0";
  if (row.delta === "+0" || row.delta === "0" || row.delta === "+0.0 pp" || row.delta === "0.0 pp") return "";
  return up === row.goodWhenBHigher ? "delta-good" : "delta-bad";
}
</script>

<template>
  <div class="glass-card panel history-panel">
    <div class="panel-head">
      <div>历史运行 <span class="hint">SQLite 索引 · 保留 30 天 · 勾选两条对比</span></div>
      <button type="button" class="btn btn-ghost" :disabled="loading" @click="refresh">
        {{ loading ? "加载中…" : "刷新" }}
      </button>
    </div>
    <p v-if="loadError" class="history-error">历史读取失败:{{ loadError }}</p>
    <p v-else-if="!runs.length && !loading" class="hint">
      还没有已入库的压测运行 —— 完成一次压测后会自动记录(报告与 JSONL 仍是完整事实源)。
    </p>
    <NDataTable
      v-else
      :columns="columns"
      :data="runs"
      :row-key="(row: StressRunSummaryDto) => row.run_id"
      v-model:checked-row-keys="compareKeys"
      :max-height="220"
      size="small"
      :bordered="false"
    />
    <div v-if="diffRows.length" class="compare">
      <div class="compare-head">
        <span>对比 · B 相对 A</span>
        <span class="hint">A #{{ comparePair.a?.run_id }} → B #{{ comparePair.b?.run_id }}</span>
      </div>
      <table class="compare-table">
        <thead>
          <tr><th>指标</th><th>A</th><th>B</th><th>Δ</th></tr>
        </thead>
        <tbody>
          <tr v-for="row in diffRows" :key="row.label">
            <td>{{ row.label }}</td>
            <td>{{ row.a }}</td>
            <td>{{ row.b }}</td>
            <td :class="deltaClass(row)">{{ row.delta }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>

<style scoped>
.history-panel { display: flex; flex-direction: column; gap: 8px; }
.panel-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.hint { font-size: var(--fs-xs); color: var(--text-secondary); font-weight: 400; }
.history-error { font-size: var(--fs-sm); color: var(--danger, #d03050); }
.compare { border-top: 1px solid var(--border-default); padding-top: 8px; }
.compare-head { display: flex; align-items: baseline; gap: 10px; font-weight: 600; margin-bottom: 6px; }
.compare-table { width: 100%; border-collapse: collapse; font-size: var(--fs-sm); }
.compare-table th, .compare-table td { padding: 3px 8px; text-align: right; border-bottom: 1px solid var(--border-default); }
.compare-table th:first-child, .compare-table td:first-child { text-align: left; }
.delta-good { color: var(--success, #18a058); font-weight: 600; }
.delta-bad { color: var(--danger, #d03050); font-weight: 600; }
</style>
