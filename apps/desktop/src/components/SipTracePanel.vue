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
  traceSearchText,
  filteredTraces,
  traceTone,
  isExpanded,
  toggleTraceRow,
  fmtTs,
  applyTraceEnabled,
  clearTraces,
} = useSipTrace();

const commonMethods = ["all", "REGISTER", "INVITE", "MESSAGE", "BYE", "ACK", "NOTIFY"] as const;

async function onToggleTrace() {
  const result = await applyTraceEnabled();
  if (!result.ok) message.error(result.error ?? "切换追踪失败");
}

async function copyRaw(raw?: string) {
  if (!raw) return;
  try {
    await navigator.clipboard.writeText(raw);
    message.success("SIP 原始报文已复制到剪切板");
  } catch {
    message.error("复制失败，请手动选取复制");
  }
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
        <input
          v-model="traceSearchText"
          type="text"
          class="trace-search"
          placeholder="搜索信令/XML..."
          aria-label="搜索信令报文或XML"
        />
        <button class="trace-clear" type="button" @click="clearTraces">清空</button>
      </div>
    </div>
    <!-- 快捷信令方法筛选胶囊条 -->
    <div class="trace-quick-chips">
      <button
        v-for="method in commonMethods"
        :key="method"
        type="button"
        class="method-chip"
        :class="{ active: traceMethodFilter === method }"
        @click="traceMethodFilter = method"
      >
        {{ method === 'all' ? '全部' : method }}
      </button>
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
        <div v-if="isExpanded(t)" class="trace-detail-box">
          <div class="trace-detail-bar">
            <span class="detail-label">SIP 原始报文与 MANSCDP XML</span>
            <button type="button" class="copy-raw-btn" @click.stop="copyRaw(t.raw)">复制报文</button>
          </div>
          <pre :id="`trace-raw-${i}`" class="trace-raw">{{ t.raw }}</pre>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.trace-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 6px; }
.trace-ctl { display: inline-flex; align-items: center; gap: 5px; flex-wrap: wrap; justify-content: flex-end; }
.trace-toggle { font-size: var(--fs-sm); color: var(--text-secondary); cursor: pointer; user-select: none; }
.trace-filter, .trace-clear {
  border: 1px solid var(--border-default); background: var(--bg-surface);
  border-radius: 4px; padding: 1px 6px; font-size: var(--fs-xs); color: var(--text-secondary); cursor: pointer;
}
.trace-filter { height: 22px; outline: none; }
.trace-filter:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.trace-log {
  height: 220px; overflow-y: auto; overflow-x: hidden; border-radius: var(--radius-xs);
  background: var(--bg-card); border: 1px solid var(--border-default);
  padding: 5px 7px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: var(--fs-sm);
}
.trace-empty { color: var(--text-secondary); padding: 8px; text-align: center; font-size: var(--fs-sm); }
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
.trace-caret { flex: 0 0 auto; color: var(--text-tertiary); font-size: var(--fs-xs); width: 7px; }
.trace-search {
  border: 1px solid var(--border-default); background: var(--bg-surface);
  border-radius: 4px; padding: 1px 6px; font-size: var(--fs-xs); color: var(--text-primary);
  height: 22px; width: 140px; outline: none; transition: border-color 0.15s;
}
.trace-search:focus { border-color: var(--accent); box-shadow: 0 0 0 2px var(--accent-dim); }
.trace-quick-chips {
  display: flex; align-items: center; gap: 4px; margin-bottom: 6px; overflow-x: auto;
}
.method-chip {
  padding: 1px 6px; border-radius: 4px; font-size: 10px; font-weight: 600;
  background: var(--bg-surface); border: 1px solid var(--border-default);
  color: var(--text-secondary); cursor: pointer; transition: all 0.12s;
}
.method-chip:hover { border-color: var(--accent); color: var(--accent); }
.method-chip.active { background: var(--accent); border-color: var(--accent); color: #fff; }
.trace-detail-box {
  margin: 0 0 6px 14px; border: 1px solid var(--border-default); border-radius: 4px; overflow: hidden;
}
.trace-detail-bar {
  display: flex; align-items: center; justify-content: space-between;
  background: var(--bg-surface); padding: 3px 8px; border-bottom: 1px solid var(--border-default);
}
.detail-label { font-size: 10px; color: var(--text-tertiary); }
.copy-raw-btn {
  background: var(--accent-dim); border: 1px solid var(--border-accent);
  color: var(--accent); border-radius: 3px; font-size: 10px; padding: 1px 6px; cursor: pointer;
}
.copy-raw-btn:hover { background: var(--accent); color: #fff; }
.trace-raw {
  margin: 0; padding: 6px 8px; background: var(--code-bg); color: var(--code-fg);
  border-radius: 0; font-size: var(--fs-xs); line-height: 1.4; white-space: pre-wrap;
  word-break: break-all; max-height: 200px; overflow-y: auto;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: var(--cyan); }
.trace-dir.out { color: var(--violet); }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }
</style>
