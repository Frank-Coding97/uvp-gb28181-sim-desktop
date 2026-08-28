<script setup lang="ts">
// SIP 信令实时追踪面板(FR-43):倒序滚动展示收发报文,可按方向/方法过滤并展开原始报文。
import { useMessage } from "naive-ui";
import { useSipTrace } from "../composables/useSipTrace";

const message = useMessage();
const {
  traces,
  traceOn,
  traceDirectionFilter,
  traceMethodFilter,
  traceMethodOptions,
  filteredTraces,
  traceTone,
  isExpanded,
  toggleTraceRow,
  fmtTs,
  applyTraceEnabled,
  clearTraces,
} = useSipTrace();

async function onToggleTrace() {
  const result = await applyTraceEnabled();
  if (!result.ok) message.error(result.error ?? "切换追踪失败");
}
</script>

<template>
  <div class="glass-card console-panel trace-panel">
    <div class="trace-head">
      <div class="panel-title">SIP 信令追踪</div>
      <div class="trace-ctl">
        <label class="trace-toggle">
          <input v-model="traceOn" type="checkbox" @change="onToggleTrace" /> 追踪
        </label>
        <select v-model="traceDirectionFilter" class="trace-filter" aria-label="方向过滤">
          <option value="all">全部方向</option>
          <option value="in">只看接收</option>
          <option value="out">只看发送</option>
        </select>
        <select v-model="traceMethodFilter" class="trace-filter" aria-label="方法过滤">
          <option v-for="m in traceMethodOptions" :key="m" :value="m">{{ m === 'all' ? '全部方法' : m }}</option>
        </select>
        <button class="trace-clear" type="button" @click="clearTraces">清空</button>
      </div>
    </div>
    <div class="trace-log">
      <div v-if="!traces.length" class="trace-empty">
        注册上线后,收发的 SIP 报文将实时显示在这里(新的在最上,点击展开详情)
      </div>
      <div v-else-if="!filteredTraces.length" class="trace-empty">当前过滤条件下没有匹配的 SIP 报文</div>
      <template v-for="(t, i) in filteredTraces" :key="`${t.ts_ms}-${i}-${t.cseq ?? ''}`">
        <button
          type="button"
          class="trace-row"
          :class="[t.direction, traceTone(t), { open: isExpanded(t) }]"
          :aria-expanded="isExpanded(t)"
          :aria-controls="`trace-raw-${i}`"
          @click="toggleTraceRow(t)"
        >
          <span class="trace-caret" aria-hidden="true">{{ isExpanded(t) ? "▾" : "▸" }}</span>
          <span class="trace-ts">{{ fmtTs(t.ts_ms) }}</span>
          <span class="trace-dir" :class="t.direction">{{ t.direction === "in" ? "◀ 收" : "▶ 发" }}</span>
          <span class="trace-sum">{{ t.summary }}</span>
          <span v-if="t.cseq" class="trace-cseq">{{ t.cseq }}</span>
        </button>
        <pre v-if="isExpanded(t)" :id="`trace-raw-${i}`" class="trace-raw">{{ t.raw }}</pre>
      </template>
    </div>
  </div>
</template>

<style scoped>
.trace-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
.trace-ctl { display: inline-flex; align-items: center; gap: 5px; flex-wrap: wrap; justify-content: flex-end; }
.trace-toggle { font-size: 10.5px; color: var(--text-secondary); cursor: pointer; user-select: none; }
.trace-filter, .trace-clear {
  border: 1px solid var(--border-default); background: var(--bg-surface);
  border-radius: 4px; padding: 1px 6px; font-size: 10px; color: var(--text-secondary); cursor: pointer;
}
.trace-filter { height: 22px; outline: none; }
.trace-filter:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.trace-log {
  height: 220px; overflow-y: auto; overflow-x: hidden; border-radius: var(--radius-xs);
  background: var(--bg-card); border: 1px solid var(--border-default);
  padding: 5px 7px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 10.5px;
}
.trace-empty { color: var(--text-secondary); padding: 8px; text-align: center; font-size: 11px; }
/* trace-row 是真正的 <button>(可 Tab 聚焦 + 回车展开),这里抹掉原生按钮外观。 */
.trace-row {
  display: flex; align-items: baseline; gap: 5px; padding: 2px 4px 2px 5px;
  width: 100%; text-align: left; font: inherit; background: none;
  border: none; border-left: 2px solid transparent;
  border-bottom: 1px solid var(--border-default); white-space: nowrap;
  cursor: pointer; transition: background 0.12s, border-color 0.12s;
}
.trace-row.in { border-left-color: var(--cyan); }
.trace-row.out { border-left-color: var(--violet); }
.trace-row.warn { background: color-mix(in srgb, var(--warning) 8%, transparent); }
.trace-row.error { background: color-mix(in srgb, var(--error) 10%, transparent); border-left-color: var(--error); }
.trace-row:hover { background: var(--accent-dim); }
.trace-row.open { background: color-mix(in srgb, var(--accent) 10%, transparent); }
.trace-caret { flex: 0 0 auto; color: var(--text-tertiary); font-size: 8px; width: 7px; }
.trace-raw {
  margin: 0 0 4px 14px; padding: 6px 8px; background: #0f172a; color: #cbd5e1;
  border-radius: 4px; font-size: 10px; line-height: 1.4; white-space: pre-wrap;
  word-break: break-all; max-height: 200px; overflow-y: auto;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: var(--cyan); }
.trace-dir.out { color: var(--violet); }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }
</style>
