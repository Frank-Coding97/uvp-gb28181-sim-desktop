import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Settings.vue", import.meta.url), "utf8");
const mediaSection = source.match(/<section v-else-if="activeKey === 'media' && mediaDraft"[\s\S]*?<\/section>/)?.[0] ?? "";

assert.match(
  source,
  /<div v-if="activeKey !== 'media'" class="content-actions">/,
  "媒体页不应复用其他设置页的全量操作栏",
);
assert.ok(mediaSection, "应存在独立的音视频配置区块");
assert.doesNotMatch(mediaSection, /恢复全部默认|保存有效设置/, "媒体页不应触发全量恢复或全量保存");
assert.match(mediaSection, /class="media-save-row"/, "媒体页保存入口应位于参数内容底部");
assert.match(mediaSection, />保存音视频设置<\/button>/, "媒体页应提供明确的音视频保存按钮");
assert.match(
  mediaSection,
  /保存后，下次启动设备时生效；运行中请先注销后修改。/,
  "媒体页提示应说明生效时机和运行中修改约束",
);
