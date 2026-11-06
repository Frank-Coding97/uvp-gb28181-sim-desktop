/**
 * 预览帧的解析与渲染调度。
 *
 * 这里刻意不依赖 Tauri 和 DOM，纯逻辑可以直接用 node 跑测试。
 */

/** envelope 头长度，与 Rust 侧 `PREVIEW_HEADER_LEN` 对应。 */
export const PREVIEW_HEADER_LEN = 36;
export const PREVIEW_ENVELOPE_VERSION = 2;
const PREVIEW_MAGIC = [0x55, 0x56, 0x50, 0x4a] as const; // UVPJ

/** 载荷类型，与 Rust `PreviewPayloadKind` 对应。 */
export type PreviewPayloadKind = "jpeg" | "h264" | "h265";
const PAYLOAD_KINDS: readonly PreviewPayloadKind[] = ["jpeg", "h264", "h265"];

export interface PreviewFrame {
  kind: PreviewPayloadKind;
  /** 编码帧是否关键帧；JPEG 恒为 true。 */
  keyFrame: boolean;
  /** 关键帧携带的 WebCodecs codec 字符串（如 `avc1.64001F`），其余为空串。 */
  codec: string;
  /** 载荷字节（JPEG 或 Annex B 访问单元），与原 buffer 共享内存，不复制。 */
  payload: Uint8Array;
  /** 兼容旧调用方：等同于 payload。 */
  jpeg: Uint8Array;
  /** 后端发出这一帧的时刻，用于算端到端延迟。 */
  capturedAtMs: number;
  generation: number;
  sequence: number;
}

/**
 * 解析后端 v2 envelope：
 * `magic(4) version(2) header_len(2) kind(1) flags(1) codec_len(2)
 *  generation(8) sequence(8) captured_at_ms(8)` + codec(codec_len) + payload。
 * 不接受旧版或字段超出 JS 安全整数的帧。
 */
export function parsePreviewFrame(raw: ArrayBuffer): PreviewFrame | null {
  if (raw.byteLength <= PREVIEW_HEADER_LEN) return null;
  const view = new DataView(raw);
  if (PREVIEW_MAGIC.some((byte, index) => view.getUint8(index) !== byte)) return null;
  if (view.getUint16(4, true) !== PREVIEW_ENVELOPE_VERSION) return null;
  const headerLength = view.getUint16(6, true);
  if (headerLength !== PREVIEW_HEADER_LEN) return null;
  const kind = PAYLOAD_KINDS[view.getUint8(8)];
  if (!kind) return null;
  const keyFrame = (view.getUint8(9) & 0x01) !== 0;
  const codecLength = view.getUint16(10, true);
  const generation = view.getBigUint64(12, true);
  const sequence = view.getBigUint64(20, true);
  const capturedAtMs = view.getBigUint64(28, true);
  if (
    generation > BigInt(Number.MAX_SAFE_INTEGER)
    || sequence > BigInt(Number.MAX_SAFE_INTEGER)
    || capturedAtMs > BigInt(Number.MAX_SAFE_INTEGER)
  ) return null;
  const payloadOffset = headerLength + codecLength;
  if (raw.byteLength <= payloadOffset) return null;
  const codec = codecLength
    ? String.fromCharCode(...new Uint8Array(raw, headerLength, codecLength))
    : "";
  const payload = new Uint8Array(raw, payloadOffset);
  if (kind === "jpeg") {
    // JPEG 必须以 SOI(FF D8 FF) 开头，否则说明流对齐出了问题。
    if (payload[0] !== 0xff || payload[1] !== 0xd8 || payload[2] !== 0xff) return null;
  } else if (!startsWithAnnexB(payload)) {
    return null;
  }
  return {
    kind,
    keyFrame: kind === "jpeg" ? true : keyFrame,
    codec,
    payload,
    jpeg: payload,
    capturedAtMs: Number(capturedAtMs),
    generation: Number(generation),
    sequence: Number(sequence),
  };
}

function startsWithAnnexB(bytes: Uint8Array): boolean {
  return (bytes[0] === 0 && bytes[1] === 0 && bytes[2] === 1)
    || (bytes[0] === 0 && bytes[1] === 0 && bytes[2] === 0 && bytes[3] === 1);
}

export interface LatestFrameScheduler {
  /** 提交一帧。渲染期间再次提交只会保留最后一帧。 */
  submit(frame: PreviewFrame): void;
  /** 因为渲染跟不上而被跳过的帧数，仅用于诊断。 */
  readonly skipped: number;
  /** 渲染失败的帧数，仅用于诊断。 */
  readonly failed: number;
  reset(): void;
}

/**
 * 只渲染最新帧的调度器。
 *
 * MJPEG 每帧自包含，旧帧对实时预览没有任何价值——渲染期间到达的帧直接覆盖
 * 待渲染槽位，天然跳过积压，**不需要任何队列深度阈值**。此前用解码队列深度
 * 判断是否跳帧，两次都把画面判死（一次卡在 GOP 频率，一次只剩首帧）。
 */
export function createLatestFrameScheduler(
  render: (frame: PreviewFrame) => Promise<void>,
): LatestFrameScheduler {
  let pending: PreviewFrame | null = null;
  let running = false;
  let skipped = 0;
  let failed = 0;

  const loop = async () => {
    running = true;
    try {
      while (pending) {
        const frame = pending;
        pending = null;
        try {
          await render(frame);
        } catch (error) {
          // 单帧画不出来不能让整条预览停掉：下一帧是完整 JPEG，可以照常渲染。
          // 这正是 MJPEG 相对 H.264 的价值——错误不会沿参考链扩散。
          failed += 1;
          console.error("[preview] frame render failed", error);
        }
      }
    } finally {
      running = false;
    }
  };

  return {
    submit(frame: PreviewFrame) {
      // 上一帧还没渲染就被新帧顶掉：这正是我们想要的，延迟永远是最新的那一帧。
      if (pending) skipped += 1;
      pending = frame;
      if (!running) void loop();
    },
    get skipped() {
      return skipped;
    },
    get failed() {
      return failed;
    },
    reset() {
      pending = null;
      skipped = 0;
      failed = 0;
    },
  };
}
