// useHydrate: 应用启动时把 localStorage 灌到 daemon (D5 三态语义 + N-H5 无条件 apply).
//
// D5: localStorage 三种状态
//   - fresh: 无值 → 用空配置 apply
//   - restored: 有值且合法 → apply
//   - corrupted: JSON 解析或结构校验失败 → 阻断 hydrate, 不 apply 空数组
//
// D6: hydrateReady + hydrateError provide 给 Simulator.vue 门禁使用.

import { ref, type Ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import {
  STORE_CHANNELS,
  STORE_ENCODING,
  type ApplyConfigResp,
  type Channel,
  type EncodingMode,
  type HydrateResult,
} from "../types/channel";

/** 严格校验单个 Channel 结构. */
function validateChannel(v: unknown): v is Channel {
  if (!v || typeof v !== "object") return false;
  const c = v as Record<string, unknown>;
  return (
    typeof c.id === "string" &&
    /^[0-9]{20}$/.test(c.id) &&
    typeof c.name === "string" &&
    (c.type === "video" || c.type === "alarm") &&
    (c.status === "ON" || c.status === "OFF")
  );
}

function validateEncoding(v: unknown): v is EncodingMode {
  return v === "GB18030" || v === "GB2312" || v === "UTF-8";
}

/** 从 localStorage 读并解析. 无值/损坏各返一态. */
export function loadFromLocalStorage(): HydrateResult & { channels?: Channel[]; encoding?: EncodingMode } {
  const rawChannels = localStorage.getItem(STORE_CHANNELS);
  const rawEncoding = localStorage.getItem(STORE_ENCODING);

  // 兼容历史脏数据: 早期 handleSave 崩溃时把字面量 "undefined" 写进过 localStorage
  // (JSON.stringify(undefined) === undefined → localStorage 存成字符串 "undefined")
  // 这些值不合法, 静默清掉当 fresh 处理, 避免 hydrate 死锁.
  const cleanChannels = rawChannels === "undefined" || rawChannels === "null" ? null : rawChannels;
  const cleanEncoding = rawEncoding === "undefined" || rawEncoding === "null" ? null : rawEncoding;
  if (cleanChannels !== rawChannels || cleanEncoding !== rawEncoding) {
    console.warn("[hydrate] 检测到 localStorage 脏数据 'undefined'/'null', 自动清理");
    if (cleanChannels === null) localStorage.removeItem(STORE_CHANNELS);
    if (cleanEncoding === null) localStorage.removeItem(STORE_ENCODING);
  }

  // fresh: 全无
  if (!cleanChannels && !cleanEncoding) {
    return { kind: "fresh" };
  }

  // 有值但解析或校验失败 = corrupted (D5)
  let channels: Channel[] = [];
  let encoding: EncodingMode = "GB18030";
  try {
    if (cleanChannels) {
      const parsed = JSON.parse(cleanChannels);
      if (!Array.isArray(parsed)) {
        return { kind: "corrupted", err: "channels 不是数组" };
      }
      for (let i = 0; i < parsed.length; i++) {
        if (!validateChannel(parsed[i])) {
          return { kind: "corrupted", err: `第 ${i + 1} 个 channel 结构非法` };
        }
      }
      channels = parsed;
    }
    if (cleanEncoding) {
      if (!validateEncoding(cleanEncoding)) {
        return { kind: "corrupted", err: `encoding 值非法: ${cleanEncoding}` };
      }
      encoding = cleanEncoding;
    }
  } catch (e) {
    return { kind: "corrupted", err: "JSON 解析失败: " + String(e) };
  }

  return { kind: "restored", channels, encoding };
}

/** hydrate 状态 (供 provide + 组件 inject 使用). */
export interface HydrateState {
  ready: Ref<boolean>;
  error: Ref<string | null>;
  status: Ref<HydrateResult["kind"] | "pending">;
}

/** 等 daemon 就绪 (poll healthz, 最多 10s). */
async function waitDaemonReady(timeoutMs = 10000): Promise<boolean> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await invoke<{ ready: boolean }>("healthz");
      if (resp?.ready) return true;
    } catch {
      // daemon 还没起来, 继续 poll
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  return false;
}

/** hydrateConfig: 启动时把 localStorage 灌到 daemon. */
export async function hydrateConfig(state: HydrateState): Promise<void> {
  state.status.value = "pending";
  state.ready.value = false;
  state.error.value = null;

  // 检测 Tauri env
  const inTauri = typeof (window as any).__TAURI_INTERNALS__ !== "undefined";
  if (!inTauri) {
    // 浏览器直连 vite 场景 (老板不常用), 跳过 hydrate 让 UI 出现
    state.status.value = "fresh";
    state.ready.value = true;
    return;
  }

  // 0. 先等 daemon 起来 (Tauri spawn daemon 是 async, 前端可能先于 daemon 挂载)
  const daemonReady = await waitDaemonReady();
  if (!daemonReady) {
    state.error.value = "daemon 10s 内未就绪, 请检查日志";
    state.ready.value = false;
    state.status.value = "corrupted";
    return;
  }

  // 1. 读 localStorage
  const loaded = loadFromLocalStorage();

  // D5 corrupted 分支: 阻断 hydrate, 不 apply 空数组
  if (loaded.kind === "corrupted") {
    state.status.value = "corrupted";
    state.error.value = loaded.err;
    state.ready.value = false;
    return;
  }

  // 2. N-H5: 无条件 apply (fresh 用空配置, restored 用恢复值)
  const encoding: EncodingMode = loaded.encoding ?? "GB18030";
  const channels: Channel[] = loaded.channels ?? [];

  try {
    await invoke<ApplyConfigResp>("apply_config", {
      req: { encoding, channels },
    });
    state.status.value = loaded.kind; // fresh | restored
    state.ready.value = true;
  } catch (e) {
    // apply_config 失败不阻断注册 (D3: 通道空也允许注册, AC-11)
    // 只标 warning, 让 UI 显示但仍允许注册
    console.warn("[hydrate] apply_config 失败, 但 daemon 已就绪, 允许注册:", e);
    state.error.value = "配置未同步 (daemon 就绪, 可注册)";
    state.status.value = "fresh";
    state.ready.value = true; // ← 关键: daemon 就绪就允许注册, 通道另外配
  }
}

/** resetLocalStorage: D5 corrupted 时供用户"重置到默认"按钮用. */
export function resetLocalStorage(): void {
  localStorage.removeItem(STORE_CHANNELS);
  localStorage.removeItem(STORE_ENCODING);
}

/** 创建初始 hydrate state (供 App.vue provide). */
export function createHydrateState(): HydrateState {
  return {
    ready: ref(false),
    error: ref<string | null>(null),
    status: ref<HydrateResult["kind"] | "pending">("pending"),
  };
}
