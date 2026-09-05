// SIP 信令实时追踪(FR-43,模块级单例):订阅后端 sip_trace 事件,滚动保留最近 N 条,
// 支持按方向/方法过滤与展开原始报文。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { formatError } from "../lib/ipcError";

export interface TraceEntry {
  ts_ms: number;
  direction: "in" | "out";
  method: string;
  status?: number;
  cseq?: string;
  call_id?: string;
  peer: string;
  summary: string;
  raw?: string;
}

const MAX_TRACE = 200;

const traces = ref<TraceEntry[]>([]);
const traceOn = ref(true);
const traceDirectionFilter = ref<"all" | "in" | "out">("all");
const traceMethodFilter = ref("all");
const traceSearchText = ref("");
// 展开态按条目身份记录,而不是按下标:下标会在新报文插到头部后错位,
// 过滤条件变化时更是对不上(旧实现靠"展开下标 +1"补偿,过滤后依然会串行)。
const expandedEntry = ref<TraceEntry | null>(null);

const traceMethodOptions = computed(() => {
  const methods = Array.from(new Set(traces.value.map((t) => t.method || "响应"))).sort();
  return ["all", ...methods];
});

const filteredTraces = computed(() =>
  traces.value.filter((t) => {
    if (traceDirectionFilter.value !== "all" && t.direction !== traceDirectionFilter.value) return false;
    if (traceMethodFilter.value !== "all" && (t.method || "响应") !== traceMethodFilter.value) return false;
    if (traceSearchText.value.trim()) {
      const q = traceSearchText.value.trim().toLowerCase();
      const matchSummary = t.summary.toLowerCase().includes(q);
      const matchRaw = t.raw ? t.raw.toLowerCase().includes(q) : false;
      const matchCSeq = t.cseq ? t.cseq.toLowerCase().includes(q) : false;
      const matchCallId = t.call_id ? t.call_id.toLowerCase().includes(q) : false;
      if (!matchSummary && !matchRaw && !matchCSeq && !matchCallId) return false;
    }
    return true;
  })
);

function traceTone(t: TraceEntry): "" | "warn" | "error" {
  // 401 是注册鉴权的正常一跳,不算异常。
  if (t.status && t.status >= 400 && t.status !== 401) return "error";
  if (t.status && t.status >= 300) return "warn";
  return "";
}

function isExpanded(t: TraceEntry): boolean {
  return expandedEntry.value === t;
}

function toggleTraceRow(t: TraceEntry) {
  expandedEntry.value = expandedEntry.value === t ? null : t;
}

function fmtTs(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
}

/** 开/关后端追踪上报。返回错误文本供调用方提示。 */
async function applyTraceEnabled(): Promise<{ ok: boolean; error?: string }> {
  try {
    await invoke<string>("set_sip_trace", { enabled: traceOn.value });
    return { ok: true };
  } catch (e) {
    return { ok: false, error: formatError(e, "切换追踪失败") };
  }
}

function clearTraces() {
  traces.value = [];
  expandedEntry.value = null;
  traceMethodFilter.value = "all";
  traceDirectionFilter.value = "all";
}

let unlistenTrace: UnlistenFn | null = null;

async function startSipTrace() {
  if (unlistenTrace) return;
  unlistenTrace = await listen<TraceEntry>("sip_trace", (e) => {
    if (!traceOn.value) return;
    traces.value.unshift(e.payload); // 新的在最上(倒序)
    if (traces.value.length > MAX_TRACE) {
      const dropped = traces.value.splice(MAX_TRACE);
      // 被滚出窗口的条目若正展开,收起以免留一个孤立的原始报文块。
      if (expandedEntry.value && dropped.includes(expandedEntry.value)) expandedEntry.value = null;
    }
  });
}

function stopSipTrace() {
  unlistenTrace?.();
  unlistenTrace = null;
}

export function useSipTrace() {
  return {
    traces,
    traceOn,
    traceDirectionFilter,
    traceMethodFilter,
    traceMethodOptions,
    traceSearchText,
    filteredTraces,
    traceTone,
    isExpanded,
    toggleTraceRow,
    fmtTs,
    applyTraceEnabled,
    clearTraces,
    startSipTrace,
    stopSipTrace,
  };
}
