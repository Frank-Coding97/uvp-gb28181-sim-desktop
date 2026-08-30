<script setup lang="ts">
// 语音广播卡片(GB/T 28181-2022 §9.12.1)。
//
// 设备在广播里是**语音流接收者**:平台下发广播通知 → 设备回应答 → 设备主动 INVITE →
// 平台把 G.711 音频推到设备这个端口。这张卡回答三个问题:
//   1. 链路建起来了吗(有没有会话、本端哪个端口)
//   2. 真的有音频进来吗(包数/字节,以及"多久没收到包")
//   3. 对面在说话吗(音量条 —— 静音流的包数照样涨,只有电平能区分)
import { useMessage } from "naive-ui";
import { payloadTypeLabel, useVoiceBroadcast } from "../composables/useVoiceBroadcast";

const message = useMessage();
const { session, lastError, hasMedia, stalled, levelPercent, hangup } = useVoiceBroadcast();

async function onHangup() {
  const result = await hangup();
  if (result.ok) message.success(result.message);
  else message.error(result.message);
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

function fmtIdle(ms: number | null): string {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}
</script>

<template>
  <div class="glass-card console-panel broadcast-panel">
    <div class="panel-head-tight">
      <div class="panel-title">语音广播</div>
      <span class="param-badge">§9.12.1 设备收流</span>
    </div>

    <div v-if="!session" class="rt-idle broadcast-idle">
      空闲。平台下发语音广播通知后,设备会主动发 INVITE 并在此显示收流状态。
    </div>

    <template v-else>
      <div class="bc-head">
        <span class="bc-state" :class="{ live: hasMedia && !stalled, warn: stalled }">
          <i />
          {{ stalled ? '链路可疑' : hasMedia ? '收流中' : '等待音频' }}
        </span>
        <button type="button" class="file-btn compact" @click="onHangup">挂断</button>
      </div>

      <!-- 音量条:静音流的包数照样涨,只有电平能说明对面是否在讲话 -->
      <div class="bc-level">
        <div
          class="bc-level-bar"
          role="meter"
          :aria-valuenow="session.recv.level_pct"
          aria-valuemin="0"
          aria-valuemax="100"
          aria-label="语音广播音量"
        >
          <div class="bc-level-fill" :style="{ width: levelPercent }" />
        </div>
        <span class="bc-level-txt">{{ session.recv.level_pct }}%</span>
      </div>

      <dl class="bc-grid">
        <div class="bc-item">
          <dt>语音流发送者</dt>
          <dd class="mono">{{ session.source_id }}</dd>
        </div>
        <div class="bc-item">
          <dt>本端收流端口</dt>
          <dd class="mono">{{ session.local_port }}</dd>
        </div>
        <div class="bc-item">
          <dt>负载类型</dt>
          <dd>{{ payloadTypeLabel(session.recv.payload_type) }}</dd>
        </div>
        <div class="bc-item">
          <dt>收包 / 字节</dt>
          <dd class="mono">{{ session.recv.packets }} / {{ fmtBytes(session.recv.payload_bytes) }}</dd>
        </div>
        <div class="bc-item">
          <dt>丢包 / 乱序</dt>
          <dd class="mono" :class="{ bad: session.recv.lost > 0 }">
            {{ session.recv.lost }} / {{ session.recv.reordered }}
          </dd>
        </div>
        <div class="bc-item">
          <dt>距上次收包</dt>
          <dd class="mono" :class="{ bad: stalled }">{{ fmtIdle(session.recv.idle_ms) }}</dd>
        </div>
      </dl>

      <div v-if="stalled" class="source-notice error">
        超过 3 秒没有收到音频包。按附录 K a),这种情况应释放媒体链路 —— 可以点「挂断」通知平台。
      </div>
    </template>

    <div v-if="lastError" class="source-notice error">{{ lastError }}</div>
  </div>
</template>

<style scoped>
.broadcast-panel { box-sizing: border-box; display: flex; flex-direction: column; }
.broadcast-idle { padding: 6px 0; line-height: 1.5; }

.bc-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.bc-state { display: inline-flex; align-items: center; gap: 5px; font-size: var(--fs-sm); color: var(--text-secondary); }
.bc-state i {
  width: 6px; height: 6px; border-radius: 50%; background: var(--text-tertiary);
}
.bc-state.live { color: var(--success); }
.bc-state.live i {
  background: var(--success);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--success) 16%, transparent);
  animation: bcPulse 1.4s ease-in-out infinite;
}
.bc-state.warn { color: var(--warning); }
.bc-state.warn i { background: var(--warning); box-shadow: 0 0 0 3px color-mix(in srgb, var(--warning) 16%, transparent); }
@keyframes bcPulse { 50% { opacity: 0.45; } }

.bc-level { display: flex; align-items: center; gap: 6px; margin: 7px 0; }
.bc-level-bar {
  flex: 1; height: 6px; border-radius: 3px; overflow: hidden;
  background: var(--bg-surface); border: 1px solid var(--border-default);
}
.bc-level-fill {
  height: 100%;
  background: linear-gradient(90deg, var(--success), var(--accent));
  border-radius: 3px;
  transition: width 0.25s ease-out;
}
.bc-level-txt {
  flex: 0 0 auto; min-width: 32px; text-align: right;
  font-size: var(--fs-xs); color: var(--text-tertiary);
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-variant-numeric: tabular-nums;
}

.bc-grid {
  display: grid; grid-template-columns: repeat(auto-fit, minmax(120px, 1fr));
  gap: 5px; margin: 0;
}
.bc-item {
  min-width: 0; padding: 5px 7px; border-radius: var(--radius-xs);
  background: var(--bg-card); border: 1px solid var(--border-default);
}
.bc-item dt {
  font-size: var(--fs-xs); color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: 0.2px;
}
.bc-item dd {
  margin: 2px 0 0; font-size: var(--fs-sm); color: var(--text-primary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.bc-item dd.mono { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-variant-numeric: tabular-nums; }
.bc-item dd.bad { color: var(--error); }
</style>
