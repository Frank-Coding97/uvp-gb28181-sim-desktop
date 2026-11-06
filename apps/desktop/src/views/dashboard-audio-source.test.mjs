import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// 首页必须能选择实时源的声音，否则音视频混合流没有入口(media-pipeline-v2 R4)。
const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");

assert.match(source, /const selectedAudio = ref<string>\(liveSourceAudio\(/, "音频选择要从现有 URI 恢复");
assert.match(source, /catalog\.value\.microphones\.map/, "麦克风选项来自枚举结果");
assert.match(source, /value: "system"/, "屏幕源提供系统声音");
assert.match(source, /microphone:\$\{item\.index\}/, "屏幕源麦克风使用 microphone:<序号>");
assert.match(source, /v-model:value="selectedAudio"/, "模板中存在声音下拉框");
assert.match(
  source,
  /buildLiveSourceUri\("camera", selectedCamera\.value, audioSource\)/,
  "摄像头 URI 带上所选声音",
);
assert.match(
  source,
  /buildLiveSourceUri\("screen", selectedScreen\.value, audioSource\)/,
  "屏幕 URI 带上所选声音",
);
// 设备运行中不能改采集配置。
assert.match(source, /:disabled="deviceLive \|\| audioOptions\.length <= 1"/);
console.log("dashboard-audio-source ok");
