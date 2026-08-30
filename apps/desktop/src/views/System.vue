<script setup lang="ts">
import { computed, nextTick, onActivated, onMounted, onUnmounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface LogLine { ts_ms: number; level: string; target: string; message: string; }
interface TraceEntry {
  ts_ms: number; direction: "in" | "out"; method: string; status?: number;
  cseq?: string; peer: string; summary: string; raw?: string;
}
interface CmdEntry { kind: string; summary: string; ts_ms: number; }

type LogTab = "sip" | "system" | "commands";
const LEVELS = ["error", "warn", "info", "debug", "trace"];
const activeTab = ref<LogTab>("sip");
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const level = ref("info");
const logs = ref<LogLine[]>([]);
const traces = ref<TraceEntry[]>([]);
const commands = ref<CmdEntry[]>([]);
const autoScroll = ref(true);
const paused = ref(false);
const filterText = ref("");
const levelFilter = ref("");
const traceOn = ref(true);
const expandedTrace = ref<number | null>(null);
const logBox = ref<HTMLElement | null>(null);
const MAX_LOGS = 3000;
const MAX_TRACES = 500;
const MAX_COMMANDS = 500;
const pageCreatedAt = Date.now();

let unlistenLog: UnlistenFn | null = null;
let unlistenTrace: UnlistenFn | null = null;
let unlistenCommand: UnlistenFn | null = null;

const tabs = [
  { key: "sip" as const, label: "SIP 日志" },
  { key: "system" as const, label: "系统日志" },
  { key: "commands" as const, label: "平台命令" },
];
const baseLevel = computed(() => (level.value.split(",")[0] || "info").trim());
const filteredLogs = computed(() => {
  const term = filterText.value.trim().toLowerCase();
  return logs.value.filter((item) => {
    if (levelFilter.value && item.level.toLowerCase() !== levelFilter.value) return false;
    return !term || `${item.target} ${item.message}`.toLowerCase().includes(term);
  });
});
const filteredTraces = computed(() => {
  const term = filterText.value.trim().toLowerCase();
  return traces.value.filter((item) => !term || `${item.summary} ${item.method} ${item.peer} ${item.raw ?? ""}`.toLowerCase().includes(term));
});
const historicalSipLogs = computed(() => {
  const term = filterText.value.trim().toLowerCase();
  return logs.value.filter((item) =>
    item.target.includes("sip_trace") && item.ts_ms < pageCreatedAt &&
    (!term || item.message.toLowerCase().includes(term))
  );
});
const tabCount = (key: LogTab) => key === "sip"
  ? traces.value.length + historicalSipLogs.value.length
  : key === "system" ? logs.value.length : commands.value.length;
const filteredCommands = computed(() => {
  const term = filterText.value.trim().toLowerCase();
  return commands.value.filter((item) => !term || `${item.kind} ${item.summary}`.toLowerCase().includes(term));
});

function fmtTs(ms: number, milliseconds = true) {
  const d = new Date(ms);
  const base = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
  return milliseconds ? `${base}.${String(d.getMilliseconds()).padStart(3, "0")}` : base;
}
function levelClass(value: string) { return `lv-${value.toLowerCase()}`; }
function commandLabel(kind: string) {
  return ({ query: "查询", control: "控制", invite: "点播", broadcast: "广播" } as Record<string, string>)[kind] ?? kind;
}
async function applyLevel(value: string) {
  try { level.value = await invoke<string>("set_log_level", { level: value }); }
  catch (error) { console.error("设置日志级别失败", error); }
}
async function scrollToBottom() {
  if (!autoScroll.value || activeTab.value === "sip") return;
  await nextTick();
  if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight;
}
async function refreshHistory() {
  try {
    logs.value = await invoke<LogLine[]>("get_recent_logs", { max: 1200 });
    await scrollToBottom();
  } catch (error) { console.error("读取日志历史失败", error); }
}
async function toggleTrace() {
  try { await invoke<string>("set_sip_trace", { enabled: traceOn.value }); }
  catch (error) { console.error("切换 SIP 追踪失败", error); }
}
function clearCurrent() {
  if (activeTab.value === "sip") {
    traces.value = [];
    expandedTrace.value = null;
  } else if (activeTab.value === "commands") commands.value = [];
  else logs.value = [];
}
function toggleTraceRow(index: number) {
  expandedTrace.value = expandedTrace.value === index ? null : index;
}

onMounted(async () => {
  if (!isTauri) return;
  try { level.value = await invoke<string>("get_log_level"); } catch { /* 使用默认 */ }
  await refreshHistory();
  unlistenLog = await listen<LogLine>("log_line", (event) => {
    if (paused.value) return;
    logs.value.push(event.payload);
    if (logs.value.length > MAX_LOGS) logs.value.splice(0, logs.value.length - MAX_LOGS);
    void scrollToBottom();
  });
  unlistenTrace = await listen<TraceEntry>("sip_trace", (event) => {
    if (!traceOn.value || paused.value) return;
    traces.value.unshift(event.payload);
    if (traces.value.length > MAX_TRACES) traces.value.splice(MAX_TRACES);
    if (expandedTrace.value !== null) expandedTrace.value += 1;
  });
  unlistenCommand = await listen<CmdEntry>("platform_command", (event) => {
    if (paused.value) return;
    commands.value.unshift(event.payload);
    if (commands.value.length > MAX_COMMANDS) commands.value.splice(MAX_COMMANDS);
  });
});
onActivated(scrollToBottom);
onUnmounted(() => { unlistenLog?.(); unlistenTrace?.(); unlistenCommand?.(); });
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div class="page-title">日志</div>
      <div class="page-sub">集中查看完整 SIP 报文、系统运行日志和平台命令，不因切换业务页面而清空</div>
    </div>

    <section class="glass-card panel">
      <div class="tabs" role="tablist" aria-label="日志类型">
        <button v-for="tab in tabs" :key="tab.key" role="tab" :aria-selected="activeTab === tab.key" :class="{ active: activeTab === tab.key }" @click="activeTab = tab.key">
          {{ tab.label }} <small>{{ tabCount(tab.key) }}</small>
        </button>
      </div>

      <div class="toolbar">
        <template v-if="activeTab === 'system'">
          <span class="label">级别</span>
          <div class="seg">
            <button v-for="item in LEVELS" :key="item" :class="{ on: baseLevel === item }" @click="applyLevel(item)">{{ item.toUpperCase() }}</button>
          </div>
          <select v-model="levelFilter" class="mini-select" aria-label="系统日志级别筛选">
            <option value="">全部级别</option>
            <option v-for="item in LEVELS" :key="item" :value="item">{{ item.toUpperCase() }}</option>
          </select>
        </template>
        <label v-if="activeTab === 'sip'" class="check"><input v-model="traceOn" type="checkbox" @change="toggleTrace" /> SIP 追踪</label>
        <input v-model="filterText" class="search" :placeholder="activeTab === 'sip' ? '筛选方法、对端或报文内容' : '筛选关键字'" />
        <div class="toolbar-right">
          <label class="check"><input v-model="paused" type="checkbox" /> 暂停</label>
          <label v-if="activeTab === 'system'" class="check"><input v-model="autoScroll" type="checkbox" /> 自动滚动</label>
          <button v-if="activeTab === 'system'" class="mini-btn" @click="refreshHistory">刷新历史</button>
          <button class="mini-btn danger" @click="clearCurrent">清空视图</button>
        </div>
      </div>

      <div v-if="activeTab === 'sip'" class="log-box sip-box">
        <div v-if="!filteredTraces.length && !historicalSipLogs.length" class="empty">等待 SIP 报文；完整报文会同时保留在系统日志和磁盘日志中</div>
        <template v-for="(item, index) in filteredTraces" :key="`${item.ts_ms}-${index}`">
          <div class="trace-row" :class="[item.direction, { open: expandedTrace === index }]" @click="toggleTraceRow(index)">
            <span>{{ expandedTrace === index ? "▾" : "▸" }}</span>
            <span class="time">{{ fmtTs(item.ts_ms, false) }}</span>
            <b :class="item.direction">{{ item.direction === "in" ? "◀ 收" : "▶ 发" }}</b>
            <span class="trace-summary">{{ item.summary }}</span>
            <code>{{ item.cseq }}</code>
          </div>
          <pre v-if="expandedTrace === index" class="raw">{{ item.raw }}</pre>
        </template>
        <template v-if="historicalSipLogs.length">
          <div class="history-divider">进入本页前的 SIP 完整报文</div>
          <div v-for="(item, index) in historicalSipLogs" :key="`history-${item.ts_ms}-${index}`" class="history-trace">
            <span class="time">{{ fmtTs(item.ts_ms) }}</span>
            <pre>{{ item.message }}</pre>
          </div>
        </template>
      </div>

      <div v-else-if="activeTab === 'commands'" class="log-box command-box">
        <div v-if="!filteredCommands.length" class="empty">等待平台下发查询、控制、点播或广播命令</div>
        <div v-for="(item, index) in filteredCommands" :key="`${item.ts_ms}-${index}`" class="command-row">
          <span class="time">{{ fmtTs(item.ts_ms) }}</span>
          <b>{{ commandLabel(item.kind) }}</b>
          <span>{{ item.summary }}</span>
        </div>
      </div>

      <div v-else ref="logBox" class="log-box system-box">
        <div v-if="!filteredLogs.length" class="empty">暂无系统日志</div>
        <div v-for="(item, index) in filteredLogs" :key="`${item.ts_ms}-${index}`" class="system-row">
          <span class="time">{{ fmtTs(item.ts_ms) }}</span>
          <b :class="levelClass(item.level)">{{ item.level }}</b>
          <code>{{ item.target }}</code>
          <pre>{{ item.message }}</pre>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.page-header { margin-bottom: 18px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.panel { padding: 18px; }
.tabs { display: flex; gap: 6px; margin-bottom: 14px; border-bottom: 1px solid var(--border-default); }
.tabs button { position: relative; padding: 9px 14px 11px; border: 0; color: var(--text-secondary); background: transparent; cursor: pointer; font-size: 13px; }
.tabs button::after { content: ""; position: absolute; right: 10px; bottom: -1px; left: 10px; height: 2px; border-radius: 2px; background: transparent; }
.tabs button.active { color: var(--accent); font-weight: 650; }
.tabs button.active::after { background: var(--accent); }
.tabs small { margin-left: 5px; color: var(--text-tertiary); font-size: 10px; }
.toolbar { display: flex; align-items: center; gap: 10px; margin-bottom: 12px; }
.label, .check { color: var(--text-secondary); font-size: 12px; white-space: nowrap; }
.seg { display: flex; gap: 2px; padding: 2px; border: 1px solid var(--border-default); border-radius: 7px; background: rgba(255,255,255,.45); }
.seg button, .mini-btn { border: 0; border-radius: 5px; color: var(--text-secondary); background: transparent; cursor: pointer; font-size: 11px; }
.seg button { padding: 4px 8px; }
.seg button.on { color: #fff; background: var(--accent); }
.mini-select, .search { height: 30px; border: 1px solid var(--border-default); border-radius: 7px; color: var(--text-primary); background: rgba(255,255,255,.65); font-size: 12px; }
.mini-select { padding: 0 8px; }
.search { min-width: 180px; flex: 1; padding: 0 10px; outline: none; }
.search:focus { border-color: var(--accent); }
.toolbar-right { display: flex; align-items: center; gap: 10px; margin-left: auto; }
.mini-btn { padding: 6px 10px; border: 1px solid var(--border-default); background: rgba(255,255,255,.45); }
.mini-btn:hover { border-color: var(--accent); color: var(--accent); }
.mini-btn.danger:hover { border-color: var(--error); color: var(--error); }
.log-box { height: min(66vh, 650px); overflow: auto; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(15,23,42,.028); font: 12px/1.55 "SF Mono", Menlo, monospace; }
.empty { padding: 34px; color: var(--text-tertiary); text-align: center; font-family: inherit; }
.time { flex: 0 0 auto; color: var(--text-tertiary); }
.trace-row, .command-row { display: flex; align-items: center; gap: 10px; padding: 7px 10px; border-bottom: 1px solid rgba(120,130,150,.09); cursor: pointer; }
.trace-row:hover, .command-row:hover { background: rgba(56,132,255,.045); }
.trace-row b { width: 38px; font-size: 11px; }
.trace-row b.in { color: var(--success); }
.trace-row b.out { color: var(--accent); }
.trace-summary { min-width: 0; flex: 1; overflow: hidden; color: var(--text-primary); text-overflow: ellipsis; white-space: nowrap; }
.trace-row code { color: var(--text-tertiary); }
.raw { margin: 0; padding: 12px 15px 15px 74px; overflow-x: auto; color: #cbdaf0; background: #111e30; white-space: pre-wrap; word-break: break-all; }
.history-divider { padding: 8px 10px; color: var(--text-tertiary); background: rgba(15,23,42,.045); font-family: inherit; font-size: 10px; letter-spacing: .04em; }
.history-trace { display: grid; grid-template-columns: 88px 1fr; gap: 10px; padding: 8px 10px; border-bottom: 1px solid rgba(120,130,150,.09); }
.history-trace pre { margin: 0; color: var(--text-primary); font: inherit; white-space: pre-wrap; word-break: break-all; }
.command-row { cursor: default; }
.command-row b { min-width: 45px; color: var(--accent); }
.command-row > span:last-child { color: var(--text-primary); }
.system-row { display: grid; grid-template-columns: 88px 50px minmax(100px, 210px) 1fr; gap: 9px; padding: 4px 10px; border-bottom: 1px solid rgba(120,130,150,.06); }
.system-row > b { text-transform: uppercase; }
.system-row code { overflow: hidden; color: var(--text-secondary); text-overflow: ellipsis; white-space: nowrap; }
.system-row pre { margin: 0; color: var(--text-primary); font: inherit; white-space: pre-wrap; word-break: break-all; }
.lv-error { color: var(--error); }.lv-warn { color: var(--warning); }.lv-info { color: var(--success); }.lv-debug { color: var(--accent); }.lv-trace { color: var(--text-tertiary); }
@media (max-width: 900px) { .toolbar { flex-wrap: wrap; }.toolbar-right { width: 100%; margin-left: 0; }.system-row { grid-template-columns: 80px 45px 1fr; }.system-row pre { grid-column: 1 / -1; } }
</style>
