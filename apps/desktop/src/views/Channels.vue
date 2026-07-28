<script setup lang="ts">
// M5 目录查询三件套通道管理页面.
//
// v4 契约 (D1/D3/D4/D5/D6):
//   - D1: 唯一保存路径 apply_config (encoding + channels 一起提交)
//   - D3: 前端本地 nextChannelIds 生成 ID, 只改 draft, 不立即调 daemon (R3-B3)
//   - D4: 只用 5 个 Tauri command (list_channels / update_channel / delete_channel /
//         apply_config / get_manscdp_encoding), 无 create_channels_batch
//   - D6: onMounted 时 daemon 已由 App.vue hydrate 到 localStorage 一致状态
import { computed, inject, onMounted, ref, type Ref } from "vue";
import { onBeforeRouteLeave } from "vue-router";
import { invoke } from "@tauri-apps/api/core";
import {
  NButton, NDrawer, NDrawerContent, NEmpty, NForm, NFormItem, NIcon,
  NInput, NInputNumber, NModal, NRadioButton, NRadioGroup, NSelect,
  useDialog, useMessage,
} from "naive-ui";
import {
  AddOutline, DownloadOutline, SaveOutline, SearchOutline,
  TrashOutline,
} from "@vicons/ionicons5";

import type {
  ApplyConfigResp, Channel, ChannelType, EncodingMode,
} from "../types/channel";
import { STORE_CHANNELS, STORE_ENCODING } from "../types/channel";

const message = useMessage();
const dialog = useDialog();

// ── 状态 ──
// draft: 前端工作副本 (未保存), saved: 最后一次成功 apply 到 daemon 的快照
const draft = ref<Channel[]>([]);
const saved = ref<Channel[]>([]);
const encoding = ref<EncodingMode>("GB18030");
const savedEncoding = ref<EncodingMode>("GB18030");
const searchKeyword = ref("");

// deviceIdPrefix: 从 SIP 配置读, 用于本地 ID 生成 (D3 R3-B3)
const deviceIdPrefix = ref<string>("");
const deviceIdFull = ref<string>("");

// isDirty: draft/encoding 与 saved 快照不一致
const isDirty = computed(() => {
  return JSON.stringify(draft.value) !== JSON.stringify(saved.value)
    || encoding.value !== savedEncoding.value;
});

// hydrate 状态 (D6): App.vue provide
const hydrateReady = inject<Ref<boolean>>("hydrateReady", ref(true));

// 搜索过滤
const filteredChannels = computed(() => {
  if (!searchKeyword.value) return draft.value;
  const kw = searchKeyword.value.toLowerCase();
  return draft.value.filter(
    (ch) => ch.name.toLowerCase().includes(kw) || ch.id.includes(kw)
  );
});

// ── onMounted: 从 daemon 拉当前生效快照 (App.vue 已 hydrate) ──
onMounted(async () => {
  await loadFromDaemon();
  loadSipConfig();
});

async function loadFromDaemon() {
  try {
    const channels = await invoke<Channel[]>("list_channels");
    draft.value = channels;
    saved.value = JSON.parse(JSON.stringify(channels));

    const encResp = await invoke<{ encoding: EncodingMode }>("get_manscdp_encoding");
    encoding.value = encResp.encoding;
    savedEncoding.value = encResp.encoding;
  } catch (e) {
    message.error("从 daemon 加载失败: " + String(e));
    // R3-H1: 不用 daemon 结果覆盖 draft, 保持空
  }
}

function loadSipConfig() {
  try {
    const raw = localStorage.getItem("uvp_sip_config");
    if (!raw) return;
    const cfg = JSON.parse(raw);
    if (typeof cfg.device_id === "string" && cfg.device_id.length >= 20) {
      deviceIdFull.value = cfg.device_id;
      deviceIdPrefix.value = cfg.device_id.slice(0, 10);
    }
  } catch { /* ignore */ }
}

// ── 新增 (R3-B3: 本地 ID 生成 + 只改 draft) ──
const showAddModal = ref(false);
const addForm = ref({
  type: "video" as ChannelType,
  name: "",
  count: 1,
});

/** 前端本地 ID 生成 (对齐 Go NextChannelIDs 算法). */
function nextChannelIds(existing: Channel[], type: ChannelType, count: number): string[] {
  if (!deviceIdPrefix.value) throw new Error("设备 ID 未加载, 请先在'设备模拟'页配置");
  const typeCode = type === "video" ? "132" : "134";
  const prefix = deviceIdPrefix.value + typeCode;
  let maxSeq = 0;
  for (const ch of existing) {
    if (ch.id.length === 20 && ch.id.slice(0, 13) === prefix) {
      const seq = parseInt(ch.id.slice(13), 10);
      if (!isNaN(seq) && seq > maxSeq) maxSeq = seq;
    }
  }
  if (maxSeq + count > 9_999_999) {
    throw new Error(`序号溢出: 已用 ${maxSeq}, 请求 +${count}`);
  }
  return Array.from({ length: count }, (_, i) =>
    prefix + String(maxSeq + i + 1).padStart(7, "0")
  );
}

function handleAdd() {
  if (!addForm.value.name.trim()) {
    message.warning("请填写通道名称");
    return;
  }
  try {
    const ids = nextChannelIds(draft.value, addForm.value.type, addForm.value.count);
    const parentId = deviceIdFull.value || "";
    const newChannels: Channel[] = ids.map((id, i) => ({
      id,
      name: addForm.value.count > 1
        ? `${addForm.value.name.trim()}-${String(i + 1).padStart(3, "0")}`
        : addForm.value.name.trim(),
      type: addForm.value.type,
      manufacturer: "UVP",
      model: "SIM-Desktop-v2",
      owner: "UVP-Simulator",
      civilCode: "340200",
      address: "",
      status: "ON",
      parentId,
    }));
    draft.value = [...draft.value, ...newChannels];
    message.success(`已添加 ${newChannels.length} 个通道 (未保存)`);
    showAddModal.value = false;
    addForm.value.name = "";
    addForm.value.count = 1;
  } catch (e) {
    message.error("添加失败: " + String(e));
  }
}

// ── 编辑 Drawer ──
const showEditDrawer = ref(false);
const editingChannel = ref<Channel | null>(null);

function openEditDrawer(ch: Channel) {
  editingChannel.value = { ...ch };
  showEditDrawer.value = true;
}

function handleEditConfirm() {
  if (!editingChannel.value) return;
  const idx = draft.value.findIndex((c) => c.id === editingChannel.value!.id);
  if (idx >= 0) {
    draft.value[idx] = { ...editingChannel.value };
    message.success("已修改 (未保存)");
  }
  showEditDrawer.value = false;
}

// ── 删除 ──
function handleDelete(ch: Channel) {
  dialog.warning({
    title: "确认删除",
    content: `删除通道 ${ch.name} (${ch.id})?`,
    positiveText: "删除",
    negativeText: "取消",
    onPositiveClick: () => {
      draft.value = draft.value.filter((c) => c.id !== ch.id);
      message.success("已删除 (未保存)");
    },
  });
}

// ── 保存: apply_config 原子提交 (D1 唯一保存路径) ──
async function handleSave() {
  try {
    const result = await invoke<ApplyConfigResp>("apply_config", {
      req: { encoding: encoding.value, channels: draft.value },
    });
    // 防御性 check: daemon 返回格式不对时早停 (R3-H1)
    if (!result || !Array.isArray(result.channels) || !result.encoding) {
      throw new Error(`daemon 返回格式异常: ${JSON.stringify(result)}`);
    }
    // daemon 成功了才落 localStorage
    localStorage.setItem(STORE_CHANNELS, JSON.stringify(result.channels));
    localStorage.setItem(STORE_ENCODING, result.encoding);
    // 更新 saved 快照
    saved.value = JSON.parse(JSON.stringify(result.channels));
    savedEncoding.value = result.encoding;
    draft.value = result.channels;
    message.success("已保存并生效");
  } catch (e) {
    message.error("保存失败: " + String(e));
  }
}

// ── 导出 (剪贴板) ──
async function handleExport() {
  const payload = JSON.stringify({
    encoding: encoding.value,
    channels: draft.value,
  }, null, 2);
  try {
    await navigator.clipboard.writeText(payload);
    message.success("已复制到剪贴板");
  } catch (e) {
    message.error("复制失败: " + String(e));
  }
}

// ── 导入 (P-H7 严格校验, 整体拒绝或整体应用) ──
const showImportModal = ref(false);
const importJson = ref("");

function handleImport() {
  let parsed: any;
  try {
    parsed = JSON.parse(importJson.value);
  } catch (e) {
    message.error("JSON 解析失败: " + String(e));
    return;
  }
  if (!Array.isArray(parsed.channels)) {
    message.error("payload 必须含 channels 数组");
    return;
  }
  // 全量校验
  for (let i = 0; i < parsed.channels.length; i++) {
    const ch = parsed.channels[i];
    if (typeof ch.id !== "string" || !/^[0-9]{20}$/.test(ch.id)) {
      message.error(`第 ${i + 1} 个通道 ID 非 20 位数字, 整体拒绝`);
      return;
    }
    if (ch.type !== "video" && ch.type !== "alarm") {
      message.error(`第 ${i + 1} 个通道 type 非法, 整体拒绝`);
      return;
    }
  }
  // 全绿 → 替换 draft
  draft.value = parsed.channels;
  if (
    parsed.encoding &&
    (parsed.encoding === "GB18030" || parsed.encoding === "GB2312" || parsed.encoding === "UTF-8")
  ) {
    encoding.value = parsed.encoding;
  }
  showImportModal.value = false;
  importJson.value = "";
  message.success(`已导入 ${parsed.channels.length} 个通道 (未保存)`);
}

// ── 离开守卫 (P-H6 Vue Router beforeRouteLeave, 支持异步 confirm) ──
onBeforeRouteLeave((_to, _from, next) => {
  if (!isDirty.value) {
    next();
    return;
  }
  dialog.warning({
    title: "有未保存的更改",
    content: "离开将丢失未保存的通道配置, 确认离开?",
    positiveText: "丢弃离开",
    negativeText: "留下",
    onPositiveClick: () => next(),
    onNegativeClick: () => next(false),
    onClose: () => next(false),
  });
});
</script>

<template>
  <div class="channels-page">
    <!-- 未 hydrate 完成提示 -->
    <div v-if="!hydrateReady" class="hydrate-warning">
      ⚠️ daemon 未就绪, 通道管理暂不可用 (回"设备模拟"页检查配置)
    </div>

    <!-- 工具栏 -->
    <div class="toolbar">
      <div class="toolbar-left">
        <n-input
          v-model:value="searchKeyword"
          placeholder="搜索通道名称或 ID"
          style="width: 240px"
        >
          <template #prefix><n-icon><SearchOutline /></n-icon></template>
        </n-input>
      </div>

      <div class="toolbar-right">
        <n-button @click="showAddModal = true" type="primary" :disabled="!hydrateReady">
          <template #icon><n-icon><AddOutline /></n-icon></template>
          新增
        </n-button>
        <n-button @click="showImportModal = true" :disabled="!hydrateReady">导入</n-button>
        <n-button @click="handleExport">
          <template #icon><n-icon><DownloadOutline /></n-icon></template>
          导出
        </n-button>
        <n-select
          v-model:value="encoding"
          :options="[
            { label: 'GB18030 (默认)', value: 'GB18030' },
            { label: 'GB2312 (兼容)', value: 'GB2312' },
            { label: 'UTF-8', value: 'UTF-8' },
          ]"
          style="width: 160px"
        />
        <n-button
          @click="handleSave"
          type="primary"
          :disabled="!isDirty || !hydrateReady"
        >
          <template #icon><n-icon><SaveOutline /></n-icon></template>
          保存{{ isDirty ? "*" : "" }}
        </n-button>
      </div>
    </div>

    <!-- 通道列表 -->
    <div class="channel-list">
      <n-empty v-if="filteredChannels.length === 0" description="暂无通道" />
      <div
        v-for="ch in filteredChannels"
        :key="ch.id"
        class="channel-item"
        @click="openEditDrawer(ch)"
      >
        <div class="channel-icon">
          {{ ch.type === "video" ? "📹" : "🚨" }}
        </div>
        <div class="channel-info">
          <div class="channel-name">{{ ch.name }}</div>
          <div class="channel-id">{{ ch.id }}</div>
        </div>
        <div class="channel-status" :class="ch.status.toLowerCase()">
          {{ ch.status }}
        </div>
        <n-button text @click.stop="handleDelete(ch)" type="error">
          <template #icon><n-icon><TrashOutline /></n-icon></template>
        </n-button>
      </div>
    </div>

    <div class="statusbar">
      {{ draft.length }} 个通道 · {{ isDirty ? "有未保存改动" : "已同步" }} ·
      当前编码 {{ encoding }} <span v-if="isDirty && encoding !== savedEncoding">(未保存)</span>
    </div>

    <!-- 新增 Modal -->
    <n-modal
      v-model:show="showAddModal"
      preset="card"
      title="新增通道"
      style="width: 480px"
    >
      <n-form>
        <n-form-item label="类型">
          <n-radio-group v-model:value="addForm.type">
            <n-radio-button value="video">视频通道</n-radio-button>
            <n-radio-button value="alarm">报警通道</n-radio-button>
          </n-radio-group>
        </n-form-item>
        <n-form-item label="名称">
          <n-input v-model:value="addForm.name" placeholder="摄像头一号" />
        </n-form-item>
        <n-form-item label="数量 (1-50)">
          <n-input-number v-model:value="addForm.count" :min="1" :max="50" />
        </n-form-item>
      </n-form>
      <template #footer>
        <n-button @click="showAddModal = false">取消</n-button>
        <n-button @click="handleAdd" type="primary">创建</n-button>
      </template>
    </n-modal>

    <!-- 导入 Modal -->
    <n-modal
      v-model:show="showImportModal"
      preset="card"
      title="导入 JSON"
      style="width: 640px"
    >
      <n-input
        v-model:value="importJson"
        type="textarea"
        :rows="12"
        placeholder='粘贴 {"encoding": "GB18030", "channels": [...]}'
      />
      <template #footer>
        <n-button @click="showImportModal = false">取消</n-button>
        <n-button @click="handleImport" type="primary">导入</n-button>
      </template>
    </n-modal>

    <!-- 编辑 Drawer -->
    <n-drawer v-model:show="showEditDrawer" :width="400" placement="right">
      <n-drawer-content title="编辑通道" closable>
        <n-form v-if="editingChannel">
          <n-form-item label="ID">
            <n-input :value="editingChannel.id" disabled />
          </n-form-item>
          <n-form-item label="名称">
            <n-input v-model:value="editingChannel.name" />
          </n-form-item>
          <n-form-item label="厂商">
            <n-input v-model:value="editingChannel.manufacturer" />
          </n-form-item>
          <n-form-item label="型号">
            <n-input v-model:value="editingChannel.model" />
          </n-form-item>
          <n-form-item label="行政区划码">
            <n-input v-model:value="editingChannel.civilCode" />
          </n-form-item>
          <n-form-item label="地址">
            <n-input v-model:value="editingChannel.address" />
          </n-form-item>
          <n-form-item label="状态">
            <n-radio-group v-model:value="editingChannel.status">
              <n-radio-button value="ON">在线</n-radio-button>
              <n-radio-button value="OFF">离线</n-radio-button>
            </n-radio-group>
          </n-form-item>
        </n-form>
        <template #footer>
          <n-button @click="showEditDrawer = false">取消</n-button>
          <n-button @click="handleEditConfirm" type="primary">应用到 draft</n-button>
        </template>
      </n-drawer-content>
    </n-drawer>
  </div>
</template>

<style scoped>
.channels-page {
  display: flex;
  flex-direction: column;
  height: 100%;
  padding: 16px;
  overflow: hidden;
}
.hydrate-warning {
  padding: 12px;
  margin-bottom: 12px;
  background: rgba(255, 200, 0, 0.15);
  border-radius: 8px;
  color: var(--warning);
}
.toolbar {
  display: flex;
  justify-content: space-between;
  margin-bottom: 16px;
  gap: 8px;
}
.toolbar-right {
  display: flex;
  gap: 8px;
  align-items: center;
}
.channel-list {
  flex: 1;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-right: 4px;
}
.channel-item {
  display: flex;
  align-items: center;
  padding: 12px;
  background: rgba(255, 255, 255, 0.5);
  border-radius: 8px;
  cursor: pointer;
  transition: background 0.2s;
  gap: 12px;
}
.channel-item:hover {
  background: rgba(255, 255, 255, 0.75);
}
.channel-icon {
  font-size: 24px;
}
.channel-info {
  flex: 1;
}
.channel-name {
  font-weight: 500;
  margin-bottom: 4px;
}
.channel-id {
  font-size: 12px;
  color: var(--text-tertiary, #666);
  font-family: ui-monospace, monospace;
}
.channel-status {
  padding: 4px 8px;
  border-radius: 4px;
  font-size: 12px;
}
.channel-status.on {
  background: var(--success, #52c41a);
  color: white;
}
.channel-status.off {
  background: #d9d9d9;
  color: #666;
}
.statusbar {
  margin-top: 12px;
  padding: 8px;
  text-align: center;
  color: var(--text-tertiary, #666);
  font-size: 12px;
}
</style>
