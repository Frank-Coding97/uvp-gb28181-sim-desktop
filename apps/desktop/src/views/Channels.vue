<script setup lang="ts">
import { computed, h, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NButton, NDataTable, NForm, NFormItem, NInput, NModal, NSelect, NSpace, NTag,
  useDialog, useMessage, type DataTableColumns,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useDevice } from "../device";
import { usePlatform, type CatalogNodeConfig } from "../platform";

type ChannelNode = CatalogNodeConfig;
interface TreeRow extends ChannelNode { children?: TreeRow[]; }
interface CatalogActivity {
  target_id: string;
  result_count: number;
  packet_count: number;
  updated_at_ms: number;
}

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const dialog = useDialog();
const { deviceLive } = useDevice();
const { config, saveDesktopConfig } = usePlatform();

const nodes = ref<ChannelNode[]>([]);
const activity = ref<CatalogActivity | null>(null);
const searchText = ref("");
const template = ref("nvr-8ch");
const channelNameDraft = ref("");
const busy = ref(false);
let unlistenCommand: UnlistenFn | null = null;

const generatedChannelId = computed(() => {
  const id = (config.value?.device.device_id ?? "").replace(/\D/g, "");
  return id.length >= 17 ? `${id.slice(0, 17)}132` : "配置尚未加载";
});

watch(config, (value) => {
  channelNameDraft.value = value?.device.channel_name ?? "";
  if (!isTauri && value?.catalog_tree) nodes.value = value.catalog_tree;
}, { immediate: true });

const templateOptions = [
  { label: "单设备（1 视频 + 1 报警）", value: "single" },
  { label: "8 通道 NVR", value: "nvr-8ch" },
  { label: "跨区划演示（3 区 × 2 通道）", value: "civil-3x2" },
  { label: "16 通道大型监控", value: "large-16ch" },
];

const typeLabel: Record<string, string> = {
  AdministrativeRegion: "行政区划",
  System: "联网系统（200）",
  Device: "设备根",
  BusinessGroup: "业务分组（215）",
  VirtualOrg: "虚拟组织（216）",
  VideoChannel: "视频通道（132）",
  AlarmChannel: "报警通道（134）",
};
const typeOptions = Object.entries(typeLabel).map(([value, label]) => ({ label, value }));
const containerTypes = new Set(["AdministrativeRegion", "System", "Device", "BusinessGroup", "VirtualOrg"]);
const typeTagType: Record<string, "warning" | "info" | "success" | "default"> = {
  AdministrativeRegion: "info", System: "info", Device: "info",
  BusinessGroup: "default", VirtualOrg: "default",
  VideoChannel: "success", AlarmChannel: "warning",
};

function countBy(type: string): number {
  return nodes.value.filter((node) => node.node_type === type).length;
}

function buildTree(source: ChannelNode[]): TreeRow[] {
  const byId = new Map<string, TreeRow>();
  source.forEach((node) => byId.set(node.id, { ...node }));
  const roots: TreeRow[] = [];
  for (const node of byId.values()) {
    if (node.parent_id === node.id || !byId.has(node.parent_id)) {
      roots.push(node);
      continue;
    }
    const parent = byId.get(node.parent_id)!;
    (parent.children ??= []).push(node);
  }
  return roots;
}

function filterTree(rows: TreeRow[], query: string): TreeRow[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return rows;
  const visit = (row: TreeRow): TreeRow | null => {
    const children = (row.children ?? []).map(visit).filter((item): item is TreeRow => item !== null);
    const own = [row.id, row.name, row.parent_id, row.civil_code, row.business_group_id, typeLabel[row.node_type]]
      .some((value) => value?.toLowerCase().includes(needle));
    return own || children.length ? { ...row, children: children.length ? children : undefined } : null;
  };
  return rows.map(visit).filter((item): item is TreeRow => item !== null);
}

const treeRows = computed(() => filterTree(buildTree(nodes.value), searchText.value));
const activityTime = computed(() => activity.value
  ? new Date(activity.value.updated_at_ms).toLocaleString("zh-CN", { hour12: false })
  : "—");

async function refreshActivity() {
  if (!isTauri || !deviceLive.value) {
    activity.value = null;
    return;
  }
  try { activity.value = await invoke<CatalogActivity | null>("get_catalog_activity"); }
  catch { activity.value = null; }
}

async function refresh() {
  if (!isTauri) {
    nodes.value = config.value?.catalog_tree ?? [];
    return;
  }
  try {
    nodes.value = await invoke<ChannelNode[]>("get_catalog_tree");
    await refreshActivity();
  } catch (cause) {
    message.error(String(cause));
  }
}

async function loadTemplate() {
  if (!isTauri) return;
  busy.value = true;
  try {
    nodes.value = await invoke<ChannelNode[]>("load_catalog_template", { template: template.value });
    message.success(`目录模板已保存，共 ${nodes.value.length} 个节点`);
  } catch (cause) {
    message.error(String(cause));
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

const showEdit = ref(false);
const isNew = ref(true);
const editing = ref<ChannelNode>(blankNode());

function blankNode(): ChannelNode {
  return {
    id: "", node_type: "VideoChannel", name: "", parent_id: "",
    civil_code: null, business_group_id: null, status: "ON",
  };
}

function flatNode(row: ChannelNode | TreeRow): ChannelNode {
  return {
    id: row.id,
    node_type: row.node_type,
    name: row.name,
    parent_id: row.parent_id,
    civil_code: row.civil_code,
    business_group_id: row.business_group_id,
    status: row.status,
  };
}

const parentOptions = computed(() => nodes.value
  .filter((node) => containerTypes.has(node.node_type) && node.id !== editing.value.id)
  .map((node) => ({ label: `${node.name} · ${node.id}`, value: node.id })));
const businessGroupOptions = computed(() => nodes.value
  .filter((node) => node.node_type === "BusinessGroup")
  .map((node) => ({ label: `${node.name} · ${node.id}`, value: node.id })));
const editingRoot = computed(() => !isNew.value && editing.value.id === editing.value.parent_id);

function openAdd(parent?: ChannelNode) {
  editing.value = blankNode();
  const root = parent && containerTypes.has(parent.node_type)
    ? parent
    : nodes.value.find((node) => node.id === node.parent_id);
  if (root) editing.value.parent_id = root.id;
  isNew.value = true;
  showEdit.value = true;
}

function openEdit(row: ChannelNode) {
  editing.value = flatNode(row);
  isNew.value = false;
  showEdit.value = true;
}

async function saveNode() {
  if (!editing.value.id.trim() || !editing.value.name.trim()) {
    return void message.warning("节点 ID 与名称必填");
  }
  busy.value = true;
  try {
    const result = await invoke<string>("upsert_channel", { node: editing.value });
    message.success(result);
    showEdit.value = false;
    await refresh();
  } catch (cause) {
    message.error(String(cause));
  } finally {
    busy.value = false;
  }
}

async function toggleStatus(row: ChannelNode) {
  busy.value = true;
  try {
    await invoke<string>("upsert_channel", {
      node: { ...flatNode(row), status: row.status === "ON" ? "OFF" : "ON" },
    });
    message.success(`已切换为${row.status === "ON" ? "离线" : "在线"}`);
    await refresh();
  } catch (cause) {
    message.error(String(cause));
  } finally {
    busy.value = false;
  }
}

function removeNode(row: ChannelNode) {
  dialog.warning({
    title: "删除目录节点",
    content: `确定删除“${row.name}”及其全部下级节点吗？运行中会向已订阅平台发送增量 NOTIFY。`,
    positiveText: "删除子树",
    negativeText: "取消",
    async onPositiveClick() {
      try {
        const result = await invoke<string>("remove_channel", { id: row.id });
        message.success(result);
        await refresh();
      } catch (cause) {
        message.error(String(cause));
      }
    },
  });
}

const showJson = ref(false);
const jsonMode = ref<"import" | "export">("export");
const jsonText = ref("");

function openExport() {
  jsonMode.value = "export";
  jsonText.value = JSON.stringify(nodes.value, null, 2);
  showJson.value = true;
}

function openImport() {
  jsonMode.value = "import";
  jsonText.value = "";
  showJson.value = true;
}

async function copyJson() {
  try {
    await navigator.clipboard.writeText(jsonText.value);
    message.success("目录 JSON 已复制");
  } catch {
    message.warning("当前环境无法自动复制，请手工选择文本");
  }
}

async function importJson() {
  try {
    const parsed = JSON.parse(jsonText.value) as unknown;
    if (!Array.isArray(parsed)) throw new Error("JSON 根节点必须是数组");
    nodes.value = await invoke<ChannelNode[]>("replace_catalog_tree", { nodes: parsed });
    showJson.value = false;
    message.success(`已导入并保存 ${nodes.value.length} 个目录节点`);
  } catch (cause) {
    message.error(`导入失败：${String(cause)}`);
  }
}

const columns: DataTableColumns<TreeRow> = [
  { title: "名称", key: "name", minWidth: 180, render: (row) => h("span", { class: "node-name" }, row.name) },
  {
    title: "类型", key: "node_type", width: 138,
    render: (row) => h(NTag, {
      size: "small", type: typeTagType[row.node_type] ?? "default", bordered: false,
    }, { default: () => typeLabel[row.node_type] ?? row.node_type }),
  },
  { title: "国标 ID", key: "id", minWidth: 205, render: (row) => h("code", row.id) },
  { title: "区划码", key: "civil_code", width: 88, render: (row) => row.civil_code ?? "—" },
  {
    title: "状态", key: "status", width: 72,
    render: (row) => h(NTag, {
      size: "small", type: row.status === "ON" ? "success" : "error", bordered: false,
    }, { default: () => row.status === "ON" ? "在线" : "离线" }),
  },
  {
    title: "操作", key: "actions", width: 286,
    render: (row) => h(NSpace, { size: 5, wrap: false }, {
      default: () => [
        h(NButton, { size: "tiny", onClick: () => openEdit(row) }, { default: () => "编辑" }),
        h(NButton, { size: "tiny", disabled: row.id === row.parent_id, onClick: () => openEdit(row) }, { default: () => "移动节点" }),
        h(NButton, { size: "tiny", disabled: row.id === row.parent_id, onClick: () => toggleStatus(row) }, { default: () => "切换状态" }),
        h(NButton, { size: "tiny", type: "error", disabled: row.id === row.parent_id, onClick: () => removeNode(row) }, { default: () => "删除" }),
      ],
    }),
  },
];

onMounted(async () => {
  await refresh();
  if (!isTauri) return;
  unlistenCommand = await listen<{ kind: string; summary: string }>("platform_command", (event) => {
    if (event.payload.kind === "query" && event.payload.summary.includes("目录")) void refreshActivity();
  });
});
onActivated(refresh);
onUnmounted(() => unlistenCommand?.());
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">目录管理</div>
        <div class="page-sub">目录树是 Rust 持久化真相；离线可编辑，运行中变更会通知已订阅平台</div>
      </div>
      <div class="activity-card">
        <span>最近平台查询</span>
        <b>{{ activity ? `${activity.result_count} 条 / ${activity.packet_count} 包` : '暂无' }}</b>
        <small>{{ activity ? `${activity.target_id} · ${activityTime}` : '注册后等待 Catalog Query' }}</small>
      </div>
    </div>

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
        <n-select v-model:value="template" :options="templateOptions" class="template-select" />
        <n-button type="primary" :loading="busy" @click="loadTemplate">载入模板</n-button>
        <n-input v-model:value="searchText" clearable class="search" placeholder="目录搜索：名称、ID、区划码、类型" />
        <n-button @click="refresh">刷新</n-button>
        <n-button type="info" @click="openAdd()">新增节点</n-button>
        <n-button @click="openImport">导入 JSON</n-button>
        <n-button @click="openExport">导出 JSON</n-button>
      </div>
      <div class="summary">
        共 {{ nodes.length }} 节点 · 视频 {{ countBy('VideoChannel') }} · 报警 {{ countBy('AlarmChannel') }} ·
        分组 {{ countBy('BusinessGroup') }} · 虚拟组织 {{ countBy('VirtualOrg') }}
      </div>
      <n-data-table
        :columns="columns" :data="treeRows" :bordered="false" size="small"
        :scroll-x="1100" :row-key="(row: TreeRow) => row.id" default-expand-all
      />
      <div v-if="nodes.length === 0" class="empty">目录为空，请载入模板或导入 JSON</div>
    </div>

    <n-modal v-model:show="showEdit" preset="card" :title="isNew ? '新增目录节点' : '编辑目录节点'" style="max-width: 560px">
      <n-form label-placement="top">
        <n-form-item label="节点 ID（行政区划允许 2/4/6/8 位，其余为 20 位）">
          <n-input v-model:value="editing.id" :disabled="!isNew" placeholder="请输入国标编码" />
        </n-form-item>
        <n-form-item label="名称"><n-input v-model:value="editing.name" /></n-form-item>
        <n-form-item label="类型"><n-select v-model:value="editing.node_type" :disabled="editingRoot" :options="typeOptions" /></n-form-item>
        <n-form-item label="父节点 / 移动到">
          <n-select v-model:value="editing.parent_id" :disabled="editingRoot" filterable :options="parentOptions" />
        </n-form-item>
        <n-form-item label="行政区划码（可选）"><n-input v-model:value="editing.civil_code" placeholder="如 310115" /></n-form-item>
        <n-form-item label="所属业务分组（可选）">
          <n-select v-model:value="editing.business_group_id" clearable filterable :options="businessGroupOptions" />
        </n-form-item>
        <n-form-item label="状态">
          <n-select v-model:value="editing.status" :options="[{ label: '在线', value: 'ON' }, { label: '离线', value: 'OFF' }]" />
        </n-form-item>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showEdit = false">取消</n-button>
          <n-button type="primary" :loading="busy" @click="saveNode">保存</n-button>
        </n-space>
      </template>
    </n-modal>

    <n-modal v-model:show="showJson" preset="card" :title="jsonMode === 'import' ? '导入目录 JSON' : '导出目录 JSON'" style="max-width: 760px">
      <n-input v-model:value="jsonText" type="textarea" :readonly="jsonMode === 'export'" :autosize="{ minRows: 16, maxRows: 26 }" placeholder="粘贴目录节点数组" />
      <template #footer>
        <n-space justify="end">
          <n-button @click="showJson = false">关闭</n-button>
          <n-button v-if="jsonMode === 'export'" type="primary" @click="copyJson">复制 JSON</n-button>
          <n-button v-else type="primary" @click="importJson">校验并导入</n-button>
        </n-space>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 18px; margin-bottom: 18px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 700; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.activity-card { min-width: 310px; padding: 11px 14px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.35); }
.activity-card span, .activity-card small { display: block; color: var(--text-tertiary); font-size: 11px; }
.activity-card b { display: block; margin: 3px 0; color: var(--text-primary); font-size: 14px; }
.activity-card small { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
.channel-config { display: grid; grid-template-columns: minmax(180px,.8fr) minmax(180px,1fr) minmax(250px,1.25fr) auto; align-items: end; gap: 14px; margin-bottom: 16px; padding: 18px 20px; }
.channel-field { display: flex; min-width: 0; flex-direction: column; gap: 6px; color: var(--text-secondary); font-size: 12px; }
.channel-id :deep(input), code { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
.section-title { color: var(--text-primary); font-size: 16px; font-weight: 600; }
.section-hint { margin-top: 4px; color: var(--text-tertiary); font-size: 12px; }
.panel { padding: 20px; }
.toolbar { display: flex; align-items: center; gap: 8px; }
.template-select { width: 255px; }
.search { min-width: 260px; flex: 1; }
.summary { margin: 12px 0 10px; color: var(--text-tertiary); font-size: 12px; }
.empty { padding: 24px; color: var(--text-tertiary); text-align: center; }
:deep(.n-data-table), :deep(.n-data-table .n-data-table-th), :deep(.n-data-table .n-data-table-td), :deep(.n-data-table .n-data-table-table), :deep(.n-data-table-base-table) { background-color: transparent !important; }
:deep(.n-data-table .n-data-table-th) { border-bottom: 1px solid rgba(120,130,150,.12) !important; color: var(--text-secondary); background-color: rgba(120,130,150,.06) !important; font-weight: 600; }
:deep(.n-data-table .n-data-table-td) { border-bottom: 1px solid rgba(120,130,150,.08) !important; }
:deep(.n-data-table .n-data-table-tr:hover .n-data-table-td) { background-color: rgba(56,132,255,.05) !important; }
@media (max-width: 1100px) { .page-header { flex-direction: column; }.activity-card { width: 100%; }.channel-config { grid-template-columns: repeat(2,minmax(0,1fr)); }.toolbar { flex-wrap: wrap; }.search { order: 3; width: 100%; flex-basis: 100%; } }
</style>
