import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const mediaModes = source.match(/const mediaModes = \[([\s\S]*?)\] as const;/)?.[1];
const detectMediaMode = source.match(/function detectMediaMode\(source: string\): MediaMode \{([\s\S]*?)\n\}/)?.[1];
const syncLiveSource = source.match(/function syncLiveSource\(\) \{([\s\S]*?)\n\}/)?.[1];
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
// 按钮禁用条件已收敛到 registerBlocker(单一真相),这里改断言其内部判定,
// 语义不变:没有实际视频源只应挡住注册,不影响注销。
const registerBlocker = source.match(/const registerBlocker = computed\(\(\) => \{([\s\S]*?)\n\}\);/)?.[1] ?? "";
assert.match(
  registerBlocker,
  /if \(!mediaSourceReady\.value\) return/,
  "没有实际视频源时应作为注册禁用原因",
);
assert.match(
  registrationButton,
  /!!registerBlocker/,
  "注册按钮禁用必须走统一判定,不能另拼条件",
);
assert.doesNotMatch(
  source,
  /当前仅进行 SIP 信令模拟|不发送媒体流/,
  "首页不应残留仅信令模式提示",
);
assert.ok(syncLiveSource, "应存在首页实时源同步逻辑");
// 音频选择由独立的 selectedAudio 状态保存(初值取自已保存 URI 的 audio=),
// 切换摄像头/屏幕时沿用它,而不是被重置为 none。
assert.match(source, /const selectedAudio = ref<string>\(liveSourceAudio\(form\.value\.video_source\)\)/, "音频选择初值应取自已保存的媒体源");
assert.match(syncLiveSource ?? "", /selectedAudio\.value/, "切换采集设备时应保留音频来源选择");
assert.doesNotMatch(
  syncLiveSource ?? "",
  /audio_codec|width|height|bitrate|video_codec|video_fps|keyframe_interval/,
  "首页实时源 URI 不应覆盖音视频配置页中的固定编码参数",
);
