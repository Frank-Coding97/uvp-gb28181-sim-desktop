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

// ── 页面滚动边界 ─────────────────────────────────────────────────────────
// 回归背景：App.vue 的 .content 是 overflow:hidden，设置页原先没有自己的滚动上下文，
// 窗口偏矮时底部提示条与「保存音视频设置」按钮被直接裁掉，且不会出现垂直滚动条。
const pageStyles = source.match(/^\.settings-page \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(pageStyles, "应存在设置页根容器样式");
assert.match(pageStyles, /overflow-y:\s*auto;/, "设置页必须可垂直滚动，不能裁切底部内容");
assert.match(pageStyles, /overflow-x:\s*hidden;/, "设置页只保留垂直滚动，避免横向溢出");
assert.match(pageStyles, /min-height:\s*0;/, "滚动容器需允许被父级 flex 压到视口高度");
assert.match(pageStyles, /padding-bottom:\s*16px;/, "滚动到底需留出底部呼吸空间");

// ── 共用页头显隐 ─────────────────────────────────────────────────────────
// 回归背景：媒体页内容顶部已有「模拟器有效配置」小节标题与「参数摘要」，
// 共用页头（主标题 / 副标题 / 平台摘要 / 锁定徽标）在该页重复且挤占首屏，仅此页隐藏。
const headerTag = source.match(/<div[^>]*class="settings-header"[^>]*>/)?.[0] ?? "";
assert.match(headerTag, /v-if="activeKey !== 'media'"/, "音视频配置页应隐藏共用页头");
assert.match(source, /class="lock-badge"/, "其他设置页的锁定状态徽标不得一并删除");
