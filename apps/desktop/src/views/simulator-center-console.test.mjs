import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./SimulatorCenter.vue", import.meta.url), "utf8");
const script = source.slice(0, source.indexOf("</script>"));
const aside = source.match(/<aside class="glass-card console">([\s\S]*?)\n      <\/aside>/)?.[1] ?? "";
const styles = source.slice(source.indexOf(".console { display: flex;"));
const panel = aside.slice(aside.indexOf('id="console-panel"'));

assert.ok(aside, "模拟器中心右侧控制台应存在");

// ── 信息架构：四个一级分区互斥，一次只呈现一个 ────────────────────────────
// 回归背景：上一版改成「状态总览 + 云台手操 + 平台回显」三段常驻，同屏铺开三层
// 却没有单一焦点，用户反馈不如旧的四页分区清晰。这里锁住分区导航口径。
assert.match(script, /const tabs: Array<\{ key: SimTab; label: string; hint: string \}> = \[/, "需保留四页分区定义");
for (const key of ["ptz", "position", "image", "device"]) {
  assert.match(script, new RegExp(`\\{ key: "${key}", label: `), `分区 ${key} 必须存在`);
}
assert.match(aside, /role="tablist" aria-label="模拟器控制台分区"/, "四个页签需具备 tablist 语义");
assert.match(aside, /:aria-selected="activeTab === tab.key"/, "页签需带 aria-selected");
assert.match(aside, /aria-controls="console-panel"/, "页签需关联唯一的 tabpanel");
assert.match(aside, /role="tabpanel"/, "单页内容需具备 tabpanel 语义");
assert.match(aside, /@click="selectTab\(tab\.key\)"/, "页签点击需切换分区");
assert.match(aside, /class="panel-head"[\s\S]*?activeTabMeta\.label/, "单页顶部需明确显示当前分区名");
assert.match(panel, /v-if="activeTab === 'ptz'"/, "云台页需作为互斥分支");
assert.match(panel, /v-else-if="activeTab === 'position'"/, "定位页需作为互斥分支");
assert.match(panel, /v-else-if="activeTab === 'image'"/, "图像页需作为互斥分支");
assert.match(panel, /<template v-else>/, "设备页需作为兜底分支");
assert.match(panel, /<PtzPad @adjust="onPad" \/>/, "云台页需保留手操方盘");
assert.ok(aside.indexOf('class="console-status"') < aside.indexOf('id="console-panel"'), "全局状态条必须常驻在单页内容之外");

// ── 平台命令自动切页写死开启 ─────────────────────────────────────────────
assert.match(script, /const autoFollow = ref\(true\)/, "自动切页写死为勾选，等价于复选框永久开启");
assert.match(script, /if \(!target \|\| target === activeTab\.value\) return;/, "同分区命令无需任何跳转或角标");
assert.match(script, /if \(autoFollow\.value\) selectTab\(target\);/, "平台命令到来时直接切到目标分区");
assert.match(script, /tabBadges\.value = \[\.\.\.tabBadges\.value, target\]/, "角标分支保留，便于日后恢复开关");
assert.match(aside, /tabBadges\.includes\(tab\.key\)/, "页签角标渲染保留");

// ── 顶部页头条整体移除 ───────────────────────────────────────────────────
// 回归背景：页头里的「平台命令自动切页」复选框属于多余决策点，改为写死勾选后
// 整条页头（标题 / 副标题 / 复选框 / 设备状态胶囊）一并删除，避免留下空行。
assert.doesNotMatch(source, /class="page-header"/, "模拟器中心不再保留独立页头行");
assert.doesNotMatch(source, /follow-toggle|<input v-model="autoFollow"/, "自动切页复选框不应再暴露给用户");
assert.doesNotMatch(source, /class="device-pill"/, "设备状态胶囊随页头一并移除");

// ── 保留下来的功能与视觉改进 ─────────────────────────────────────────────
assert.match(aside, /@click="callPreset\(preset\)"/, "预置位仍应可本机调用");
assert.match(script, /sim\.seekToPreset\(preset\.id, preset\.name, \{/, "预置位调用必须复用 store 的姿态积分");
assert.doesNotMatch(source, /class="headline"/, "页头不应重复显示 headline");
assert.doesNotMatch(source, /\.headline\s*\{/, "页头 headline 样式应一并移除");
assert.match(source, /<PtzCamera3D/, "左侧 3D 舞台必须保留");
assert.match(source, /class="stage-foot"/, "左侧舞台底部命令条必须保留");
assert.doesNotMatch(styles, /font-size:\s*[89]px/, "控制台不应再出现 8~9px 字号");
assert.match(styles, /:deep\(\.hold-key\) \{ min-width: 38px; height: 34px;/, "按键命中区域需保持 34px");
assert.match(styles, /:focus-visible/, "交互控件需保留键盘焦点环");

// ── 非全屏 / 矮窗口适配边界 ───────────────────────────────────────────────
// 回归背景：父级 .content 是 overflow:hidden，页面原先被 minmax(0,1fr) 硬压进一屏，
// 1280×720 / 1366×768 下内容全被裁掉；1024×768 改成单列后只定义了一行，
// 控制台落在隐式行里同样被裁。以下断言锁住修复口径。
assert.match(source, /\.sim-page \{[^}]*overflow-y: auto/, "视口不足时页面必须可整页滚动，不能裁切内容");
assert.match(source, /\.sim-layout \{[^}]*grid-template-rows: auto;/, "行高需由内容撑开，不得再压回 minmax(0,1fr)");
assert.match(source, /grid-template-columns: minmax\(0, 1fr\) clamp\(440px, 34%, 520px\)/, "右栏需保底 440px 操作宽度");
assert.doesNotMatch(styles, /\.console \{[^}]*height: 100%/, "控制台不应再被固定行高约束");
assert.match(styles, /\.console-body \{[^}]*overflow-y: auto/, "单页内容需保留滚动兜底");
assert.match(styles, /\.console-body \{[^}]*max-height: clamp\(320px, 52vh, 560px\)/, "单页滚动区需有界，避免撑高左栏切掉舞台底部命令条");
assert.match(styles, /@media \(max-width: 1180px\)[\s\S]*?grid-template-rows: auto auto;/, "单列布局必须显式定义两行");
assert.match(styles, /@media \(max-width: 1180px\)[\s\S]*?\.console-body \{ grid-template-columns: repeat\(2/, "窄窗口单页卡片需两列铺开");
assert.match(styles, /@media \(max-height: 820px\) and \(min-width: 1181px\)/, "矮窗口需有独立紧凑档");
const compact = styles.slice(styles.indexOf("@media (max-height: 820px)"));
assert.doesNotMatch(compact, /font-size|min-width: 3[0-7]px|height: 3[0-3]px/, "紧凑档只收紧留白，不缩字号与按键命中区域");
const stageCss = source.slice(source.indexOf(".stage-card {"), source.indexOf(".console { display: flex;"));
assert.match(stageCss, /min-height: 648px/, "左侧 3D 舞台基准高度保持不动");
