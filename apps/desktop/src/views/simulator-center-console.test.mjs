import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./SimulatorCenter.vue", import.meta.url), "utf8");
const script = source.slice(0, source.indexOf("</script>"));
const aside = source.match(/<aside class="glass-card console">([\s\S]*?)\n      <\/aside>/)?.[1] ?? "";
const styles = source.slice(source.indexOf(".console { display: flex;"));

assert.ok(aside, "模拟器中心右侧控制台应存在");
assert.match(aside, /<PtzPad @adjust="onPad" \/>/, "云台手操必须常驻，不随回显分组切换");
assert.doesNotMatch(aside, /activeTab === 'ptz'|key: "ptz"/, "云台不应再作为页签存在");
assert.match(aside, /role="tablist" aria-label="平台回显分组"/, "回显分组需具备 tablist 语义");
assert.match(aside, /:aria-selected="detailTab === tab.key"/, "分组按钮需带 aria-selected");
assert.match(aside, /aria-controls/, "分组按钮需关联对应的 tabpanel");
assert.match(aside, /role="tabpanel"/, "回显内容需具备 tabpanel 语义");
assert.match(aside, /@click="callPreset\(preset\)"/, "预置位应可本机调用");
assert.match(script, /sim\.seekToPreset\(preset\.id, preset\.name, \{/, "预置位调用必须复用 store 的姿态积分");
assert.match(
  script,
  /if \(!target \|\| target === "ptz" \|\| target === detailTab\.value\) return;/,
  "平台云台命令不得切走手操区",
);
assert.doesNotMatch(source, /class="headline"/, "页头不应重复显示 headline");
assert.doesNotMatch(source, /\.headline\s*\{/, "页头 headline 样式应一并移除");
assert.match(source, /<PtzCamera3D/, "左侧 3D 舞台必须保留");
assert.match(source, /class="stage-foot"/, "左侧舞台底部命令条必须保留");
assert.doesNotMatch(styles, /font-size:\s*[89]px/, "控制台不应再出现 8~9px 字号");
assert.match(styles, /:deep\(\.hold-key\) \{ min-width: 38px; height: 34px;/, "按键命中区域需放大");
assert.match(styles, /:focus-visible/, "交互控件需保留键盘焦点环");

// ── 非全屏 / 矮窗口适配边界 ───────────────────────────────────────────────
// 回归背景：父级 .content 是 overflow:hidden，页面原先被 minmax(0,1fr) 硬压进一屏，
// 1280×720 / 1366×768 下平台回显卡片全被裁掉；1024×768 改成单列后只定义了一行，
// 控制台落在隐式行里同样被裁。以下断言锁住修复口径。
assert.match(source, /\.sim-page \{[^}]*overflow-y: auto/, "视口不足时页面必须可整页滚动，不能裁切内容");
assert.match(source, /\.sim-layout \{[^}]*grid-template-rows: auto;/, "行高需由内容撑开，不得再压回 minmax(0,1fr)");
assert.match(source, /grid-template-columns: minmax\(0, 1fr\) clamp\(440px, 34%, 520px\)/, "右栏需保底 440px 操作宽度");
assert.doesNotMatch(styles, /\.console \{[^}]*height: 100%/, "控制台不应再被固定行高约束");
assert.match(styles, /\.feedback-body \{[^}]*max-height: clamp\(/, "回显滚动体需有界，避免无限撑高整页");
assert.match(styles, /@media \(max-width: 1180px\)[\s\S]*?grid-template-rows: auto auto;/, "单列布局必须显式定义两行");
assert.match(styles, /@media \(max-height: 820px\) and \(min-width: 1181px\)/, "矮窗口需有独立紧凑档");
const compact = styles.slice(styles.indexOf("@media (max-height: 820px)"));
assert.doesNotMatch(compact, /font-size|min-width: 3[0-7]px|height: 3[0-3]px/, "紧凑档只收紧留白，不缩字号与按键命中区域");
const stageCss = source.slice(source.indexOf(".stage-card {"), source.indexOf(".console { display: flex;"));
assert.match(stageCss, /min-height: 648px/, "左侧 3D 舞台基准高度保持不动");
