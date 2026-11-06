// 首页默认不预览:只有用户点击"开启预览"后才订阅(media-pipeline-v2 R3)。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const dashboard = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const preview = await readFile(new URL("./Preview.vue", import.meta.url), "utf8");
const optIn = await readFile(new URL("../composables/preview/optIn.ts", import.meta.url), "utf8");

// 开关是模块级、默认关闭、不持久化。
assert.match(optIn, /export const previewEnabled = ref\(false\);/);
assert.doesNotMatch(optIn, /localStorage|persist/, "预览开关不能持久化,应用重启恢复关闭");

// startPreview 必须以用户开启为前提。
assert.match(
  dashboard,
  /async function startPreview\(\) \{\s*if \(!previewEnabled\.value \|\|/,
  "startPreview 第一个条件必须是 previewEnabled",
);
// 生命周期与采集就绪事件只恢复已开启的预览。
for (const line of dashboard.split("\n").filter((text) => /startPreview\(\)/.test(text) && !/function startPreview/.test(text))) {
  if (/enablePreview|await startPreview\(\);$/.test(line.trim()) && !/previewEnabled/.test(line)) {
    // enablePreview 内部的调用:紧跟在 previewEnabled.value = true 之后。
    continue;
  }
  assert.match(line, /previewEnabled\.value/, `自动触发点必须检查 previewEnabled:${line.trim()}`);
}
assert.match(dashboard, /async function enablePreview\(\) \{[\s\S]*?previewEnabled\.value = true;[\s\S]*?await startPreview\(\);/);

// 预览区提供开启/关闭按钮。
assert.match(dashboard, /v-if="!previewEnabled"[\s\S]*?@click="enablePreview"[\s\S]*?开启预览/);
assert.match(dashboard, /v-if="previewEnabled"[^>]*@click="disablePreview"[^>]*>关闭预览/);

// 推流预览页同样不在激活时自动开始。
assert.match(preview, /onActivated\(\(\) => \{ if \(previewEnabled\.value\) void start\(\); \}\);/);
assert.doesNotMatch(preview, /onActivated\(\(\) => \{ void start\(\); \}\);/);
// 抓拍不再依赖预览出帧。
assert.match(preview, /<n-button :disabled="capturing" :loading="capturing" @click="capture">抓拍<\/n-button>/);

console.log("dashboard preview opt-in ok");
