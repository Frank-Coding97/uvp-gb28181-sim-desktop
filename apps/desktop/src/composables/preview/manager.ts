import {
  createLatestFrameScheduler,
  parsePreviewFrame,
  type PreviewFrame,
} from "./scheduler.ts";

export type PreviewUiState = "idle" | "starting" | "playing" | "stopped" | "error";

export interface PreviewBackendStatus {
  generation: number;
  preview_generation: number;
  phase: string;
  reason: string | null;
  h264_input_frames?: number;
  jpeg_output_frames?: number;
  dropped_frames?: number;
  recoveries?: number;
  last_input_at_ms?: number | null;
  last_output_at_ms?: number | null;
}

export interface PreviewConnection {
  token: number;
  status: PreviewBackendStatus;
}

export interface PreviewTransport {
  connect(
    onFrame: (raw: ArrayBuffer) => void,
    onStatus: (status: PreviewBackendStatus) => void,
  ): Promise<PreviewConnection>;
  disconnect(token: number): Promise<void>;
  retry(): Promise<void>;
}

export interface PreviewBitmap {
  width: number;
  height: number;
  close(): void;
}

export interface PreviewCanvas {
  width: number;
  height: number;
  getContext(kind: "2d"): { drawImage(bitmap: PreviewBitmap, x: number, y: number): void } | null;
}

export interface PreviewCallbacks {
  onState?: (state: PreviewUiState) => void;
  onError?: (error: string) => void;
  onFrame?: (stats: { latencyMs: number; fps: number; skipped: number }) => void;
  onStatus?: (status: PreviewBackendStatus) => void;
}

interface ActiveCanvas {
  id: number;
  canvas: PreviewCanvas;
  callbacks: PreviewCallbacks;
}

export interface PreviewManagerOptions {
  transport: PreviewTransport;
  decode(jpeg: Uint8Array): Promise<PreviewBitmap>;
  now?: () => number;
  performanceNow?: () => number;
}

/** 一个应用实例只创建一个；路由页面只替换 active Canvas。 */
export function createPreviewManager(options: PreviewManagerOptions) {
  let active: ActiveCanvas | null = null;
  let nextCanvasId = 1;
  let connection: PreviewConnection | null = null;
  let connecting: Promise<boolean> | null = null;
  let latestFrame: PreviewFrame | null = null;
  let lastGeneration = 0;
  let lastSequence = 0;
  let uiState: PreviewUiState = "idle";
  let latestStatus: PreviewBackendStatus | null = null;
  let fpsStart = 0;
  let fpsFrames = 0;
  let reportedFps = 0;
  const now = options.now ?? Date.now;
  const performanceNow = options.performanceNow ?? (() => performance.now());

  const setState = (next: PreviewUiState) => {
    uiState = next;
    active?.callbacks.onState?.(next);
  };

  const fail = (reason: string) => {
    setState("error");
    active?.callbacks.onError?.(reason);
  };

  const handleStatus = (status: PreviewBackendStatus) => {
    if (latestStatus && (
      status.generation < latestStatus.generation
      || (status.generation === latestStatus.generation
        && status.preview_generation < latestStatus.preview_generation)
    )) return;
    latestStatus = status;
    active?.callbacks.onStatus?.(status);
    if (status.phase === "unavailable") {
      fail(status.reason || "预览 worker 当前不可用");
    } else if (["stopping", "stopped", "idle"].includes(status.phase)) {
      setState(status.phase === "idle" ? "idle" : "stopped");
    } else if (status.phase !== "playing" || uiState !== "playing") {
      setState("starting");
    }
  };

  const renderFrame = async (frame: PreviewFrame) => {
    const target = active;
    if (!target) return;
    let bitmap: PreviewBitmap;
    try {
      bitmap = await options.decode(frame.jpeg);
    } catch (error) {
      target.callbacks.onError?.(`预览帧解码失败：${String(error)}`);
      throw error;
    }
    try {
      if (!active || active.id !== target.id || active.canvas !== target.canvas) return;
      if (target.canvas.width !== bitmap.width || target.canvas.height !== bitmap.height) {
        target.canvas.width = bitmap.width;
        target.canvas.height = bitmap.height;
      }
      target.canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
    } finally {
      bitmap.close();
    }
    if (!active || active.id !== target.id) return;
    setState("playing");
    const latencyMs = Math.max(0, now() - frame.capturedAtMs);
    const sampledAt = performanceNow();
    if (!fpsStart) fpsStart = sampledAt;
    fpsFrames += 1;
    if (sampledAt - fpsStart >= 1_000) {
      reportedFps = Math.round((fpsFrames * 1_000) / (sampledAt - fpsStart));
      fpsFrames = 0;
      fpsStart = sampledAt;
    }
    target.callbacks.onFrame?.({ latencyMs, fps: reportedFps, skipped: scheduler.skipped });
  };

  const scheduler = createLatestFrameScheduler(renderFrame);

  const handleRaw = (raw: ArrayBuffer) => {
    const frame = parsePreviewFrame(raw);
    if (!frame) {
      fail("预览帧协议不兼容或 JPEG 数据损坏");
      return;
    }
    if (
      frame.generation < lastGeneration
      || (frame.generation === lastGeneration && frame.sequence <= lastSequence)
    ) return;
    lastGeneration = frame.generation;
    lastSequence = frame.sequence;
    latestFrame = frame;
    if (active) scheduler.submit(frame);
  };

  const ensureConnected = async (): Promise<boolean> => {
    if (connection) return true;
    if (connecting) return connecting;
    setState("starting");
    connecting = options.transport
      .connect(handleRaw, handleStatus)
      .then((opened) => {
        connection = opened;
        handleStatus(opened.status);
        return true;
      })
      .catch((error) => {
        fail(`预览通道启动失败：${String(error)}`);
        return false;
      })
      .finally(() => {
        connecting = null;
      });
    return connecting;
  };

  return {
    async attach(canvas: PreviewCanvas, callbacks: PreviewCallbacks = {}): Promise<number> {
      const id = nextCanvasId++;
      active = { id, canvas, callbacks };
      callbacks.onState?.(uiState);
      if (latestStatus) callbacks.onStatus?.(latestStatus);
      const connected = await ensureConnected();
      if (connected && active?.id === id && latestFrame) scheduler.submit(latestFrame);
      return id;
    },

    detach(id: number): boolean {
      if (!active || active.id !== id) return false;
      active.callbacks.onState?.("stopped");
      active = null;
      scheduler.reset();
      fpsStart = 0;
      fpsFrames = 0;
      reportedFps = 0;
      return true;
    },

    async retry(): Promise<void> {
      await options.transport.retry();
      setState("starting");
    },

    async shutdown(): Promise<void> {
      active = null;
      scheduler.reset();
      if (connecting) await connecting;
      const token = connection?.token;
      connection = null;
      if (token !== undefined) await options.transport.disconnect(token);
      uiState = "stopped";
    },

    get state() {
      return uiState;
    },
  };
}
