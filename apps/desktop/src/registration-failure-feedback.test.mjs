import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
const app = await readFile(new URL("./App.vue", import.meta.url), "utf8");

assert.match(
  rust,
  /DeviceEvent::RegisterFailure \{ message, \.\. \}[\s\S]*"scope": "register"[\s\S]*"message": message/,
  "注册失败必须发出包含原因的 device_error 事件",
);
assert.match(
  app,
  /deviceState\.value === "Failed" && deviceLive\.value[\s\S]*停止重试/,
  "顶栏在后台重试期间必须显示停止重试",
);
