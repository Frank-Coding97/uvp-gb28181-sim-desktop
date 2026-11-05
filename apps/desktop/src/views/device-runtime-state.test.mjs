import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./Dashboard.vue", import.meta.url), "utf8");

test("首页不再展示设备状态真相面板", () => {
  assert.doesNotMatch(source, /设备状态真相|DEVICE RUNTIME/);
  assert.doesNotMatch(source, /runtime-truth|runtime-head|runtime-grid|runtime-wide|runtime-empty/);
  assert.doesNotMatch(source, /布防状态|报警状态|当前位置|最近变更|会话参数|平台已应用配置|无运行中设备/);
});

test("首页不再轮询 Rust 运行时快照，订阅状态显示仍保留", () => {
  assert.doesNotMatch(source, /get_device_runtime_state|DeviceRuntimeState|refreshRuntimeState|scheduleRuntimeRefresh|resetRuntimeUi|runtimeState|runtimeRefreshTimer|unlistenPlatformCommand|CmdEntry/);
  assert.match(source, /subscriptions\.Alarm\.active/, "快捷卡片仍需显示报警订阅状态");
  assert.match(source, /watch\(deviceLive,[\s\S]*resetSubscriptions/, "设备停止后仍需清空订阅状态");
  assert.match(source, /onActivated\(async \(\) =>/);
});
