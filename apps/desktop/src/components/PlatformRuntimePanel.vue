<script setup lang="ts">
// 平台实时状态卡片:活跃订阅、平台下发的 OSD 配置、长任务(抓拍/升级)进度。
import { kindLabel, usePlatformRuntime } from "../composables/usePlatformRuntime";

const { subList, osd, progress } = usePlatformRuntime();
</script>

<template>
  <div class="glass-card console-panel runtime-panel">
    <div class="panel-head-tight">
      <div class="panel-title">平台实时状态</div>
      <span class="param-badge">运行时订阅与 OSD</span>
    </div>
    <div class="runtime-grid">
      <div class="rt-block">
        <div class="rt-label">活跃订阅</div>
        <div v-if="subList.length" class="sub-list">
          <div v-for="s in subList" :key="s.kind" class="sub-item">
            <span class="sub-dot" />
            <span class="sub-kind">{{ kindLabel[s.kind] ?? s.kind }}</span>
            <span class="sub-count">NOTIFY {{ s.notify_count }}</span>
          </div>
        </div>
        <div v-else class="rt-idle">无（平台订阅目录、报警或位置后显示）</div>
      </div>
      <div class="rt-block">
        <div class="rt-label">OSD 设置（平台下发）</div>
        <div v-if="osd.received" class="osd-applied">
          <span class="osd-badge" :class="osd.time_show ? 'on' : 'off'">时间 {{ osd.time_show ? '开' : '关' }}</span>
          <span class="osd-badge" :class="osd.osd_show ? 'on' : 'off'">信息 {{ osd.osd_show ? '开' : '关' }}</span>
          <span class="rt-time">{{ osd.at }}</span>
        </div>
        <div v-else class="rt-idle">无（平台下发 OSDConfig 后显示）</div>
      </div>
      <div v-if="progress" class="rt-block progress-block">
        <div class="rt-label">{{ kindLabel[progress.kind] ?? progress.kind }}进度</div>
        <div class="prog-wrap">
          <div
            class="prog-bar"
            role="progressbar"
            :aria-valuenow="progress.percent"
            aria-valuemin="0"
            aria-valuemax="100"
          >
            <div class="prog-fill" :style="{ width: progress.percent + '%' }" />
          </div>
          <div class="prog-txt">{{ progress.current }}/{{ progress.total }} · {{ progress.percent }}%</div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.runtime-panel { height: 100%; box-sizing: border-box; display: flex; flex-direction: column; }
.runtime-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 6px; }
.runtime-grid .rt-block {
  min-width: 0; padding: 7px 9px; border: 1px solid var(--border-default); border-radius: var(--radius-xs);
  background: var(--bg-card); border-top: 1px solid var(--border-default);
}
.runtime-grid .progress-block { grid-column: 1 / -1; }

/* 活跃订阅面板 */
.sub-list { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }
.sub-item {
  display: inline-flex; align-items: center; gap: 4px; padding: 3px 6px;
  background: color-mix(in srgb, var(--success) 10%, transparent); border-radius: 4px; font-size: 10.5px;
}
.sub-dot {
  width: 5px; height: 5px; border-radius: 50%; background: var(--success);
  box-shadow: 0 0 0 2px color-mix(in srgb, var(--success) 18%, transparent);
}
.sub-kind { font-weight: 600; color: var(--text-primary); }
.sub-count { color: var(--text-tertiary); font-size: 9.5px; }

/* OSD 设置(平台下发) */
.osd-applied { display: flex; align-items: center; gap: 6px; padding: 2px 0; }
.osd-badge { font-size: 9.5px; font-weight: 600; padding: 1px 6px; border-radius: 6px; }
.osd-badge.on { color: var(--success); background: color-mix(in srgb, var(--success) 14%, transparent); }
.osd-badge.off { color: var(--text-tertiary); background: var(--bg-surface); }

/* 长任务进度 */
.prog-wrap { display: flex; align-items: center; gap: 6px; margin-top: 4px; }
.prog-bar { flex: 1; height: 5px; background: var(--bg-surface); border-radius: 3px; overflow: hidden; }
.prog-fill { height: 100%; background: var(--accent); border-radius: 3px; transition: width 0.4s; }
.prog-txt { font-size: 10.5px; color: var(--text-secondary); font-family: ui-monospace, "SF Mono", Menlo, monospace; }
</style>
