import {
  shutdownPreviewSession,
  usePreviewSession,
} from "./session.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

function ok(value, message) {
  if (!value) throw new Error(message);
}

function flush() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

const calls = [];
const callbacks = new Map();
let nextCallbackId = 1;
let startPreviewResolve = null;
let stopPreviewResolve = null;
let previewChannel = null;
let decodeCalls = 0;

globalThis.window = {
  __TAURI_INTERNALS__: {
    transformCallback(callback) {
      const id = nextCallbackId++;
      callbacks.set(id, callback);
      return id;
    },
    unregisterCallback(id) {
      callbacks.delete(id);
    },
    invoke(command, args = {}) {
      calls.push({ command, args });
      if (command === "plugin:event|listen") return Promise.resolve(100 + calls.length);
      if (command === "plugin:event|unlisten") return Promise.resolve();
      if (command === "is_preview_window_visible") return Promise.resolve(true);
      if (command === "start_binary_preview") {
        previewChannel = args.channel;
        return new Promise((resolve) => {
          startPreviewResolve = () => resolve({
            token: 41,
            status: {
              generation: 1,
              preview_generation: 1,
              phase: "preview_starting",
              reason: null,
            },
          });
        });
      }
      if (command === "stop_binary_preview") {
        return new Promise((resolve) => {
          stopPreviewResolve = resolve;
        });
      }
      if (command === "retry_binary_preview") return Promise.resolve();
      throw new Error(`unexpected invoke ${command}`);
    },
  },
};

const fakeCanvas = {
  width: 0,
  height: 0,
  drawn: [],
  getContext() {
    return { drawImage() { fakeCanvas.drawn.push(true); } };
  },
};

globalThis.createImageBitmap = async () => {
  decodeCalls += 1;
  return { width: 480, height: 270, close() {} };
};

function envelope() {
  const jpeg = [0xff, 0xd8, 0xff, 0x01, 0xff, 0xd9];
  const bytes = new Uint8Array(32 + jpeg.length);
  bytes.set([0x55, 0x56, 0x50, 0x4a], 0);
  const view = new DataView(bytes.buffer);
  view.setUint16(4, 1, true);
  view.setUint16(6, 32, true);
  view.setBigUint64(8, 1n, true);
  view.setBigUint64(16, 1n, true);
  view.setBigUint64(24, BigInt(Date.now()), true);
  bytes.set(jpeg, 32);
  return bytes.buffer;
}

// start_binary_preview 尚未完成时 stop，start 完成后也必须保持 stopped，且释放迟到 token。
const session = usePreviewSession(fakeCanvas);
const startPromise = session.start();
for (let attempt = 0; attempt < 10 && !startPreviewResolve; attempt += 1) {
  await flush();
}
ok(startPreviewResolve, "start_binary_preview should be pending");

const stopPromise = session.stop();
equal(session.state.state, "stopped");

// stop 已经拿到同步 binding 后，连接 IPC 仍 pending 时到达的 Raw Channel 帧也不能解码。
callbacks.get(previewChannel?.id)?.({ index: 0, message: envelope() });
await flush();
equal(decodeCalls, 0);
equal(fakeCanvas.drawn.length, 0);

startPreviewResolve();
for (let attempt = 0; attempt < 10 && !stopPreviewResolve; attempt += 1) {
  await flush();
}
stopPreviewResolve?.();
await Promise.all([startPromise, stopPromise]);
await flush();
equal(session.state.state, "stopped");
equal(calls.filter(({ command }) => command === "start_binary_preview").length, 1);
equal(calls.filter(({ command }) => command === "stop_binary_preview").length, 1);
equal(calls.find(({ command }) => command === "stop_binary_preview").args.token, 41);

await shutdownPreviewSession();
