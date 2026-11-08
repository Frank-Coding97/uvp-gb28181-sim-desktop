import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const audio = await readFile(new URL("../composables/broadcastAudio.ts", import.meta.url), "utf8");
const configGrid = source.match(/<div class="config-grid">([\s\S]*?)<div class="primary-actions">/)?.[1];
const saveConfig = source.match(/function saveConfig\(\) \{([\s\S]*?)\n\}/)?.[1];
const resetDraft = source.match(/function resetDraft\(\) \{([\s\S]*?)\n\}/)?.[1];
const toggleRegistration = source.match(/async function toggleRegistration\(\) \{([\s\S]*?)\n\}/)?.[1];

assert.ok(configGrid, "应存在 SIP 参数表单区");
// 信令传输 / 对讲流传输都改成卡片式单选,不再用下拉框。
assert.match(
  configGrid,
  /信令传输[\s\S]*class="transport-cards"[\s\S]*v-for="option in signalingTransportOptions"[\s\S]*role="radio"[\s\S]*:aria-checked="draft\.transport === option\.value"[\s\S]*@click="draft\.transport = option\.value"/,
  "信令传输应做成卡片式单选",
);
assert.match(
  configGrid,
  /对讲流传输模式[\s\S]*class="transport-cards"[\s\S]*v-for="option in audioTransportOptions"[\s\S]*role="radio"[\s\S]*:aria-checked="draft\.audio_transport === option\.value"[\s\S]*@click="draft\.audio_transport = option\.value"/,
  "对讲流传输模式应做成卡片式单选",
);
assert.ok(
  !/v-model:value="draft\.(transport|audio_transport)"[^>]*:options=/.test(configGrid),
  "两种传输都不应再使用下拉框",
);
assert.match(
  source,
  /const audioTransportOptions = \[[\s\S]*value: "UDP"[\s\S]*value: "TCP_ACTIVE"[\s\S]*value: "TCP_PASSIVE"/,
  "对讲流传输应覆盖 UDP / TCP 主动 / TCP 被动",
);
// 卡片只保留选项名，不再带说明文案。
assert.ok(!configGrid.includes("共享 socket"), "信令传输卡片不应保留共享 socket 提示");
assert.ok(!configGrid.includes("NAT"), "信令传输卡片不应保留 NAT 提示");
assert.ok(!/option\.hint/.test(configGrid), "卡片不应再渲染提示文案");
assert.ok(!source.includes("对讲传输将在后续阶段接入"), "占位说明应被真实配置项替换");
assert.match(saveConfig ?? "", /audio_transport:\s*draft\.audio_transport/, "保存时应持久化音频流传输");
assert.match(resetDraft ?? "", /draft\.audio_transport = active\.value\.audio_transport/, "进入编辑时应载入已保存的音频流传输");

// 对讲状态与播放:事件监听 + 状态条 + 用户手势解锁。
assert.match(source, /useBroadcastAudio\(\)/, "应接入语音广播音频/状态 composable");
assert.match(source, /broadcastState\.active/, "应在首页显示对讲中状态");
assert.match(audio, /listen<BroadcastAudioPayload>\("broadcast_audio"/, "composable 应监听设备侧 PCM 事件");
assert.match(audio, /createBufferSource|createBuffer\(/, "composable 应用 Web Audio 播放 PCM");
assert.match(toggleRegistration ?? "", /unlockBroadcastAudio\(\)/, "注册点击应解锁 Web Audio 自动播放");

// 对讲中标识的位置：必须与 LIVE 并排在预览面板头部，而不是留在右侧配置面板里单独占一行。
const headActions = source.match(/<div class="preview-head-actions">([\s\S]*?)<div class="preview-slot">/)?.[1];
assert.ok(headActions, "应存在预览面板头部操作区");
assert.match(
  headActions,
  /class="preview-watching"[\s\S]*class="preview-talking"/,
  "对讲中标识应紧跟在 LIVE 之后，同一行显示",
);
assert.match(headActions, /v-if="broadcastState\.active"/, "对讲中标识仍应绑定对讲会话状态");
assert.match(headActions, /:title="broadcastDetail"/, "通道/传输/端口/包数应收进 title，不占胶囊正文");
assert.match(
  headActions,
  /class="talking-mute"[\s\S]*aria-pressed="broadcastMuted"[\s\S]*broadcastMuted = !broadcastMuted/,
  "静音开关应保留在胶囊内",
);
assert.ok(!/class="broadcast-indicator"/.test(source), "对讲中标识不应再留在右侧配置面板");
assert.match(source, /\.preview-talking \{[^}]*height: 22px;/, "对讲中胶囊应与 LIVE 同高");
assert.match(source, /\.preview-head-actions \{[^}]*flex-wrap: wrap;/, "头部操作区应允许整组换行，避免挤压状态文字");
