import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const platform = await readFile(new URL("./platform.ts", import.meta.url), "utf8");
const device = await readFile(new URL("./device.ts", import.meta.url), "utf8");

const profile = platform.match(/export interface PlatformProfile \{([\s\S]*?)\n\}/)?.[1] ?? "";
assert.match(platform, /SignalingTransport = "UDP" \| "TCP"/, "传输枚举必须匹配 Rust UPPERCASE serde 契约");
assert.match(profile, /server_id:\s*string;/, "平台档案应保留 20 位平台 ID");
assert.match(profile, /server_domain:\s*string;/, "平台档案应独立保留 10 位平台域");
assert.match(profile, /password:\s*string;/, "模拟器平台密码应进入持久化档案 DTO");

assert.match(platform, /invoke<DesktopConfigV1>\("get_desktop_config"\)/, "配置必须从 Rust 真相源加载");
assert.match(platform, /invoke<DesktopConfigV1>\("save_desktop_config"/, "平台保存必须写入 Rust ConfigStore");
assert.match(platform, /invoke<DesktopConfigV1>\("reset_desktop_config"\)/, "损坏配置必须提供显式重置入口");
assert.doesNotMatch(platform, /localStorage\.clear\(/, "迁移不得清空其他功能的浏览器数据");
assert.doesNotMatch(platform, /localStorage\.setItem\(/, "localStorage 不得继续充当运行配置真相源");

const migratedSave = platform.indexOf('invoke<DesktopConfigV1>("save_desktop_config"');
const legacyDelete = platform.indexOf("localStorage.removeItem", migratedSave);
assert.ok(migratedSave >= 0 && legacyDelete > migratedSave, "旧 key 只能在 Rust 保存成功后删除");

assert.match(device, /invoke<string>\("start_device",\s*\{ input \}\)/, "启动命令只应提交 StartDeviceInput");
assert.match(device, /profile_id:\s*platform\.id/, "启动输入应引用已保存的平台档案 ID");
assert.match(device, /password:/, "启动输入应使用已保存的平台密码");
assert.doesNotMatch(device, /server_host:\s*platform\.server_host/, "前端不得再拼装完整 DeviceConfig");
assert.doesNotMatch(device, /localStorage\.setItem\(/, "设备配置不得再写 localStorage");
