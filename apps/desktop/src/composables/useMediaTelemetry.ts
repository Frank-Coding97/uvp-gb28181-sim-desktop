// 推流会话的媒体遥测(发送侧统计 + 平台回的 RTCP 接收报告),模块级单例。
//
// 为什么要单独一张卡:设备侧一直只能看到"我发了多少包",看不到"平台收到了多少"。
// 排"平台画面卡但设备日志说一直在发"这类问题时,平台按 RFC 3550 回的 RTCP RR
// (丢包率 / 累计丢包 / 抖动 / RTT)才是唯一的客观证据。
//
// 轮询而不是事件流:发送包数每帧都在变(25fps 下每秒几十次),RR 则 5 秒才来一条。
// 推事件等于给 webview 灌洪水,500ms 轮询已经比 RR 本身的周期密得多。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../lib/ipcError";

/** 平台回报的收流质量(对应 Rust 侧 media_rtp::RtcpFeedbackSnapshot)。 */
export interface RtcpFeedback {
  /** 累计收到的接收报告块数;0 = 平台一条 RR 都没回。 */
  reports: number;
  /** 最近一次报告的丢包率,百分比。 */
  loss_percent: number;
  /** 累计丢包数。可为负:平台收到的重复包多于丢包时。 */
  cumulative_lost: number;
  /** 最近一次报告的到达间隔抖动,毫秒。 */
  jitter_ms: number;
  /** 峰值抖动,毫秒。 */
  jitter_peak_ms: number;
  /** 往返时延估算,毫秒;平台还没回过 SR 时为 null。 */
  rtt_ms: number | null;
  /** 报告方(平台)SSRC。 */
  reporter_ssrc: number;
  /** 距最近一次报告的毫秒数;从未收到为 null。 */
  age_ms: number | null;
  /** 平台是否发过 RTCP BYE。 */
  peer_bye: boolean;
  /** 无法解析的 RTCP 包数。 */
  unparsed: number;
}

/** 推流会话遥测快照(对应 Rust 侧 MediaTelemetrySnapshot)。 */
export interface MediaTelemetry {
  /** 是回放/下载会话(true)还是实时(false)。 */
  is_history: boolean;
  /** 附录 K c) 的媒体源标识;平台不带 Subject 时为 null。 */
  media_source: string | null;
  /** 本端 RTCP 端口;TCP 模式或未抢到 RTP+1 时为 null,此时收不到 RR。 */
  local_rtcp_port: number | null;
  sent_packets: number;
  sent_bytes: number;
  sent_rtcp: number;
  send_errors: number;
  feedback: RtcpFeedback;
}

/** 轮询周期。与语音广播卡一致,便于两张卡的读数看起来同步。 */
const POLL_MS = 500;
/**
 * RR 超过这么久没来就标"反馈中断"。
 *
 * RFC 3550 建议的最小报告间隔是 5 秒,给两倍余量再加一拍网络抖动。
 */
const FEEDBACK_STALE_MS = 12_000;
/** 丢包率高于这个值就标红。1% 已经足够让 H.264 画面出现可见花屏。 */
const LOSS_WARN_PERCENT = 1;
/** 抖动高于这个值就标红(毫秒)。 */
const JITTER_WARN_MS = 50;

const telemetry = ref<MediaTelemetry | null>(null);
const lastError = ref("");

/** 是否有活跃推流会话。 */
const streaming = computed(() => telemetry.value !== null);

/** 平台是否回过 RTCP 接收报告。 */
const hasFeedback = computed(() => (telemetry.value?.feedback.reports ?? 0) > 0);

/**
 * 反馈通道是否不可用(而不是"质量差")。
 *
 * 两种情形都算:本端没能绑到 RTP+1 端口,或平台压根没开 RTCP。
 * 这不是故障 —— 很多平台确实不发 RR,但界面必须说清"没数据"和"数据是 0"的区别。
 */
const feedbackUnavailable = computed(
  () => streaming.value && !hasFeedback.value,
);

/** 反馈曾经来过但断了(比"从来没有"更值得警惕)。 */
const feedbackStale = computed(() => {
  const age = telemetry.value?.feedback.age_ms;
  return age !== null && age !== undefined && age > FEEDBACK_STALE_MS;
});

/** 平台报告的质量是否已经到了要提醒的程度。 */
const qualityWarning = computed(() => {
  const fb = telemetry.value?.feedback;
  if (!fb || fb.reports === 0) return false;
  return fb.loss_percent >= LOSS_WARN_PERCENT || fb.jitter_ms >= JITTER_WARN_MS;
});

/** 平均码率(kbps),按本次会话累计字节与发送包数粗估的瞬时值无关,故只在有包时给。 */
export function formatBitrate(bytes: number, packets: number): string {
  if (packets === 0) return "—";
  // 没有会话时长就不硬编一个"平均码率" —— 会误导。这里给单包均值,
  // 它能直接反映"分片是否过小"(过小意味着 MTU 设置有问题)。
  return `${Math.round(bytes / packets)} B/包`;
}

/** 字节数人读化。 */
export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** 毫秒人读化。 */
export function formatMs(ms: number | null): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return `${ms.toFixed(ms < 10 ? 1 : 0)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

let timer: ReturnType<typeof setInterval> | null = null;

async function poll() {
  try {
    telemetry.value = await invoke<MediaTelemetry | null>("get_media_telemetry");
    lastError.value = "";
  } catch (e) {
    // 设备未启动/无会话后端已按 null 返回,落到这里的才是真异常。
    lastError.value = formatError(e, "读取推流质量失败");
    telemetry.value = null;
  }
}

function startMediaTelemetry() {
  if (timer !== null) return;
  poll();
  // 用全局 setInterval 而非 window.setInterval:无 DOM 的测试环境也能直接跑。
  timer = setInterval(poll, POLL_MS);
}

function stopMediaTelemetry() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

export function useMediaTelemetry() {
  return {
    telemetry,
    lastError,
    streaming,
    hasFeedback,
    feedbackUnavailable,
    feedbackStale,
    qualityWarning,
    refreshMediaTelemetry: poll,
    startMediaTelemetry,
    stopMediaTelemetry,
  };
}
