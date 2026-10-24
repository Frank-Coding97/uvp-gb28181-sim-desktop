// 语音广播状态(GB/T 28181-2022 §9.12.1,模块级单例)。
//
// 为什么轮询而不是订阅事件:收流统计每 20 ms 变一次,推成事件等于给 webview 灌洪水。
// 会话的建立/结束本身会走 `platform_command` 事件进命令时间线,这里只补统计与电平,
// 500 ms 一次足够让音量条看起来是活的。
import { computed, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { formatError } from "../lib/ipcError";

// 类型来自 Rust 侧 ts-rs 生成(见 src/types/generated),别名保持既有导入路径稳定。
import type { BroadcastSnapshot } from "../types/generated/BroadcastSnapshot";
import type { RecvStatsSnapshot } from "../types/generated/RecvStatsSnapshot";

type RecvStats = RecvStatsSnapshot;
type BroadcastSession = BroadcastSnapshot;
export type { BroadcastSession, RecvStats };

/** 轮询周期:音量条要看起来连续,又不至于把 IPC 打满。 */
const POLL_MS = 500;
/** 多久没收到包就认为链路可疑(附录 K a) 的媒体流丢失监测)。 */
const STALE_MS = 3000;

const session = ref<BroadcastSession | null>(null);
const lastError = ref("");

/** 负载类型 → 人读名称。 */
export function payloadTypeLabel(pt: number | null): string {
  if (pt === null) return "—";
  if (pt === 8) return "G.711A / PCMA";
  if (pt === 0) return "G.711U / PCMU";
  if (pt === 96) return "PS (96)";
  return `PT ${pt}`;
}

/** 会话是否在收到实际音频(不只是信令建起来了)。 */
const hasMedia = computed(() => (session.value?.recv.packets ?? 0) > 0);

/** 链路是否可疑:建流了但迟迟没有包,或包断了超过 3 秒。 */
const stalled = computed(() => {
  const idle = session.value?.recv.idle_ms;
  return idle !== null && idle !== undefined && idle > STALE_MS;
});

/** 音量条宽度(百分比字符串)。 */
const levelPercent = computed(() => `${Math.min(100, session.value?.recv.level_pct ?? 0)}%`);

let timer: ReturnType<typeof setInterval> | null = null;

async function poll() {
  try {
    session.value = await invoke<BroadcastSession | null>("get_broadcast_status");
    lastError.value = "";
  } catch (e) {
    // 设备未启动等情形后端已按"无会话"返回,这里剩下的才是真异常。
    lastError.value = formatError(e, "读取语音广播状态失败");
    session.value = null;
  }
}

function startBroadcastPolling() {
  if (timer !== null) return;
  poll();
  // 用全局 setInterval 而不是 window.setInterval:这样这个 composable 在
  // 无 DOM 的测试环境里也能直接跑,不必为了测试去搭一个假 window。
  timer = setInterval(poll, POLL_MS);
}

function stopBroadcastPolling() {
  if (timer !== null) {
    clearInterval(timer);
    timer = null;
  }
}

/** 设备侧主动挂断(会按附录 K a) 发会话内 BYE)。 */
async function hangup(): Promise<{ ok: boolean; message: string }> {
  try {
    const message = await invoke<string>("hangup_broadcast");
    await poll();
    return { ok: true, message };
  } catch (e) {
    return { ok: false, message: formatError(e, "挂断语音广播失败") };
  }
}

export function useVoiceBroadcast() {
  return {
    session,
    lastError,
    hasMedia,
    stalled,
    levelPercent,
    hangup,
    startBroadcastPolling,
    stopBroadcastPolling,
  };
}
