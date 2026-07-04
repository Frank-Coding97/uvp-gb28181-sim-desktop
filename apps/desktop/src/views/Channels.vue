<script setup lang="ts">
// 多通道目录管理页(FR-34):模板选择 + 目录树 CRUD + OSD 配置(配置面对齐上游,不烧入画面)。
// 通道操作需设备已在「单设备联调」页启动;OSD 配置仅本地持久化(与上游一致,不进协议)。
import { ref, onMounted, onActivated } from "vue";
import {
  NButton, NSpace, NSelect, NDataTable, NModal, NForm, NFormItem, NInput,
  NTag, NSwitch, NInputNumber, useMessage, type DataTableColumns,
} from "naive-ui";
import { invoke } from "@tauri-apps/api/core";

const message = useMessage();

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

async function removeNode(row: ChannelNode) {
  try {
    await invoke<string>("remove_channel", { id: row.id });
    message.success("已删除通道");
    await refresh();
  } catch (e) {
    message.error(String(e));
  }
}

const columns: DataTableColumns<ChannelNode> = [
  { title: "名称", key: "name", width: 160 },
  {
    title: "类型", key: "node_type", width: 110,
    render: (r) => h(NTag, { size: "small", type: r.node_type === "AlarmChannel" ? "warning" : "default" }, { default: () => typeLabel[r.node_type] ?? r.node_type }),
  },
  { title: "国标 ID", key: "id", width: 190 },
  { title: "父节点", key: "parent_id", width: 190 },
  { title: "区划码", key: "civil_code", width: 90, render: (r) => r.civil_code ?? "—" },
  {
    title: "状态", key: "status", width: 70,
    render: (r) => h(NTag, { size: "small", type: r.status === "ON" ? "success" : "error" }, { default: () => r.status }),
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

// ── OSD 配置(FR-35,配置面对齐上游:本地持久化,不烧入画面/不进协议)──
const OSD_KEY = "uvp_osd_config";
const osd = ref({
  timestamp: true, timestampPos: "TOP_LEFT",
  channelName: true, channelNamePos: "TOP_RIGHT",
  watermark: false, watermarkText: "UVP-Sim", watermarkAlpha: 0.28,
  size: "MEDIUM",
});
const posOptions = [
  { label: "左上", value: "TOP_LEFT" }, { label: "右上", value: "TOP_RIGHT" },
  { label: "左下", value: "BOTTOM_LEFT" }, { label: "右下", value: "BOTTOM_RIGHT" },
];
const sizeOptions = [
  { label: "小(3.5%)", value: "SMALL" }, { label: "中(5%)", value: "MEDIUM" }, { label: "大(7%)", value: "LARGE" },
];
function saveOsd() {
  localStorage.setItem(OSD_KEY, JSON.stringify(osd.value));
  message.success("OSD 配置已保存(本地渲染叠加,不影响平台收流)");
}

import { h } from "vue";
onMounted(() => {
  const saved = localStorage.getItem(OSD_KEY);
  if (saved) { try { Object.assign(osd.value, JSON.parse(saved)); } catch {} }
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
      </n-space>
      <n-data-table
        :columns="columns"
        :data="nodes"
        :bordered="false"
        size="small"
        style="margin-top: 16px"
        :row-key="(r: ChannelNode) => r.id"
      />
      <div v-if="nodes.length === 0" class="empty">设备未启动或目录为空。请到「单设备联调」启动设备,或载入模板。</div>
    </div>

    <div class="glass-card panel" style="margin-top: 20px">
      <div class="section-title">OSD 叠加配置</div>
      <div class="section-hint">
        与上游一致:OSD 为本地渲染叠加(时间/通道名/水印),不进 GB28181 协议、不影响平台收流。
      </div>
      <n-form label-placement="left" label-width="96" style="max-width: 520px; margin-top: 12px">
        <n-form-item label="时间戳">
          <n-switch v-model:value="osd.timestamp" />
          <n-select v-model:value="osd.timestampPos" :options="posOptions" size="small" style="width: 110px; margin-left: 12px" :disabled="!osd.timestamp" />
        </n-form-item>
        <n-form-item label="通道名">
          <n-switch v-model:value="osd.channelName" />
          <n-select v-model:value="osd.channelNamePos" :options="posOptions" size="small" style="width: 110px; margin-left: 12px" :disabled="!osd.channelName" />
        </n-form-item>
        <n-form-item label="水印">
          <n-switch v-model:value="osd.watermark" />
          <n-input v-model:value="osd.watermarkText" size="small" placeholder="水印文字" style="width: 160px; margin-left: 12px" :disabled="!osd.watermark" />
        </n-form-item>
        <n-form-item label="水印透明度" v-if="osd.watermark">
          <n-input-number v-model:value="osd.watermarkAlpha" :min="0" :max="1" :step="0.02" style="width: 140px" />
        </n-form-item>
        <n-form-item label="字号">
          <n-select v-model:value="osd.size" :options="sizeOptions" style="width: 160px" />
        </n-form-item>
      </n-form>
      <n-button type="primary" @click="saveOsd">保存 OSD 配置</n-button>
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
.section-title { font-size: 16px; font-weight: 600; color: var(--text-primary); }
.section-hint { font-size: 12px; color: var(--text-tertiary); margin-top: 4px; }
</style>
