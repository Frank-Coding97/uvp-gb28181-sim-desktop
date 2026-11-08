import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";
import {
  ALL_TYPES,
  CATALOG_TEMPLATES,
  TYPE_CODES,
  allowedChildTypes,
  attachCatalogNode,
  availableTemplates,
  buildTreeRows,
  canMove,
  collectDescendants,
  countByType,
  decodeCatalogJson,
  displayParent,
  encodeCatalogJson,
  filterTreeByQuery,
  genChildId,
  makeNode,
  moveCatalogNode,
  nextSeqId,
  validateDraft,
} from "./catalog-model.ts";

const CTX = {
  deviceId: "34020000001320000001",
  deviceName: "演示设备",
  channelName: "前置通道",
  domain: "3402000000",
  gbVersion: "V2022",
};

const byId = (list, id) => list.find((node) => node.id === id);

test("ID 生成与 Rust gen_child_id / 移动端 IdEncoder 同口径", () => {
  assert.equal(genChildId("3402000000", "VideoChannel", 1), "34020000001320000001");
  assert.equal(genChildId("3402000000", "BusinessGroup", 2), "34020000002150000002");
  assert.equal(genChildId("3402000000", "AlarmChannel", 7), "34020000001340000007");
  assert.equal(Object.keys(TYPE_CODES).length, 7);
  assert.equal(ALL_TYPES.length, 7);
});

test("模板集合：移动端 2 个 + 桌面端扩展 3 个，标准业务分组仅 2022", () => {
  assert.equal(CATALOG_TEMPLATES.length, 5);
  assert.deepEqual(availableTemplates("V2016").map((t) => t.id), ["single", "nvr-8ch", "civil-3x2", "large-16ch"]);
  assert.deepEqual(availableTemplates("V2022").map((t) => t.id), [
    "single", "standard-business-group", "nvr-8ch", "civil-3x2", "large-16ch",
  ]);
  assert.equal(availableTemplates("V2022").filter((t) => t.origin === "mobile").length, 2);
});

test("5 个模板全部能通过保存预检，且节点 ID 无重复", () => {
  for (const tpl of CATALOG_TEMPLATES) {
    const nodes = tpl.build(CTX);
    assert.deepEqual(validateDraft(nodes), [], `${tpl.id} 模板应通过校验`);
    assert.equal(new Set(nodes.map((n) => n.id)).size, nodes.length, `${tpl.id} 不应有重复 ID`);
  }
  assert.equal(byId(CATALOG_TEMPLATES, "single").build(CTX).length, 3);
  assert.equal(byId(CATALOG_TEMPLATES, "nvr-8ch").build(CTX).length, 10);
  assert.equal(byId(CATALOG_TEMPLATES, "large-16ch").build(CTX).length, 20);
  const standard = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  assert.equal(standard[0].node_type, "System");
  assert.equal(standard[0].id.slice(10, 13), "200");
  assert.equal(countByType(standard, "VirtualOrg"), 2);
  assert.ok(standard.filter((n) => n.node_type === "VirtualOrg").every((n) => n.business_group_id));
});

test("新增子节点类型按父节点限制，与移动端 AddChildDialog 一致", () => {
  assert.deepEqual(allowedChildTypes(makeNode("r", "System", "系统", "r")), ["BusinessGroup", "VideoChannel", "AlarmChannel"]);
  assert.deepEqual(allowedChildTypes(makeNode("g", "BusinessGroup", "分组", "r")), ["VirtualOrg"]);
  assert.deepEqual(allowedChildTypes(makeNode("o", "VirtualOrg", "组织", "g")), ["VirtualOrg", "VideoChannel", "AlarmChannel"]);
  assert.deepEqual(allowedChildTypes(null), ["VideoChannel", "AlarmChannel"]);
});

test("canMove：根不可动、叶子不可作父、不可移入自身子树、按类型限制目标", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  const [system, group, firstOrg, nestedOrg, ch1] = tree;
  assert.equal(canMove(system, group.id, tree), false, "System 根不可移动");
  assert.equal(canMove(group, system.id, tree), true, "215 可挂 200");
  assert.equal(canMove(group, firstOrg.id, tree), false, "215 不可挂 216");
  assert.equal(canMove(firstOrg, nestedOrg.id, tree), false, "不可移入自身子树");
  assert.equal(canMove(ch1, firstOrg.id, tree), true, "通道可挂虚拟组织");
  assert.equal(canMove(ch1, nestedOrg.id, tree), true);
  assert.equal(canMove(ch1, group.id, tree), false, "通道不可挂业务分组");
  assert.equal(canMove(ch1, ch1.id, tree), false, "不可移到自身");
  assert.equal(canMove(group, ch1.id, tree), false, "叶子不可作父");
});

test("displayParent / collectDescendants 处理移动端空父与复合父", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  assert.equal(displayParent(tree[0], tree), null, "根节点无展示父");
  assert.equal(displayParent(tree[2], tree), tree[1].id, "首层 216 落到 215");
  const composite = { ...tree[4], parent_id: `${tree[0].id}/${tree[2].id}` };
  assert.equal(displayParent(composite, [...tree.filter((n) => n.id !== tree[4].id), composite]), tree[2].id);
  const descendants = collectDescendants(tree[2].id, tree);
  assert.ok(descendants.has(tree[3].id));
  assert.ok(descendants.has(tree[4].id));
  assert.ok(!descendants.has(tree[2].id));
  const emptyParent = { ...tree[2], parent_id: "", business_group_id: tree[1].id };
  assert.equal(displayParent(emptyParent, tree), tree[1].id, "空 parent 时按 BusinessGroupID 归组");
});

test("移动虚拟组织会同步后代的 BusinessGroupID；移动通道清除分组", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  const [, group, firstOrg] = tree;
  const anotherGroup = makeNode(genChildId(CTX.domain, "BusinessGroup", 9), "BusinessGroup", "新分组", tree[0].id);
  const withGroup = [...tree, anotherGroup];
  const moved = moveCatalogNode(firstOrg, anotherGroup, withGroup);
  assert.equal(byId(moved, firstOrg.id).business_group_id, anotherGroup.id);
  assert.equal(byId(moved, tree[3].id).business_group_id, anotherGroup.id, "嵌套 216 跟随父组织");
  const chMoved = moveCatalogNode(tree[4], tree[3], tree);
  assert.equal(byId(chMoved, tree[4].id).parent_id, tree[3].id);
  assert.equal(byId(chMoved, tree[4].id).business_group_id, null);
  assert.ok(group.id);
});

test("attachCatalogNode 按父类型设置分组归属", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  const group = tree[1];
  const firstOrg = tree[2];
  const newOrg = makeNode(genChildId(CTX.domain, "VirtualOrg", 9), "VirtualOrg", "新组织", "");
  assert.equal(attachCatalogNode(newOrg, group).business_group_id, group.id);
  assert.equal(attachCatalogNode(newOrg, firstOrg).business_group_id, firstOrg.business_group_id);
  const newChannel = makeNode(genChildId(CTX.domain, "VideoChannel", 9), "VideoChannel", "新通道", "");
  assert.equal(attachCatalogNode(newChannel, firstOrg).parent_id, firstOrg.id);
  assert.equal(attachCatalogNode(newChannel, firstOrg).business_group_id, null);
});

test("nextSeqId 排除根设备占用的 132 编码，避免序号跳到七位数", () => {
  const tree = byId(CATALOG_TEMPLATES, "single").build(CTX);
  assert.equal(nextSeqId(CTX.domain, tree, "VideoChannel"), "34020000001320000003");
  assert.equal(nextSeqId(CTX.domain, tree, "AlarmChannel"), "34020000001340000002");
  assert.equal(nextSeqId(CTX.domain, tree, "BusinessGroup"), "34020000002150000001");
});

test("搜索命中子节点时保留完整祖先链", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  const hit = filterTreeByQuery(tree, "教学区域");
  assert.deepEqual(hit.map((n) => n.id), tree.slice(0, 4).map((n) => n.id), "命中嵌套节点应带回全链路祖先");
  assert.ok(filterTreeByQuery(tree, "216").length >= 2);
  assert.deepEqual(filterTreeByQuery(tree, "不存在的节点"), []);
  assert.equal(filterTreeByQuery(tree, "  ").length, tree.length);
  assert.equal(filterTreeByQuery(tree, CTX.domain.slice(0, 6)).length >= 1, true, "区划码可搜");
});

test("buildTreeRows 不丢节点，孤立节点提升为根", () => {
  const tree = byId(CATALOG_TEMPLATES, "single").build(CTX);
  const rows = buildTreeRows(tree);
  assert.equal(rows.length, 1);
  assert.equal(rows[0].children.length, 2);
  const orphan = makeNode("999", "VideoChannel", "孤儿", "missing");
  const withOrphan = buildTreeRows([...tree, orphan]);
  assert.equal(withOrphan.length, 2);
});

test("保存预检捕获空树、重复 ID、类型码不符、坏状态、悬空父、叶子作父", () => {
  const tree = byId(CATALOG_TEMPLATES, "single").build(CTX);
  assert.ok(validateDraft([]).some((e) => e.includes("不能为空")));
  assert.ok(validateDraft([...tree, { ...tree[1] }]).some((e) => e.includes("重复")));
  assert.ok(validateDraft([...tree, { ...tree[1], id: "34020000002150000009" }]).some((e) => e.includes("不匹配")));
  assert.ok(validateDraft([...tree, { ...tree[1], id: "34020000001320000099", status: "BAD" }]).some((e) => e.includes("ON 或 OFF")));
  assert.ok(validateDraft([...tree, makeNode("34020000001320000098", "VideoChannel", "悬空", "34020000009990000001")])
    .some((e) => e.includes("父节点不存在")));
  assert.ok(validateDraft([...tree, makeNode("34020000001320000097", "AlarmChannel", "挂叶子", tree[1].id)])
    .some((e) => e.includes("不能承载子节点")));
});

test("JSON 双格式：桌面端原生往返无损", () => {
  const tree = byId(CATALOG_TEMPLATES, "standard-business-group").build(CTX);
  const result = decodeCatalogJson(encodeCatalogJson(tree, "desktop"));
  assert.equal(result.flavor, "desktop");
  assert.deepEqual(result.nodes, tree);
});

test("JSON 双格式：移动端格式可导入并映射字段，扩展字段与复合父给出告警", () => {
  const mobile = [
    { id: "34020000002000000001", type: "System", name: "系统", parentId: "34020000002000000001", fields: { CivilCode: "340200" } },
    { id: "34020000002160000001", type: "VirtualOrg", name: "出入口", parentId: "", fields: { BusinessGroupID: "34020000002150000001", Status: "OFF", Manufacturer: "UVP", Model: "X1" } },
    { id: "34020000001320000001", type: "VideoChannel", name: "通道", parentId: "34020000002000000001/34020000002160000001", fields: { Status: "ON" } },
  ];
  const result = decodeCatalogJson(JSON.stringify(mobile));
  assert.equal(result.flavor, "mobile");
  assert.equal(result.nodes[0].civil_code, "340200");
  assert.equal(result.nodes[1].business_group_id, "34020000002150000001");
  assert.equal(result.nodes[1].status, "OFF");
  assert.equal(result.nodes[1].parent_id, "34020000002150000001", "移动端空父 216 显式挂到业务分组");
  assert.equal(result.nodes[2].parent_id, "34020000002160000001", "复合父取末段");
  assert.ok(result.warnings.some((w) => w.includes("扩展字段")));
  assert.ok(result.warnings.some((w) => w.includes("复合 ParentID")));
  assert.equal(encodeCatalogJson(result.nodes, "mobile").includes('"parentId"'), true);
  assert.equal(encodeCatalogJson(result.nodes, "desktop").includes('"node_type"'), true);
});

test("JSON 导入拒绝非法输入", () => {
  assert.throws(() => decodeCatalogJson("{oops"), /解析失败/);
  assert.throws(() => decodeCatalogJson("{}"), /根节点必须是数组/);
  assert.throws(() => decodeCatalogJson("[]"), /不能为空/);
  assert.throws(() => decodeCatalogJson('[{"type":"Nope","id":"1","name":"x"}]'), /type 未知/);
});

test("Channels.vue 承载移动端对齐后的关键能力", async () => {
  const view = await readFile(new URL("../Channels.vue", import.meta.url), "utf8");
  for (const keyword of [
    "目录搜索", "移动节点", "切换状态", "导入 JSON", "导出 JSON",
    "当前通道名称", "克隆节点", "预览 NOTIFY", "复位到默认", "未保存", "批量新增",
    "保存*", "放弃修改", "标准业务分组", "解析预览",
  ]) {
    assert.ok(view.includes(keyword), `目录管理应包含「${keyword}」`);
  }
  assert.doesNotMatch(view, /page-header|最近平台查询/, "目录管理页不再展示顶部标题横条与平台查询卡片");
});

test("草稿—保存模型：落盘点唯一、离开有保护、XML 预览命令已注册", async () => {
  const [view, lib, platform] = await Promise.all([
    readFile(new URL("../Channels.vue", import.meta.url), "utf8"),
    readFile(new URL("../../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
    readFile(new URL("../../platform.ts", import.meta.url), "utf8"),
  ]);
  assert.match(view, /onBeforeRouteLeave/, "未保存离开必须被路由守卫拦截");
  assert.match(view, /replace_catalog_tree/, "保存是唯一落盘点");
  assert.doesNotMatch(view, /upsert_channel|remove_channel|load_catalog_template/,
    "编辑期间不得逐条即时落盘（草稿模型）");
  assert.match(view, /previewCatalogXml/);
  assert.match(lib, /async fn preview_catalog_xml/, "后端需提供纯预览命令");
  assert.match(lib, /preview_catalog_xml,/, "命令必须注册到 invoke_handler");
  assert.match(platform, /previewCatalogXml/, "前端需有 IPC 封装");
});
