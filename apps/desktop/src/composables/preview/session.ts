import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  createPreviewManager,
  type PreviewBackendStatus,
  type PreviewCallbacks,
  type PreviewCanvas,
  type PreviewConnection,
  type PreviewTransport,
  type PreviewUiState,
} from "./manager";

export interface PreviewSessionState {
  state: PreviewUiState;
  latencyMs: number | null;
  fps: number;
  skipped: number;
  error: string;
  backend: PreviewBackendStatus | null;
}

export interface PreviewSessionCallbacks extends PreviewCallbacks {}

function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

class TauriPreviewTransport implements PreviewTransport {
  private unlistenStatus: UnlistenFn | null = null;

  async connect(
    onFrame: (raw: ArrayBuffer) => void,
    onStatus: (status: PreviewBackendStatus) => void,
  ): Promise<PreviewConnection> {
    this.unlistenStatus?.();
    this.unlistenStatus = await listen<PreviewBackendStatus>("preview_status", (event) => {
      onStatus(event.payload);
    });
    const channel = new Channel<ArrayBuffer>((raw) => onFrame(raw));
    try {
      return await invoke<PreviewConnection>("start_binary_preview", { channel });
    } catch (error) {
      this.unlistenStatus?.();
      this.unlistenStatus = null;
      throw error;
    }
  }

  async disconnect(token: number): Promise<void> {
    await invoke("stop_binary_preview", { token }).catch(() => undefined);
    this.unlistenStatus?.();
    this.unlistenStatus = null;
  }

  async retry(): Promise<void> {
    await invoke("retry_binary_preview");
  }
}

const previewManager = createPreviewManager({
  transport: new TauriPreviewTransport(),
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
  };
  let bindingId: number | null = null;

  const setState = (next: PreviewUiState) => {
    state.state = next;
    callbacks.onState?.(next);
  };

  const start = async (canvas = initialCanvas): Promise<boolean> => {
    if (!isTauriRuntime() || !canvas) return false;
    if (bindingId !== null) previewManager.detach(bindingId);
    state.error = "";
    setState("starting");
    bindingId = await previewManager.attach(canvas as unknown as PreviewCanvas, {
      onState: setState,
      onError: (reason) => {
        state.error = reason;
        callbacks.onError?.(reason);
      },
      onFrame: (stats) => {
        state.latencyMs = stats.latencyMs;
        state.fps = stats.fps;
        state.skipped = stats.skipped;
        callbacks.onFrame?.(stats);
      },
      onStatus: (status) => {
        state.backend = status;
        callbacks.onStatus?.(status);
      },
    });
    return previewManager.state !== "error";
  };

  const stop = async () => {
    if (bindingId !== null) previewManager.detach(bindingId);
    bindingId = null;
    state.latencyMs = null;
    state.fps = 0;
    state.skipped = 0;
    setState("stopped");
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
