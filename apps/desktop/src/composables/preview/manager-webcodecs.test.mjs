// WebCodecs 编码帧路径:用假 VideoDecoder 验证 manager 的配置、丢帧、恢复与回退。
import { createPreviewManager } from "./manager.ts";
import { envelopeV2, jpegEnvelope } from "./envelope.fixture.mjs";

function equal(actual, expected, message = "") {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${message} expected ${e}, got ${a}`);
}

function ok(value, message) {
  if (!value) throw new Error(message);
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

const KEY = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1f, 0, 0, 0, 1, 0x68, 0xeb, 0, 0, 0, 1, 0x65, 0x88];
const DELTA = [0, 0, 0, 1, 0x41, 0x9a];

function key(sequence, { generation = 5, codec = "avc1.64001F", kind = "h264" } = {}) {
  return envelopeV2({ kind, key: true, codec, generation, sequence, capturedAtMs: 9_990, payload: KEY });
}

function delta(sequence, generation = 5) {
  return envelopeV2({ kind: "h264", key: false, generation, sequence, capturedAtMs: 9_990, payload: DELTA });
}

class FakeTransport {
  onFrame = null;
  fallbackCalls = [];
  async connect(onFrame) {
    this.onFrame = onFrame;
    return { token: 1, status: { generation: 1, preview_generation: 1, phase: "waiting_keyframe", reason: null } };
  }
  async disconnect() {}
  async retry() {}
  async setFallback(value) { this.fallbackCalls.push(value); }
}

function fakeDecoderFactory({ supported = true, queue = 0 } = {}) {
  const created = [];
  const probes = [];
  return {
    created,
    probes,
    async isConfigSupported(codec) {
      probes.push(codec);
      return supported;
    },
    create(output, error) {
      const decoder = {
        state: "unconfigured",
        decodeQueueSize: queue,
        configs: [],
        chunks: [],
        closed: false,
        configure(config) { this.configs.push(config); this.state = "configured"; },
        decode(chunk) {
          this.chunks.push(chunk.type);
          output({ displayWidth: 640, displayHeight: 360, timestamp: chunk.timestamp, close() { decoder.framesClosed += 1; } });
        },
        close() { this.closed = true; this.state = "closed"; },
        framesClosed: 0,
        fail(reason) { error(new Error(reason)); },
      };
      created.push(decoder);
      return decoder;
    },
  };
}

function canvas() {
  const drawn = [];
  return { width: 0, height: 0, drawn, getContext() { return { drawImage(image) { drawn.push(image.displayWidth ?? image.width); } }; } };
}

async function setup(factoryOptions) {
  const transport = new FakeTransport();
  const factory = factoryOptions === null ? undefined : fakeDecoderFactory(factoryOptions);
  const frames = [];
  const manager = createPreviewManager({
    transport,
    videoDecoder: factory,
    now: () => 10_000,
    performanceNow: () => 10_000,
    async decode() { return { width: 320, height: 240, close() {} }; },
  });
  const target = canvas();
  await manager.attach(target, { onFrame: (stats) => frames.push(stats) });
  return { manager, transport, factory, target, frames };
}

// 1. 关键帧带 codec → 检测 → 建解码器 → 立即解这一帧(不用等下一个 GOP)。
{
  const { manager, transport, factory, target, frames } = await setup({});
  transport.onFrame(delta(1));
  equal(factory.created.length, 0, "起播前的 delta 不应建解码器");
  transport.onFrame(key(2));
  await flush();
  equal(factory.probes, ["avc1.64001F"]);
  equal(factory.created.length, 1);
  const decoder = factory.created[0];
  equal(decoder.configs[0], { codec: "avc1.64001F", hardwareAcceleration: "prefer-hardware", optimizeForLatency: true });
  equal(decoder.chunks, ["key"], "检测期间到达的关键帧在配置后立即解码");
  transport.onFrame(delta(3));
  transport.onFrame(delta(4));
  equal(decoder.chunks, ["key", "delta", "delta"]);
  equal(target.width, 640);
  equal(target.drawn.length, 3);
  equal(decoder.framesClosed, 3, "VideoFrame 画完必须立即 close");
  equal(frames.at(-1).mode, "webcodecs");
  equal(frames.at(-1).codec, "avc1.64001F");
  equal(frames.at(-1).latencyMs, 10);
  // 乱序/重复帧丢弃。
  transport.onFrame(delta(4));
  equal(decoder.chunks.length, 3);
  await manager.shutdown();
}

// 2. 解码队列积压时丢 delta,下一个关键帧恢复。
{
  const { manager, transport, factory } = await setup({ queue: 5 });
  transport.onFrame(key(1));
  await flush();
  const decoder = factory.created[0];
  transport.onFrame(delta(2));
  transport.onFrame(delta(3));
  equal(decoder.chunks, ["key"], "积压时 delta 全部丢弃");
  decoder.decodeQueueSize = 0;
  transport.onFrame(delta(4));
  equal(decoder.chunks, ["key"], "丢过帧后必须等关键帧");
  transport.onFrame(key(5));
  transport.onFrame(delta(6));
  equal(decoder.chunks, ["key", "key", "delta"]);
  await manager.shutdown();
}

// 3. 解码器报错 → 重建,从下一个关键帧继续。
{
  const { manager, transport, factory } = await setup({});
  transport.onFrame(key(1));
  await flush();
  factory.created[0].fail("hw reset");
  transport.onFrame(delta(2));
  equal(factory.created.length, 1, "出错后 delta 不能重建解码器");
  transport.onFrame(key(3));
  equal(factory.created.length, 2);
  equal(factory.created[1].chunks, ["key"]);
  await manager.shutdown();
}

// 4. 新代际(设备重启)→ 旧解码器关闭,重新检测。
{
  const { manager, transport, factory } = await setup({});
  transport.onFrame(key(1));
  await flush();
  transport.onFrame(key(1, { generation: 6, codec: "hvc1.1.6.L120.90", kind: "h265" }));
  await flush();
  ok(factory.created[0].closed, "旧代际解码器必须关闭");
  equal(factory.probes, ["avc1.64001F", "hvc1.1.6.L120.90"]);
  equal(factory.created[1].configs[0].codec, "hvc1.1.6.L120.90");
  await manager.shutdown();
}

// 5. 不支持该编码 → 请求 JPEG 回退一次;之后 JPEG 帧照常渲染。
{
  const { manager, transport, factory, target, frames } = await setup({ supported: false });
  transport.onFrame(key(1, { codec: "hvc1.1.6.L120.90", kind: "h265" }));
  await flush();
  equal(transport.fallbackCalls, [true]);
  equal(factory.created.length, 0);
  transport.onFrame(key(2, { codec: "hvc1.1.6.L120.90", kind: "h265" }));
  await flush();
  equal(transport.fallbackCalls, [true], "只请求一次回退");
  transport.onFrame(jpegEnvelope(9, 1, 9_990));
  await flush();
  await flush();
  equal(target.drawn, [320]);
  equal(frames.at(-1).mode, "jpeg");
  await manager.shutdown();
}

// 6. 环境没有 WebCodecs → 第一帧就请求回退。
{
  const { manager, transport } = await setup(null);
  transport.onFrame(key(1));
  equal(transport.fallbackCalls, [true]);
  await manager.shutdown();
}

console.log("manager-webcodecs ok");
