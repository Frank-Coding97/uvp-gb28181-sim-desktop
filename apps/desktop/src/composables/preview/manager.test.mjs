import { createPreviewManager } from "./manager.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

function ok(value, message) {
  if (!value) throw new Error(message);
}

function flush() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function envelope(generation, sequence, capturedAtMs = Date.now()) {
  const jpeg = [0xff, 0xd8, 0xff, sequence & 0xff, 0xff, 0xd9];
  const bytes = new Uint8Array(32 + jpeg.length);
  bytes.set([0x55, 0x56, 0x50, 0x4a], 0);
  const view = new DataView(bytes.buffer);
  view.setUint16(4, 1, true);
  view.setUint16(6, 32, true);
  view.setBigUint64(8, BigInt(generation), true);
  view.setBigUint64(16, BigInt(sequence), true);
  view.setBigUint64(24, BigInt(capturedAtMs), true);
  bytes.set(jpeg, 32);
  return bytes.buffer;
}

function status(generation = 1, previewGeneration = 1, phase = "preview_starting", reason = null) {
  return { generation, preview_generation: previewGeneration, phase, reason };
}

/** manager 在 Node 下没有 DOM；这里提供最小的 visibilitychange 实现。 */
function installDocument(initialVisibility = "visible") {
  const previous = globalThis.document;
  const listeners = new Set();
  globalThis.document = {
    visibilityState: initialVisibility,
    addEventListener(type, listener) {
      if (type === "visibilitychange") listeners.add(listener);
    },
    removeEventListener(type, listener) {
      if (type === "visibilitychange") listeners.delete(listener);
    },
    setVisibility(next) {
      this.visibilityState = next;
      for (const listener of [...listeners]) listener();
    },
  };
  return {
    setVisibility(next) { globalThis.document.setVisibility(next); },
    restore() {
      if (previous === undefined) delete globalThis.document;
      else globalThis.document = previous;
    },
  };
}

class FakeTransport {
  connectCalls = 0;
  disconnectCalls = [];
  connections = [];

  constructor({ deferred = false } = {}) {
    this.deferred = deferred;
    this.serverActiveToken = null;
  }

  async connect(onFrame, onStatus) {
    const connection = {
      token: 40 + this.connections.length + 1,
      onFrame,
      onStatus,
      resolve: null,
      reject: null,
      closed: false,
    };
    this.connections.push(connection);
    this.connectCalls += 1;
    const opened = {
      token: connection.token,
      status: status(1, this.connections.length, "preview_starting"),
    };
    if (!this.deferred) return opened;
    return new Promise((resolve, reject) => {
      connection.resolve = () => resolve(opened);
      connection.reject = reject;
    });
  }

  resolveConnection(index = 0) {
    const connection = this.connections[index];
    if (!connection?.resolve) throw new Error(`connection ${index} is not pending`);
    this.serverActiveToken = connection.token;
    connection.resolve();
  }

  async disconnect(token) {
    this.disconnectCalls.push(token);
    const connection = this.connections.find((candidate) => candidate.token === token);
    if (connection) connection.closed = true;
    if (this.serverActiveToken === token) this.serverActiveToken = null;
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

function createTestManager(transport, options = {}) {
  const decoded = [];
  const closed = [];
  const managerOptions = {
    transport,
    now: () => 10_000,
    performanceNow: () => 10_000,
    async decode(jpeg) {
      decoded.push(jpeg[3]);
      if (options.awaitDecode) {
        await new Promise((resolve) => { options.releaseDecode = resolve; });
      }
      return {
        id: jpeg[3],
        width: 480,
        height: 270,
        close() { closed.push(jpeg[3]); },
      };
    },
  };
  if (options.watchVisibility) managerOptions.watchVisibility = options.watchVisibility;
  const manager = createPreviewManager(managerOptions);
  return { manager, decoded, closed };
}

// 旧页面的迟到 detach 不能解除当前页面；页面替换期间仍复用一个可见 Channel。
{
  const visibility = installDocument();
  const transport = new FakeTransport();
  const { manager } = createTestManager(transport);
  const firstCanvas = canvas();
  const secondCanvas = canvas();
  const first = await manager.attach(firstCanvas);
  const second = await manager.attach(secondCanvas);
  equal(transport.connectCalls, 1);

  transport.connections[0].onFrame(envelope(7, 1));
  await flush();
  equal(firstCanvas.drawn.length, 0);
  equal(secondCanvas.drawn.join(","), "1");

  equal(await manager.detach(first), false);
  transport.connections[0].onFrame(envelope(7, 2));
  await flush();
  equal(secondCanvas.drawn.join(","), "1,2");

  await manager.shutdown();
  visibility.restore();
}

// P1-01：detach 必须释放原 token；迟到帧不解码、不回画，重新挂载也不能显示旧缓存。
{
  const visibility = installDocument();
  const transport = new FakeTransport();
  const { manager, decoded } = createTestManager(transport);
  const firstCanvas = canvas();
  const secondCanvas = canvas();
  const first = await manager.attach(firstCanvas);
  transport.connections[0].onFrame(envelope(7, 1));
  await flush();
  equal(firstCanvas.drawn.join(","), "1");

  equal(await manager.detach(first), true);
  equal(transport.disconnectCalls.join(","), "41");
  transport.connections[0].onFrame(envelope(7, 2));
  await flush();
  equal(decoded.join(","), "1");

  const second = await manager.attach(secondCanvas);
  equal(transport.connectCalls, 2);
  transport.connections[1].onFrame(envelope(8, 3));
  await flush();
  equal(secondCanvas.drawn.join(","), "3");
  equal(second, 2);

  await manager.shutdown();
  equal(transport.disconnectCalls.join(","), "41,42");
  visibility.restore();
}

// P1-02：延迟 connect 期间发生快速切页，迟到的旧连接不能覆盖当前目标。
{
  const visibility = installDocument();
  const transport = new FakeTransport({ deferred: true });
  const { manager } = createTestManager(transport);
  const firstCanvas = canvas();
  const secondCanvas = canvas();
  const firstAttach = manager.attach(firstCanvas);
  const secondAttach = manager.attach(secondCanvas);
  await Promise.resolve();
  equal(transport.connectCalls, 1);
  transport.resolveConnection();
  const first = await firstAttach;
  const second = await secondAttach;
  ok(first !== second, "each attach must have a distinct binding id");
  equal(await manager.detach(first), false);
  equal(await manager.detach(second), true);
  equal(transport.disconnectCalls.join(","), "41");
  visibility.restore();
}

// 连接请求必须串行：A 在后台迟到完成时，不能把后端目标改回 A 再清掉 B。
{
  const visibility = installDocument();
  const transport = new FakeTransport({ deferred: true });
  const { manager } = createTestManager(transport);
  const target = canvas();
  const firstAttach = manager.attach(target);
  await Promise.resolve();
  visibility.setVisibility("hidden");
  await flush();
  visibility.setVisibility("visible");
  await flush();
  equal(transport.connectCalls, 1);

  transport.resolveConnection(0);
  await flush();
  await flush();
  equal(transport.connectCalls, 2);
  transport.resolveConnection(1);
  await firstAttach;
  await flush();
  equal(transport.serverActiveToken, 42);

  await manager.shutdown();
  visibility.restore();
}

// P1-03：只有“有效 Canvas 且文档可见”才建立 Channel；隐藏期间释放连接，恢复后新连接不回放旧帧。
{
  const visibility = installDocument("hidden");
  const transport = new FakeTransport();
  const { manager, decoded } = createTestManager(transport);
  const target = canvas();
  const binding = await manager.attach(target);
  equal(binding, 1);
  equal(transport.connectCalls, 0);
  equal(manager.state === "error", false);

  visibility.setVisibility("visible");
  await flush();
  equal(transport.connectCalls, 1);
  transport.connections[0].onFrame(envelope(7, 1));
  await flush();
  equal(target.drawn.join(","), "1");

  visibility.setVisibility("hidden");
  await flush();
  equal(transport.disconnectCalls.join(","), "41");
  transport.connections[0].onFrame(envelope(7, 2));
  transport.connections[0].onStatus(status(99, 99, "unavailable", "旧连接故障"));
  await flush();
  equal(decoded.join(","), "1");
  equal(manager.state === "error", false);

  visibility.setVisibility("visible");
  await flush();
  equal(transport.connectCalls, 2);
  transport.connections[1].onStatus(status(2, 1, "paused", null));
  transport.connections[1].onFrame(envelope(8, 3));
  await flush();
  equal(target.drawn.join(","), "1,3");
  equal(manager.state === "error", false);

  await manager.shutdown();
  visibility.restore();
}

// 正在解码时进入后台，已经拿到的 bitmap 只能 close，不能再回画到隐藏 Canvas。
{
  const visibility = installDocument();
  const transport = new FakeTransport();
  const decodeOptions = { awaitDecode: true, releaseDecode: null };
  const { manager, closed } = createTestManager(transport, decodeOptions);
  const target = canvas();
  await manager.attach(target);
  transport.connections[0].onFrame(envelope(7, 1));
  await Promise.resolve();
  visibility.setVisibility("hidden");
  decodeOptions.releaseDecode();
  await flush();
  equal(target.drawn.length, 0);
  equal(closed.join(","), "1");

  await manager.shutdown();
  visibility.restore();
}

// 同 generation 的迟到序号不得回画。
{
  const visibility = installDocument();
  const transport = new FakeTransport();
  const { manager } = createTestManager(transport);
  const target = canvas();
  await manager.attach(target);
  transport.connections[0].onFrame(envelope(7, 2));
  await flush();
  const beforeStaleFrame = target.drawn.join(",");
  transport.connections[0].onFrame(envelope(7, 1));
  await flush();
  equal(target.drawn.join(","), beforeStaleFrame);
  await manager.shutdown();
  visibility.restore();
}

// 原生窗口最小化时 document 仍可能 visible；nativeVisible=false 时不得创建预览 Channel。
{
  const visibility = installDocument("visible");
  const transport = new FakeTransport();
  let nativeChange = null;
  let watchCalls = 0;
  let unsubscribeCalls = 0;
  const { manager } = createTestManager(transport, {
    watchVisibility: async (onVisible) => {
      watchCalls += 1;
      nativeChange = onVisible;
      return {
        visible: false,
        unsubscribe() { unsubscribeCalls += 1; },
      };
    },
  });
  const target = canvas();
  const binding = await manager.attach(target);
  equal(binding, 1);
  equal(transport.connectCalls, 0);

  nativeChange(true);
  await flush();
  equal(transport.connectCalls, 1);
  nativeChange(true);
  await flush();
  equal(transport.connectCalls, 1);
  nativeChange(false);
  await flush();
  equal(transport.disconnectCalls.join(","), "41");
  nativeChange(false);
  await flush();
  equal(transport.disconnectCalls.join(","), "41");
  nativeChange(true);
  await flush();
  equal(transport.connectCalls, 2);

  await manager.detach(binding);
  equal(transport.disconnectCalls.join(","), "41,42");
  await manager.attach(target);
  equal(watchCalls, 1);
  await manager.shutdown();
  equal(unsubscribeCalls, 1);
  visibility.restore();
}
