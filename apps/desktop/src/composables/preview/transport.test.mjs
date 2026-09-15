import {
  TauriPreviewTransport,
  watchPreviewWindowVisibility,
} from "./session.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

const unlistenCalls = [];
const callbacks = new Map();
const eventHandlers = new Map();
let nextCallbackId = 1;
let nextEventId = 100;
let nextToken = 41;
let nativeInitialResolve = null;

globalThis.window = {
  __TAURI_EVENT_PLUGIN_INTERNALS__: {
    unregisterListener() {},
  },
  __TAURI_INTERNALS__: {
    transformCallback(callback) {
      const id = nextCallbackId++;
      callbacks.set(id, callback);
      return id;
    },
    unregisterCallback() {},
    invoke(command, args = {}) {
      if (command === "plugin:event|listen") {
        const eventId = nextEventId++;
        eventHandlers.set(args.event, callbacks.get(args.handler));
        return Promise.resolve(eventId);
      }
      if (command === "plugin:event|unlisten") {
        unlistenCalls.push(args.eventId);
        return Promise.resolve();
      }
      if (command === "start_binary_preview") {
        return Promise.resolve({
          token: nextToken++,
          status: {
            generation: 1,
            preview_generation: 1,
            phase: "preview_starting",
            reason: null,
          },
        });
      }
      if (command === "stop_binary_preview") return Promise.resolve();
      if (command === "is_preview_window_visible") {
        return new Promise((resolve) => {
          nativeInitialResolve = () => resolve(true);
        });
      }
      throw new Error(`unexpected invoke ${command}`);
    },
  },
};

const transport = new TauriPreviewTransport();
const first = await transport.connect(() => {}, () => {});
const second = await transport.connect(() => {}, () => {});

// disconnect 旧 token 不能清理后来连接的 status listener。
await transport.disconnect(first.token);
equal(unlistenCalls.join(","), "100");
await transport.disconnect(second.token);
equal(unlistenCalls.join(","), "100,101");

// 原生窗口事件先于初值查询到达时，事件值优先，且先建立监听再查询初值。
const nativeEvents = [];
const visibilityPromise = watchPreviewWindowVisibility((visible) => {
  nativeEvents.push(visible);
});
for (let attempt = 0; attempt < 10 && !eventHandlers.get("preview_window_visibility"); attempt += 1) {
  await Promise.resolve();
}
const nativeHandler = eventHandlers.get("preview_window_visibility");
if (!nativeHandler) throw new Error("preview window visibility listener should be registered");
nativeHandler({ payload: false });
for (let attempt = 0; attempt < 10 && !nativeInitialResolve; attempt += 1) {
  await Promise.resolve();
}
if (!nativeInitialResolve) throw new Error("initial native visibility query should be pending");
nativeInitialResolve();
const visibility = await visibilityPromise;
equal(visibility.visible, false);
equal(nativeEvents.join(","), "false");
equal(unlistenCalls.join(","), "100,101");
await visibility.unsubscribe();
equal(unlistenCalls.join(","), "100,101,102");
