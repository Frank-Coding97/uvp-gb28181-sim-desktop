import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, backend, protocol] = await Promise.all([
  readFile(new URL("./Channels.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/id_codec.rs", import.meta.url), "utf8"),
]);

test("目录管理提供搜索移动状态和 JSON 导入导出", () => {
  assert.match(view, /目录搜索/);
  assert.match(view, /移动节点/);
  assert.match(view, /切换状态/);
  assert.match(view, /导入 JSON/);
  assert.match(view, /导出 JSON/);
  assert.doesNotMatch(view, /最近平台查询/, "平台查询状态不再占用目录管理页顶部横条");
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
