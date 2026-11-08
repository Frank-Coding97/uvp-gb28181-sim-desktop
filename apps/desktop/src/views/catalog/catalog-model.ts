// 目录管理纯逻辑层：与渲染无关，便于用 node:test 逐条机检。
//
// 口径来源：
// - 移动端 uvp-gb28181-sim 的 CatalogTreeStore / CatalogMoveDialog / AddChildDialog /
//   CatalogSearchBar / CatalogJsonDialogs（交互与约束）；
// - 桌面端 crates/gb28181-protocol/src/id_codec.rs（ID 规则、模板、保存校验）。
//
// 边界说明：桌面端 Rust 的 CatalogNode 是固定 7 字段结构（不是移动端的 fields Map），
// 协议层与 SIP 报文组装都依赖它，本次重做**不改数据模型**。因此：
// - 扩展字段（Manufacturer/Model/Owner/Address 等）不落盘，JSON 互换时显式告警；
// - 移动端用 "/" 表达的复合 ParentID 在桌面端不可表达，导入时取末段；
// - 保存校验以 Rust validate_catalog_tree 为准，前端做同口径预检；
// - 移动端的父子类型白名单用于「新增/移动」候选过滤，不作为保存硬约束，
//   以保留桌面端既有的 nvr-8ch / large-16ch 等模板（通道挂 215 分组）；
// - 例外：桌面端默认目录根是 设备(111)，若沿用移动端「设备 → 仅通道」的白名单，
//   用户必须先应用「标准业务分组」模板才能建 215/216，入口过深。移动端数据层
//   （CatalogTreeStore.isAllowedParent）本就允许 215 挂 Device/System、216 挂
//   Device/System/215/216，Rust validate_catalog_tree 也不校验父子类型，
//   因此设备根下放开 215/216，与 canMove 的候选口径保持一致。

import type { CatalogNodeConfig } from "../../platform";

export type CatalogNodeType = CatalogNodeConfig["node_type"];
export type CatalogNode = CatalogNodeConfig;

/** 国标 20 位编码第 11-13 位类型码；与移动端 CatalogNodeType / Rust id_codec 一致。 */
export const TYPE_CODES: Record<CatalogNodeType, string> = {
  AdministrativeRegion: "",
  System: "200",
  Device: "111",
  BusinessGroup: "215",
  VirtualOrg: "216",
  VideoChannel: "132",
  AlarmChannel: "134",
};

export const TYPE_LABELS: Record<CatalogNodeType, string> = {
  AdministrativeRegion: "行政区划",
  System: "联网系统（200）",
  Device: "设备根（111）",
  BusinessGroup: "业务分组（215）",
  VirtualOrg: "虚拟组织（216）",
  VideoChannel: "视频通道（132）",
  AlarmChannel: "报警通道（134）",
};

export const ALL_TYPES = Object.keys(TYPE_CODES) as CatalogNodeType[];
export const LEAF_TYPES = new Set<CatalogNodeType>(["VideoChannel", "AlarmChannel"]);
export const CONTAINER_TYPES = new Set<CatalogNodeType>(
  ALL_TYPES.filter((type) => !LEAF_TYPES.has(type)),
);

export function isLeaf(node: CatalogNode): boolean {
  return LEAF_TYPES.has(node.node_type);
}

export function isRoot(node: CatalogNode): boolean {
  return node.id === node.parent_id;
}

export function makeNode(
  id: string,
  nodeType: CatalogNodeType,
  name: string,
  parentId: string,
  extra: Partial<Pick<CatalogNode, "civil_code" | "business_group_id" | "status">> = {},
): CatalogNode {
  return {
    id,
    node_type: nodeType,
    name,
    parent_id: parentId,
    civil_code: null,
    business_group_id: null,
    status: "ON",
    ...extra,
  };
}

/** domain 前 10 位 + 类型码(3) + 序号(7)；与 Rust gen_child_id / 移动端 IdEncoder 一致。 */
export function genChildId(domain: string, nodeType: CatalogNodeType, seq: number): string {
  const prefix = domain.slice(0, 10).padEnd(10, "0");
  return `${prefix}${TYPE_CODES[nodeType]}${String(Math.max(1, Math.trunc(seq))).padStart(7, "0")}`;
}

// ── 模板 ────────────────────────────────────────────────────────────────

/** 视频通道默认名前缀：Uvp-Camera-序号。 */
export const CHANNEL_NAME_PREFIX = "Uvp-Camera";

/** 解析 `Uvp-Camera-N` 形式的序号；不是该前缀返回 null。 */
function channelSeqOf(name: string, prefix: string): number | null {
  const trimmed = name.trim();
  const head = `${prefix}-`;
  if (!trimmed.startsWith(head)) return null;
  const tail = trimmed.slice(head.length);
  return /^\d+$/.test(tail) ? Number(tail) : null;
}

/** 下一个可用通道序号：树内 VideoChannel 节点中同前缀 `Uvp-Camera-N` 的最大 N + 1。
 * 只扫描 VideoChannel，避免被重命名的业务分组/虚拟组织节点影响序号。 */
export function nextChannelSeq(tree: CatalogNode[], prefix = CHANNEL_NAME_PREFIX): number {
  let max = 0;
  for (const node of tree) {
    if (node.node_type !== "VideoChannel") continue;
    const seq = channelSeqOf(node.name, prefix);
    if (seq !== null) max = Math.max(max, seq);
  }
  return max + 1;
}

/** 新增视频通道的默认名，如 Uvp-Camera-3。 */
export function defaultChannelName(tree: CatalogNode[], prefix = CHANNEL_NAME_PREFIX): string {
  return `${prefix}-${nextChannelSeq(tree, prefix)}`;
}

export interface TemplateContext {
  deviceId: string;
  deviceName: string;
  channelName: string;
  /** 平台国标域，取 active_profile.server_domain。 */
  domain: string;
  gbVersion: "V2016" | "V2022";
}

export interface CatalogTemplateDef {
  id: string;
  title: string;
  description: string;
  /** mobile = 移动端同款；desktop = 桌面端保留的扩展模板。 */
  origin: "mobile" | "desktop";
  build: (ctx: TemplateContext) => CatalogNode[];
}

/** 根设备编码可能占用某个类型码的序号 1，生成子节点时整体跳过，避免 ID 冲突。 */
function templateSeq(ctx: TemplateContext, type: CatalogNodeType, seq: number): string {
  const shift = genChildId(ctx.domain, type, 1) === ctx.deviceId ? 1 : 0;
  return genChildId(ctx.domain, type, seq + shift);
}

function singleTemplate(ctx: TemplateContext): CatalogNode[] {
  const root = ctx.deviceId;
  return [
    makeNode(root, "Device", ctx.deviceName, root),
    makeNode(templateSeq(ctx, "VideoChannel", 1), "VideoChannel", ctx.channelName || `${CHANNEL_NAME_PREFIX}-1`, root),
    makeNode(templateSeq(ctx, "AlarmChannel", 1), "AlarmChannel", `${ctx.deviceName}-报警`, root),
  ];
}

function nvr8Template(ctx: TemplateContext): CatalogNode[] {
  const root = ctx.deviceId;
  const group = templateSeq(ctx, "BusinessGroup", 1);
  const org = templateSeq(ctx, "VirtualOrg", 1);
  const nodes: CatalogNode[] = [
    makeNode(root, "Device", ctx.deviceName, root),
    makeNode(group, "BusinessGroup", "NVR-8", root),
    makeNode(org, "VirtualOrg", "默认虚拟目录", group, { business_group_id: group }),
  ];
  for (let i = 1; i <= 8; i += 1) {
    nodes.push(makeNode(templateSeq(ctx, "VideoChannel", i), "VideoChannel", `${CHANNEL_NAME_PREFIX}-${i}`, org));
  }
  return nodes;
}

function civil3x2Template(ctx: TemplateContext): CatalogNode[] {
  const root = ctx.deviceId;
  const areas: Array<[string, string]> = [["浦东", "310115"], ["黄浦", "310101"], ["徐汇", "310104"]];
  const nodes: CatalogNode[] = [makeNode(root, "Device", ctx.deviceName, root)];
  let channelSeq = 1;
  areas.forEach(([area, civil], index) => {
    const org = templateSeq(ctx, "VirtualOrg", index + 1);
    nodes.push(makeNode(org, "VirtualOrg", `${area}区`, root, { civil_code: civil }));
    for (let k = 1; k <= 2; k += 1) {
      nodes.push(makeNode(
        templateSeq(ctx, "VideoChannel", channelSeq),
        "VideoChannel",
        `${CHANNEL_NAME_PREFIX}-${channelSeq}`,
        org,
        { civil_code: civil },
      ));
      channelSeq += 1;
    }
  });
  return nodes;
}

function large16Template(ctx: TemplateContext): CatalogNode[] {
  const root = ctx.deviceId;
  const indoor = templateSeq(ctx, "BusinessGroup", 1);
  const outdoor = templateSeq(ctx, "BusinessGroup", 2);
  const indoorOrg = templateSeq(ctx, "VirtualOrg", 1);
  const outdoorOrg = templateSeq(ctx, "VirtualOrg", 2);
  const nodes: CatalogNode[] = [
    makeNode(root, "Device", ctx.deviceName, root),
    makeNode(indoor, "BusinessGroup", "室内监控", root),
    makeNode(outdoor, "BusinessGroup", "室外监控", root),
    makeNode(indoorOrg, "VirtualOrg", "室内虚拟目录", indoor, { business_group_id: indoor }),
    makeNode(outdoorOrg, "VirtualOrg", "室外虚拟目录", outdoor, { business_group_id: outdoor }),
  ];
  let channelSeq = 1;
  for (let i = 1; i <= 8; i += 1) {
    nodes.push(makeNode(templateSeq(ctx, "VideoChannel", channelSeq), "VideoChannel", `${CHANNEL_NAME_PREFIX}-${channelSeq}`, indoorOrg));
    channelSeq += 1;
  }
  for (let i = 1; i <= 8; i += 1) {
    nodes.push(makeNode(templateSeq(ctx, "VideoChannel", channelSeq), "VideoChannel", `${CHANNEL_NAME_PREFIX}-${channelSeq}`, outdoorOrg));
    channelSeq += 1;
  }
  nodes.push(makeNode(templateSeq(ctx, "AlarmChannel", 1), "AlarmChannel", "总报警", root));
  return nodes;
}

/**
 * 从基础通道名派生第 seq 个通道名，与 `confirmAdd` 的前缀提取逻辑保持一致。
 * - "Uvp-Camera-1" → seq=2 → "Uvp-Camera-2"
 * - "FrontDoor"    → seq=2 → "FrontDoor-2"
 * - "Gate-1"       → seq=2 → "Gate-2"
 */
function deriveChannelName(base: string, seq: number): string {
  const m = /^(.*?)-(\d+)$/.exec(base);
  return m ? `${m[1]}-${seq}` : (seq === 1 ? base : `${base}-${seq}`);
}

/**
 * GB/T 28181-2022 附录 J 标准业务分组样例（移动端 standard-business-group 同款）。
 *
 * 移动端首层 216 不写 ParentID、只用 BusinessGroupID 归组，且通道用 "system/org" 复合父；
 * 桌面端协议层不表达这两种形式，因此这里做等价映射：
 * 216 的 parent_id 指向 215（或直接父组织），通道 parent_id 指向所属 216。
 */
function standardBusinessGroupTemplate(ctx: TemplateContext): CatalogNode[] {
  const deviceId = ctx.deviceId;
  const systemId = deviceId.length === 20
    ? `${deviceId.slice(0, 10)}200${deviceId.slice(13)}`
    : `${ctx.domain.slice(0, 10).padEnd(10, "0")}2000000001`;
  const civilCode = ctx.domain.slice(0, 6);
  const groupId = templateSeq(ctx, "BusinessGroup", 1);
  const firstOrg = templateSeq(ctx, "VirtualOrg", 1);
  const nestedOrg = templateSeq(ctx, "VirtualOrg", 2);
  return [
    makeNode(systemId, "System", `${ctx.deviceName}-下级平台`, systemId, { civil_code: civilCode }),
    makeNode(groupId, "BusinessGroup", "校园安防", systemId, { civil_code: civilCode }),
    makeNode(firstOrg, "VirtualOrg", "出入口", groupId, { business_group_id: groupId }),
    makeNode(nestedOrg, "VirtualOrg", "教学区域", firstOrg, { business_group_id: groupId }),
    makeNode(templateSeq(ctx, "VideoChannel", 1), "VideoChannel", deriveChannelName(ctx.channelName || `${CHANNEL_NAME_PREFIX}-1`, 1), firstOrg),
    makeNode(templateSeq(ctx, "VideoChannel", 2), "VideoChannel", deriveChannelName(ctx.channelName || `${CHANNEL_NAME_PREFIX}-1`, 2), nestedOrg),
  ];
}

/**
 * 下级平台业务目录（215/216）：与 Rust `id_codec::catalog_template("business-platform")` 同款结构。
 *
 * 设备根 → 业务分组(215) → 两个顶层虚拟组织(216) + 一个嵌在「教学楼」下的二级虚拟组织(216)，
 * 三条通道分别归属各自虚拟组织。用于联调业务目录解析与「通道按归属过滤」。
 */
function businessPlatformTemplate(ctx: TemplateContext): CatalogNode[] {
  const root = ctx.deviceId;
  const group = templateSeq(ctx, "BusinessGroup", 1);
  const teaching = templateSeq(ctx, "VirtualOrg", 1);
  const dormitory = templateSeq(ctx, "VirtualOrg", 2);
  const classroom = templateSeq(ctx, "VirtualOrg", 3);
  const orgAttrs = { business_group_id: group };
  return [
    makeNode(root, "Device", ctx.deviceName, root),
    makeNode(group, "BusinessGroup", "校园安防", root),
    makeNode(teaching, "VirtualOrg", "教学楼", group, orgAttrs),
    makeNode(dormitory, "VirtualOrg", "宿舍楼", group, orgAttrs),
    makeNode(classroom, "VirtualOrg", "教学楼一层", teaching, orgAttrs),
    makeNode(templateSeq(ctx, "VideoChannel", 1), "VideoChannel", `${CHANNEL_NAME_PREFIX}-1`, teaching),
    makeNode(templateSeq(ctx, "VideoChannel", 2), "VideoChannel", `${CHANNEL_NAME_PREFIX}-2`, dormitory),
    makeNode(templateSeq(ctx, "VideoChannel", 3), "VideoChannel", `${CHANNEL_NAME_PREFIX}-3`, classroom),
  ];
}

export const CATALOG_TEMPLATES: CatalogTemplateDef[] = [
  {
    id: "single",
    title: "单设备（默认）",
    description: "1 设备 + 1 视频通道 + 1 报警通道，最简结构。移动端同款。",
    origin: "mobile",
    build: singleTemplate,
  },
  {
    id: "standard-business-group",
    title: "标准业务分组（下级平台）",
    description: "系统(200) → 业务分组(215) → 虚拟组织(216) → 通道；仅 GB/T 28181-2022 可用。移动端同款。",
    origin: "mobile",
    build: standardBusinessGroupTemplate,
  },
  {
    id: "nvr-8ch",
    title: "8 通道 NVR",
    description: "设备根 + NVR 业务分组 + 8 个视频通道。桌面端扩展。",
    origin: "desktop",
    build: nvr8Template,
  },
  {
    id: "civil-3x2",
    title: "跨区划演示（3 区 × 2 通道）",
    description: "3 个虚拟组织各带 CivilCode，演示行政区划查询。桌面端扩展。",
    origin: "desktop",
    build: civil3x2Template,
  },
  {
    id: "large-16ch",
    title: "16 通道大型监控",
    description: "室内/室外两个业务分组各 8 通道 + 1 报警通道。桌面端扩展。",
    origin: "desktop",
    build: large16Template,
  },
  {
    id: "business-platform",
    title: "下级平台业务目录（215/216）",
    description: "业务分组 + 嵌套虚拟组织 + 3 通道，用于联调目录层级与通道归属。桌面端扩展。",
    origin: "desktop",
    build: businessPlatformTemplate,
  },
];

export function availableTemplates(gbVersion: TemplateContext["gbVersion"]): CatalogTemplateDef[] {
  return CATALOG_TEMPLATES.filter(
    (tpl) => tpl.id !== "standard-business-group" || gbVersion === "V2022",
  );
}

// ── 树关系 ──────────────────────────────────────────────────────────────

/**
 * 编辑器展示父关系。移动端首层 216 允许空 parentId + BusinessGroupID 归组；
 * 复合 ParentID（"a/b"）优先取虚拟组织那一段。
 */
export function displayParent(node: CatalogNode, tree: CatalogNode[]): string | null {
  if (node.parent_id === node.id) return null;
  if (node.node_type === "VirtualOrg" && !node.parent_id.trim()) {
    return node.business_group_id || null;
  }
  const parts = node.parent_id.split("/").map((item) => item.trim()).filter((item) => item && item !== node.id);
  const org = parts.find((id) => tree.some((item) => item.id === id && item.node_type === "VirtualOrg"));
  return org ?? parts[0] ?? null;
}

export function collectDescendants(id: string, tree: CatalogNode[]): Set<string> {
  const visited = new Set<string>([id]);
  const visit = (parent: string) => {
    for (const node of tree) {
      if (displayParent(node, tree) === parent && !visited.has(node.id)) {
        visited.add(node.id);
        visit(node.id);
      }
    }
  };
  visit(id);
  visited.delete(id);
  return visited;
}

/** 对齐移动端 canMove：根不可动、叶子不可作父、不可移入自身子树、按类型限制目标。 */
export function canMove(node: CatalogNode, targetId: string, tree: CatalogNode[]): boolean {
  if (node.node_type === "Device" || node.node_type === "System") return false;
  const target = tree.find((item) => item.id === targetId);
  if (!target || targetId === node.id || isLeaf(target)) return false;
  if (collectDescendants(node.id, tree).has(targetId)) return false;
  switch (node.node_type) {
    case "BusinessGroup":
      return target.node_type === "System" || target.node_type === "Device";
    case "VirtualOrg":
      return target.node_type === "BusinessGroup" || target.node_type === "VirtualOrg"
        || target.node_type === "System" || target.node_type === "Device";
    case "VideoChannel":
    case "AlarmChannel":
      return target.node_type === "System" || target.node_type === "Device" || target.node_type === "VirtualOrg";
    default:
      return true;
  }
}

/**
 * 新增子节点可选类型。
 *
 * 与移动端 AddChildDialog 的差异只有一处：设备(111) 根下额外放开 215/216，
 * 让「业务分组 / 虚拟组织」不必先套标准业务分组模板就能建（见文件头说明）。
 * 通道排在前面，保持「新增节点」默认选中视频通道的既有习惯。
 */
export function allowedChildTypes(parent: CatalogNode | null): CatalogNodeType[] {
  switch (parent?.node_type) {
    case "System":
      // 216 不直接挂 200：移动端保存校验在存在 System 根时要求 216 的父必须是 216，
      // 直接挂 200 的树导回移动端会被判非法，因此保持「先建 215 再挂 216」。
      return ["BusinessGroup", "VideoChannel", "AlarmChannel"];
    case "BusinessGroup":
      return ["VirtualOrg"];
    case "VirtualOrg":
      return ["VirtualOrg", "VideoChannel", "AlarmChannel"];
    case "Device":
      return ["VideoChannel", "AlarmChannel", "BusinessGroup", "VirtualOrg"];
    default:
      return ["VideoChannel", "AlarmChannel"];
  }
}

/**
 * 移动节点并修正业务分组归属（对齐移动端 CatalogRelations.moveCatalogNode）。
 * 桌面端不支持复合 ParentID，统一写单父；后代虚拟组织的分组随父同步。
 */
export function moveCatalogNode(node: CatalogNode, parent: CatalogNode, tree: CatalogNode[]): CatalogNode[] {
  const groupId = node.node_type === "VirtualOrg"
    ? (parent.node_type === "BusinessGroup" ? parent.id : parent.business_group_id)
    : null;
  const updated: CatalogNode = {
    ...node,
    parent_id: parent.id,
    ...(node.node_type === "VirtualOrg" ? { business_group_id: groupId ?? null } : {}),
    ...(isLeaf(node) ? { business_group_id: null } : {}),
  };
  const descendants = node.node_type === "VirtualOrg" ? collectDescendants(node.id, tree) : new Set<string>();
  return tree.map((item) => {
    if (item.id === node.id) return updated;
    if (descendants.has(item.id) && item.node_type === "VirtualOrg") {
      return { ...item, business_group_id: groupId ?? null };
    }
    return item;
  });
}

/** 新增节点挂载到父节点（对齐移动端 attachCatalogNode 的分组语义）。 */
export function attachCatalogNode(node: CatalogNode, parent: CatalogNode): CatalogNode {
  if (node.node_type === "VirtualOrg") {
    return {
      ...node,
      parent_id: parent.id,
      business_group_id: parent.node_type === "BusinessGroup" ? parent.id : (parent.business_group_id ?? null),
    };
  }
  if (isLeaf(node)) {
    return { ...node, parent_id: parent.id, business_group_id: null };
  }
  return { ...node, parent_id: parent.id };
}

/** 生成下一个同类型可用 ID；排除根设备占用的编码，避免序号被顶到七位数。 */
export function nextSeqId(domain: string, tree: CatalogNode[], nodeType: CatalogNodeType): string {
  const rootIds = new Set(tree.filter((node) => isRoot(node)
    && (node.node_type === "Device" || node.node_type === "System")).map((node) => node.id));
  const code = TYPE_CODES[nodeType];
  let max = 0;
  for (const node of tree) {
    if (rootIds.has(node.id) || node.id.length !== 20 || node.id.slice(10, 13) !== code) continue;
    const tail = node.id.slice(13);
    if (/^\d+$/.test(tail)) max = Math.max(max, Number(tail));
  }
  return genChildId(domain, nodeType, max + 1);
}

/** 搜索：名称/ID/父 ID/区划码/业务分组/类型；匹配节点保留完整祖先链，避免树断裂。 */
export function filterTreeByQuery(tree: CatalogNode[], query: string): CatalogNode[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return tree;
  const matched = tree.filter((node) => [
    node.name, node.id, node.parent_id, node.civil_code, node.business_group_id,
    TYPE_LABELS[node.node_type], node.node_type,
  ].some((value) => value?.toLowerCase().includes(needle)));
  if (matched.length === 0) return [];
  const byId = new Map(tree.map((node) => [node.id, node]));
  const visible = new Set<string>();
  for (const node of matched) {
    let current: CatalogNode | undefined = node;
    const guard = new Set<string>();
    while (current && !guard.has(current.id)) {
      guard.add(current.id);
      visible.add(current.id);
      if (isRoot(current)) break;
      const parentId = displayParent(current, tree);
      current = parentId ? byId.get(parentId) : undefined;
    }
  }
  return tree.filter((node) => visible.has(node.id));
}

export interface TreeRow extends CatalogNode {
  children?: TreeRow[];
}

/** 扁平节点数组 → NDataTable 树形行；孤立的父子关系一律提升为根，保证不丢节点。 */
export function buildTreeRows(source: CatalogNode[]): TreeRow[] {
  const byId = new Map<string, TreeRow>();
  for (const node of source) byId.set(node.id, { ...node });
  const roots: TreeRow[] = [];
  for (const row of byId.values()) {
    const parentId = displayParent(row, source);
    const parent = parentId ? byId.get(parentId) : undefined;
    if (!parent || parent === row) {
      roots.push(row);
      continue;
    }
    (parent.children ??= []).push(row);
  }
  return roots;
}

// ── 草稿预检（与 Rust validate_catalog_tree 同口径） ──────────────────────

export function validateDraft(tree: CatalogNode[]): string[] {
  const errors: string[] = [];
  if (tree.length === 0) return ["目录树不能为空，请载入模板或导入 JSON"];
  const ids = new Set<string>();
  for (const node of tree) {
    if (!node.name.trim()) errors.push(`节点 ${node.id} 的名称不能为空`);
    const digits = /^\d+$/.test(node.id);
    const idValid = node.node_type === "AdministrativeRegion"
      ? [2, 4, 6, 8].includes(node.id.length) && digits
      : node.id.length === 20 && digits
        && (node.node_type === "Device" || node.id.slice(10, 13) === TYPE_CODES[node.node_type]);
    if (!idValid) errors.push(`节点 ${node.id} 的国标 ID 与类型 ${TYPE_LABELS[node.node_type]} 不匹配`);
    if (ids.has(node.id)) errors.push(`节点 ID 重复：${node.id}`);
    ids.add(node.id);
    if (node.status !== "ON" && node.status !== "OFF") errors.push(`节点 ${node.id} 的状态必须是 ON 或 OFF`);
  }
  const roots = tree.filter(isRoot);
  if (roots.length !== 1 || roots.some((root) => isLeaf(root))) {
    errors.push(`目录树必须有且仅有一个目录根节点，当前 ${roots.length} 个`);
  }
  const byId = new Map(tree.map((node) => [node.id, node]));
  for (const node of tree.filter((item) => !isRoot(item))) {
    const parent = byId.get(displayParent(node, tree) ?? "");
    if (!parent) {
      errors.push(`节点 ${node.name}(${node.id}) 的父节点不存在`);
    } else if (isLeaf(parent)) {
      errors.push(`叶子节点 ${parent.id} 不能承载子节点 ${node.id}`);
    }
    if (node.business_group_id) {
      const group = byId.get(node.business_group_id);
      if (!group || group.node_type !== "BusinessGroup") {
        errors.push(`节点 ${node.name}(${node.id}) 的业务分组 ${node.business_group_id} 不存在`);
      }
    }
    let current: CatalogNode | undefined = node;
    const seen = new Set<string>();
    while (current && !isRoot(current)) {
      if (seen.has(current.id)) {
        errors.push(`节点 ${node.name}(${node.id}) 存在循环引用`);
        break;
      }
      seen.add(current.id);
      current = byId.get(displayParent(current, tree) ?? "");
    }
  }
  return [...new Set(errors)];
}

// ── JSON 双格式适配 ──────────────────────────────────────────────────────

export type JsonFlavor = "mobile" | "desktop";

interface MobileCatalogNode {
  id: string;
  type: string;
  name: string;
  parentId: string;
  fields: Record<string, string>;
}

/** 移动端扩展字段中，桌面端能落盘的只有这三个。 */
const MOBILE_FIELD_MAP: Record<string, keyof Pick<CatalogNode, "civil_code" | "business_group_id" | "status">> = {
  CivilCode: "civil_code",
  BusinessGroupID: "business_group_id",
  Status: "status",
};

export function encodeCatalogJson(nodes: CatalogNode[], flavor: JsonFlavor): string {
  if (flavor === "desktop") return JSON.stringify(nodes, null, 2);
  const mobile: MobileCatalogNode[] = nodes.map((node) => {
    const fields: Record<string, string> = {};
    if (node.civil_code) fields.CivilCode = node.civil_code;
    if (node.business_group_id) fields.BusinessGroupID = node.business_group_id;
    fields.Status = node.status;
    return { id: node.id, type: node.node_type, name: node.name, parentId: node.parent_id, fields };
  });
  return JSON.stringify(mobile, null, 2);
}

export interface DecodeResult {
  nodes: CatalogNode[];
  flavor: JsonFlavor;
  warnings: string[];
}

function asRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("节点必须是对象");
  return value as Record<string, unknown>;
}

function str(value: unknown, fallback = ""): string {
  return typeof value === "string" ? value : fallback;
}

export function decodeCatalogJson(text: string): DecodeResult {
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch (cause) {
    throw new Error(`JSON 解析失败：${String(cause)}`);
  }
  if (!Array.isArray(parsed)) throw new Error("JSON 根节点必须是数组");
  if (parsed.length === 0) throw new Error("目录节点数组不能为空");
  const first = asRecord(parsed[0]);
  const flavor: JsonFlavor = "node_type" in first ? "desktop" : "mobile";
  const warnings: string[] = [];
  let ignoredFields = 0;
  let collapsedParents = 0;
  const nodes: CatalogNode[] = parsed.map((raw, index) => {
    const item = asRecord(raw);
    const id = str(item.id).trim();
    if (!id) throw new Error(`第 ${index + 1} 个节点缺少 id`);
    if (flavor === "desktop") {
      const nodeType = str(item.node_type) as CatalogNodeType;
      if (!ALL_TYPES.includes(nodeType)) throw new Error(`第 ${index + 1} 个节点的 node_type 未知：${String(item.node_type)}`);
      return {
        id,
        node_type: nodeType,
        name: str(item.name),
        parent_id: str(item.parent_id, id),
        civil_code: typeof item.civil_code === "string" ? item.civil_code : null,
        business_group_id: typeof item.business_group_id === "string" ? item.business_group_id : null,
        status: str(item.status, "ON") === "OFF" ? "OFF" : "ON",
      };
    }
    const nodeType = str(item.type) as CatalogNodeType;
    if (!ALL_TYPES.includes(nodeType)) throw new Error(`第 ${index + 1} 个节点的 type 未知：${String(item.type)}`);
    const fields = asRecord(item.fields ?? {});
    let civilCode: string | null = null;
    let businessGroupId: string | null = null;
    let status: "ON" | "OFF" = "ON";
    if (str(fields.CivilCode)) civilCode = str(fields.CivilCode);
    if (str(fields.BusinessGroupID)) businessGroupId = str(fields.BusinessGroupID);
    if (str(fields.Status) === "OFF") status = "OFF";
    for (const key of Object.keys(fields)) {
      if (!(key in MOBILE_FIELD_MAP)) ignoredFields += 1;
    }
    const rawParent = str(item.parentId, id);
    const parentParts = rawParent.split("/").map((part) => part.trim()).filter(Boolean);
    if (parentParts.length > 1) collapsedParents += 1;
    // 移动端首层 216 用空 ParentID + BusinessGroupID 归组；桌面端改为显式挂到 215，
    // 否则会被当成第二个根，保存校验必然失败。
    const parentId = parentParts.length
      ? parentParts[parentParts.length - 1]
      : (nodeType === "VirtualOrg" && businessGroupId ? businessGroupId : id);
    return makeNode(id, nodeType, str(item.name), parentId, {
      status,
      civil_code: civilCode,
      business_group_id: businessGroupId,
    });
  });
  if (ignoredFields > 0) {
    warnings.push(`已忽略 ${ignoredFields} 个桌面端协议层不支持的扩展字段（如 Manufacturer / Model / Owner）`);
  }
  if (collapsedParents > 0) {
    warnings.push(`${collapsedParents} 个节点使用移动端复合 ParentID，桌面端仅保留末段作为父节点`);
  }
  if (flavor === "mobile") warnings.push("来源：移动端 JSON（已转换为桌面端字段）");
  else warnings.push("来源：桌面端原生 JSON");
  return { nodes, flavor, warnings };
}

// ── 展示辅助 ────────────────────────────────────────────────────────────

export function humanizedAgo(epochMs: number | null): string {
  if (!epochMs) return "—";
  const secs = Math.max(0, Math.floor((Date.now() - epochMs) / 1000));
  if (secs < 5) return "刚刚";
  if (secs < 60) return `${secs} 秒前`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟前`;
  if (secs < 86400) return `${Math.floor(secs / 3600)} 小时前`;
  return `${Math.floor(secs / 86400)} 天前`;
}

export function countByType(tree: CatalogNode[], nodeType: CatalogNodeType): number {
  return tree.filter((node) => node.node_type === nodeType).length;
}
