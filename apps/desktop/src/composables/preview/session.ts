import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  createPreviewManager,
  type PreviewBackendStatus,
  type PreviewCallbacks,
  type PreviewCanvas,
  type PreviewConnection,
  type PreviewDecodeMode,
  type PreviewTransport,
  type PreviewUiState,
  type PreviewVideoDecoder,
  type PreviewVideoDecoderFactory,
  type PreviewVisibilitySubscription,
} from "./manager.ts";

export interface PreviewSessionState {
  state: PreviewUiState;
  latencyMs: number | null;
  fps: number;
  skipped: number;
  error: string;
  backend: PreviewBackendStatus | null;
  mode: PreviewDecodeMode | null;
  codec: string;
}

export interface PreviewSessionCallbacks extends PreviewCallbacks {}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function watchPreviewWindowVisibility(
  onVisible: (visible: boolean) => void,
): Promise<PreviewVisibilitySubscription> {
  let latestEvent: boolean | null = null;
  const unlisten = await listen<boolean>("preview_window_visibility", (event) => {
    latestEvent = event.payload;
    onVisible(event.payload);
  });
  try {
    const initialVisible = await invoke<boolean>("is_preview_window_visible");
    const visible = latestEvent ?? initialVisible;
    if (latestEvent === null) onVisible(initialVisible);
    return {
      visible,
      unsubscribe: () => Promise.resolve(unlisten()),
    };
  } catch (error) {
    await Promise.resolve(unlisten()).catch(() => undefined);
    throw error;
  }
}

export class TauriPreviewTransport implements PreviewTransport {
  private statusListeners = new Map<number, UnlistenFn>();

  private async releaseStatusListener(unlisten: UnlistenFn): Promise<void> {
    await Promise.resolve(unlisten()).catch(() => undefined);
  }

  async connect(
    onFrame: (raw: ArrayBuffer) => void,
    onStatus: (status: PreviewBackendStatus) => void,
  ): Promise<PreviewConnection> {
    const unlisten = await listen<PreviewBackendStatus>("preview_status", (event) => {
      onStatus(event.payload);
    });
    try {
      const channel = new Channel<ArrayBuffer>((raw) => onFrame(raw));
      const opened = await invoke<PreviewConnection>("start_binary_preview", { channel });
      this.statusListeners.set(opened.token, unlisten);
      return opened;
    } catch (error) {
      await this.releaseStatusListener(unlisten);
      throw error;
    }
  }

  async disconnect(token: number): Promise<void> {
    const listener = this.statusListeners.get(token);
    this.statusListeners.delete(token);
    if (listener) await this.releaseStatusListener(listener);
    await invoke("stop_binary_preview", { token }).catch(() => undefined);
  }

  async retry(): Promise<void> {
    await invoke("retry_binary_preview");
  }

  async setFallback(fallback: boolean): Promise<void> {
    await invoke("set_preview_fallback", { fallback });
  }
}

/** 浏览器 WebCodecs。WebView2(Chromium)与 WKWebView(macOS 13.3+)都提供。 */
function webCodecsFactory(): PreviewVideoDecoderFactory | undefined {
  const VideoDecoderCtor = (globalThis as { VideoDecoder?: any }).VideoDecoder;
  const EncodedVideoChunkCtor = (globalThis as { EncodedVideoChunk?: any }).EncodedVideoChunk;
  if (!VideoDecoderCtor || !EncodedVideoChunkCtor) return undefined;
  return {
    async isConfigSupported(codec) {
      // 先问硬件;不行再问软件(H.264 软解在桌面端也很便宜)。
      for (const hardwareAcceleration of ["prefer-hardware", "no-preference"]) {
        try {
          const result = await VideoDecoderCtor.isConfigSupported({ codec, hardwareAcceleration, optimizeForLatency: true });
          if (result?.supported) return true;
        } catch {
          // 非法 codec 字符串等:当作不支持。
        }
      }
      return false;
    },
    create(output, error) {
      const decoder = new VideoDecoderCtor({ output, error });
      return {
        get state() { return decoder.state; },
        get decodeQueueSize() { return decoder.decodeQueueSize; },
        configure(config) { decoder.configure(config); },
        decode(chunk) { decoder.decode(new EncodedVideoChunkCtor(chunk)); },
        close() { decoder.close(); },
      } satisfies PreviewVideoDecoder;
    },
  };
}

const previewManager = createPreviewManager({
  transport: new TauriPreviewTransport(),
  watchVisibility: watchPreviewWindowVisibility,
  videoDecoder: webCodecsFactory(),
  async decode(jpeg) {
    const bytes = jpeg.buffer.slice(
      jpeg.byteOffset,
      jpeg.byteOffset + jpeg.byteLength,
    ) as ArrayBuffer;
    return createImageBitmap(new Blob([bytes], { type: "image/jpeg" }));
  },
});

/** 页面级句柄只挂载 Canvas；Raw Channel、最新帧与状态由应用级 singleton 持有。 */
export function usePreviewSession(
  initialCanvas: HTMLCanvasElement | null,
  callbacks: PreviewSessionCallbacks = {},
) {
  const state: PreviewSessionState = {
    state: "idle",
    latencyMs: null,
    fps: 0,
    skipped: 0,
    error: "",
    backend: null,
    mode: null,
    codec: "",
  };
  let bindingId: number | null = null;
  let startGeneration = 0;

  const setState = (next: PreviewUiState) => {
    state.state = next;
    callbacks.onState?.(next);
  };

  const start = async (canvas = initialCanvas): Promise<boolean> => {
    if (!isTauriRuntime() || !canvas) return false;
    const generation = ++startGeneration;
    const previousBinding = bindingId;
    bindingId = null;
    if (previousBinding !== null) await previewManager.detach(previousBinding);
    if (generation !== startGeneration) return false;
    state.error = "";
    setState("starting");
    const attachPromise = previewManager.attach(canvas as unknown as PreviewCanvas, {
      onState: (next) => {
        if (generation === startGeneration) setState(next);
      },
      onError: (reason) => {
        if (generation !== startGeneration) return;
        state.error = reason;
        callbacks.onError?.(reason);
      },
      onFrame: (stats) => {
        if (generation !== startGeneration) return;
        state.latencyMs = stats.latencyMs;
        state.fps = stats.fps;
        state.skipped = stats.skipped;
        state.mode = stats.mode;
        state.codec = stats.codec;
        callbacks.onFrame?.(stats);
      },
      onStatus: (status) => {
        if (generation !== startGeneration) return;
        state.backend = status;
        callbacks.onStatus?.(status);
      },
    }, (id) => {
      if (generation === startGeneration) bindingId = id;
      else void previewManager.detach(id);
    });
    const attachedBinding = await attachPromise;
    if (generation !== startGeneration) {
      if (bindingId === attachedBinding) bindingId = null;
      await previewManager.detach(attachedBinding);
      return false;
    }
    bindingId = attachedBinding;
    return previewManager.state !== "error";
  };

  const stop = async () => {
    startGeneration += 1;
    const currentBinding = bindingId;
    bindingId = null;
    state.latencyMs = null;
    state.fps = 0;
    state.skipped = 0;
    state.mode = null;
    state.codec = "";
    setState("stopped");
    if (currentBinding !== null) await previewManager.detach(currentBinding);
  };

  const retry = async () => {
    state.error = "";
    setState("starting");
    await previewManager.retry();
  };

  return { state, start, stop, retry };
}

export async function shutdownPreviewSession(): Promise<void> {
  await previewManager.shutdown();
}
