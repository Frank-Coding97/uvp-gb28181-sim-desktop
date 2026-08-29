import {
  createLatestFrameScheduler,
  parsePreviewFrame,
  PREVIEW_HEADER_LEN,
} from "./scheduler.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

function envelope(capturedAtMs, jpegBytes, generation = 7, sequence = 11) {
  const bytes = new Uint8Array(PREVIEW_HEADER_LEN + jpegBytes.length);
  bytes.set([0x55, 0x56, 0x50, 0x4a], 0);
  const view = new DataView(bytes.buffer);
  view.setUint16(4, 1, true);
  view.setUint16(6, PREVIEW_HEADER_LEN, true);
  view.setBigUint64(8, BigInt(generation), true);
  view.setBigUint64(16, BigInt(sequence), true);
  view.setBigUint64(24, BigInt(capturedAtMs), true);
  bytes.set(jpegBytes, PREVIEW_HEADER_LEN);
  return bytes.buffer;
}

const jpeg = [0xff, 0xd8, 0xff, 0xe0, 0x11, 0x22, 0xff, 0xd9];

// ---- envelope 解析 ----

const frame = parsePreviewFrame(envelope(1234, jpeg));
if (!frame) throw new Error("expected frame to parse");
equal(frame.capturedAtMs, 1234);
equal(frame.generation, 7);
equal(frame.sequence, 11);
equal(frame.jpeg.length, jpeg.length);
equal(frame.jpeg[0], 0xff);
equal(frame.jpeg[1], 0xd8);

// payload 与原 buffer 共享内存，不做多余复制。
const shared = envelope(1, jpeg);
equal(parsePreviewFrame(shared).jpeg.buffer, shared);

// 只有头部、没有画面数据。
equal(parsePreviewFrame(new ArrayBuffer(PREVIEW_HEADER_LEN)), null);
const badMagic = envelope(1, jpeg);
new Uint8Array(badMagic)[0] = 0;
equal(parsePreviewFrame(badMagic), null);
// 不以 SOI 开头说明流对齐出了问题，必须报错而不是喂给解码器。
equal(parsePreviewFrame(envelope(1, [0x00, 0x01, 0x02, 0x03])), null);

// ---- 最新帧调度 ----

// 渲染慢于到帧时，中间帧应被跳过，最后渲染的必须是最新那一帧。
// 关键：这里没有任何队列深度阈值——历史上两次用队列深度判断跳帧，
// 一次把画面卡在 GOP 频率，一次只剩首帧。
let release;
const rendered = [];
const slowScheduler = createLatestFrameScheduler(async (frame) => {
  rendered.push(frame.capturedAtMs);
  await new Promise((resolve) => { release = resolve; });
});

slowScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 1 });
await Promise.resolve();
equal(rendered.length, 1);
equal(rendered[0], 1);

// 第 1 帧还在渲染时连来三帧：只有最后一帧应该被保留。
slowScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 2 });
slowScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 3 });
slowScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 4 });
equal(rendered.length, 1);
equal(slowScheduler.skipped, 2);

release();
await Promise.resolve();
await Promise.resolve();
equal(rendered.length, 2);
equal(rendered[1], 4); // 跳过 2 和 3，直接渲染最新的 4

// 渲染跟得上时不应丢任何帧。
const fastRendered = [];
const fastScheduler = createLatestFrameScheduler(async (frame) => {
  fastRendered.push(frame.capturedAtMs);
});
for (const ts of [10, 11, 12]) {
  fastScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: ts });
  await new Promise((resolve) => setTimeout(resolve, 0));
}
equal(fastRendered.length, 3);
equal(fastScheduler.skipped, 0);

// 渲染抛错不能让调度器卡死，后续帧仍要能画出来。
let calls = 0;
const recovered = [];
const failingScheduler = createLatestFrameScheduler(async (frame) => {
  calls += 1;
  if (calls === 1) throw new Error("render boom");
  recovered.push(frame.capturedAtMs);
});
failingScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 20 });
await new Promise((resolve) => setTimeout(resolve, 0));
failingScheduler.submit({ jpeg: new Uint8Array(jpeg), capturedAtMs: 21 });
await new Promise((resolve) => setTimeout(resolve, 0));
equal(recovered.length, 1);
equal(recovered[0], 21);
