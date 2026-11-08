import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const ts = await readFile(new URL("./device-metadata.ts", import.meta.url), "utf8");
const rust = await readFile(new URL("../src-tauri/src/device_metadata.rs", import.meta.url), "utf8");
const settings = await readFile(new URL("./views/Settings.vue", import.meta.url), "utf8");
const platform = await readFile(new URL("./platform.ts", import.meta.url), "utf8");
const configStore = await readFile(new URL("../src-tauri/src/config_store.rs", import.meta.url), "utf8");
const manscdp = await readFile(new URL("../../../crates/gb28181-protocol/src/manscdp.rs", import.meta.url), "utf8");
const device = await readFile(new URL("../../../crates/gb28181-simulator/src/device.rs", import.meta.url), "utf8");
const lib = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

function tsOptions(source, name) {
  const block = source.match(new RegExp(`export const ${name}[^=]*=\\s*\\[([\\s\\S]*?)\\n\\];`))?.[1] ?? "";
  return [...block.matchAll(/\{\s*value:\s*"([a-z_]+)",\s*label:\s*"([^"]+)"\s*\}/g)]
    .map((match) => [match[1], match[2]]);
}

function rustOptions(source, name) {
  const block = source.match(new RegExp(`pub const ${name}[^=]*=\\s*&\\[([\\s\\S]*?)\\n\\];`))?.[1] ?? "";
  return [...block.matchAll(/\("([a-z_]+)",\s*"([^"]+)"\)/g)]
    .map((match) => [match[1], match[2]]);
}

// 前后端各存一份真值表，任何单边增删/改文案都必须在这里失败。
const pairs = [
  ["DEVICE_LOCATION_OPTIONS", "LOCATION_OPTIONS", "安装位置"],
  ["CAMERA_TYPE_OPTIONS", "CAMERA_TYPE_OPTIONS", "摄像头类型"],
];
for (const [tsName, rustName, label] of pairs) {
  const front = tsOptions(ts, tsName);
  const back = rustOptions(rust, rustName);
  assert.ok(front.length >= 8, `${label}前端选项不应为空`);
  assert.deepEqual(front, back, `${label}的前端与 Rust 真值表必须逐项一致（含顺序）`);
  assert.equal(new Set(front.map(([id]) => id)).size, front.length, `${label} id 不得重复`);
}

// 配置层：旧配置文件缺字段时必须回落空串，非法 id 必须被拒。
assert.match(configStore, /#\[serde\(default\)\]\s*pub location: String,/, "location 必须兼容旧配置缺省");
assert.match(configStore, /#\[serde\(default\)\]\s*pub camera_type: String,/, "camera_type 必须兼容旧配置缺省");
assert.match(configStore, /validate_location\(&self\.device\.location\)/, "保存时必须校验安装位置");
assert.match(configStore, /validate_camera_type\(&self\.device\.camera_type\)/, "保存时必须校验摄像头类型");
assert.match(platform, /location:\s*string;/, "前端 DeviceSettings 必须带 location");
assert.match(platform, /camera_type:\s*string;/, "前端 DeviceSettings 必须带 camera_type");
assert.match(platform, /metadataValue\(legacyDevice\.location, DEVICE_LOCATION_OPTIONS/, "旧配置迁移必须过滤非法位置");
assert.match(platform, /metadataValue\(legacyDevice\.camera_type, CAMERA_TYPE_OPTIONS/, "旧配置迁移必须过滤非法类型");

// UI：两个下拉框必须绑定草稿，且参与脏标记比较，否则会被后端刷新覆盖。
assert.match(settings, /v-model="deviceDraft\.location"/, "安装位置必须绑定设备草稿");
assert.match(settings, /v-model="deviceDraft\.camera_type"/, "摄像头类型必须绑定设备草稿");
const sameDraft = settings.match(/function sameDeviceDraft\([\s\S]*?\n\}/)?.[0] ?? "";
assert.match(sameDraft, /left\.location === right\.location/, "草稿比较必须覆盖 location");
assert.match(sameDraft, /left\.camera_type === right\.camera_type/, "草稿比较必须覆盖 camera_type");

// 协议联动：只走标准字段，且 id → 中文标签在 Rust 启动时完成。
assert.match(manscdp, /#\[serde\(rename = "Address", skip_serializing_if = "Option::is_none"\)\]/, "目录项 Address 必须是可选标准字段");
assert.match(manscdp, /#\[serde\(rename = "DeviceType", skip_serializing_if = "Option::is_none"\)\]/, "设备信息 DeviceType 必须是可选标准字段");
assert.match(device, /address: self\.config\.device_info\.installation_address\.clone\(\)/, "目录应答必须填安装地址");
assert.match(device, /device_type: self\.config\.device_info\.device_type\.clone\(\)/, "设备信息应答必须填摄像头类型");
assert.match(lib, /device_metadata::location_label\(&resolved\.device\.location\)/, "启动设备时必须把位置 id 映射为标签");
assert.match(lib, /device_metadata::camera_type_label\(&resolved\.device\.camera_type\)/, "启动设备时必须把类型 id 映射为标签");
