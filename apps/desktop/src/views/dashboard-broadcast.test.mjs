import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const audio = await readFile(new URL("../composables/broadcastAudio.ts", import.meta.url), "utf8");
const configGrid = source.match(/<div class="config-grid">([\s\S]*?)<div class="primary-actions">/)?.[1];
const saveConfig = source.match(/function saveConfig\(\) \{([\s\S]*?)\n\}/)?.[1];
const resetDraft = source.match(/function resetDraft\(\) \{([\s\S]*?)\n\}/)?.[1];
const toggleRegistration = source.match(/async function toggleRegistration\(\) \{([\s\S]*?)\n\}/)?.[1];

assert.ok(configGrid, "应存在 SIP 参数表单区");
assert.match(
  configGrid,
  /音频流传输[\s\S]*v-model:value="draft\.audio_transport"[\s\S]*:options="audioTransportOptions"/,
  "首页 SIP 配置应提供音频流传输下拉",
);
assert.match(
  source,
  /const audioTransportOptions = \[[\s\S]*value: "UDP"[\s\S]*value: "TCP_ACTIVE"[\s\S]*value: "TCP_PASSIVE"/,
  "音频流传输应覆盖 UDP / TCP 主动 / TCP 被动",
);
assert.ok(!source.includes("对讲传输将在后续阶段接入"), "占位说明应被真实配置项替换");
assert.match(saveConfig ?? "", /audio_transport:\s*draft\.audio_transport/, "保存时应持久化音频流传输");
assert.match(resetDraft ?? "", /draft\.audio_transport = active\.value\.audio_transport/, "进入编辑时应载入已保存的音频流传输");

// 对讲状态与播放:事件监听 + 状态条 + 用户手势解锁。
assert.match(source, /useBroadcastAudio\(\)/, "应接入语音广播音频/状态 composable");
assert.match(source, /broadcastState\.active/, "应在首页显示对讲中状态");
assert.match(audio, /listen<BroadcastAudioPayload>\("broadcast_audio"/, "composable 应监听设备侧 PCM 事件");
assert.match(audio, /createBufferSource|createBuffer\(/, "composable 应用 Web Audio 播放 PCM");
assert.match(toggleRegistration ?? "", /unlockBroadcastAudio\(\)/, "注册点击应解锁 Web Audio 自动播放");
