<script setup lang="ts">
// 平台命令时间线:后端每收到一条平台命令(查询/控制/点播/广播…)推一条语义事件。
import { fmtCmdTs, kindLabel, usePlatformRuntime } from "../composables/usePlatformRuntime";

const { commands } = usePlatformRuntime();
</script>

<template>
  <div class="glass-card console-panel timeline-panel">
    <div class="panel-head-tight">
      <div class="panel-title">平台命令时间线</div>
      <span class="param-badge">交互命令流水</span>
    </div>
    <div v-if="commands.length === 0" class="rt-idle empty-timeline">
      等待平台下发命令(注册后平台会查目录、下发控制等)…
    </div>
    <ul v-else class="cmd-list">
      <li v-for="c in commands" :key="`${c.ts_ms}-${c.kind}-${c.summary}`" class="cmd-item">
        <span class="cmd-ts">{{ fmtCmdTs(c.ts_ms) }}</span>
        <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
        <span class="cmd-sum">{{ c.summary }}</span>
      </li>
    </ul>
  </div>
</template>

<style scoped>
.timeline-panel { height: 100%; box-sizing: border-box; display: flex; flex-direction: column; }
.empty-timeline { padding: 12px 0; font-size: var(--fs-sm); }
.cmd-list { margin: 4px 0 0; padding: 0; list-style: none; max-height: 180px; overflow-y: auto; }
.cmd-item {
  display: flex; align-items: center; gap: 6px; padding: 3px 0;
  border-bottom: 1px solid var(--border-default); font-size: var(--fs-sm);
}
.cmd-ts { color: var(--text-tertiary); font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: var(--fs-xs); flex-shrink: 0; }
.cmd-tag {
  flex-shrink: 0; font-size: var(--fs-xs); font-weight: 600; padding: 1px 5px; border-radius: 4px;
  background: var(--accent-dim); color: var(--accent);
}
.cmd-tag.k-control { background: color-mix(in srgb, var(--warning) 14%, transparent); color: var(--warning); }
.cmd-tag.k-invite { background: color-mix(in srgb, var(--success) 14%, transparent); color: var(--success); }
.cmd-tag.k-broadcast { background: color-mix(in srgb, var(--violet) 16%, transparent); color: var(--violet); }
.cmd-sum { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
</style>
