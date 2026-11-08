import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, backend, protocol] = await Promise.all([
  readFile(new URL("./Channels.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/id_codec.rs", import.meta.url), "utf8"),
]);

test("目录管理提供搜索移动状态和 JSON 导入导出", () => {
  assert.match(view, /搜索名称/);
  assert.match(view, /移动节点/);
  assert.match(view, /切换状态/);
  assert.match(view, /导入 JSON/);
  assert.match(view, /导出 JSON/);
  assert.doesNotMatch(view, /最近平台查询/, "平台查询状态不再占用目录管理页顶部横条");
});

test("操作按钮按语义分配互不相同的主题色", () => {
  const palette = view.match(/const ACTION_PALETTE = \{([\s\S]*?)\} as const;/)?.[1] ?? "";
  const colors = [...palette.matchAll(/#[0-9a-fA-F]{6}/g)].map((match) => match[0]);
  assert.equal(colors.length, 5, "五个操作色应集中定义在 ACTION_PALETTE");
  assert.equal(new Set(colors).size, colors.length, "操作色不得重复");
  assert.match(view, /secondary: true, color, disabled/, "彩色按钮应使用淡彩底而非实心色块");
  assert.match(view, /type: "error", disabled: isRoot\(row\)/, "删除保留危险色");
});

test("工具栏七个动作各有互不相同的语义色，保存保留实心主色", () => {
  const palette = view.match(/const TOOLBAR_PALETTE = \{([\s\S]*?)\} as const;/)?.[1] ?? "";
  const colors = [...palette.matchAll(/#[0-9a-fA-F]{6}/g)].map((match) => match[0]);
  assert.equal(colors.length, 7, "刷新/新增/模板/导入/导出/预览/复位 七个动作各一色");
  assert.equal(new Set(colors).size, colors.length, "工具栏颜色不得重复");
  for (const key of ["refresh", "add", "template", "importJson", "exportJson", "preview", "reset"]) {
    assert.match(view, new RegExp(`:color="TOOLBAR_PALETTE\\.${key}"`), `工具栏按钮应绑定 ${key} 色`);
  }
  // 保存是唯一落盘动作，保持实心主色以维持视觉权重；其余一律淡彩底。
  assert.match(view, /<n-button type="primary" :loading="busy" :disabled="!isDirty" @click="saveDraft">/);
  const tinted = [...view.matchAll(/<n-button secondary type="primary" :color="TOOLBAR_PALETTE\./g)];
  assert.equal(tinted.length, 7, "七个次要动作应统一走淡彩底");
  assert.match(palette, /reset: "#e5484d"/, "复位默认是破坏性动作，取危险色");
});

test("固定列遮罩不透明，横向滚动时状态列不会透出遮挡操作按钮", () => {
  assert.match(view, /title: "状态", key: "status", width: 88/, "状态列需留足在线/离线标签宽度");
  assert.match(view, /title: "操作", key: "actions", width: 460, fixed: "right"/, "操作列需容纳六个按钮且固定在右侧");
  assert.match(view, /n-data-table-th--fixed-right\) \{ background-color: #eef5fd !important; \}/, "固定列表头必须不透明");
  assert.match(view, /n-data-table-td--fixed-right[\s\S]{0,200}background-color: #f5f9fd !important;/, "固定列单元格必须不透明");
  assert.match(view, /n-data-table-tr:hover \.n-data-table-td--fixed-right/, "hover 态同样要保留遮罩");
});

test("目录通过 Rust 真相源持久化并校验", () => {
  assert.match(backend, /replace_catalog_tree/);
  assert.match(backend, /get_catalog_activity/);
  assert.match(protocol, /validate_catalog_tree/);
  assert.match(protocol, /select_catalog_nodes/);
  assert.match(protocol, /BusinessGroup => "215"/);
  assert.match(protocol, /VirtualOrg => "216"/);
});
