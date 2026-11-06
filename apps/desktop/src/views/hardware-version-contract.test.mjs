import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, platform, backend, config, protocol, simDevice, route] = await Promise.all([
  readFile(new URL("./Settings.vue", import.meta.url), "utf8"),
  readFile(new URL("../platform.ts", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/config_store.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/manscdp.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-simulator/src/device.rs", import.meta.url), "utf8"),
  readFile(new URL("../main.ts", import.meta.url), "utf8"),
]);

test("硬件版本在设置页可编辑并持久化", () => {
  assert.match(view, /v-model="deviceDraft\.hardware_version"/);
  assert.match(view, /maxlength="64"/);
  assert.match(platform, /hardware_version: string/);
  assert.match(config, /pub hardware_version: String/);
  // 旧配置文件没有这个字段，缺省必须是空串而不是解析失败。
  assert.match(config, /#\[serde\(default\)\]\s*pub hardware_version: String/);
});

test("硬件版本只在 GB-2022 应答，空值不输出空节点", () => {
  assert.match(backend, /resolved\.profile\.gb_version == common::GbVersion::V2022/);
  assert.match(backend, /!resolved\.device\.hardware_version\.trim\(\)\.is_empty\(\)/);
  assert.match(protocol, /rename = "HardwareVersion"/);
  assert.match(protocol, /pub hardware_version: Option<String>/);
  assert.match(simDevice, /\.filter\(\|value\| !value\.trim\(\)\.is_empty\(\)\)/);
});

test("硬件版本输入拦住 XML 元字符与超长内容", () => {
  // 该值会直接拼进 DeviceInfo XML，不挡住 < > & 会破坏报文体。
  assert.match(view, /\[<>&\]/);
  assert.match(view, /硬件版本不得超过 64 个字符/);
});

test("前端类型与旧配置迁移都保留硬件版本", () => {
  assert.match(platform, /hardware_version: string;/);
  assert.match(platform, /hardware_version: stringValue\(legacyDevice\.hardware_version/);
});

test("协议单测覆盖硬件版本的输出条件", () => {
  assert.match(protocol, /fn 设备信息应答可携带硬件版本/);
});

test("路由与菜单不因设置项改动而变化", () => {
  // 硬件版本落在既有「设置」页里，不该新增一级菜单。
  assert.doesNotMatch(route, /HardwareVersion/);
});
