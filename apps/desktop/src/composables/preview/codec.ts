/**
 * WebCodecs H.264 快路径。
 *
 * 生产侧的 PreviewPacket 是 Annex-B 访问单元；WebCodecs 的 avc 格式需要
 * avcC description 和 length-prefixed NAL。这里保持解析/转换纯函数，方便
 * 用固定码流 fixture 测试，不把 Tauri 或 Vue 状态耦合进 codec 层。
 */

export type DecoderPath = "hardware-preferred" | "software-fallback" | "mjpeg-fallback";
export type DecoderState =
  | "idle"
  | "probing"
  | "hardware-preferred"
  | "software-fallback"
  | "waiting-keyframe"
  | "playing"
  | "error";

export interface PreviewPacketMetadata {
  data: Uint8Array;
  key_frame: boolean;
  codec: "H264" | "H265" | "h264" | "h265";
  session_id: number;
  sequence: number;
  fps: number;
  pts_90k: number;
  captured_at_ms: number;
}

export interface H264Config {
  codec: string;
  description: Uint8Array;
}

export interface DecoderMetrics {
  state: DecoderState;
  path: DecoderPath | null;
  fallbackReason: string | null;
  droppedFrames: number;
  lastSequence: number;
  sessionId: number | null;
  firstFrameAt: number | null;
}

export interface DecoderCallbacks {
  onFrame: (frame: VideoFrame, packet: PreviewPacketMetadata) => void;
  onAck: (sessionId: number, sequence: number) => void;
  onMetrics?: (metrics: DecoderMetrics) => void;
  onFallback?: (reason: string) => void;
}

function isH264(codec: PreviewPacketMetadata["codec"]): boolean {
  return codec.toLowerCase() === "h264";
}

/** 提取 Annex-B 中的 NAL 单元，支持 3 字节和 4 字节起始码。 */
export function annexBNals(data: Uint8Array): Uint8Array[] {
  const starts: number[] = [];
  for (let i = 0; i + 3 < data.length; i += 1) {
    if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 1) {
      starts.push(i);
      i += 2;
    } else if (data[i] === 0 && data[i + 1] === 0 && data[i + 2] === 0 && data[i + 3] === 1) {
      starts.push(i);
      i += 3;
    }
  }
  return starts.map((start, index) => {
    const prefix = data[start + 2] === 1 ? 3 : 4;
    const end = index + 1 < starts.length ? starts[index + 1] : data.length;
    return data.slice(start + prefix, end);
  }).filter((nal) => nal.length > 0);
}

export function h264ParameterSets(data: Uint8Array): { sps: Uint8Array; pps: Uint8Array } | null {
  let sps: Uint8Array | undefined;
  let pps: Uint8Array | undefined;
  for (const nal of annexBNals(data)) {
    const type = nal[0] & 0x1f;
    if (type === 7 && !sps) sps = nal;
    if (type === 8 && !pps) pps = nal;
  }
  return sps && pps ? { sps, pps } : null;
}

/** 从 SPS/PPS 构造 AVCDecoderConfigurationRecord 和 avc1 codec string。 */
export function buildH264Config(data: Uint8Array): H264Config | null {
  const sets = h264ParameterSets(data);
  if (!sets || sets.sps.length < 4) return null;
  const { sps, pps } = sets;
  const description = new Uint8Array(11 + sps.length + pps.length);
  let offset = 0;
  description[offset++] = 1;
  description[offset++] = sps[1];
  description[offset++] = sps[2];
  description[offset++] = sps[3];
  description[offset++] = 0xff; // 4-byte NAL length field.
  description[offset++] = 0xe1; // one SPS.
  description[offset++] = (sps.length >> 8) & 0xff;
  description[offset++] = sps.length & 0xff;
  description.set(sps, offset);
  offset += sps.length;
  description[offset++] = 1; // one PPS.
  description[offset++] = (pps.length >> 8) & 0xff;
  description[offset++] = pps.length & 0xff;
  description.set(pps, offset);
  const codec = `avc1.${[sps[1], sps[2], sps[3]].map((value) => value.toString(16).padStart(2, "0")).join("")}`;
  return { codec, description };
}

/** 把单个 Annex-B 访问单元转换成 WebCodecs avc 格式的长度前缀 NAL。 */
export function annexBToAvcc(data: Uint8Array): Uint8Array {
  const nals = annexBNals(data);
  const total = nals.reduce((sum, nal) => sum + 4 + nal.length, 0);
  const output = new Uint8Array(total);
  const view = new DataView(output.buffer);
  let offset = 0;
  for (const nal of nals) {
    view.setUint32(offset, nal.length);
    offset += 4;
    output.set(nal, offset);
    offset += nal.length;
  }
  return output;
}

function timestampUs(pts90k: number): number {
  return Math.round(pts90k * 1_000_000 / 90_000);
}

/** 可注入 WebCodecs 构造器，便于无浏览器环境做状态机 fixture。 */
export interface VideoDecoderLike {
  configure(config: VideoDecoderConfig): void;
  decode(chunk: EncodedVideoChunk): void;
  flush(): Promise<void>;
  reset(): void;
  close(): void;
  readonly decodeQueueSize: number;
}

export interface VideoDecoderFactory {
  isConfigSupported(config: VideoDecoderConfig): Promise<{ supported?: boolean }>;
  create(options: VideoDecoderInit): VideoDecoderLike;
}

export class WebCodecsH264Decoder {
  private readonly factory: VideoDecoderFactory;
  private readonly callbacks: DecoderCallbacks;
  private decoder: VideoDecoderLike | null = null;
  private state: DecoderState = "idle";
  private path: DecoderPath | null = null;
  private fallbackReason: string | null = null;
  private sessionId: number | null = null;
  private lastSequence = 0;
  private firstFrameAt: number | null = null;
  private droppedFrames = 0;
  private config: H264Config | null = null;
  private fallbackAttempted = false;
  private needsKeyframe = true;

  constructor(factory: VideoDecoderFactory, callbacks: DecoderCallbacks) {
    this.factory = factory;
    this.callbacks = callbacks;
  }

  metrics(): DecoderMetrics {
    return {
      state: this.state,
      path: this.path,
      fallbackReason: this.fallbackReason,
      droppedFrames: this.droppedFrames,
      lastSequence: this.lastSequence,
      sessionId: this.sessionId,
      firstFrameAt: this.firstFrameAt,
    };
  }

  private report(): void {
    this.callbacks.onMetrics?.(this.metrics());
  }

  private setState(state: DecoderState): void {
    this.state = state;
    this.report();
  }

  async configureFrom(packet: PreviewPacketMetadata): Promise<boolean> {
    if (!isH264(packet.codec)) return false;
    const config = buildH264Config(packet.data);
    if (!config) {
      this.setState("waiting-keyframe");
      return false;
    }
    this.config = config;
    if (this.sessionId !== packet.session_id) {
      this.fallbackAttempted = false;
      this.fallbackReason = null;
    }
    this.sessionId = packet.session_id;
    this.lastSequence = 0;
    this.needsKeyframe = true;
    this.setState("probing");
    try {
      const preferred: VideoDecoderConfig = {
        codec: config.codec,
        description: config.description,
        hardwareAcceleration: "prefer-hardware",
        optimizeForLatency: true,
      };
      const support = await this.factory.isConfigSupported(preferred);
      if (support.supported === false) throw new Error("WebCodecs 不支持 H.264 硬件偏好配置");
      this.createDecoder(preferred, "hardware-preferred");
      return true;
    } catch (error) {
      return this.fallbackToSoftware(String(error));
    }
  }

  private createDecoder(config: VideoDecoderConfig, path: DecoderPath): void {
    this.decoder?.close();
    this.decoder = this.factory.create({
      output: (frame) => {
        const packet = this.pendingPacket;
        if (!packet) {
          frame.close();
          return;
        }
        if (this.firstFrameAt === null) this.firstFrameAt = performance.now();
        this.needsKeyframe = false;
        this.setState("playing");
        this.callbacks.onFrame(frame, packet);
        this.callbacks.onAck(packet.session_id, packet.sequence);
      },
      error: (error) => {
        void this.handleDecoderError(String(error));
      },
    });
    this.decoder.configure(config);
    this.path = path;
    this.setState(path === "hardware-preferred" ? "hardware-preferred" : "software-fallback");
  }

  private pendingPacket: PreviewPacketMetadata | null = null;

  private async handleDecoderError(reason: string): Promise<void> {
    if (!this.fallbackAttempted) {
      await this.fallbackToSoftware(reason);
    } else {
      this.fallbackReason = reason;
      this.setState("error");
    }
  }

  private async fallbackToSoftware(reason: string): Promise<boolean> {
    if (!this.config || this.fallbackAttempted) {
      this.fallbackReason = reason;
      this.setState("error");
      return false;
    }
    this.fallbackAttempted = true;
    this.fallbackReason = reason;
    this.callbacks.onFallback?.(reason);
    try {
      const config: VideoDecoderConfig = {
        codec: this.config.codec,
        description: this.config.description,
        hardwareAcceleration: "prefer-software",
        optimizeForLatency: true,
      };
      const support = await this.factory.isConfigSupported(config);
      if (support.supported === false) throw new Error("WebCodecs 不支持软件 H.264 配置");
      this.createDecoder(config, "software-fallback");
      this.needsKeyframe = true;
      this.setState("waiting-keyframe");
      return true;
    } catch (error) {
      this.fallbackReason = `${reason}; 软件回退失败: ${String(error)}`;
      this.setState("error");
      return false;
    }
  }

  async push(packet: PreviewPacketMetadata): Promise<void> {
    if (!isH264(packet.codec)) return;
    if (!this.config || this.sessionId !== packet.session_id || (packet.key_frame && !this.decoder)) {
      await this.configureFrom(packet);
    }
    if (!this.decoder || !this.config || this.sessionId !== packet.session_id) return;
    if (this.lastSequence && packet.sequence > this.lastSequence + 1) {
      this.needsKeyframe = true;
      this.droppedFrames += 1;
      this.setState("waiting-keyframe");
    }
    this.lastSequence = packet.sequence;
    if (this.needsKeyframe && !packet.key_frame) {
      this.droppedFrames += 1;
      this.report();
      return;
    }
    this.pendingPacket = packet;
    const chunk = new EncodedVideoChunk({
      type: packet.key_frame ? "key" : "delta",
      timestamp: timestampUs(packet.pts_90k),
      data: annexBToAvcc(packet.data),
    });
    try {
      this.decoder.decode(chunk);
    } catch (error) {
      await this.handleDecoderError(String(error));
    }
  }

  close(): void {
    this.decoder?.close();
    this.decoder = null;
    this.pendingPacket = null;
    this.setState("idle");
  }
}
