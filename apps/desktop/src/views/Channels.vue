<script setup lang="ts">
// 目录管理（参照移动端 CatalogManagementScreen 重做）。
//
// 交互口径：整页是一份「草稿」，新增/编辑/移动/克隆/删除/模板/导入都只改内存，
// 只有点「保存*」才通过 Rust replace_catalog_tree 落盘 + 应用运行态 + 推增量 NOTIFY。
// 数据模型仍为桌面端固定 7 字段（Rust CatalogNode），协议层零改动；
// 与移动端 JSON 的字段/复合父差异由 catalog/catalog-model.ts 的适配层处理。
import { computed, h, onActivated, onMounted, onUnmounted, ref, watch, type VNode } from "vue";
import { onBeforeRouteLeave, useRouter } from "vue-router";
import {
  NAlert, NButton, NDataTable, NDropdown, NForm, NFormItem, NInput, NModal,
  NSelect, NSpace, NTag, useDialog, useMessage,
  type DataTableColumns, type DropdownOption,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useDevice } from "../device";
import { previewCatalogXml, usePlatform, type CatalogNodeConfig } from "../platform";
import {
  TYPE_LABELS, allowedChildTypes, attachCatalogNode, availableTemplates,
  buildTreeRows, canMove, collectDescendants, countByType, decodeCatalogJson,
  displayParent, encodeCatalogJson, filterTreeByQuery, humanizedAgo, isLeaf, isRoot,
  makeNode, moveCatalogNode, nextSeqId, validateDraft,
  type CatalogTemplateDef, type DecodeResult, type JsonFlavor, type TreeRow,
} from "./catalog/catalog-model";

type ChannelNode = CatalogNodeConfig;
interface CatalogActivity {
  target_id: string;
  result_count: number;
  packet_count: number;
  updated_at_ms: number;
}

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const dialog = useDialog();
const router = useRouter();
const { deviceLive } = useDevice();
const { config, active, saveDesktopConfig } = usePlatform();

/** 已保存的真相（Rust 持久化快照），用于脏标记与「放弃修改」回退。 */
const baseline = ref<ChannelNode[]>([]);
/** 当前编辑中的草稿；所有 UI 操作只改它。 */
const draft = ref<ChannelNode[]>([]);
const savedAt = ref<number | null>(null);
const activity = ref<CatalogActivity | null>(null);
const busy = ref(false);
const searchText = ref("");
const showOnboarding = ref(true);
const channelNameDraft = ref("");
const forceLeave = ref(false);
const pendingPath = ref<string | null>(null);
let unlistenCommand: UnlistenFn | null = null;

/** 固定键顺序比较，避免因对象属性插入顺序不同造成「假脏」。 */
function stableTree(nodes: ChannelNode[]): string {
  return JSON.stringify(nodes.map((node) => ({
    id: node.id,
    node_type: node.node_type,
    name: node.name,
    parent_id: node.parent_id,
    civil_code: node.civil_code ?? null,
    business_group_id: node.business_group_id ?? null,
    status: node.status,
  })));
}
const isDirty = computed(() => stableTree(draft.value) !== stableTree(baseline.value));
const generatedChannelId = computed(() => {
  const id = (config.value?.device.device_id ?? "").replace(/\D/g, "");
  return id.length >= 17 ? `${id.slice(0, 17)}132` : "配置尚未加载";
});
const templateContext = computed(() => {
  const deviceId = config.value?.device.device_id || "34020000001320000001";
  return {
    deviceId,
    deviceName: config.value?.device.device_name || "演示设备",
    channelName: channelNameDraft.value || config.value?.device.channel_name || "通道1",
    domain: active.value?.server_domain || deviceId.slice(0, 10) || "3402000000",
    gbVersion: (active.value?.gb_version ?? "V2022") as "V2016" | "V2022",
  };
});
const gbVersion = computed(() => templateContext.value.gbVersion);
const domain = computed(() => templateContext.value.domain);
const rootNode = computed(() => draft.value.find((node) => isRoot(node))
  ?? draft.value.find((node) => node.node_type === "Device" || node.node_type === "System")
  ?? null);

const cloneTree = (nodes: ChannelNode[]): ChannelNode[] => nodes.map((node) => ({ ...node }));
const findNode = (id: string | null): ChannelNode | null => draft.value.find((node) => node.id === id) ?? null;
const setDraft = (nodes: ChannelNode[]) => { draft.value = nodes; };

// ── 加载 / 保存 ──────────────────────────────────────────────────────────

async function refreshActivity() {
  if (!isTauri || !deviceLive.value) {
    activity.value = null;
    return;
  }
  try { activity.value = await invoke<CatalogActivity | null>("get_catalog_activity"); }
  catch { activity.value = null; }
}
const activityTime = computed(() => activity.value
  ? new Date(activity.value.updated_at_ms).toLocaleString("zh-CN", { hour12: false })
  : "—");

async function loadTree() {
  if (!isTauri) {
    const nodes = config.value?.catalog_tree ?? [];
    baseline.value = cloneTree(nodes);
    draft.value = cloneTree(nodes);
    return;
  }
  try {
    const nodes = await invoke<ChannelNode[]>("get_catalog_tree");
    baseline.value = cloneTree(nodes);
    draft.value = cloneTree(nodes);
    await refreshActivity();
  } catch (cause) {
    message.error(String(cause));
  }
}

async function reloadTree() {
  if (isDirty.value) {
    dialog.warning({
      title: "重新载入目录",
      content: "当前有未保存的修改，重新载入会丢弃草稿。",
      positiveText: "丢弃并载入",
      negativeText: "取消",
      onPositiveClick: () => { void loadTree(); },
    });
    return;
  }
  await loadTree();
}

async function saveDraft() {
  const errors = validateDraft(draft.value);
  if (errors.length) {
    message.error(`保存前校验未通过：${errors.slice(0, 3).join("；")}`);
    return;
  }
  busy.value = true;
  try {
    const saved = isTauri
      ? await invoke<ChannelNode[]>("replace_catalog_tree", { nodes: draft.value })
      : cloneTree(draft.value);
    baseline.value = cloneTree(saved);
    draft.value = cloneTree(saved);
    savedAt.value = Date.now();
    if (!isTauri) {
      message.success(`开发预览模式：已把 ${saved.length} 个节点写入内存草稿（未连接 Rust 持久化）`);
    } else {
      message.success(`已保存 ${saved.length} 个节点；运行中会向已订阅平台推送目录 NOTIFY`);
    }
    // 移动端保存时会把 SIP 注册身份切换到目录根（标准业务分组用 System/200）。
    // 桌面端协议层保持 device_id 独立，仅提示，避免隐式改身份导致注册失败。
    const systemRoot = rootNode.value?.node_type === "System" ? rootNode.value : null;
    if (isTauri && systemRoot && config.value && config.value.device.device_id !== systemRoot.id) {
      message.warning("当前目录根是系统(200)编码，与设备注册 ID 不一致；桌面端不会自动切换注册身份，请到设备配置核对");
    }
  } catch (cause) {
    message.error(`保存失败：${String(cause)}`);
  } finally {
    busy.value = false;
  }
}

async function saveChannelConfig() {
  if (!config.value || deviceLive.value) return;
  const channelName = channelNameDraft.value.trim();
  if (!channelName) return void message.warning("当前通道名称不能为空");
  try {
    await saveDesktopConfig({
      ...config.value,
      device: { ...config.value.device, channel_name: channelName },
    });
    message.success("当前通道配置已保存");
  } catch (cause) {
    message.error(String(cause));
  }
}

// ── 草稿编辑操作 ─────────────────────────────────────────────────────────

function openAdd(parent: ChannelNode | null = null) {
  if (!parent && draft.value.length === 0) {
    showTemplate.value = true;
    message.info("目录为空，请先应用一个模板再新增节点");
    return;
  }
  const target = parent ?? rootNode.value;
  addParentId.value = target?.id ?? null;
  const options = allowedChildTypes(target);
  addType.value = options[0] ?? "VideoChannel";
  addName.value = "新节点";
  addCountText.value = "1";
  showAdd.value = true;
}

function confirmAdd() {
  const parent = findNode(addParentId.value);
  const count = addBatch.value
    ? Math.min(50, Math.max(1, Number(addCountText.value) || 1))
    : 1;
  const ids = new Set(draft.value.map((node) => node.id));
  const created: ChannelNode[] = [];
  for (let i = 0; i < count; i += 1) {
    let id = nextSeqId(domain.value, [...draft.value, ...created], addType.value);
    while (ids.has(id)) {
      const tail = Number(id.slice(13)) + 1;
      id = `${id.slice(0, 13)}${String(tail).padStart(7, "0")}`;
    }
    ids.add(id);
    const name = count > 1
      ? `${addName.value.trim() || TYPE_LABELS[addType.value]}-${String(i + 1).padStart(3, "0")}`
      : (addName.value.trim() || TYPE_LABELS[addType.value]);
    let node = makeNode(id, addType.value, name, parent?.id ?? id);
    if (parent) node = attachCatalogNode(node, parent);
    created.push(node);
  }
  setDraft([...draft.value, ...created]);
  showAdd.value = false;
  message.success(`已加入 ${created.length} 个节点（待保存）`);
}

function openEdit(row: ChannelNode) {
  editorNode.value = { ...row };
  showEditor.value = true;
}

function saveEditor() {
  if (!editorNode.value.name.trim()) return void message.warning("节点名称不能为空");
  const next = { ...editorNode.value };
  setDraft(draft.value.map((node) => node.id === next.id ? next : node));
  showEditor.value = false;
}

function cloneNode(row: ChannelNode) {
  const id = nextSeqId(domain.value, draft.value, row.node_type);
  const clone: ChannelNode = { ...row, id, name: `${row.name}-副本` };
  setDraft([...draft.value, clone]);
  openEdit(clone);
  message.info("已克隆节点（待保存），请确认名称");
}

function toggleStatus(row: ChannelNode) {
  if (!isLeaf(row)) return;
  setDraft(draft.value.map((node) => node.id === row.id
    ? { ...node, status: node.status === "ON" ? "OFF" : "ON" }
    : node));
  message.info(`已切换状态为${row.status === "ON" ? "离线" : "在线"}（待保存）`);
}

function removeNode(row: ChannelNode) {
  const descendants = collectDescendants(row.id, draft.value);
  dialog.warning({
    title: "删除目录节点",
    content: `确定删除“${row.name}”及其 ${descendants.size} 个下级节点吗？保存后会向已订阅平台发送增量 NOTIFY。`,
    positiveText: "删除子树",
    negativeText: "取消",
    onPositiveClick: () => {
      setDraft(draft.value.filter((node) => node.id !== row.id && !descendants.has(node.id)));
    },
  });
}

function openMove(row: ChannelNode) {
  movingId.value = row.id;
  moveTargetId.value = candidatesFor(row)[0]?.id ?? null;
  showMove.value = true;
}

function confirmMove() {
  const node = findNode(movingId.value);
  const parent = findNode(moveTargetId.value);
  if (!node || !parent || !canMove(node, parent.id, draft.value)) {
    return void message.warning("目标父节点不合法");
  }
  setDraft(moveCatalogNode(node, parent, draft.value));
  showMove.value = false;
  message.success(`已移动到「${parent.name}」（待保存）`);
}

function resetToDefault() {
  const tpl = availableTemplates(gbVersion.value).find((item) => item.id === "single");
  if (tpl) setDraft(tpl.build(templateContext.value));
  showResetConfirm.value = false;
  message.info("已复位为默认树，点保存生效");
}

function applyTemplate(tpl: CatalogTemplateDef) {
  setDraft(tpl.build(templateContext.value));
  showTemplate.value = false;
  message.success(`已应用模板「${tpl.title}」（待保存）`);
}

function candidatesFor(row: ChannelNode): ChannelNode[] {
  return draft.value.filter((item) => canMove(row, item.id, draft.value));
}

// ── 模板 / JSON / XML 对话框状态 ──────────────────────────────────────────

const showEditor = ref(false);
const editorNode = ref<ChannelNode>(makeNode("", "VideoChannel", "", ""));
const showAdd = ref(false);
const addParentId = ref<string | null>(null);
const addType = ref<ChannelNode["node_type"]>("VideoChannel");
const addName = ref("新节点");
const addCountText = ref("1");
const showMove = ref(false);
const movingId = ref<string | null>(null);
const moveTargetId = ref<string | null>(null);
const showTemplate = ref(false);
const showJson = ref(false);
const jsonMode = ref<"import" | "export">("export");
const jsonText = ref("");
const jsonFlavor = ref<JsonFlavor>("mobile");
const importPreview = ref<DecodeResult | null>(null);
const importError = ref("");
const showXml = ref(false);
const xmlText = ref("");
const showXmlLocal = ref(false);
const showLeaveConfirm = ref(false);
const showResetConfirm = ref(false);

const addParent = computed(() => findNode(addParentId.value));
const addTypes = computed(() => allowedChildTypes(addParent.value));
const addBatch = computed(() => addType.value === "VideoChannel" || addType.value === "AlarmChannel");
const addTypeOptions = computed(() => addTypes.value.map((value) => ({ label: TYPE_LABELS[value], value })));
const movingNode = computed(() => findNode(movingId.value));
const moveCandidates = computed(() => {
  const node = movingNode.value;
  if (!node) return [];
  return candidatesFor(node).map((item) => ({
    label: `${item.name} · ${TYPE_LABELS[item.node_type]} · ${item.id}`, value: item.id,
  }));
});
const businessGroupOptions = computed(() => draft.value
  .filter((node) => node.node_type === "BusinessGroup")
  .map((node) => ({ label: `${node.name} · ${node.id}`, value: node.id })));
const templateCards = computed(() => availableTemplates(gbVersion.value).map((tpl) => ({
  id: tpl.id, title: tpl.title, description: tpl.description, origin: tpl.origin,
  nodeCount: tpl.build(templateContext.value).length, def: tpl,
})));
const moreOptions: DropdownOption[] = [
  { label: "应用模板", key: "template" },
  { label: "导入 JSON", key: "import" },
  { label: "导出 JSON", key: "export" },
  { label: "复位到默认", key: "reset" },
];

function onMoreSelect(key: string | number) {
  switch (String(key)) {
    case "template": showTemplate.value = true; break;
    case "import": openImport(); break;
    case "export": openExport(); break;
    case "reset": showResetConfirm.value = true; break;
    default: break;
  }
}

function openExport() {
  jsonMode.value = "export";
  jsonFlavor.value = "mobile";
  jsonText.value = encodeCatalogJson(draft.value, jsonFlavor.value);
  showJson.value = true;
}

function openImport() {
  jsonMode.value = "import";
  jsonText.value = "";
  importPreview.value = null;
  importError.value = "";
  showJson.value = true;
}

function parseImport() {
  try {
    importPreview.value = decodeCatalogJson(jsonText.value);
    importError.value = "";
    message.success(`解析成功：${importPreview.value.nodes.length} 个节点`);
  } catch (cause) {
    importPreview.value = null;
    importError.value = String(cause);
  }
}

function confirmImport() {
  const result = importPreview.value;
  if (!result) return void message.warning("请先点「解析预览」确认 JSON 合法");
  setDraft(result.nodes);
  showJson.value = false;
  message.success(`已导入 ${result.nodes.length} 个节点（待保存）`);
}

function onJsonFlavorChange(value: string) {
  jsonFlavor.value = value === "desktop" ? "desktop" : "mobile";
  if (jsonMode.value === "export") jsonText.value = encodeCatalogJson(draft.value, jsonFlavor.value);
}

async function copyText(text: string, label: string) {
  try {
    await navigator.clipboard.writeText(text);
    message.success(`${label}已复制`);
  } catch {
    message.warning("当前环境无法自动复制，请手工选择文本");
  }
}

function buildLocalPreviewXml(nodes: ChannelNode[], deviceId: string): string {
  const esc = (value: string) => value.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  const items = nodes.map((node) => [
    "    <Item>",
    `      <DeviceID>${esc(node.id)}</DeviceID>`,
    `      <Name>${esc(node.name)}</Name>`,
    node.civil_code ? `      <CivilCode>${esc(node.civil_code)}</CivilCode>` : "",
    `      <Parental>${isLeaf(node) ? 0 : 1}</Parental>`,
    `      <ParentID>${esc(node.parent_id)}</ParentID>`,
    node.business_group_id ? `      <BusinessGroupID>${esc(node.business_group_id)}</BusinessGroupID>` : "",
    "      <Event>ON</Event>",
    `      <Status>${esc(node.status)}</Status>`,
    "    </Item>",
  ].filter(Boolean).join("\n")).join("\n");
  return `<?xml version="1.0" encoding="GB2312"?>\n<Notify>\n  <CmdType>Catalog</CmdType>\n  <SN>1</SN>\n  <DeviceID>${esc(deviceId)}</DeviceID>\n  <SumNum>${nodes.length}</SumNum>\n  <DeviceList Num="${nodes.length}">\n${items}\n  </DeviceList>\n</Notify>`;
}

async function openXmlPreview() {
  const deviceId = rootNode.value?.id || templateContext.value.deviceId;
  const errors = validateDraft(draft.value);
  if (errors.length) message.warning(`目录存在校验问题，预览可能失败：${errors[0]}`);
  if (isTauri) {
    try {
      xmlText.value = await previewCatalogXml(draft.value, deviceId);
      showXmlLocal.value = false;
    } catch (cause) {
      message.error(`预览 NOTIFY 失败：${String(cause)}`);
      return;
    }
  } else {
    xmlText.value = buildLocalPreviewXml(draft.value, deviceId);
    showXmlLocal.value = true;
  }
  showXml.value = true;
}

// ── 未保存离开保护 ───────────────────────────────────────────────────────

onBeforeRouteLeave((to) => {
  if (!isDirty.value || forceLeave.value) return true;
  pendingPath.value = to.fullPath;
  showLeaveConfirm.value = true;
  return false;
});

function confirmLeave() {
  showLeaveConfirm.value = false;
  // keep-alive 下组件不会被销毁，必须显式把草稿回退到真相，否则切回页面仍带着「已放弃」的修改。
  setDraft(cloneTree(baseline.value));
  forceLeave.value = true;
  const path = pendingPath.value;
  pendingPath.value = null;
  if (path) void router.push(path).finally(() => { forceLeave.value = false; });
  else forceLeave.value = false;
}

// ── 展示 ────────────────────────────────────────────────────────────────

const typeTagType: Record<string, "warning" | "info" | "success" | "default"> = {
  AdministrativeRegion: "info", System: "info", Device: "info",
  BusinessGroup: "default", VirtualOrg: "default",
  VideoChannel: "success", AlarmChannel: "warning",
};
const treeRows = computed<TreeRow[]>(() => buildTreeRows(filterTreeByQuery(draft.value, searchText.value)));
/** 与 buildTreeRows 共用同一份父关系判定，避免表格层级与「父节点」列口径不一致。 */
function displayParentName(row: ChannelNode): string | null {
  return displayParent(row, draft.value);
}
function parentName(row: ChannelNode): string {
  const id = displayParentName(row);
  const node = draft.value.find((item) => item.id === id);
  return node ? node.name : (id ?? "—");
}
const summary = computed(() => `共 ${draft.value.length} 节点 · 视频 ${countByType(draft.value, "VideoChannel")}`
  + ` · 报警 ${countByType(draft.value, "AlarmChannel")} · 分组 ${countByType(draft.value, "BusinessGroup")}`
  + ` · 虚拟组织 ${countByType(draft.value, "VirtualOrg")}`);

function actionButtons(row: ChannelNode): VNode[] {
  const buttons: Array<VNode | null> = [
    h(NButton, { size: "tiny", onClick: () => openEdit(row) }, { default: () => "编辑字段" }),
    !isLeaf(row) && !isRoot(row)
      ? h(NButton, { size: "tiny", onClick: () => openAdd(row) }, { default: () => "新增子节点" })
      : null,
    !isRoot(row)
      ? h(NButton, { size: "tiny", onClick: () => cloneNode(row) }, { default: () => "克隆节点" })
      : null,
    h(NButton, {
      size: "tiny", disabled: isRoot(row) || candidatesFor(row).length === 0,
      onClick: () => openMove(row),
    }, { default: () => "移动节点" }),
    h(NButton, { size: "tiny", disabled: !isLeaf(row), onClick: () => toggleStatus(row) },
      { default: () => "切换状态" }),
    h(NButton, { size: "tiny", type: "error", disabled: isRoot(row), onClick: () => removeNode(row) },
      { default: () => "删除" }),
  ];
  return buttons.filter((item): item is VNode => item !== null);
}

const columns: DataTableColumns<TreeRow> = [
  { title: "名称", key: "name", minWidth: 180, render: (row) => h("span", { class: "node-name" }, row.name) },
  {
    title: "类型", key: "node_type", width: 148,
    render: (row) => h(NTag, { size: "small", type: typeTagType[row.node_type] ?? "default", bordered: false },
      { default: () => TYPE_LABELS[row.node_type] ?? row.node_type }),
  },
  { title: "国标 ID", key: "id", minWidth: 205, render: (row) => h("code", row.id) },
  { title: "父节点", key: "parent_id", width: 150, render: (row) => parentName(row) },
  { title: "区划码", key: "civil_code", width: 88, render: (row) => row.civil_code ?? "—" },
  {
    title: "状态", key: "status", width: 72,
    render: (row) => h(NTag, { size: "small", type: row.status === "ON" ? "success" : "error", bordered: false },
      { default: () => row.status === "ON" ? "在线" : "离线" }),
  },
  {
    title: "操作", key: "actions", width: 400, fixed: "right",
    render: (row) => h(NSpace, { size: 4, wrap: false }, { default: () => actionButtons(row) }),
  },
];

watch(config, (value) => {
  channelNameDraft.value = value?.device.channel_name ?? "";
  if (!isTauri && baseline.value.length === 0 && (value?.catalog_tree?.length ?? 0) > 0) {
    baseline.value = cloneTree(value!.catalog_tree);
    draft.value = cloneTree(value!.catalog_tree);
  }
}, { immediate: true });

onMounted(async () => {
  await loadTree();
  if (!isTauri) return;
  unlistenCommand = await listen<{ kind: string; summary: string }>("platform_command", (event) => {
    if (event.payload.kind === "query" && event.payload.summary.includes("目录")) void refreshActivity();
  });
});
onActivated(() => { void refreshActivity(); });
onUnmounted(() => unlistenCommand?.());
</script>

<template>
  <div class="page page-wrap">
    <div class="page-header">
      <div>
        <div class="page-title">目录管理</div>
        <div class="page-sub">目录树以 Rust 持久化为真相；本页改动先进入草稿，点「保存*」才落盘并通知已订阅平台</div>
      </div>
      <div class="activity-card">
        <span>最近平台查询</span>
        <b>{{ activity ? `${activity.result_count} 条 / ${activity.packet_count} 包` : '暂无' }}</b>
        <small>{{ activity ? `${activity.target_id} · ${activityTime}` : '注册后等待 Catalog Query' }}</small>
      </div>
    </div>

    <n-alert v-if="showOnboarding" type="info" closable class="onboarding" @close="showOnboarding = false">
      <template #header>目录管理使用提示</template>
      • 编辑、克隆、移动、模板与导入都只改草稿，点「保存*」才生效；离开页面会提示未保存的修改<br />
      • 标准业务分组场景：系统 → 分组 → 虚拟组织 → 通道；仅 GB/T 28181-2022 可用<br />
      • 移动端与桌面端 JSON 可互导；扩展字段与复合 ParentID 会在导入时给出提示
    </n-alert>

    <div class="glass-card channel-config">
      <div>
        <div class="section-title">默认视频通道</div>
        <div class="section-hint">仅用于首次生成默认树；目录树可以独立维护</div>
      </div>
      <label class="channel-field">
        <span>当前通道名称</span>
        <n-input v-model:value="channelNameDraft" :disabled="deviceLive" placeholder="请输入通道名称" />
      </label>
      <label class="channel-field channel-id">
        <span>通道 ID（运行时生成）</span>
        <n-input :value="generatedChannelId" disabled />
      </label>
      <n-button type="primary" :disabled="deviceLive || !config" @click="saveChannelConfig">保存默认通道</n-button>
    </div>

    <div class="glass-card panel">
      <div class="toolbar">
        <n-input v-model:value="searchText" clearable class="search" placeholder="目录搜索：名称、ID、区划码、分组、类型" />
        <n-button @click="reloadTree">刷新</n-button>
        <n-button type="info" @click="openAdd()">新增节点</n-button>
        <n-button @click="openXmlPreview">预览 NOTIFY</n-button>
        <n-dropdown trigger="click" :options="moreOptions" @select="onMoreSelect">
          <n-button>更多 ···</n-button>
        </n-dropdown>
        <n-button type="primary" :loading="busy" :disabled="!isDirty" @click="saveDraft">
          {{ isDirty ? "保存*" : "保存" }}
        </n-button>
      </div>
      <div class="summary">
        {{ summary }}
        <span v-if="isDirty" class="dirty">· 有未保存的修改</span>
        <span v-else class="clean">· 已与持久化真相一致</span>
        <span class="saved-at">· 上次保存：{{ humanizedAgo(savedAt) }}</span>
      </div>
      <n-data-table
        :columns="columns" :data="treeRows" :bordered="false" size="small"
        :scroll-x="1500" :row-key="(row: TreeRow) => row.id" default-expand-all
      />
      <div v-if="draft.length === 0" class="empty">目录为空，请在「更多」中应用模板或导入 JSON</div>
    </div>

    <!-- 节点字段编辑（对齐移动端 NodeEditorSheet：ID/类型只读，父关系走移动对话框） -->
    <n-modal v-model:show="showEditor" preset="card" title="编辑目录节点" style="max-width: 560px">
      <n-form label-placement="top">
        <n-form-item label="节点 ID（不可修改）">
          <n-input :value="editorNode.id" disabled />
        </n-form-item>
        <n-form-item label="类型（不可修改）">
          <n-input :value="TYPE_LABELS[editorNode.node_type]" disabled />
        </n-form-item>
        <n-form-item label="名称"><n-input v-model:value="editorNode.name" /></n-form-item>
        <n-form-item label="行政区划码（可选）">
          <n-input :value="editorNode.civil_code ?? ''"
            @update:value="(v: string) => editorNode.civil_code = String(v ?? '')" />
        </n-form-item>
        <n-form-item v-if="editorNode.node_type === 'VirtualOrg' && gbVersion === 'V2022'" label="所属业务分组（BusinessGroupID）">
          <n-select :value="editorNode.business_group_id ?? null" clearable filterable
            :options="businessGroupOptions"
            @update:value="(v: string) => editorNode.business_group_id = v" />
        </n-form-item>
        <n-form-item label="状态">
          <n-select :value="editorNode.status"
            :options="[{ label: '在线', value: 'ON' }, { label: '离线', value: 'OFF' }]"
            @update:value="(v: string) => editorNode.status = v === 'OFF' ? 'OFF' : 'ON'" />
        </n-form-item>
        <n-form-item label="Manufacturer / Model（设备级只读回显）">
          <n-input :value="`${config?.device.manufacturer || '—'} / ${config?.device.model || '—'}`" disabled />
        </n-form-item>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showEditor = false">取消</n-button>
          <n-button type="primary" @click="saveEditor">完成</n-button>
        </n-space>
      </template>
    </n-modal>

    <!-- 新增子节点（对齐移动端 AddChildDialog：按父类型限制 + 批量新增 + 自动 ID） -->
    <n-modal v-model:show="showAdd" preset="card" title="新增子节点" style="max-width: 560px">
      <n-form label-placement="top">
        <n-form-item label="父节点">
          <n-input :value="addParent ? `${addParent.name} · ${addParent.id}` : '（根目录）'" disabled />
        </n-form-item>
        <n-form-item label="类型">
          <n-select :value="addType" :options="addTypeOptions"
            @update:value="(v: string) => addType = v as ChannelNode['node_type']" />
        </n-form-item>
        <n-form-item label="名称"><n-input v-model:value="addName" /></n-form-item>
        <n-form-item v-if="addBatch" label="数量（1-50）">
          <n-input :value="addCountText"
            @update:value="(v: string) => addCountText = String(v ?? '').replace(/\D/g, '').slice(0, 2)" />
        </n-form-item>
        <div class="hint">批量新增（1-50）按 name-001 顺序命名；国标 ID 由域编码 + 类型码 + 下一个可用序号自动生成。</div>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showAdd = false">取消</n-button>
          <n-button type="primary" @click="confirmAdd">{{ addBatch && Number(addCountText) > 1 ? `批量新增 (${addCountText})` : "确定" }}</n-button>
        </n-space>
      </template>
    </n-modal>

    <!-- 独立移动对话框（对齐移动端 MoveDialog + canMove） -->
    <n-modal v-model:show="showMove" preset="card" :title="`移动节点 · ${movingNode?.name ?? ''}`" style="max-width: 620px">
      <n-form label-placement="top">
        <n-form-item label="目标父节点（已过滤非法目标）">
          <n-select :value="moveTargetId" filterable :options="moveCandidates" placeholder="选择目标父节点"
            @update:value="(v: string) => moveTargetId = v" />
        </n-form-item>
        <div class="hint">根节点与通道不可作为父节点；不可移入自身子树；业务分组/虚拟组织/通道各有合法的父类型。</div>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showMove = false">取消</n-button>
          <n-button type="primary" :disabled="!moveTargetId" @click="confirmMove">移动</n-button>
        </n-space>
      </template>
    </n-modal>

    <!-- 应用模板（移动端 2 个 + 桌面端扩展 3 个） -->
    <n-modal v-model:show="showTemplate" preset="card" title="应用模板" style="max-width: 680px">
      <div class="hint template-hint">应用后会替换当前编辑中的树（待保存，可反悔）。标准业务分组仅 GB/T 28181-2022 可用。</div>
      <div class="template-list">
        <div v-for="card in templateCards" :key="card.id" class="template-card" @click="applyTemplate(card.def)">
          <div class="template-head">
            <b>{{ card.title }}</b>
            <span>{{ card.nodeCount }} 节点 · {{ card.origin === "mobile" ? "移动端同款" : "桌面端扩展" }}</span>
          </div>
          <small>{{ card.description }}</small>
        </div>
      </div>
    </n-modal>

    <!-- JSON 导入 / 导出（双格式适配） -->
    <n-modal v-model:show="showJson" preset="card"
      :title="jsonMode === 'import' ? '导入目录 JSON' : '导出目录 JSON'" style="max-width: 760px">
      <n-form label-placement="top">
        <n-form-item v-if="jsonMode === 'export'" label="交换格式">
          <n-select :value="jsonFlavor" :options="[
            { label: '移动端格式（跨平台互导，推荐）', value: 'mobile' },
            { label: '桌面端原生格式（snake_case 固定字段）', value: 'desktop' },
          ]" @update:value="(v: string) => onJsonFlavorChange(v)" />
        </n-form-item>
        <n-input v-model:value="jsonText" type="textarea" :readonly="jsonMode === 'export'"
          :autosize="{ minRows: 14, maxRows: 22 }" placeholder="粘贴移动端或桌面端目录节点数组" />
        <div v-if="jsonMode === 'import' && importPreview" class="hint ok">
          解析成功：{{ importPreview.nodes.length }} 个节点。{{ importPreview.warnings.join("；") }}
        </div>
        <div v-if="jsonMode === 'import' && importError" class="hint err">解析失败：{{ importError }}</div>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showJson = false">关闭</n-button>
          <n-button v-if="jsonMode === 'export'" @click="copyText(jsonText, '目录 JSON')">复制 JSON</n-button>
          <template v-else>
            <n-button @click="parseImport">解析预览</n-button>
            <n-button type="primary" :disabled="!importPreview" @click="confirmImport">确定替换（待保存）</n-button>
          </template>
        </n-space>
      </template>
    </n-modal>

    <!-- Catalog NOTIFY XML 预览 -->
    <n-modal v-model:show="showXml" preset="card" title="Catalog NOTIFY 预览" style="max-width: 820px">
      <div class="hint">
        与真实出站共用 CatalogNotify 组装逻辑，SN 固定为 1；实际发送还会按 UDP 预算分片。
        <span v-if="showXmlLocal">当前为开发预览模式（未连接 Rust 后端），仅用于核对字段结构。</span>
      </div>
      <n-input :value="xmlText" type="textarea" readonly :autosize="{ minRows: 16, maxRows: 24 }" />
      <template #footer>
        <n-space justify="end">
          <n-button @click="showXml = false">关闭</n-button>
          <n-button type="primary" @click="copyText(xmlText, 'NOTIFY XML')">复制 XML</n-button>
        </n-space>
      </template>
    </n-modal>

    <!-- 未保存离开 / 复位默认确认 -->
    <n-modal v-model:show="showLeaveConfirm" preset="card" title="放弃未保存的修改?" style="max-width: 440px">
      <div>当前有未保存的目录树修改，离开后会丢失。</div>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showLeaveConfirm = false">继续编辑</n-button>
          <n-button type="error" @click="confirmLeave">放弃修改</n-button>
        </n-space>
      </template>
    </n-modal>
    <n-modal v-model:show="showResetConfirm" preset="card" title="复位到默认?" style="max-width: 440px">
      <div>把当前编辑中的目录树替换为默认树（设备配置生成的 3 节点扁平树）。替换后还需要点保存才会真正落盘。</div>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showResetConfirm = false">取消</n-button>
          <n-button type="error" @click="resetToDefault">复位</n-button>
        </n-space>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.activity-card { min-width: 310px; padding: 11px 14px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.35); }
.activity-card span, .activity-card small { display: block; color: var(--text-tertiary); font-size: 11px; }
.activity-card b { display: block; margin: 3px 0; color: var(--text-primary); font-size: 14px; }
.activity-card small { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
.onboarding { margin-bottom: 14px; line-height: 1.8; }
.channel-config { display: grid; grid-template-columns: minmax(180px,.8fr) minmax(180px,1fr) minmax(250px,1.25fr) auto; align-items: end; gap: 14px; margin-bottom: 16px; padding: 18px 20px; }
.channel-field { display: flex; min-width: 0; flex-direction: column; gap: 6px; color: var(--text-secondary); font-size: 12px; }
.channel-id :deep(input), code { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
.section-title { color: var(--text-primary); font-size: 16px; font-weight: 600; }
.section-hint { margin-top: 4px; color: var(--text-tertiary); font-size: 12px; }
.panel { padding: 20px; }
.toolbar { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
.search { min-width: 280px; flex: 1; }
.summary { margin: 12px 0 10px; color: var(--text-tertiary); font-size: 12px; }
.dirty { color: #d97706; font-weight: 600; }
.clean { color: #16a34a; }
.saved-at { margin-left: 6px; }
.empty { padding: 24px; color: var(--text-tertiary); text-align: center; }
.hint { margin-top: 8px; color: var(--text-tertiary); font-size: 11.5px; line-height: 1.7; }
.hint.ok { color: #16a34a; }
.hint.err { color: #dc2626; }
.template-hint { margin: 0 0 12px; }
.template-list { display: grid; grid-template-columns: repeat(auto-fill, minmax(290px, 1fr)); gap: 10px; }
.template-card { padding: 12px 14px; border: 1px solid var(--border-default); border-radius: 8px; cursor: pointer; transition: border-color .15s, background .15s; }
.template-card:hover { border-color: var(--primary-color, #3884ff); background: rgba(56,132,255,.05); }
.template-head { display: flex; align-items: baseline; justify-content: space-between; gap: 8px; }
.template-head b { font-size: 13px; }
.template-head span { color: var(--text-tertiary); font-size: 11px; white-space: nowrap; }
.template-card small { display: block; margin-top: 6px; color: var(--text-secondary); font-size: 11.5px; line-height: 1.6; }
:deep(.n-data-table), :deep(.n-data-table .n-data-table-th), :deep(.n-data-table .n-data-table-td), :deep(.n-data-table .n-data-table-table), :deep(.n-data-table-base-table) { background-color: transparent !important; }
:deep(.n-data-table .n-data-table-th) { border-bottom: 1px solid rgba(120,130,150,.12) !important; color: var(--text-secondary); background-color: rgba(120,130,150,.06) !important; font-weight: 600; }
:deep(.n-data-table .n-data-table-td) { border-bottom: 1px solid rgba(120,130,150,.08) !important; }
:deep(.n-data-table .n-data-table-tr:hover .n-data-table-td) { background-color: rgba(56,132,255,.05) !important; }
@media (max-width: 1100px) { .page-header { flex-direction: column; }.activity-card { width: 100%; }.channel-config { grid-template-columns: repeat(2,minmax(0,1fr)); }.toolbar { flex-wrap: wrap; }.search { order: 3; width: 100%; flex-basis: 100%; } }
</style>
