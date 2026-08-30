import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const source = readFileSync(new URL("./Device.vue", import.meta.url), "utf8");

test("设备联调页从 Rust 运行时快照读取状态真相", () => {
  assert.match(source, /invoke<DeviceRuntimeState>\("get_device_runtime_state"\)/);
  assert.match(source, /const runtimeState = ref<DeviceRuntimeState \| null>/);
  assert.match(source, /设备状态真相/);
  assert.match(source, /布防状态/);
  assert.match(source, /报警状态/);
  assert.match(source, /报警订阅/);
  assert.match(source, /当前位置/);
  assert.match(source, /会话参数/);
});

test("事件只触发 Rust 快照刷新并且停止后清除旧状态", () => {
  assert.match(source, /unlistenCmd = await listen<CmdEntry>[\s\S]*refreshRuntimeState/);
  assert.match(source, /unlistenSub = await listen<SubState>[\s\S]*refreshRuntimeState/);
  assert.match(source, /runtimeState\.value = null/);
  assert.match(source, /onActivated\(async \(\) =>/);
  assert.match(source, /无运行中设备/);
});
