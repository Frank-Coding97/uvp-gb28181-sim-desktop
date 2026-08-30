import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";

const app = await readFile(new URL("./App.vue", import.meta.url), "utf8");
const router = await readFile(new URL("./main.ts", import.meta.url), "utf8");
const backend = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

assert.doesNotMatch(app, /camera-demo|视频采集demo/, "侧边栏不应再暴露视频采集 demo");
assert.doesNotMatch(router, /CameraDemo|camera-demo/, "路由不应再加载视频采集 demo");
assert.doesNotMatch(
  backend,
  /acquire_camera_demo|release_camera_demo|camera_lease/,
  "Tauri 后端不应保留 demo 命令或专用摄像头租约",
);

for (const path of [
  "./views/CameraDemo.vue",
  "./composables/camera-demo/session.ts",
  "./composables/camera-demo/session.test.mjs",
  "../src-tauri/src/camera_lease.rs",
]) {
  await assert.rejects(access(new URL(path, import.meta.url)), `${path} 应随 demo 一并删除`);
}
