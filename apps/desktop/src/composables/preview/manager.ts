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
  /** 前端不支持当前编码的解码时,请后端改送 JPEG(或切回)。 */
  setFallback(fallback: boolean): Promise<void>;
}

export interface PreviewBitmap {
  width: number;
  height: number;
  close(): void;
}

/** WebCodecs `VideoFrame` 的最小子集:能画到 canvas、能释放。 */
export interface PreviewVideoFrame {
  displayWidth: number;
  displayHeight: number;
  /** 解码前传入的 chunk timestamp(微秒),这里等于 `sequence * 1000`。 */
  timestamp: number;
  close(): void;
}

export interface PreviewCanvas {
  width: number;
  height: number;
  getContext(kind: "2d"): {
    drawImage(image: PreviewBitmap | PreviewVideoFrame, x: number, y: number): void;
  } | null;
}

/** 当前画面的来源,用于界面叠加信息。 */
export type PreviewDecodeMode = "webcodecs" | "jpeg";

export interface PreviewFrameStats {
  latencyMs: number;
  fps: number;
  skipped: number;
  mode: PreviewDecodeMode;
  /** WebCodecs 模式下的 codec 字符串。 */
  codec: string;
}

export interface PreviewCallbacks {
  onState?: (state: PreviewUiState) => void;
  onError?: (error: string) => void;
  onFrame?: (stats: PreviewFrameStats) => void;
  onStatus?: (status: PreviewBackendStatus) => void;
}

/** WebCodecs `VideoDecoder` 的最小子集,便于在 node 测试里替换。 */
export interface PreviewVideoDecoder {
  readonly state: "unconfigured" | "configured" | "closed";
  readonly decodeQueueSize: number;
  configure(config: { codec: string; hardwareAcceleration?: string; optimizeForLatency?: boolean }): void;
  decode(chunk: { type: "key" | "delta"; timestamp: number; data: Uint8Array }): void;
  close(): void;
}

export interface PreviewVideoDecoderFactory {
  isConfigSupported(codec: string): Promise<boolean>;
  create(
    output: (frame: PreviewVideoFrame) => void,
    error: (error: unknown) => void,
  ): PreviewVideoDecoder;
}

/** 解码队列超过这么多帧就丢 delta、等下一个关键帧,保证低延迟。 */
const MAX_DECODE_QUEUE = 3;

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
  /** 没有 WebCodecs 的环境传 undefined:编码帧到达时直接请求 JPEG 回退。 */
  videoDecoder?: PreviewVideoDecoderFactory;
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
    resetVideo();
    fallbackRequested = false;
  };

  // ---- WebCodecs 编码帧路径 ----

  let decoder: PreviewVideoDecoder | null = null;
  /** 每次重建解码器递增;旧解码器迟到的输出直接丢弃。 */
  let decoderEpoch = 0;
  let decoderCodec = "";
  /** 正在检测的 codec,检测完成前到达的帧丢弃。 */
  let probingCodec = "";
  let videoGeneration = 0;
  let videoSequence = 0;
  let needKeyFrame = true;
  let fallbackRequested = false;
  let skippedVideo = 0;
  const pendingCapturedAt = new Map<number, number>();

  const resetVideo = () => {
    decoderEpoch += 1;
    if (decoder && decoder.state !== "closed") {
      try { decoder.close(); } catch { /* 已关闭 */ }
    }
    decoder = null;
    decoderCodec = "";
    probingCodec = "";
    videoGeneration = 0;
    videoSequence = 0;
    needKeyFrame = true;
    skippedVideo = 0;
    pendingCapturedAt.clear();
  };

  const requestFallback = (reason: string) => {
    if (fallbackRequested) return;
    fallbackRequested = true;
    console.warn("[preview] 切换到 JPEG 回退:", reason);
    void options.transport.setFallback(true).catch((error) => {
      fail(`预览回退失败：${String(error)}`);
    });
  };

  const reportFrame = (target: ActiveCanvas, capturedAtMs: number, mode: PreviewDecodeMode, skipped: number) => {
    setState("playing");
    const latencyMs = Math.max(0, now() - capturedAtMs);
    const sampledAt = performanceNow();
    if (!fpsStart) fpsStart = sampledAt;
    fpsFrames += 1;
    if (sampledAt - fpsStart >= 1_000) {
      reportedFps = Math.round((fpsFrames * 1_000) / (sampledAt - fpsStart));
      fpsFrames = 0;
      fpsStart = sampledAt;
    }
    target.callbacks.onFrame?.({
      latencyMs,
      fps: reportedFps,
      skipped,
      mode,
      codec: mode === "webcodecs" ? decoderCodec : "",
    });
  };

  const createDecoder = (codec: string) => {
    const factory = options.videoDecoder!;
    const epoch = ++decoderEpoch;
    const instance = factory.create(
      (frame) => {
        try {
          const target = active;
          if (epoch !== decoderEpoch || !target || !isVisible()) return;
          const canvas = target.canvas;
          if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
            canvas.width = frame.displayWidth;
            canvas.height = frame.displayHeight;
          }
          canvas.getContext("2d")?.drawImage(frame, 0, 0);
          const sequence = Math.round(frame.timestamp / 1_000);
          const capturedAt = pendingCapturedAt.get(sequence) ?? now();
          pendingCapturedAt.delete(sequence);
          reportFrame(target, capturedAt, "webcodecs", skippedVideo);
        } finally {
          frame.close();
        }
      },
      (error) => {
        if (epoch !== decoderEpoch) return;
        // 解码器出错(数据损坏或硬件解码器被回收):重建,从下一个关键帧继续。
        console.warn("[preview] VideoDecoder error, recreating", error);
        decoderEpoch += 1;
        decoder = null;
        needKeyFrame = true;
      },
    );
    instance.configure({ codec, hardwareAcceleration: "prefer-hardware", optimizeForLatency: true });
    decoder = instance;
    decoderCodec = codec;
  };

  const handleEncoded = (frame: PreviewFrame) => {
    if (fallbackRequested) return;
    if (frame.generation !== videoGeneration) {
      // 新的后端订阅(设备重启、预览重建):旧解码器和参考帧作废。
      resetVideo();
      videoGeneration = frame.generation;
    } else if (frame.sequence <= videoSequence) {
      return;
    }
    videoSequence = frame.sequence;
    if (!options.videoDecoder) {
      requestFallback("当前环境不支持 WebCodecs");
      return;
    }
    if (frame.keyFrame && frame.codec && frame.codec !== decoderCodec && frame.codec !== probingCodec) {
      // 首个关键帧或编码配置变化:先确认能解,再建解码器。检测期间到达的帧丢弃。
      const codec = frame.codec;
      const generation = frame.generation;
      probingCodec = codec;
      // 检测通过后立即解这一帧,不用再等一个 GOP 才出画面。
      const heldKeyFrame = frame;
      options.videoDecoder.isConfigSupported(codec).then((supported) => {
        if (probingCodec !== codec || videoGeneration !== generation) return;
        probingCodec = "";
        if (!supported) {
          requestFallback(`本机 WebView 不支持解码 ${codec}`);
          return;
        }
        resetVideoDecoderOnly();
        createDecoder(codec);
        needKeyFrame = true;
        decodeFrame(heldKeyFrame);
      }, (error) => {
        if (probingCodec !== codec) return;
        probingCodec = "";
        requestFallback(`解码能力检测失败：${String(error)}`);
      });
      needKeyFrame = true;
      return;
    }
    if (probingCodec) return;
    if (!decoder && decoderCodec && frame.keyFrame) {
      // 解码器出错后重建。
      createDecoder(decoderCodec);
    }
    decodeFrame(frame);
  };

  const decodeFrame = (frame: PreviewFrame) => {
    if (!decoder || decoder.state !== "configured") return;
    if (!frame.keyFrame && (needKeyFrame || decoder.decodeQueueSize > MAX_DECODE_QUEUE)) {
      // 积压时丢 delta 等下一个关键帧,不让延迟累积。
      needKeyFrame = true;
      skippedVideo += 1;
      return;
    }
    needKeyFrame = false;
    pendingCapturedAt.set(frame.sequence, frame.capturedAtMs);
    if (pendingCapturedAt.size > 32) {
      const oldest = pendingCapturedAt.keys().next().value;
      if (oldest !== undefined) pendingCapturedAt.delete(oldest);
    }
    try {
      decoder.decode({
        type: frame.keyFrame ? "key" : "delta",
        // 微秒时间戳,只要求单调。
        timestamp: frame.sequence * 1_000,
        data: frame.payload,
      });
    } catch (error) {
      console.warn("[preview] decode threw, waiting for next key frame", error);
      needKeyFrame = true;
    }
  };

  /** 只换解码器实例,保留当前代际/序号状态。 */
  const resetVideoDecoderOnly = () => {
    decoderEpoch += 1;
    if (decoder && decoder.state !== "closed") {
      try { decoder.close(); } catch { /* 已关闭 */ }
    }
    decoder = null;
    decoderCodec = "";
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
    reportFrame(target, frame.capturedAtMs, "jpeg", scheduler.skipped);
  };

  const scheduler = createLatestFrameScheduler(renderFrame);

  const handleRaw = (raw: ArrayBuffer, epoch: number) => {
    if (!isCurrentEpoch(epoch)) return;
    const frame = parsePreviewFrame(raw);
    if (!frame) {
      fail("预览帧协议不兼容或数据损坏");
      return;
    }
    if (frame.kind !== "jpeg") {
      // 编码帧必须按序逐帧解码,不能走 latest-wins 调度。
      if (active) handleEncoded(frame);
      return;
    }
    // JPEG 回退已生效:释放硬件解码器。
    if (decoder) resetVideoDecoderOnly();
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
