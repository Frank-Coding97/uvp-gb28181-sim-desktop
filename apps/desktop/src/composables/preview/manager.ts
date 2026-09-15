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

export interface PreviewVisibilitySubscription {
  visible: boolean;
  unsubscribe(): void | Promise<void>;
}

interface ActiveCanvas {
  id: number;
  canvas: PreviewCanvas;
  callbacks: PreviewCallbacks;
}

interface ActiveConnection extends PreviewConnection {
  epoch: number;
}

interface PendingConnection {
  epoch: number;
  cancelled: boolean;
  promise: Promise<boolean>;
}

export interface PreviewManagerOptions {
  transport: PreviewTransport;
  decode(jpeg: Uint8Array): Promise<PreviewBitmap>;
  now?: () => number;
  performanceNow?: () => number;
  watchVisibility?: (
    onVisible: (visible: boolean) => void,
  ) => PreviewVisibilitySubscription | Promise<PreviewVisibilitySubscription>;
}

/** 一个应用实例只创建一个；路由页面只替换 active Canvas。 */
export function createPreviewManager(options: PreviewManagerOptions) {
  let active: ActiveCanvas | null = null;
  let nextCanvasId = 1;
  let connection: ActiveConnection | null = null;
  let connecting: PendingConnection | null = null;
  let connectTail: Promise<void> = Promise.resolve();
  let lifecycleEpoch = 0;
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
  const documentTarget = typeof document === "undefined" ? null : document;
  let visibilityListenerInstalled = false;
  let nativeVisible = true;
  let nativeVisibilityReady = !options.watchVisibility;
  let nativeVisibilityRevision = 0;
  let visibilityInit: Promise<void> | null = null;
  let nativeUnsubscribe: (() => void | Promise<void>) | null = null;

  const isDocumentVisible = () => documentTarget?.visibilityState !== "hidden";
  const isVisible = () => isDocumentVisible() && nativeVisible;

  const handleVisibilityChanged = () => {
    if (isVisible()) void resume();
    else void pause();
  };

  const handleNativeVisibility = (visible: boolean) => {
    const changed = nativeVisible !== visible;
    nativeVisibilityRevision += 1;
    nativeVisible = visible;
    if (nativeVisibilityReady && changed) handleVisibilityChanged();
  };

  const ensureVisibilityWatch = async (): Promise<void> => {
    if (!options.watchVisibility || nativeVisibilityReady) return;
    if (visibilityInit) return visibilityInit;
    const revision = nativeVisibilityRevision;
    visibilityInit = (async () => {
      let subscription: PreviewVisibilitySubscription;
      try {
        subscription = await options.watchVisibility!(handleNativeVisibility);
      } catch {
        // 原生可见性查询失败时沿用文档可见性，保持既有预览行为。
        nativeVisibilityReady = true;
        if (active && !isVisible()) await pause();
        return;
      }
      if (nativeVisibilityRevision === revision) nativeVisible = subscription.visible;
      nativeUnsubscribe = subscription.unsubscribe;
      nativeVisibilityReady = true;
      if (active && !isVisible()) await pause();
    })();
    return visibilityInit;
  };

  const resetPlayback = () => {
    latestFrame = null;
    lastGeneration = 0;
    lastSequence = 0;
    latestStatus = null;
    scheduler.reset();
    fpsStart = 0;
    fpsFrames = 0;
    reportedFps = 0;
  };

  const disconnectToken = (token: number): Promise<void> => {
    return options.transport.disconnect(token).catch(() => undefined).then(() => undefined);
  };

  const appendToConnectTail = (tasks: Promise<void>[]) => {
    if (tasks.length === 0) return;
    const previous = connectTail;
    connectTail = Promise.all([previous, ...tasks]).then(() => undefined, () => undefined);
  };

  const invalidateConnection = (): Promise<void>[] => {
    const pending = connecting;
    if (pending) pending.cancelled = true;
    connecting = null;
    lifecycleEpoch += 1;
    const opened = connection;
    connection = null;
    resetPlayback();
    const tasks = opened ? [disconnectToken(opened.token)] : [];
    appendToConnectTail(tasks);
    return tasks;
  };

  const isCurrentEpoch = (epoch: number) => (
    epoch === lifecycleEpoch && active !== null && isVisible()
  );

  const setState = (next: PreviewUiState) => {
    uiState = next;
    active?.callbacks.onState?.(next);
  };

  const fail = (reason: string) => {
    setState("error");
    active?.callbacks.onError?.(reason);
  };

  const handleStatus = (status: PreviewBackendStatus, epoch: number) => {
    if (!isCurrentEpoch(epoch)) return;
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
    const epoch = lifecycleEpoch;
    if (!target || !isVisible()) return;
    let bitmap: PreviewBitmap;
    try {
      bitmap = await options.decode(frame.jpeg);
    } catch (error) {
      if (isCurrentEpoch(epoch) && active?.id === target.id) {
        target.callbacks.onError?.(`预览帧解码失败：${String(error)}`);
      }
      if (!isCurrentEpoch(epoch) || active?.id !== target.id) return;
      throw error;
    }
    try {
      if (!isCurrentEpoch(epoch) || active?.id !== target.id || active.canvas !== target.canvas) return;
      if (target.canvas.width !== bitmap.width || target.canvas.height !== bitmap.height) {
        target.canvas.width = bitmap.width;
        target.canvas.height = bitmap.height;
      }
      target.canvas.getContext("2d")?.drawImage(bitmap, 0, 0);
    } finally {
      bitmap.close();
    }
    if (!isCurrentEpoch(epoch) || active?.id !== target.id) return;
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

  const handleRaw = (raw: ArrayBuffer, epoch: number) => {
    if (!isCurrentEpoch(epoch)) return;
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
    if (options.watchVisibility && !nativeVisibilityReady) await ensureVisibilityWatch();
    if (!active || !isVisible()) return false;
    if (connection && connection.epoch === lifecycleEpoch) return true;
    if (connecting && !connecting.cancelled && connecting.epoch === lifecycleEpoch) {
      return connecting.promise;
    }
    setState("starting");
    const epoch = ++lifecycleEpoch;
    const previousTail = connectTail;
    const pending: PendingConnection = {
      epoch,
      cancelled: false,
      promise: Promise.resolve(false),
    };
    pending.promise = (async () => {
      await previousTail;
      if (
        pending.cancelled
        || connecting !== pending
        || lifecycleEpoch !== epoch
        || !active
        || !isVisible()
      ) return false;
      try {
        const opened = await options.transport.connect(
          (raw) => {
            if (isCurrentEpoch(epoch)) handleRaw(raw, epoch);
          },
          (status) => {
            if (isCurrentEpoch(epoch)) handleStatus(status, epoch);
          },
        );
        if (
          pending.cancelled
          || connecting !== pending
          || lifecycleEpoch !== epoch
          || !active
          || !isVisible()
        ) {
          await disconnectToken(opened.token);
          return false;
        }
        connection = { ...opened, epoch };
        handleStatus(opened.status, epoch);
        return true;
      } catch (error) {
        if (
          !pending.cancelled
          && connecting === pending
          && lifecycleEpoch === epoch
          && active
          && isVisible()
        ) {
          fail(`预览通道启动失败：${String(error)}`);
        }
        return false;
      } finally {
        if (connecting === pending) connecting = null;
      }
    })();
    connecting = pending;
    connectTail = pending.promise.then(() => undefined, () => undefined);
    return pending.promise;
  };

  const pause = async () => {
    if (isVisible()) return;
    const tasks = invalidateConnection();
    if (active) setState("stopped");
    await Promise.all(tasks);
  };

  const resume = async () => {
    if (!active || !isVisible()) return;
    setState("starting");
    const connected = await ensureConnected();
    if (connected && active && isVisible() && latestFrame) {
      scheduler.submit(latestFrame);
    }
  };

  const visibilityListener = () => {
    handleVisibilityChanged();
  };

  const installVisibilityListener = () => {
    if (documentTarget && !visibilityListenerInstalled) {
      documentTarget.addEventListener("visibilitychange", visibilityListener);
      visibilityListenerInstalled = true;
    }
  };

  const shutdownVisibilityWatch = async () => {
    if (visibilityInit) await visibilityInit;
    const unsubscribe = nativeUnsubscribe;
    nativeUnsubscribe = null;
    if (unsubscribe) await Promise.resolve(unsubscribe()).catch(() => undefined);
    nativeVisibilityReady = !options.watchVisibility;
    nativeVisible = true;
    nativeVisibilityRevision += 1;
    visibilityInit = null;
  };

  return {
    async attach(
      canvas: PreviewCanvas,
      callbacks: PreviewCallbacks = {},
      onBound?: (id: number) => void,
    ): Promise<number> {
      const id = nextCanvasId++;
      installVisibilityListener();
      active = { id, canvas, callbacks };
      onBound?.(id);
      callbacks.onState?.(uiState);
      if (latestStatus && isVisible()) callbacks.onStatus?.(latestStatus);
      if (options.watchVisibility && !nativeVisibilityReady) await ensureVisibilityWatch();
      if (active?.id !== id) return id;
      if (!isVisible()) {
        setState("stopped");
        return id;
      }
      const connected = await ensureConnected();
      if (connected && active?.id === id && isVisible() && latestFrame) {
        scheduler.submit(latestFrame);
      }
      return id;
    },

    async detach(id: number): Promise<boolean> {
      if (!active || active.id !== id) return false;
      active.callbacks.onState?.("stopped");
      active = null;
      const pending = connecting?.promise;
      const tasks = invalidateConnection();
      const tail = connectTail;
      uiState = "stopped";
      if (pending) await pending;
      await tail;
      await Promise.all(tasks);
      return true;
    },

    async retry(): Promise<void> {
      await options.transport.retry();
      setState("starting");
    },

    async shutdown(): Promise<void> {
      active = null;
      const pending = connecting?.promise;
      const tasks = invalidateConnection();
      const tail = connectTail;
      documentTarget?.removeEventListener("visibilitychange", visibilityListener);
      visibilityListenerInstalled = false;
      if (pending) await pending;
      await tail;
      await Promise.all(tasks);
      await shutdownVisibilityWatch();
      uiState = "stopped";
    },

    get state() {
      return uiState;
    },
  };
}
