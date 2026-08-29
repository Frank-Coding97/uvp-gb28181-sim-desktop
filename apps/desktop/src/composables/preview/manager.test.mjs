import { createPreviewManager } from "./manager.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

function envelope(generation, sequence) {
  const jpeg = [0xff, 0xd8, 0xff, sequence, 0xff, 0xd9];
  const bytes = new Uint8Array(32 + jpeg.length);
  bytes.set([0x55, 0x56, 0x50, 0x4a], 0);
  const view = new DataView(bytes.buffer);
  view.setUint16(4, 1, true);
  view.setUint16(6, 32, true);
  view.setBigUint64(8, BigInt(generation), true);
  view.setBigUint64(16, BigInt(sequence), true);
  view.setBigUint64(24, BigInt(Date.now()), true);
  bytes.set(jpeg, 32);
  return bytes.buffer;
}

class FakeTransport {
  connectCalls = 0;
  disconnectCalls = [];
  onFrame;
  onStatus;

  async connect(onFrame, onStatus) {
    this.connectCalls += 1;
    this.onFrame = onFrame;
    this.onStatus = onStatus;
    return {
      token: 41,
      status: { generation: 1, preview_generation: 1, phase: "preview_starting", reason: null },
    };
  }

  async disconnect(token) {
    this.disconnectCalls.push(token);
  }

  async retry() {}
}

function canvas() {
  const drawn = [];
  return {
    width: 0,
    height: 0,
    drawn,
    getContext() {
      return { drawImage(bitmap) { drawn.push(bitmap.id); } };
    },
  };
}

const transport = new FakeTransport();
const manager = createPreviewManager({
  transport,
  async decode(jpeg) {
    return { id: jpeg[3], width: 480, height: 270, close() {} };
  },
});
const firstCanvas = canvas();
const secondCanvas = canvas();
const first = await manager.attach(firstCanvas);
const second = await manager.attach(secondCanvas);
equal(transport.connectCalls, 1);

transport.onFrame(envelope(7, 1));
await new Promise((resolve) => setTimeout(resolve, 0));
equal(firstCanvas.drawn.length, 0);
equal(secondCanvas.drawn.join(","), "1");

// 旧页面的迟到 detach 不能解除当前页面。
manager.detach(first);
transport.onFrame(envelope(7, 2));
await new Promise((resolve) => setTimeout(resolve, 0));
equal(secondCanvas.drawn.join(","), "1,2");

// 无 Canvas 时不解码；再次挂载会立即渲染期间收到的 latest 帧。
manager.detach(second);
transport.onFrame(envelope(7, 3));
await new Promise((resolve) => setTimeout(resolve, 0));
equal(secondCanvas.drawn.join(","), "1,2");
await manager.attach(firstCanvas);
await new Promise((resolve) => setTimeout(resolve, 0));
equal(firstCanvas.drawn.join(","), "3");
equal(transport.connectCalls, 1);

// keep-alive 页面往返 10 轮只替换 Canvas，不重建后端 Channel。
let activeBinding = null;
for (let round = 0; round < 10; round += 1) {
  if (activeBinding !== null) manager.detach(activeBinding);
  const target = round % 2 === 0 ? secondCanvas : firstCanvas;
  activeBinding = await manager.attach(target);
  transport.onFrame(envelope(7, 4 + round));
  await new Promise((resolve) => setTimeout(resolve, 0));
}
equal(transport.connectCalls, 1);
manager.detach(activeBinding);

// 同 generation 的迟到序号不得回画。
activeBinding = await manager.attach(firstCanvas);
await new Promise((resolve) => setTimeout(resolve, 0));
const beforeStaleFrame = firstCanvas.drawn.join(",");
transport.onFrame(envelope(7, 2));
await new Promise((resolve) => setTimeout(resolve, 0));
equal(firstCanvas.drawn.join(","), beforeStaleFrame);

await manager.shutdown();
equal(transport.disconnectCalls.join(","), "41");
