import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const mediaModes = source.match(/const mediaModes = \[([\s\S]*?)\] as const;/)?.[1];
const detectMediaMode = source.match(/function detectMediaMode\(source: string\): MediaMode \{([\s\S]*?)\n\}/)?.[1];
const toggleRegistration = source.match(/async function toggleRegistration\(\) \{([\s\S]*?)\n\}/)?.[1];
const registrationButton = source.match(/<n-button[^>]*@click="toggleRegistration"[^>]*>/s)?.[0];

assert.match(
  source,
  /type MediaMode = "camera" \| "screen" \| "file";/,
  "首页媒体来源类型只应保留摄像头、屏幕和视频文件",
);
assert.ok(mediaModes, "应存在首页媒体来源选项");
assert.doesNotMatch(
  mediaModes,
  /value:\s*"none"|label:\s*"仅信令"/,
  "首页视频采集区不应再展示仅信令模式",
);
assert.match(
  detectMediaMode ?? "",
  /return "camera";/,
  "旧配置没有视频源时应迁移到摄像头模式",
);
assert.ok(toggleRegistration, "应存在首页注册/注销处理函数");
assert.match(
  toggleRegistration,
  /if \(!deviceLive\.value && !mediaSourceReady\.value\) return;/,
  "没有实际视频源时只应拒绝注册，不能阻止已运行设备注销",
);
assert.ok(registrationButton, "应存在首页注册/注销按钮");
assert.match(
  registrationButton,
  /\(!deviceLive && !mediaSourceReady\)/,
  "没有实际视频源时只应禁用注册按钮，不能禁用注销按钮",
);
assert.doesNotMatch(
  source,
  /当前仅进行 SIP 信令模拟|不发送媒体流/,
  "首页不应残留仅信令模式提示",
);
