import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Recordings.vue", import.meta.url), "utf8");

// ── 页头移除但标题与功能不丢 ─────────────────────────────────────────────
// 回归背景：录像中心原先是「page-head 标题横幅 + status-strip 圆角块 + 左右两张独立卡片」，
// 三块区域割裂；现整条标题横幅移除，标题与「刷新 / 开始录像」迁移到卡片冻结头部。
assert.doesNotMatch(source, /class="page-head"/, "不得再保留独立的页面标题横幅");
assert.match(source, /<h1>录像中心<\/h1>/, "页面标题仍须与一级菜单一致（menu-information-architecture 依赖此字符串）");
assert.match(source, /<main class="recording-card glass-card">/, "内容须收进同一张撑满视口的玻璃主卡片");

const headStart = source.indexOf('<div class="recording-head">');
const bodyStart = source.indexOf('<div class="recording-body">');
const headBlock = headStart >= 0 && bodyStart > headStart ? source.slice(headStart, bodyStart) : "";
assert.ok(headBlock, "应存在卡片内的冻结头部");
assert.match(headBlock, /<h1>录像中心<\/h1>/, "标题须落在冻结头部而不是另起一条横幅");
assert.doesNotMatch(headBlock, /class="eyebrow"/, "冻结头部保持单行紧凑，不残留英文装饰横条");
assert.match(headBlock, /@click="refresh\(\)"[\s\S]*?>刷新<\/n-button>/, "刷新按钮须随冻结头部常驻");
assert.match(headBlock, /@click="toggleRecording"/, "开始/停止录像按钮须随冻结头部常驻");
assert.match(headBlock, /:disabled="!isActive && !canStart"/, "开始录像的禁用条件不得在迁移中丢失");
assert.match(headBlock, /:loading="actionBusy"/, "录像按钮的忙碌态不得丢失");

const bodyBlock = bodyStart >= 0 ? source.slice(bodyStart, source.indexOf("</main>")) : "";
assert.ok(bodyBlock, "卡片内须有内容容器");
assert.match(bodyBlock, /class="recording-note"/, "原标题描述须作为卡片内提示保留");
assert.match(bodyBlock, /class="status-strip"/, "状态条须并入主卡片");
assert.match(bodyBlock, /class="workspace"/, "双栏工作区须并入主卡片");
assert.ok(
  bodyBlock.indexOf('class="status-strip"') < bodyBlock.indexOf('class="workspace"'),
  "状态条须在工作区之前，保持原有信息顺序",
);
assert.match(bodyBlock, /class="library-panel"/, "录像列表仍在");
assert.match(bodyBlock, /class="player-panel"/, "本地播放面板仍在");

// ── 卡片内滚动与四角圆角 ─────────────────────────────────────────────────
const pageStyles = source.match(/^\.recording-page \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(pageStyles, "应存在页面外层样式");
assert.match(pageStyles, /overflow:\s*hidden;/, "外层不得产生第二条滚动轴");
assert.match(pageStyles, /display:\s*flex;/, "外层须把可用高度传递给主卡片");
assert.match(pageStyles, /padding-bottom:\s*16px;/, "卡片底部须留出呼吸空间");
const cardStyles = source.match(/^\.recording-card \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(cardStyles, "应存在主卡片样式");
assert.match(cardStyles, /overflow-y:\s*auto;/, "滚动必须发生在主卡片内部");
assert.match(cardStyles, /overflow-x:\s*hidden;/, "主卡片只保留垂直滚动");
assert.match(cardStyles, /flex:\s*1 1 auto;/, "主卡片须撑满可用高度，滚动时始终留在视口内");
assert.match(cardStyles, /min-height:\s*0;/, "滚动容器需允许被父级 flex 压到视口高度");
assert.match(cardStyles, /padding:\s*0;/, "卡片不留内边距，头部才能铺满宽度、与卡片同体");
assert.match(source, /class="recording-card glass-card"/, "主卡片须复用全局 16px 圆角与玻璃样式");

// ── 冻结头部与卡片同体 ───────────────────────────────────────────────────
const headStyles = source.match(/^\.recording-head \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(headStyles, "应存在冻结头部样式");
assert.match(headStyles, /position:\s*sticky;/, "标题行必须 sticky");
assert.match(headStyles, /top:\s*0;/, "标题行必须吸附在滚动容器顶部");
assert.match(headStyles, /z-index:\s*\d+;/, "标题行必须盖在滚动内容之上");
assert.match(headStyles, /background:/, "标题行必须有不透明背景，避免滚动内容透出叠字");
assert.match(headStyles, /border-radius:\s*var\(--radius-lg\) var\(--radius-lg\) 0 0;/, "头部顶部两角须沿用卡片圆角");
assert.match(headStyles, /box-shadow:\s*none;/, "头部不得带悬浮投影，避免看起来是独立区域");
assert.match(headStyles, /padding:\s*14px 16px 15px;/, "头部须铺满卡片宽度并保留左右内缩");

// ── 状态条与双栏不再是独立卡片 ───────────────────────────────────────────
const stripStyles = source.match(/^\.status-strip \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(stripStyles, "应存在状态条样式");
assert.doesNotMatch(stripStyles, /border-radius:/, "状态条不得再是独立圆角块");
assert.doesNotMatch(stripStyles, /background:\s*rgba\(255,255,255/, "状态条不得再叠独立白底");
assert.match(stripStyles, /border-bottom:/, "状态条改由底部分隔线与工作区区分");
const panelStyles = source.match(/^\.library-panel, \.player-panel \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(panelStyles, "应存在左右面板共用样式");
assert.doesNotMatch(panelStyles, /border:\s*1px solid/, "左右面板不得各自套独立边框");
assert.doesNotMatch(panelStyles, /border-radius:/, "左右面板不得各自套独立圆角");
assert.match(source, /^\.library-panel \{[^}]*border-right:/m, "宽视口下用一条竖向分隔线区分左右");

const narrowRule = source.match(/@media \(max-width: 1000px\) \{([\s\S]*?)\n\}/)?.[1] ?? "";
assert.match(narrowRule, /\.recording-body \{ flex: 0 0 auto; \}/, "窄视口内容区须按自然高度撑开，由主卡片统一滚动");
assert.match(narrowRule, /\.workspace \{[^}]*min-height: auto;/, "窄视口工作区不得被 flex 压到 0 高");
assert.match(narrowRule, /\.workspace \{[^}]*grid-template-columns: 1fr;[^}]*overflow: visible;/, "窄视口须回退单列且不产生嵌套滚动");
assert.match(narrowRule, /\.library-panel \{[^}]*min-height: 320px;/, "窄视口列表须保留最小可见高度，避免被播放器挤成一条");
assert.match(narrowRule, /\.library-panel \{[^}]*border-right: 0;/, "单列时竖向分隔线须改为横向");
assert.match(narrowRule, /\.player-panel \{ min-height: 360px;/, "单列时播放器仍保有最小高度");
