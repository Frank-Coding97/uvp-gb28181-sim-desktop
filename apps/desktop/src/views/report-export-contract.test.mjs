// 压测报告导出契约：报告必须是「人看得懂」的 HTML 报表，而不是纯 JSON。
//
// 这些断言锁住三件事：
// 1) 导出走「另存为」对话框，用户能自己选位置（不再默默丢在临时目录）；
// 2) 主产物是带图表的 HTML，JSON 只作为同目录附属文件；
// 3) 报告里的密码一律脱敏，任何分支都不能漏。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const scenario = await readFile(new URL("./Scenario.vue", import.meta.url), "utf8");
const backend = await readFile(
  new URL("../../src-tauri/src/lib.rs", import.meta.url),
  "utf8",
);
const report = await readFile(
  new URL("../../src-tauri/src/report.rs", import.meta.url),
  "utf8",
);

const exportFn = scenario.match(/async function exportReport\(\) \{([\s\S]*?)\n\}/)?.[1] ?? "";
assert.match(exportFn, /saveDialog\(/, "导出报告应弹出「另存为」让用户选路径");
assert.match(exportFn, /extensions: \["html"\]/, "报告默认扩展名应为 html");
assert.match(
  exportFn,
  /invoke<string>\("export_report", \{ targetPath: targetPath \?\? null \}\)/,
  "应把目标路径传给后端（未选路径时显式传 null）",
);
assert.match(exportFn, /if \(typeof picked !== "string"\) return;/, "用户取消时不应报错");
assert.doesNotMatch(exportFn, /message\.error\("undefined"\)/, "取消导出不应弹错误");

assert.match(backend, /async fn export_report\([\s\S]*?target_path: Option<String>/, "后端应接受可选目标路径");
assert.match(backend, /set_extension\("html"\)/, "未带扩展名时应补 .html");
assert.match(backend, /with_extension\("json"\)/, "同目录应落一份 JSON 原始数据");
assert.match(backend, /fn scenario_report_rows/, "场景参数应翻译成中文标签");
assert.match(backend, /已隐藏（报告不保存密码）/, "参数表不得出现密码");

// 渲染层：必须有图表、必须转义、必须给结论。
assert.match(report, /<svg/, "报告应内联 SVG 图表");
assert.match(report, /fn esc\(/, "注入文本必须转义");
assert.match(report, /fn verdict\(/, "报告应给出成功率评级");
assert.doesNotMatch(report, /https?:\/\//, "报告不得依赖外部资源，否则离线打不开");
