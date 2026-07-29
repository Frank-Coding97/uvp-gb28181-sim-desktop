<script setup lang="ts">
// 多通道目录管理页(FR-34):模板选择 + 目录树 CRUD。
// 通道操作需设备已在「单设备联调」页启动。
// (OSD 是平台→设备的配置命令,展示在单设备页"OSD 设置(平台下发)",不在此页。)
import { ref, onMounted, onActivated } from "vue";
import {
  NButton, NSpace, NSelect, NDataTable, NModal, NForm, NFormItem, NInput,
  NTag, useDialog, useMessage, type DataTableColumns,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";

const message = useMessage();
const dialog = useDialog();

interface ChannelNode {
  id: string;
  node_type: string;
  name: string;
  parent_id: string;
  civil_code?: string | null;
  status: string;
}

const templateOptions = [
  { label: "单设备(1 视频 + 1 报警)", value: "single" },
  { label: "8 通道 NVR", value: "nvr-8ch" },
  { label: "跨区划演示(3 区 × 2 通道)", value: "civil-3x2" },
  { label: "16 通道大型监控", value: "large-16ch" },
];
const template = ref("nvr-8ch");
const nodes = ref<ChannelNode[]>([]);

const typeLabel: Record<string, string> = {
  Device: "设备(根)",
  BusinessGroup: "业务分组",
  VirtualOrg: "虚拟组织",
  VideoChannel: "视频通道",
  AlarmChannel: "报警通道",
};
const typeOptions = Object.entries(typeLabel).map(([value, label]) => ({ label, value }));

async function loadTemplate() {
  try {
    nodes.value = await invoke<ChannelNode[]>("load_catalog_template", { template: template.value });
    message.success(`已载入模板,共 ${nodes.value.length} 个节点`);
  } catch (e) {
    message.error(String(e));
  }
}

async function refresh() {
  try {
    nodes.value = await invoke<ChannelNode[]>("get_catalog_tree");
  } catch {
    // 设备未启动时忽略
    nodes.value = [];
  }
}
function countBy(type: string): number {
  return nodes.value.filter((n) => n.node_type === type).length;
}

// ── 新增/编辑弹窗 ──
const showEdit = ref(false);
const editing = ref<ChannelNode>(blankNode());
const isNew = ref(true);

function blankNode(): ChannelNode {
  return { id: "", node_type: "VideoChannel", name: "", parent_id: "", civil_code: null, status: "ON" };
}

function openAdd() {
  editing.value = blankNode();
  // 缺省父节点取根设备(第一个 Device 节点)。
  const root = nodes.value.find((n) => n.node_type === "Device");
  if (root) editing.value.parent_id = root.id;
  isNew.value = true;
  showEdit.value = true;
}

function openEdit(row: ChannelNode) {
  editing.value = { ...row };
  isNew.value = false;
  showEdit.value = true;
}

async function saveNode() {
  if (!editing.value.id || !editing.value.name) {
    message.warning("通道 ID 与名称必填");
    return;
  }
  try {
    const msg = await invoke<string>("upsert_channel", { node: editing.value });
    message.success(msg);
    showEdit.value = false;
    await refresh();
  } catch (e) {
    message.error(String(e));
  }
}

function removeNode(row: ChannelNode) {
  dialog.warning({
    title: "删除目录节点",
    content: `确定删除“${row.name}”（${row.id}）吗？该操作会向已订阅平台发送增量通知。`,
    positiveText: "删除",
    negativeText: "取消",
    async onPositiveClick() {
      try {
        await invoke<string>("remove_channel", { id: row.id });
        message.success("已删除通道");
        await refresh();
      } catch (e) {
        message.error(String(e));
      }
    },
  });
}

const typeTagType: Record<string, "warning" | "info" | "success" | "default"> = {
  Device: "info", BusinessGroup: "default", VirtualOrg: "default",
  VideoChannel: "success", AlarmChannel: "warning",
};
const columns: DataTableColumns<ChannelNode> = [
  {
    title: "名称", key: "name", width: 150, ellipsis: { tooltip: true },
    render: (r) => h("span", { style: "font-weight:500" }, r.name),
  },
  {
    title: "类型", key: "node_type", width: 100,
    render: (r) => h(NTag, { size: "small", type: typeTagType[r.node_type] ?? "default", bordered: false }, { default: () => typeLabel[r.node_type] ?? r.node_type }),
  },
  {
    title: "国标 ID", key: "id", minWidth: 200,
    render: (r) => h("span", { style: "font-family:monospace;font-size:12px;white-space:nowrap" }, r.id),
  },
  {
    title: "父节点", key: "parent_id", minWidth: 200,
    render: (r) => h("span", { style: "font-family:monospace;font-size:12px;color:var(--text-tertiary);white-space:nowrap" }, r.parent_id),
  },
  { title: "区划码", key: "civil_code", width: 84, render: (r) => r.civil_code ?? "—" },
  {
    title: "状态", key: "status", width: 72,
    render: (r) => h(NTag, { size: "small", type: r.status === "ON" ? "success" : "error", bordered: false }, { default: () => (r.status === "ON" ? "在线" : "离线") }),
  },
  {
    title: "操作", key: "actions", width: 130,
    render: (r) =>
      h(NSpace, {}, {
        default: () => [
          h(NButton, { size: "tiny", onClick: () => openEdit(r) }, { default: () => "编辑" }),
          h(NButton, { size: "tiny", type: "error", disabled: r.node_type === "Device", onClick: () => removeNode(r) }, { default: () => "删除" }),
        ],
      }),
  },
];

import { h } from "vue";
onMounted(() => {
  refresh();
});
onActivated(refresh);
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div class="page-title">多通道目录</div>
      <div class="page-sub">虚拟通道 CRUD + 目录模板;通道变更实时增量 NOTIFY 推送平台(需先在单设备页启动设备)</div>
    </div>

    <div class="glass-card panel">
      <n-space align="center">
        <span class="label">目录模板</span>
        <n-select v-model:value="template" :options="templateOptions" style="width: 260px" />
        <n-button type="primary" @click="loadTemplate">载入模板</n-button>
        <n-button @click="refresh">刷新</n-button>
        <n-button type="info" @click="openAdd">+ 新增通道</n-button>
        <span v-if="nodes.length" class="chan-summary">
          共 {{ nodes.length }} 节点 · 视频 {{ countBy('VideoChannel') }} · 报警 {{ countBy('AlarmChannel') }} · 分组 {{ countBy('BusinessGroup') + countBy('VirtualOrg') }}
        </span>
      </n-space>
      <n-data-table
        :columns="columns"
        :data="nodes"
        :bordered="false"
        size="small"
        style="margin-top: 16px"
        :scroll-x="900"
        :row-key="(r: ChannelNode) => r.id"
      />
      <div v-if="nodes.length === 0" class="empty">设备未启动或目录为空。请到「单设备联调」启动设备,或载入模板。</div>
    </div>


    <n-modal v-model:show="showEdit" preset="card" :title="isNew ? '新增通道' : '编辑通道'" style="max-width: 480px">
      <n-form label-placement="top">
        <n-form-item label="通道 ID(20 位国标)">
          <n-input v-model:value="editing.id" placeholder="如 34020000001320000009" :disabled="!isNew" />
        </n-form-item>
        <n-form-item label="名称">
          <n-input v-model:value="editing.name" placeholder="通道名称" />
        </n-form-item>
        <n-form-item label="类型">
          <n-select v-model:value="editing.node_type" :options="typeOptions" />
        </n-form-item>
        <n-form-item label="父节点 ID">
          <n-input v-model:value="editing.parent_id" placeholder="父节点国标 ID(根设备 ID)" />
        </n-form-item>
        <n-form-item label="行政区划码(可选)">
          <n-input v-model:value="editing.civil_code" placeholder="如 310115" />
        </n-form-item>
      </n-form>
      <template #footer>
        <n-space justify="end">
          <n-button @click="showEdit = false">取消</n-button>
          <n-button type="primary" @click="saveNode">保存</n-button>
        </n-space>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
.page { max-width: 980px; }
.page-header { margin-bottom: 20px; }
.page-title { font-size: 24px; font-weight: 700; color: var(--text-primary); }
.page-sub { font-size: 13px; color: var(--text-tertiary); margin-top: 6px; }
.panel { padding: 24px; }
.label { color: var(--text-secondary); font-size: 13px; }
.empty { color: var(--text-tertiary); font-size: 13px; margin-top: 12px; text-align: center; padding: 20px; }
.chan-summary { font-size: 12.5px; color: var(--text-tertiary); margin-left: auto; }

/* 表格半透明质感:去掉 naive-ui 默认白底,融入磨砂卡片 */
:deep(.n-data-table),
:deep(.n-data-table .n-data-table-th),
:deep(.n-data-table .n-data-table-td),
:deep(.n-data-table .n-data-table-table),
:deep(.n-data-table-base-table) { background-color: transparent !important; }
:deep(.n-data-table .n-data-table-th) {
  background-color: rgba(120,130,150,0.06) !important;
  color: var(--text-secondary); font-weight: 600;
  border-bottom: 1px solid rgba(120,130,150,0.12) !important;
}
:deep(.n-data-table .n-data-table-td) {
  border-bottom: 1px solid rgba(120,130,150,0.08) !important;
}
:deep(.n-data-table .n-data-table-tr:hover .n-data-table-td) {
  background-color: rgba(56,132,255,0.05) !important;
}
:deep(.n-data-table .n-data-table-empty) { background-color: transparent !important; }
.section-title { font-size: 16px; font-weight: 600; color: var(--text-primary); }
.section-hint { font-size: 12px; color: var(--text-tertiary); margin-top: 4px; }
</style>
