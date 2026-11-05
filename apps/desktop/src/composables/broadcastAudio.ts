// 语音广播 / 对讲下行(平台 → 设备)的前端播放与状态。
//
// Rust 侧把 G.711 解码后的 8kHz 单声道 PCM 通过 `broadcast_audio` 事件发过来,
// 这里用 Web Audio 建队列播放:每包按采样点排到 playhead 上,起播前留 60ms 抖动缓冲。
// 不引入原生音频依赖,桌面端与 WebView 生命周期一致。
import { onUnmounted, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface BroadcastState {
  /** 是否处于广播/对讲会话中。 */
  active: boolean;
  /** 被广播的通道 ID(开始事件携带)。 */
  channel_id?: string;
  /** 平台侧音源 ID。 */
  source_id?: string;
  /** 音频流传输方式:UDP / TCP_ACTIVE / TCP_PASSIVE。 */
  transport?: string;
  /** 本端媒体端口。 */
  local_port?: number;
  /** 结束原因(结束事件携带)。 */
  reason?: string;
  /** 结束时累计包数 / 字节数。 */
  packets?: number;
  bytes?: number;
}

interface BroadcastAudioPayload {
  sample_rate: number;
  pcm: number[];
}

interface BroadcastProgressPayload {
  packets: number;
  bytes: number;
}

/** 起播前的抖动缓冲(秒)。 */
const JITTER_LEAD = 0.06;
/** 落后超过该值视为断流,重新对齐而不是追赶。 */
const RESYNC_BEHIND = 0.25;

export function useBroadcastAudio() {
  const state = ref<BroadcastState>({ active: false });
  const packets = ref(0);
  const bytes = ref(0);
  const muted = ref(false);
  const audioAvailable = typeof AudioContext !== "undefined";

  let context: AudioContext | null = null;
  let playhead = 0;
  let started = false;
  let unlisteners: UnlistenFn[] = [];

  function ensureContext(): AudioContext | null {
    if (!audioAvailable) return null;
    if (!context) {
      try {
        context = new AudioContext();
      } catch {
        return null;
      }
    }
    return context;
  }

  /** 必须在用户手势里调用:解锁 WebView 的自动播放限制。 */
  async function unlock() {
    const ctx = ensureContext();
    if (ctx && ctx.state === "suspended") {
      try {
        await ctx.resume();
      } catch {
        // 解锁失败不影响后续;下次用户点击还会再试。
      }
    }
  }

  function schedule(pcm: number[], sampleRate: number) {
    if (muted.value || pcm.length === 0) return;
    const ctx = ensureContext();
    if (!ctx || ctx.state !== "running") return;
    const rate = sampleRate > 0 ? sampleRate : 8000;
    const buffer = ctx.createBuffer(1, pcm.length, rate);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < pcm.length; i += 1) {
      const sample = pcm[i] / 32768;
      channel[i] = sample > 1 ? 1 : sample < -1 ? -1 : sample;
    }
    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(ctx.destination);

    const now = ctx.currentTime;
    if (playhead < now - RESYNC_BEHIND || playhead < now) {
      // 首包、断流或调度落后:从现在起重新排,避免整段音频一次性挤出去。
      playhead = now + JITTER_LEAD;
    }
    source.start(playhead);
    playhead += buffer.duration;
  }

  async function start() {
    if (started || !audioAvailable) return;
    started = true;
    unlisteners.push(
      await listen<BroadcastAudioPayload>("broadcast_audio", (event) => {
        schedule(event.payload.pcm ?? [], event.payload.sample_rate ?? 8000);
      }),
    );
    unlisteners.push(
      await listen<BroadcastState>("broadcast_state", (event) => {
        state.value = event.payload;
        if (event.payload.active) {
          packets.value = 0;
          bytes.value = 0;
          return;
        }
        // 会话结束:丢掉排程,下一次从新的 playhead 开始。
        playhead = 0;
        packets.value = event.payload.packets ?? packets.value;
        bytes.value = event.payload.bytes ?? bytes.value;
      }),
    );
    unlisteners.push(
      await listen<BroadcastProgressPayload>("broadcast_progress", (event) => {
        packets.value = event.payload.packets;
        bytes.value = event.payload.bytes;
      }),
    );
  }

  function stopListening() {
    for (const unlisten of unlisteners) unlisten();
    unlisteners = [];
    started = false;
  }

  onUnmounted(stopListening);

  return {
    state,
    packets,
    bytes,
    muted,
    audioAvailable,
    unlock,
    start,
    stopListening,
  };
}
