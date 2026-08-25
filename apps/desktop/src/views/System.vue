<script setup lang="ts">
// 系统信息页:目前含日志模块——配置日志级别 + 实时日志流。
import { ref, onMounted, onUnmounted, onActivated, computed, nextTick } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

interface LogLine { ts_ms: number; level: string; target: string; message: string; }

const LEVELS = ["error", "warn", "info", "debug", "trace"];
const level = ref("info");
const logs = ref<LogLine[]>([]);
const MAX = 3000;
const autoScroll = ref(true);
const paused = ref(false);
const filterText = ref("");
const levelFilter = ref<string>("");   // 空=全部
const logBox = ref<HTMLElement | null>(null);
let unlisten: UnlistenFn | null = null;

// 当前级别文本里取主级别(EnvFilter 可能是 "info,media_rtp=debug",取开头)。
const baseLevel = computed(() => (level.value.split(",")[0] || "info").trim());

const filtered = computed(() => {
  const t = filterText.value.trim().toLowerCase();
  const lv = levelFilter.value;
  return logs.value.filter((l) => {
    if (lv && l.level.toLowerCase() !== lv) return false;
    if (t && !(l.message.toLowerCase().includes(t) || l.target.toLowerCase().includes(t))) return false;
    return true;
  });
});

function fmtTs(ms: number): string {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}.${String(d.getMilliseconds()).padStart(3, "0")}`;
}
function levelClass(lv: string): string {
  return "lv-" + lv.toLowerCase();
}

async function applyLevel(lv: string) {
  try {
    const got = await invoke<string>("set_log_level", { level: lv });
    level.value = got;
  } catch (e) {
    console.error("设置日志级别失败", e);
  }
}

async function scrollToBottom() {
  if (!autoScroll.value) return;
  await nextTick();
  if (logBox.value) logBox.value.scrollTop = logBox.value.scrollHeight;
}

function clearLogs() { logs.value = []; }

async function refreshHistory() {
  try {
    const hist = await invoke<LogLine[]>("get_recent_logs", { max: 800 });
    logs.value = hist;
    scrollToBottom();
  } catch (e) {
    console.error(e);
  }
}

onMounted(async () => {
  try { level.value = await invoke<string>("get_log_level"); } catch { /* 忽略 */ }
  await refreshHistory();
  unlisten = await listen<LogLine>("log_line", (e) => {
    if (paused.value) return;
    logs.value.push(e.payload);
    if (logs.value.length > MAX) logs.value.splice(0, logs.value.length - MAX);
    scrollToBottom();
  });
});
onActivated(scrollToBottom);
onUnmounted(() => unlisten?.());
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div class="page-title">系统信息</div>
      <div class="page-sub">软件运行日志:配置级别、实时查看、按级别/关键字筛选</div>
    </div>

    <div class="glass-card panel">
      <div class="panel-title">日志</div>

      <!-- 控制栏:级别切换 + 筛选 + 开关 -->
      <div class="log-ctl">
        <div class="ctl-group">
          <span class="ctl-label">级别</span>
          <div class="seg">
            <button
              v-for="lv in LEVELS"
              :key="lv"
              :class="{ on: baseLevel === lv }"
              @click="applyLevel(lv)"
            >{{ lv.toUpperCase() }}</button>
          </div>
        </div>
        <div class="ctl-group">
          <span class="ctl-label">只看</span>
          <select v-model="levelFilter" class="mini-select">
            <option value="">全部</option>
            <option v-for="lv in LEVELS" :key="lv" :value="lv">{{ lv.toUpperCase() }}</option>
          </select>
        </div>
        <input v-model="filterText" class="log-search" placeholder="关键字过滤(消息/模块)" />
        <div class="ctl-right">
          <label class="ck"><input type="checkbox" v-model="autoScroll" /> 自动滚动</label>
          <label class="ck"><input type="checkbox" v-model="paused" /> 暂停</label>
          <button class="mini-btn" @click="refreshHistory">刷新</button>
          <button class="mini-btn" @click="clearLogs">清空</button>
        </div>
      </div>

      <!-- 日志视图 -->
      <div ref="logBox" class="log-box">
        <div v-if="!filtered.length" class="log-empty">暂无日志(操作设备/压测后将实时显示在此)</div>
        <div v-for="(l, i) in filtered" :key="i" class="log-row">
          <span class="log-ts">{{ fmtTs(l.ts_ms) }}</span>
          <span class="log-lv" :class="levelClass(l.level)">{{ l.level }}</span>
          <span class="log-target">{{ l.target }}</span>
          <span class="log-msg">{{ l.message }}</span>
        </div>
      </div>
      <div class="log-foot">共 {{ filtered.length }} 条 · 当前级别 {{ level }}</div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1320px; }
.page-header { margin-bottom: 10px; }
.page-title { font-size: 17px; font-weight: 700; color: var(--text-primary); }
.page-sub { font-size: 11.5px; color: var(--text-tertiary); margin-top: 2px; }
.panel { padding: 12px 14px; border-radius: var(--radius-md); }
.panel-title {
  font-size: 11px; font-weight: 650; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: .4px; margin-bottom: 8px;
}
.log-ctl { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; margin-bottom: 8px; }
.ctl-group { display: inline-flex; align-items: center; gap: 6px; }
.ctl-label { font-size: 11px; color: var(--text-secondary); }
.seg {
  display: inline-flex; padding: 2px; gap: 2px; border-radius: var(--radius-xs);
  background: rgba(255,255,255,0.5); border: 1px solid var(--border-default);
}
.seg button {
  border: none; background: transparent; padding: 2px 8px; border-radius: 4px;
  font-size: 10.5px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.seg button.on { background: var(--accent); color: #fff; box-shadow: 0 1px 4px var(--accent-glow); }
.mini-select {
  height: 24px; padding: 0 6px; border: 1px solid var(--border-default);
  border-radius: var(--radius-xs); background: rgba(255,255,255,0.7); font-size: 11px; color: var(--text-primary);
}
.log-search {
  flex: 1 1 180px; min-width: 140px; height: 24px; padding: 0 8px;
  border: 1px solid var(--border-default); border-radius: var(--radius-xs);
  background: rgba(255,255,255,0.7); font-size: 11px; color: var(--text-primary); outline: none;
}
.log-search:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.ctl-right { display: inline-flex; align-items: center; gap: 8px; margin-left: auto; }
.ck { font-size: 11px; color: var(--text-secondary); cursor: pointer; user-select: none; }
.mini-btn {
  border: 1px solid var(--border-default); background: rgba(255,255,255,0.6);
  border-radius: 4px; padding: 2px 8px; font-size: 11px; color: var(--text-secondary); cursor: pointer; height: 24px;
}
.mini-btn:hover { border-color: var(--accent); color: var(--accent); }
.log-box {
  height: 480px; overflow-y: auto; border-radius: var(--radius-xs);
  background: rgba(15,23,42,0.03); border: 1px solid var(--border-default);
  padding: 6px 8px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 11px; line-height: 1.45;
}
.log-empty { color: var(--text-tertiary); text-align: center; padding: 18px; font-size: 11.5px; }
.log-row { display: flex; gap: 8px; padding: 1px 0; white-space: nowrap; }
.log-ts { color: var(--text-tertiary); flex: 0 0 auto; font-size: 10.5px; }
.log-lv { flex: 0 0 46px; font-weight: 700; text-transform: uppercase; font-size: 10.5px; }
.lv-error, .lv-ERROR { color: var(--error); }
.lv-warn, .lv-WARN { color: var(--warning); }
.lv-info, .lv-INFO { color: var(--success); }
.lv-debug, .lv-DEBUG { color: var(--accent); }
.lv-trace, .lv-TRACE { color: var(--text-tertiary); }
.log-target { color: var(--text-secondary); flex: 0 0 auto; max-width: 200px; overflow: hidden; text-overflow: ellipsis; font-size: 10.5px; }
.log-msg { color: var(--text-primary); flex: 1 1 auto; white-space: pre-wrap; word-break: break-all; }
.log-foot { margin-top: 6px; font-size: 10.5px; color: var(--text-tertiary); }
</style>
