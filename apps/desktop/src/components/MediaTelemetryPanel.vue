<script setup lang="ts">
// 推流质量卡:本端发送统计 + 平台回的 RTCP 接收报告(RFC 3550 §6.4.2)。
//
// 回答的问题是"平台到底收到了多少":
//   · 左半是本端视角(发了多少包/字节、SR 发了几条、有没有本地发送失败)
//   · 右半是平台视角(丢包率、累计丢包、抖动、RTT)—— 只有 RR 能给出这些
// 平台不回 RR 时明确显示"平台未回报",而不是把 0% 丢包画成"一切正常"。
import {
  formatBitrate,
  formatBytes,
  formatMs,
  useMediaTelemetry,
} from "../composables/useMediaTelemetry";

const {
  telemetry,
  lastError,
  hasFeedback,
  feedbackUnavailable,
  feedbackStale,
  qualityWarning,
} = useMediaTelemetry();
</script>

<template>
  <div class="glass-card console-panel telemetry-panel">
    <div class="panel-head-tight">
      <div class="panel-title">推流质量</div>
      <span class="param-badge">RTP/RTCP 反馈</span>
    </div>

    <div v-if="!telemetry" class="rt-idle telemetry-idle">
      无推流会话。平台下发 INVITE 建立实时/回放流后,这里显示发送统计与平台回报的收流质量。
    </div>

    <template v-else>
      <div class="tm-head">
        <span class="tm-state" :class="{ live: !qualityWarning, warn: qualityWarning }">
          <i />
          {{ telemetry.is_history ? '回放/下载推流中' : '实时推流中' }}
        </span>
        <span v-if="telemetry.media_source" class="tm-source mono" :title="telemetry.media_source">
          {{ telemetry.media_source }}
        </span>
      </div>

      <div class="tm-split">
        <!-- 本端视角 -->
        <section class="tm-side">
          <h4 class="tm-side-title">本端发送</h4>
          <dl class="tm-grid">
            <div class="tm-item">
              <dt>RTP 包 / 字节</dt>
              <dd class="mono">
                {{ telemetry.sent_packets }} / {{ formatBytes(telemetry.sent_bytes) }}
              </dd>
            </div>
            <div class="tm-item">
              <dt>平均包长</dt>
              <dd class="mono">
                {{ formatBitrate(telemetry.sent_bytes, telemetry.sent_packets) }}
              </dd>
            </div>
            <div class="tm-item">
              <dt>RTCP SR 已发</dt>
              <dd class="mono">{{ telemetry.sent_rtcp }}</dd>
            </div>
            <div class="tm-item">
              <dt>本地发送失败</dt>
              <dd class="mono" :class="{ bad: telemetry.send_errors > 0 }">
                {{ telemetry.send_errors }}
              </dd>
            </div>
          </dl>
        </section>

        <!-- 平台视角(RTCP RR) -->
        <section class="tm-side">
          <h4 class="tm-side-title">
            平台回报
            <span v-if="hasFeedback" class="tm-age">{{ formatMs(telemetry.feedback.age_ms) }}前</span>
          </h4>

          <div v-if="feedbackUnavailable" class="tm-nofeedback">
            平台未回报 RTCP RR。
            <template v-if="telemetry.local_rtcp_port === null">
              本端也没有独立 RTCP 端口(TCP 传输,或 RTP 端口 +1 被占用)。
            </template>
            <template v-else>
              本端 RTCP 端口 {{ telemetry.local_rtcp_port }} 正在监听 —— 多数平台不发 RR,这不代表链路有问题。
            </template>
          </div>

          <dl v-else class="tm-grid">
            <div class="tm-item">
              <dt>丢包率</dt>
              <dd
                class="mono"
                :class="{ bad: telemetry.feedback.loss_percent >= 1 }"
              >
                {{ telemetry.feedback.loss_percent.toFixed(2) }}%
              </dd>
            </div>
            <div class="tm-item">
              <dt>累计丢包</dt>
              <dd class="mono" :class="{ bad: telemetry.feedback.cumulative_lost > 0 }">
                {{ telemetry.feedback.cumulative_lost }}
              </dd>
            </div>
            <div class="tm-item">
              <dt>抖动 / 峰值</dt>
              <dd class="mono" :class="{ bad: telemetry.feedback.jitter_ms >= 50 }">
                {{ telemetry.feedback.jitter_ms.toFixed(1) }} /
                {{ telemetry.feedback.jitter_peak_ms.toFixed(1) }} ms
              </dd>
            </div>
            <div class="tm-item">
              <dt>往返时延</dt>
              <dd class="mono">{{ formatMs(telemetry.feedback.rtt_ms) }}</dd>
            </div>
          </dl>
        </section>
      </div>

      <div v-if="feedbackStale" class="source-notice error">
        平台的 RTCP 报告已中断超过 12 秒(RFC 3550 建议 5 秒一报)。链路可能已经不通,但 SIP 会话还挂着。
      </div>
      <div v-else-if="qualityWarning" class="source-notice error">
        平台报告丢包率 {{ telemetry.feedback.loss_percent.toFixed(2) }}%、抖动
        {{ telemetry.feedback.jitter_ms.toFixed(1) }} ms —— 画面很可能已经出现花屏或卡顿。
      </div>
      <div v-if="telemetry.feedback.peer_bye" class="source-notice error">
        平台发过 RTCP BYE,表示它已主动结束收流。
      </div>
    </template>

    <div v-if="lastError" class="source-notice error">{{ lastError }}</div>
  </div>
</template>

<style scoped>
.telemetry-panel { box-sizing: border-box; display: flex; flex-direction: column; }
.telemetry-idle { padding: 6px 0; line-height: 1.5; }

.tm-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
.tm-state { display: inline-flex; align-items: center; gap: 5px; font-size: 11px; color: var(--text-secondary); }
.tm-state i { width: 6px; height: 6px; border-radius: 50%; background: var(--text-tertiary); }
.tm-state.live { color: var(--success); }
.tm-state.live i {
  background: var(--success);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--success) 16%, transparent);
  animation: tmPulse 1.4s ease-in-out infinite;
}
.tm-state.warn { color: var(--warning); }
.tm-state.warn i {
  background: var(--warning);
  box-shadow: 0 0 0 3px color-mix(in srgb, var(--warning) 16%, transparent);
}
@keyframes tmPulse { 50% { opacity: 0.45; } }

.tm-source {
  flex: 0 1 auto; min-width: 0; font-size: 10px; color: var(--text-tertiary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}

.tm-split {
  display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
  gap: 8px; margin-top: 7px;
}
.tm-side { min-width: 0; }
.tm-side-title {
  display: flex; align-items: baseline; gap: 5px;
  margin: 0 0 4px; font-size: 9.5px; font-weight: 600;
  color: var(--text-tertiary); text-transform: uppercase; letter-spacing: 0.3px;
}
.tm-age { font-weight: 400; letter-spacing: 0; text-transform: none; }

.tm-grid { display: grid; grid-template-columns: minmax(0, 1fr); gap: 5px; margin: 0; }
.tm-item {
  min-width: 0; padding: 5px 7px; border-radius: var(--radius-xs);
  background: var(--bg-card); border: 1px solid var(--border-default);
}
.tm-item dt {
  font-size: 8.5px; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: 0.2px;
}
.tm-item dd {
  margin: 2px 0 0; font-size: 10.5px; color: var(--text-primary);
  overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
}
.tm-item dd.mono {
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-variant-numeric: tabular-nums;
}
.tm-item dd.bad { color: var(--error); }

.tm-nofeedback {
  padding: 6px 7px; border-radius: var(--radius-xs);
  background: var(--bg-surface); border: 1px dashed var(--border-default);
  font-size: 10px; line-height: 1.5; color: var(--text-tertiary);
}

@media (max-width: 1080px) {
  .tm-split { grid-template-columns: 1fr; }
}
</style>
