/**
 * 预览帧的解析与渲染调度。
 *
 * 这里刻意不依赖 Tauri 和 DOM，纯逻辑可以直接用 node 跑测试。
 */

/** envelope 头长度，与 Rust 侧 `PREVIEW_HEADER_LEN` 对应。 */
export const PREVIEW_HEADER_LEN = 32;
export const PREVIEW_ENVELOPE_VERSION = 1;
const PREVIEW_MAGIC = [0x55, 0x56, 0x50, 0x4a] as const; // UVPJ

export interface PreviewFrame {
  /** 完整 JPEG 字节，与原 buffer 共享内存，不复制。 */
  jpeg: Uint8Array;
  /** 采集侧读到这一帧的时刻，用于算端到端延迟。 */
  capturedAtMs: number;
  generation: number;
  sequence: number;
}

/** 解析后端 versioned envelope；不接受旧版或字段超出 JS 安全整数的帧。 */
export function parsePreviewFrame(raw: ArrayBuffer): PreviewFrame | null {
  if (raw.byteLength <= PREVIEW_HEADER_LEN) return null;
  const view = new DataView(raw);
  if (PREVIEW_MAGIC.some((byte, index) => view.getUint8(index) !== byte)) return null;
  if (view.getUint16(4, true) !== PREVIEW_ENVELOPE_VERSION) return null;
  const headerLength = view.getUint16(6, true);
  if (headerLength !== PREVIEW_HEADER_LEN || raw.byteLength <= headerLength) return null;
  const generation = view.getBigUint64(8, true);
  const sequence = view.getBigUint64(16, true);
  const capturedAtMs = view.getBigUint64(24, true);
  if (
    generation > BigInt(Number.MAX_SAFE_INTEGER)
    || sequence > BigInt(Number.MAX_SAFE_INTEGER)
    || capturedAtMs > BigInt(Number.MAX_SAFE_INTEGER)
  ) return null;
  const jpeg = new Uint8Array(raw, headerLength);
  // JPEG 必须以 SOI(FF D8 FF) 开头，否则说明流对齐出了问题。
  if (jpeg[0] !== 0xff || jpeg[1] !== 0xd8 || jpeg[2] !== 0xff) return null;
  return {
    jpeg,
    capturedAtMs: Number(capturedAtMs),
    generation: Number(generation),
    sequence: Number(sequence),
  };
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
