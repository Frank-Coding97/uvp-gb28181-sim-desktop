import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Settings.vue", import.meta.url), "utf8");

assert.doesNotMatch(source, /localStorage\./, "设置页不得继续维护浏览器持久化副本");
assert.match(source, /saveDesktopConfig/, "有效字段必须保存到 Rust ConfigStore");
assert.match(source, /resetDesktopConfig/, "恢复默认必须调用 Rust 显式重置命令");
assert.match(source, /effectiveConfig/, "运行状态必须回读 Rust EffectiveDeviceConfig");
assert.match(source, /register_expires_secs/, "注册有效期必须接入有效 DTO");
assert.match(source, /heartbeat_fail_threshold/, "连续心跳失败阈值必须接入有效 DTO");
assert.match(source, /video_fps/, "视频 FPS 必须接入有效 DTO");
assert.match(source, /bind_mode/, "绑定模式必须接入有效 DTO");
assert.match(source, /sip_trace/, "SIP trace 必须接入有效 DTO");
assert.match(source, /min="3600"/, "前端应与 Rust 一致拒绝小于 3600 秒的注册有效期");
assert.match(source, /后续阶段/, "未实现能力必须明确标注后续阶段");
assert.match(source, /disabled[^>]*>目录分页/, "目录分页必须显示但禁用");
assert.match(source, /disabled[^>]*>硬件版本/, "硬件版本必须显示但禁用");
assert.match(source, /const editableSection = computed/, "未实现区域不得显示可用的保存动作");
