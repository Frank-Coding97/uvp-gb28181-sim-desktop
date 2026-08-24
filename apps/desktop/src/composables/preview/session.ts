import { Channel, invoke } from "@tauri-apps/api/core";
import { annexBNals, WebCodecsH264Decoder, type DecoderMetrics, type PreviewPacketMetadata, type VideoDecoderFactory } from "./codec";

export interface PreviewSessionState {
  state: "idle" | "starting" | "playing" | "stopped" | "error";
  decoder: DecoderMetrics;
  latencyMs: number | null;
  fps: number;
  error: string;
}

export interface PreviewSessionCallbacks {
  onState?: (state: PreviewSessionState["state"]) => void;
  onError?: (error: string) => void;
}

function decoderFactory(): VideoDecoderFactory | null {
  const ctor = (globalThis as typeof globalThis & { VideoDecoder?: typeof VideoDecoder }).VideoDecoder;
  if (!ctor) return null;
  return {
    isConfigSupported: (config) => ctor.isConfigSupported(config),
    create: (options) => new ctor(options) as unknown as ReturnType<VideoDecoderFactory["create"]>,
  };
}

function parseEnvelope(raw: ArrayBuffer): PreviewPacketMetadata | null {
  if (raw.byteLength < 48) return null;
  const bytes = new Uint8Array(raw);
  if (String.fromCharCode(...bytes.slice(0, 4)) !== "UVP1" || bytes[4] !== 1) return null;
  const view = new DataView(raw);
  const codec = bytes[5] === 0 ? "H264" : bytes[5] === 1 ? "H265" : null;
  if (!codec) return null;
  const payloadLength = view.getUint32(44, true);
  if (payloadLength !== raw.byteLength - 48) return null;
  return {
    data: bytes.slice(48),
    key_frame: (view.getUint16(6, true) & 1) !== 0,
    codec,
    session_id: Number(view.getBigUint64(8, true)),
    sequence: Number(view.getBigUint64(16, true)),
    fps: view.getUint32(24, true),
    pts_90k: Number(view.getBigUint64(28, true)),
    captured_at_ms: Number(view.getBigUint64(36, true)),
  };
}

export function usePreviewSession(canvas: HTMLCanvasElement | null, callbacks: PreviewSessionCallbacks = {}) {
  const state: PreviewSessionState = {
    state: "idle",
    decoder: { state: "idle", path: null, fallbackReason: null, droppedFrames: 0, lastSequence: 0, sessionId: null, firstFrameAt: null },
    latencyMs: null,
    fps: 0,
    error: "",
  };
  let decoder: WebCodecsH264Decoder | null = null;
  let channel: Channel<ArrayBuffer> | null = null;
  let fpsStart = 0;
  let fpsFrames = 0;

  const drawFrame = (frame: VideoFrame, packet: PreviewPacketMetadata) => {
    if (canvas) {
      canvas.width = frame.displayWidth || frame.codedWidth;
      canvas.height = frame.displayHeight || frame.codedHeight;
      canvas.getContext("2d")?.drawImage(frame, 0, 0, canvas.width, canvas.height);
    }
    frame.close();
    state.state = "playing";
    callbacks.onState?.("playing");
    state.latencyMs = Math.max(0, Date.now() - packet.captured_at_ms);
    const now = performance.now();
    if (!fpsStart) fpsStart = now;
    fpsFrames += 1;
    if (now - fpsStart >= 1000) {
      state.fps = Math.round(fpsFrames * 1000 / (now - fpsStart));
      fpsFrames = 0;
      fpsStart = now;
    }
  };

  const start = async (): Promise<boolean> => {
    if (!canvas || typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return false;
    const factory = decoderFactory();
    if (!factory) return false;
    state.state = "starting";
    decoder = new WebCodecsH264Decoder(factory, {
      onFrame: drawFrame,
      onAck: (sessionId, sequence) => { void invoke("ack_preview_frame", { sessionId, sequence }); },
      onMetrics: (metrics) => { state.decoder = metrics; },
      onFallback: (reason) => { state.error = `WebCodecs 回退软件解码：${reason}`; },
    });
    channel = new Channel<ArrayBuffer>((raw) => {
      const packet = parseEnvelope(raw);
      if (!packet) {
        state.error = "预览二进制帧格式错误";
        state.state = "error";
        callbacks.onError?.(state.error);
        callbacks.onState?.("error");
        return;
      }
      if (packet.codec === "H264") void decoder?.push(packet);
      else state.error = "当前 WebView 不支持 H.265 快路径，等待 FFmpeg 回退";
    });
    try {
      await invoke("start_binary_preview", { channel });
      return true;
    } catch (error) {
      state.error = String(error);
      state.state = "error";
      callbacks.onError?.(state.error);
      callbacks.onState?.("error");
      return false;
    }
  };

  const stop = async () => {
    await invoke("stop_binary_preview").catch(() => undefined);
    decoder?.close();
    decoder = null;
    channel = null;
    state.state = "stopped";
    callbacks.onState?.("stopped");
  };

  return { state, start, stop, parseEnvelope, annexBNals };
}
