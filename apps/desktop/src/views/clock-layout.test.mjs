import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Clock.vue", import.meta.url), "utf8");

// ── 页头横幅移除 ─────────────────────────────────────────────────────────
// 回归背景：项目已统一移除各页顶部「主标题 + 副标题 + 右侧操作区」独立横幅，
// 校时页此前仍有 .page-header，卡片未顶到顶栏下方、滚动时圆角会被裁掉。
assert.doesNotMatch(
  source,
  /class="page-header"|class="page-title"|class="page-sub"/,
  "校时页不得再保留独立页头横幅",
);
assert.doesNotMatch(source, /class="page\s+page-wrap"/, "外层不再使用全局滚动容器 .page-wrap");
assert.match(source, /不修改操作系统时间/, "原页头说明文案须保留在卡片内容中，避免信息丢失");

// ── 操作按钮上移到冻结头部 ───────────────────────────────────────────────
const headStart = source.indexOf('<div class="clock-head">');
const headEnd = source.indexOf('<section class="clock-section">');
const head = headStart >= 0 && headEnd > headStart ? source.slice(headStart, headEnd) : "";
assert.ok(head, "应能定位到卡片内冻结头部");
assert.match(head, />刷新<\/n-button>/, "刷新按钮须随冻结头部常驻");
assert.match(head, />立即校时<\/n-button>/, "立即校时按钮须随冻结头部常驻");
assert.match(head, /:disabled="!isTauri"/, "立即校时按钮仍需保留 Tauri 环境判断");
assert.match(head, /<n-tag :type="sourceType" round>/, "来源标签须与操作按钮同处冻结头部");

// ── 卡片内滚动与圆角保持 ─────────────────────────────────────────────────
const cardStyles = source.match(/^\.clock-card \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(cardStyles, "应存在主卡片样式");
assert.match(cardStyles, /overflow-y:\s*auto;/, "滚动必须发生在卡片内部");
assert.match(cardStyles, /overflow-x:\s*hidden;/, "卡片只保留垂直滚动");
assert.match(cardStyles, /min-height:\s*0;/, "滚动容器需允许被父级 flex 压到视口高度");
assert.match(cardStyles, /flex:\s*1 1 auto;/, "卡片须撑满可用高度，滚动时始终留在视口内");
assert.match(cardStyles, /display:\s*flex;/, "卡片须建立纵向布局，头部才能作为固定头");
assert.match(cardStyles, /padding:\s*0;/, "卡片不再留内边距，头部才能铺满宽度");
const pageStyles = source.match(/^\.clock-page \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(pageStyles, "应存在页面根容器样式");
assert.match(pageStyles, /display:\s*flex;/, "页面须把可用高度传递给卡片");
assert.match(pageStyles, /overflow:\s*hidden;/, "外层不应再产生第二条滚动轴");
assert.match(pageStyles, /min-height:\s*0;/, "页面需允许被父级 flex 压到视口高度");
assert.match(pageStyles, /padding-bottom:\s*16px;/, "卡片底部需留出呼吸空间");

// ── 冻结头部与卡片同体 ───────────────────────────────────────────────────
const headStyles = source.match(/^\.clock-head \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(headStyles, "应存在冻结头部样式");
assert.match(headStyles, /position:\s*sticky;/, "头部必须 position:sticky");
assert.match(headStyles, /top:\s*0;/, "头部必须吸附在卡片内顶边");
assert.match(headStyles, /z-index:\s*\d+;/, "头部必须盖在滚动内容之上");
assert.match(headStyles, /background:/, "头部必须有不透明背景，避免滚动内容透出叠字");
assert.match(headStyles, /border-radius:/, "头部顶部两角须沿用卡片圆角");
assert.match(headStyles, /box-shadow:\s*none;/, "头部不得带悬浮投影，避免看起来是独立区域");
assert.match(headStyles, /padding:\s*14px 16px 15px;/, "头部铺满卡片宽度的同时保留左右内缩");

// ── 两块内容合并进同一张卡片 ─────────────────────────────────────────────
const sectionCount = (source.match(/<section class="clock-section">/g) ?? []).length;
assert.equal(sectionCount, 2, "校时状态与 NTP 配置须作为同一张卡片内的两个区块");
assert.doesNotMatch(source, /<section class="glass-card panel">/, "不得再保留两张独立玻璃卡片");
assert.match(source, /class="clock-section-head"/, "NTP 区块仍须有独立小标题");
assert.match(source, />保存配置<\/n-button>/, "NTP 保存入口不得丢失");
assert.match(source, /<n-switch v-model:value="draft.ntp_enabled"/, "NTP 开关不得丢失");
assert.match(source, /\.clock-section \+ \.clock-section \{ border-top:/, "两个区块之间用分隔线区分而非各自成卡");
