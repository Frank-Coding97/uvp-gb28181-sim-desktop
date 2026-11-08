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
assert.doesNotMatch(
  mediaSection,
  /media-save-row|>保存音视频设置<\/button>/,
  "保存入口已上移标题行，参数区底部不得重复出现按钮",
);
assert.match(
  mediaSection,
  /保存后，下次启动设备时生效；运行中请先注销后修改。/,
  "媒体页提示应说明生效时机和运行中修改约束",
);

// ── 紧凑布局适用范围 ─────────────────────────────────────────────────────
// 回归背景：「隐藏共用页头 + 冻结标题行」从媒体页、设备页扩到 OSD 水印、网络配置后，
// 四个可编辑设置页已全覆盖，唯一例外是关于页。从页签清单机检范围，避免条件与清单脱节。
const settingsBlock = source.match(/const settings = \[([\s\S]*?)\];/)?.[1] ?? "";
const settingKeys = [...settingsBlock.matchAll(/key:\s*"([a-z]+)"/g)].map((match) => match[1]);
assert.deepEqual(settingKeys, ["device", "media", "osd", "network", "about"], "设置页签应固定为五个区域");
assert.match(
  source,
  /const editableSection = computed\(\(\) => activeKey\.value !== "about"\);/,
  "可编辑区域须恰好排除关于页，紧凑布局才能覆盖四个参数页",
);

// ── 保存入口固定在标题行最右侧 ───────────────────────────────────────────
const headingStart = source.indexOf('<div v-if="editableSection" class="content-heading"');
const headingEnd = source.indexOf('<div v-if="feedback"');
const contentHeading = headingStart >= 0 && headingEnd > headingStart
  ? source.slice(headingStart, headingEnd)
  : "";
assert.ok(contentHeading, "应能定位到「模拟器有效配置」标题行");
assert.match(
  contentHeading,
  /sticky: editableSection/,
  "四个可编辑设置页（含 OSD 水印、网络配置）标题行全部冻结，关于页无标题行",
);
const mediaActions = contentHeading.match(/<div v-else class="content-actions">[\s\S]*?<\/div>/)?.[0] ?? "";
assert.ok(mediaActions, "标题行右侧应有媒体页专属操作区");
assert.match(mediaActions, />保存音视频设置<\/button>/, "保存按钮应位于标题行右侧操作区");
assert.match(
  mediaActions,
  /:disabled="locked \|\| !config \|\| !mediaDirty"/,
  "上移后保存按钮仍须保留锁定、配置与脏值三重禁用条件",
);

// ── 四个参数页共用全量操作栏 ─────────────────────────────────────────────
// 回归背景：媒体页先做了「卡片内滚动 + 标题行冻结」，设备 / OSD / 网络页当时未加 sticky，
// 滚动后「恢复默认 / 保存有效设置」随内容一起滚走；现四页共用同一套冻结头部。
const sharedActions = contentHeading.match(/<div v-if="activeKey !== 'media'" class="content-actions">[\s\S]*?<\/div>/)?.[0] ?? "";
assert.ok(sharedActions, "设备 / OSD / 网络页应保留全量操作栏");
assert.match(sharedActions, /activeKey === 'osd'[\s\S]*?>恢复 OSD 默认<\/button>/, "OSD 水印页恢复默认按钮须随冻结标题行常驻");
assert.match(sharedActions, /<button v-else class="btn ghost"[\s\S]*?>恢复全部默认<\/button>/, "设备 / 网络页恢复默认按钮须随冻结标题行常驻");
assert.match(sharedActions, />保存有效设置<\/button>/, "设备 / OSD / 网络页保存按钮须随冻结标题行常驻");
assert.match(sharedActions, /:disabled="locked \|\| !config"/, "全量保存按钮禁用条件不得改动");

// ── 标题行冻结 ───────────────────────────────────────────────────────────
const stickyRule = source.match(/^\.content-heading\.sticky \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(stickyRule, "应存在冻结标题行的 sticky 规则");
assert.match(stickyRule, /position:\s*sticky;/, "标题行必须 position:sticky");
assert.match(stickyRule, /top:\s*0;/, "标题行必须吸附在滚动容器顶部");
assert.match(stickyRule, /z-index:\s*\d+;/, "标题行必须盖在滚动内容之上");
assert.match(stickyRule, /background:/, "标题行必须有不透明背景，避免滚动内容透出叠字");
assert.match(stickyRule, /border-radius:/, "冻结时标题行顶部两角须沿用卡片圆角");
assert.match(stickyRule, /box-shadow:\s*none;/, "标题行不得带悬浮投影，避免看起来是独立区域");

// ── 卡片内滚动与圆角保持 ─────────────────────────────────────────────────
// 回归背景：原先 .settings-page 是滚动容器，卡片整体会滚出视口，sticky 标题行被钉成
// 一条直角独立横条，卡片圆角在滚动时丢失；现滚动下沉到卡片内部，卡片始终留在视口内。
const cardStyles = source.match(/^\.settings-main \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(cardStyles, "应存在设置卡片样式");
assert.match(cardStyles, /overflow-y:\s*auto;/, "参数区滚动必须发生在卡片内部");
assert.match(cardStyles, /overflow-x:\s*hidden;/, "卡片只保留垂直滚动，避免横向溢出");
assert.match(cardStyles, /min-height:\s*0;/, "滚动容器需允许被父级 flex 压到视口高度");
assert.match(cardStyles, /flex:\s*1 1 auto;/, "卡片须撑满可用高度，滚动时始终留在视口内");
assert.match(cardStyles, /display:\s*flex;/, "卡片须建立纵向布局，标题行才能作为固定头");
assert.match(cardStyles, /padding:\s*0;/, "卡片不再留内边距，标题行才能铺满宽度、与卡片同体");
const headingStyles = source.match(/^\.content-heading \{([^}]*)\}/m)?.[1] ?? "";
assert.match(headingStyles, /padding:\s*14px 16px 15px;/, "标题行铺满卡片宽度的同时保留左右内缩");
const sectionStyles = source.match(/^\.settings-section \{([^}]*)\}/m)?.[1] ?? "";
assert.match(sectionStyles, /padding:\s*18px 16px 16px;/, "参数区自行承担横向留白与底部呼吸空间");

// ── 页面外层容器 ─────────────────────────────────────────────────────────
// 回归背景：App.vue 的 .content 是 overflow:hidden，设置页原先没有自己的滚动上下文，
// 窗口偏矮时底部提示条被直接裁掉；现滚动在卡片内完成，外层只负责高度传递。
const pageStyles = source.match(/^\.settings-page \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(pageStyles, "应存在设置页根容器样式");
assert.match(pageStyles, /display:\s*flex;/, "页面须把可用高度传递给卡片");
assert.match(pageStyles, /overflow:\s*hidden;/, "外层不应再产生第二条滚动轴");
assert.match(pageStyles, /min-height:\s*0;/, "滚动容器需允许被父级 flex 压到视口高度");
assert.match(pageStyles, /padding-bottom:\s*16px;/, "卡片底部需留出呼吸空间");

// ── 共用页头显隐 ─────────────────────────────────────────────────────────
// 回归背景：四个可编辑设置页内容顶部都已有「模拟器有效配置」小节标题，
// 共用页头（主标题 / 副标题 / 平台摘要 / 锁定徽标）重复且挤占首屏，四页一并隐藏；
// 关于页保留页头与锁定徽标，避免把版本信息页的状态提示一起删掉。
const headerTag = source.match(/<div[^>]*class="settings-header"[^>]*>/)?.[0] ?? "";
assert.match(
  headerTag,
  /v-if="!editableSection"/,
  "四个可编辑设置页（含 OSD 水印、网络配置）应隐藏共用页头，仅关于页保留",
);
assert.match(source, /class="lock-badge"/, "关于页的锁定状态徽标不得一并删除");
assert.match(source, /class="settings-section about-section"/, "关于页内容区仍须存在");

// ── OSD 预览与图层配置左右分栏 ───────────────────────────────────────────
// 回归背景：OSD 参数区原先是「预览在上、三个图层配置依次堆在预览底部」，
// 调字号 / 颜色时要反复上下滚动对照画面；现改为预览在左、配置在右同屏。
const osdSection = source.match(/<section v-else-if="activeKey === 'osd' && osdDraft"[\s\S]*?<\/section>/)?.[0] ?? "";
assert.ok(osdSection, "应存在 OSD 水印配置区块");
const layoutBlock = osdSection.match(/<div class="osd-layout">([\s\S]*?)<div class="osd-footnote">/)?.[1] ?? "";
assert.ok(layoutBlock, "预览与配置须由同一个分栏容器包裹");
const previewPaneAt = layoutBlock.indexOf('class="osd-pane osd-preview-pane"');
const configPaneAt = layoutBlock.indexOf('class="osd-pane osd-config-pane"');
assert.ok(previewPaneAt >= 0 && configPaneAt > previewPaneAt, "预览列在前、配置列在后，配置位于预览右侧");
const previewPane = layoutBlock.slice(previewPaneAt, configPaneAt);
assert.match(previewPane, /class="osd-preview"/, "左列须仍是预览画面");
const configPane = layoutBlock.slice(configPaneAt);
assert.match(configPane, /v-for="layer in osdLayers"/, "三个图层配置须整体落在右列");
assert.match(configPane, />图层配置</, "右列须有独立小标题与预览标题对齐");
assert.match(osdSection, /class="osd-scope"/, "生效范围说明保持在分栏之外、顶部全宽");
assert.match(osdSection, /class="osd-footnote"/, "底部状态提示保持在分栏之外");

const osdLayoutStyles = source.match(/^\.osd-layout \{([^}]*)\}/m)?.[1] ?? "";
assert.ok(osdLayoutStyles, "应存在 OSD 分栏样式");
assert.match(osdLayoutStyles, /display:\s*grid;/, "分栏须使用 grid");
assert.match(
  osdLayoutStyles,
  /grid-template-columns:\s*minmax\(0, 1\.1fr\) minmax\(300px, 1fr\);/,
  "须为两列，且配置列有 300px 下限避免字段栅格被压垮",
);
assert.match(osdLayoutStyles, /align-items:\s*start;/, "两列顶部对齐，不拉伸预览");
const osdNarrowRule = source.match(/@media \(max-width: 980px\) \{ \.osd-layout \{([^}]*)\}/)?.[1] ?? "";
assert.match(osdNarrowRule, /grid-template-columns:\s*1fr;/, "窄视口须回退为单列");
assert.ok(
  source.indexOf(".osd-layout { grid-template-columns: 1fr; }") > source.indexOf(".osd-layout { display: grid;"),
  "窄屏回退规则须排在基础两列规则之后，否则同特异性下会被覆盖",
);
