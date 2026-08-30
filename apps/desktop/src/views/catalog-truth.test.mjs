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
  assert.match(view, /最近平台查询/);
});

test("目录通过 Rust 真相源持久化并校验", () => {
  assert.match(backend, /replace_catalog_tree/);
  assert.match(backend, /get_catalog_activity/);
  assert.match(protocol, /validate_catalog_tree/);
  assert.match(protocol, /select_catalog_nodes/);
  assert.match(protocol, /BusinessGroup => "215"/);
  assert.match(protocol, /VirtualOrg => "216"/);
});
