<script setup lang="ts">
import { computed, onActivated, onMounted, ref, watch } from "vue";
import { NButton, NEmpty, NInput, NInputNumber, NModal, NSelect, NTag, useMessage } from "naive-ui";
import { useDevice } from "../device";
import {
  clearAlarmHistory,
  listAlarmCapability,
  listAlarmHistory,
  previewAlarmXml,
  reportAlarm,
  resetAlarm,
  type AlarmMethodCapability,
  type AlarmRecord,
  type AlarmReport,
} from "../platform";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const { form, deviceState } = useDevice();

const PRIORITIES = [
  { label: "1 紧急一级", value: 1 },
  { label: "2 紧急二级", value: 2 },
  { label: "3 紧急三级", value: 3 },
  { label: "4 一般报警", value: 4 },
];
/** 方式与类型都取自协议层的标准表，见 `get_alarm_capability`。 */
const capability = ref<AlarmMethodCapability[]>([]);
const TIME_OPTIONS = [
  { label: "现在", value: 0 },
  { label: "5 分钟前", value: 5 },
  { label: "15 分钟前", value: 15 },
  { label: "30 分钟前", value: 30 },
  { label: "60 分钟前", value: 60 },
];

/**
 * 固定单只发标准里成立的组合（方式 + 类型必须成对合法）：
 * 方式 2 → 1-5；方式 5 → 1-13；方式 6 → 1-2；方式 7 不携带类型。
 * 以前「视频丢失」发的是方式 5/类型 1（标准里那是人工视频报警），
 * 「设备故障」发方式 6/类型 4、「人工触发」发方式 7/类型 5 —— 平台都只能显示「未知」。
 */
const PRESETS: Array<{ label: string; report: AlarmReport }> = [
  { label: "视频丢失", report: { priority: 4, method: 2, alarm_type: 1, description: "通道视频信号丢失" } },
  { label: "设备防拆", report: { priority: 2, method: 2, alarm_type: 2, description: "设备外壳被打开" } },
  { label: "存储满", report: { priority: 4, method: 2, alarm_type: 3, description: "存储空间不足" } },
  { label: "移动侦测", report: { priority: 4, method: 5, alarm_type: 2, description: "画面检测到运动目标" } },
  { label: "磁盘故障", report: { priority: 2, method: 6, alarm_type: 1, description: "录像存储磁盘异常" } },
  { label: "人工报警", report: { priority: 3, method: 7, alarm_type: null, description: "值守人员手动触发" } },
];

const draft = ref<AlarmReport>({
  priority: 1,
  method: 5,
  alarm_type: 2,
  description: "移动侦测报警",
  longitude: null,
  latitude: null,
  minutes_ago: 0,
});
const history = ref<AlarmRecord[]>([]);
const sending = ref(false);
const loading = ref(false);
const xml = ref("");
const showXml = ref(false);
const busy = ref("");

const methodOptions = computed(() =>
  capability.value.map((item) => ({ label: `${item.value} ${item.name}`, value: item.value })),
);
const typeOptions = computed(() => {
  const current = capability.value.find((item) => item.value === draft.value.method);
  return (current?.types ?? []).map((item) => ({ label: `${item.value} ${item.name}`, value: item.value }));
});
const typeHint = computed(() =>
  typeOptions.value.length ? "清空则报文不带 <Info>" : "该报警方式不携带类型",
);
const typePlaceholder = computed(() =>
  typeOptions.value.length ? "报警类型" : "该方式无类型",
);

const deviceIdText = computed(() => form.value.device_id || "未配置设备 ID");
const canSend = computed(() => deviceState.value !== "Disconnected" && !sending.value);

async function loadCapability() {
  if (!isTauri || capability.value.length) return;
  try {
    capability.value = await listAlarmCapability();
  } catch (error) {
    message.error(`读取报警类型表失败：${String(error)}`);
  }
}

// 换方式后旧类型可能不合法（如从方式 5 切到方式 6），直接清空而不是发一个平台查不到的值。
watch(
  () => draft.value.method,
  () => {
    const allowed = typeOptions.value.map((item) => item.value);
    const current = draft.value.alarm_type;
    if (current !== null && current !== undefined && !allowed.includes(current)) {
      draft.value.alarm_type = undefined;
    }
  },
);

function applyPreset(preset: (typeof PRESETS)[number]) {
  draft.value = {
    ...draft.value,
    ...preset.report,
    longitude: draft.value.longitude,
    latitude: draft.value.latitude,
    minutes_ago: draft.value.minutes_ago,
  };
}

/** 随机发一条：模板、等级、时间都随机，用来验证平台对不同取值的处理。 */
function randomize() {
  const preset = PRESETS[Math.floor(Math.random() * PRESETS.length)];
  const minutes = [0, 5, 15, 30, 60][Math.floor(Math.random() * 5)];
  draft.value = {
    priority: preset.report.priority,
    method: preset.report.method,
    alarm_type: preset.report.alarm_type,
    description: preset.report.description,
    longitude: draft.value.longitude,
    latitude: draft.value.latitude,
    minutes_ago: minutes,
  };
  return draft.value;
}

function fmtTime(ms: number) {
  const date = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function methodLabel(code: number) {
  const method = capability.value.find((item) => item.value === code);
  return method ? `${method.value} ${method.name}` : String(code);
}

function statusText(record: AlarmRecord) {
  if (record.error) return "失败";
  if (record.status === 200) return "已送达";
  if (record.status === null) return "无应答";
  return `应答 ${record.status}`;
}

function statusType(record: AlarmRecord) {
  if (record.error || (record.status !== null && record.status !== 200)) return "error";
  return "success";
}

async function load() {
  if (!isTauri) return;
  loading.value = true;
  try {
    history.value = await listAlarmHistory();
  } catch (error) {
    message.error(`读取报警历史失败：${String(error)}`);
  } finally {
    loading.value = false;
  }
}

async function send() {
  sending.value = true;
  try {
    const record = await reportAlarm(draft.value);
    message.success(
      `报警已发送（${record.via_notify ? "订阅 NOTIFY" : "独立 MESSAGE"}，平台应答 ${record.status ?? "无"}）`,
    );
  } catch (error) {
    // 失败也会落进历史，所以这里先刷新再提示，避免用户以为没发出去就没记录。
    message.error(`报警发送失败：${String(error)}`);
  } finally {
    sending.value = false;
    await load();
  }
}

async function sendRandom() {
  randomize();
  await send();
}

async function openXml() {
  try {
    xml.value = await previewAlarmXml(draft.value);
    showXml.value = true;
  } catch (error) {
    message.error(`生成 XML 预览失败：${String(error)}`);
  }
}

async function doReset() {
  busy.value = "reset";
  try {
    message.success(await resetAlarm());
  } catch (error) {
    message.error(`复位失败：${String(error)}`);
  } finally {
    busy.value = "";
  }
}

async function clearHistory() {
  busy.value = "clear";
  try {
    await clearAlarmHistory();
    history.value = [];
    message.success("已清空报警历史");
  } catch (error) {
    message.error(`清空失败：${String(error)}`);
  } finally {
    busy.value = "";
  }
}

onMounted(() => {
  void loadCapability();
  void load();
});
onActivated(() => { void load(); });
</script>

<template>
  <div class="page page-wrap">
    <div class="layout">
      <section class="glass-card panel">
        <div class="panel-head">
          <div class="panel-title">报警单</div>
          <span class="source">报警源 <code class="channel">{{ deviceIdText }}</code></span>
        </div>

        <div class="presets">
          <span class="label">固定单</span>
          <button v-for="preset in PRESETS" :key="preset.label" class="preset" @click="applyPreset(preset)">
            {{ preset.label }}
          </button>
        </div>

        <div class="form-grid">
          <div class="field">
            <span class="label">报警等级 <code>AlarmPriority</code></span>
            <n-select v-model:value="draft.priority" :options="PRIORITIES" size="small" />
          </div>

          <div class="field">
            <span class="label">报警方式 <code>AlarmMethod</code></span>
            <n-select v-model:value="draft.method" :options="methodOptions" size="small" />
          </div>

          <div class="field">
            <span class="label">报警类型 <code>AlarmType</code></span>
            <n-select
              v-model:value="draft.alarm_type"
              :options="typeOptions"
              :placeholder="typePlaceholder"
              :disabled="!typeOptions.length"
              size="small"
              clearable
            />
            <span class="hint-inline">{{ typeHint }}</span>
          </div>

          <div class="field">
            <span class="label">报警时间</span>
            <n-select v-model:value="draft.minutes_ago" :options="TIME_OPTIONS" size="small" />
            <span class="hint-inline">只改 AlarmTime，不延迟发送</span>
          </div>

          <div class="field span-2">
            <span class="label">报警描述</span>
            <n-input v-model:value="draft.description" size="small" placeholder="如：磁吸触发" />
          </div>

          <div class="field span-2">
            <span class="label">经纬度（可选）</span>
            <div class="coords">
              <n-input-number v-model:value="draft.longitude" size="small" :step="0.000001" placeholder="经度" />
              <n-input-number v-model:value="draft.latitude" size="small" :step="0.000001" placeholder="纬度" />
            </div>
          </div>
        </div>

        <div class="panel-foot">
          <div class="actions">
            <n-button type="primary" size="small" :loading="sending" :disabled="!canSend" @click="send">发送报警</n-button>
            <n-button size="small" :disabled="!canSend" @click="sendRandom">随机发一条</n-button>
            <n-button size="small" :disabled="!canSend" @click="openXml">预览 XML</n-button>
            <n-button
              class="reset-btn"
              size="small"
              quaternary
              :loading="busy === 'reset'"
              :disabled="deviceState === 'Disconnected'"
              @click="doReset"
            >复位本机报警</n-button>
          </div>
          <div class="hint">复位只清设备本地 alarming 标记（下一次 DeviceStatus 回到“无报警”），不补发任何报文。</div>
        </div>
      </section>

      <section class="glass-card panel history-panel">
        <div class="history-head">
          <div class="panel-title">报警历史 <small>最近 {{ history.length }} 条</small></div>
          <div class="history-actions">
            <n-button size="tiny" :loading="loading" @click="load">刷新</n-button>
            <n-button size="tiny" quaternary :loading="busy === 'clear'" :disabled="!history.length" @click="clearHistory">清空</n-button>
          </div>
        </div>

        <div v-if="!history.length" class="empty-wrap">
          <n-empty description="还没有发送过报警">
            <template #extra>
              <span class="empty-tip">发送后这里会显示时间、平台应答与发送路径</span>
            </template>
          </n-empty>
        </div>

        <div v-else class="rows">
          <div v-for="record in history" :key="record.id" class="row-item">
            <div class="row-top">
              <span class="time">{{ fmtTime(record.captured_at_ms) }}</span>
              <n-tag size="small" :type="statusType(record)" :bordered="false">{{ statusText(record) }}</n-tag>
              <span class="route">{{ record.via_notify ? "订阅 NOTIFY" : "独立 MESSAGE" }}</span>
            </div>
            <div class="row-main">
              <b>P{{ record.priority }} · {{ methodLabel(record.method) }}</b>
              <span>{{ record.description || "（无描述）" }}</span>
            </div>
            <div class="row-meta">
              <span>AlarmTime {{ record.alarm_time }}</span>
              <span v-if="record.alarm_type">类型 {{ record.alarm_type }}</span>
              <span v-if="record.longitude !== null && record.latitude !== null">
                {{ record.longitude }}, {{ record.latitude }}
              </span>
              <span v-if="record.error" class="err">{{ record.error }}</span>
            </div>
          </div>
        </div>
      </section>
    </div>

    <n-modal v-model:show="showXml" preset="card" class="xml-card" title="报警 XML 预览（SN 固定为 1）">
      <pre class="xml">{{ xml }}</pre>
    </n-modal>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.layout { display: grid; grid-template-columns: minmax(0, 640px) minmax(0, 1fr); gap: 18px; align-items: stretch; }.panel { display: flex; flex-direction: column; padding: 18px; }
.panel-head { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; }
.panel-title { color: var(--text-primary); font-size: 15px; font-weight: 700; }
.panel-title small { margin-left: 6px; color: var(--text-tertiary); font-size: 11px; font-weight: 500; }
.source { color: var(--text-tertiary); font-size: 11px; }
.channel { color: var(--text-secondary); font-size: 11px; font-variant-numeric: tabular-nums; }
.presets { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin: 14px 0 16px; }
.preset { padding: 5px 11px; border: 1px solid var(--border-default); border-radius: 999px; background: rgba(255,255,255,.6); color: var(--text-secondary); font-size: 12px; line-height: 1.2; cursor: pointer; transition: var(--transition); }
.preset:hover { border-color: var(--border-accent); background: var(--bg-hover); color: var(--accent); }
.form-grid { display: grid; grid-template-columns: 1fr 1fr; column-gap: 16px; row-gap: 12px; }
.field { display: grid; gap: 6px; align-content: start; }
.field.span-2 { grid-column: span 2; }
.field .coords { display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }
.label { display: flex; align-items: baseline; gap: 6px; color: var(--text-tertiary); font-size: 11px; }
.label code { color: var(--text-tertiary); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 10px; opacity: .75; }
.hint-inline { color: var(--text-tertiary); font-size: 10px; line-height: 1.4; }
.panel-foot { margin-top: auto; }
.actions { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; margin-top: 18px; padding-top: 14px; border-top: 1px solid var(--border-default); }
.reset-btn { margin-left: auto; }
.hint { margin-top: 12px; color: var(--text-tertiary); font-size: 10.5px; line-height: 1.6; }
.history-panel { min-height: 452px; max-height: 560px; }
.history-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 12px; }
.history-actions { display: flex; gap: 6px; }
.empty-wrap { flex: 1; display: grid; place-items: center; min-height: 280px; padding: 24px 0; }
.empty-tip { color: var(--text-tertiary); font-size: 11px; }
.rows { display: grid; flex: 1; align-content: start; gap: 10px; min-height: 0; overflow: auto; }
.row-item { display: grid; gap: 5px; padding: 10px 12px; border: 1px solid var(--border-default); border-radius: 10px; background: rgba(255,255,255,.5); }
.row-top { display: flex; align-items: center; gap: 8px; }
.row-top .time { color: var(--text-tertiary); font-size: 11px; font-variant-numeric: tabular-nums; }
.route { color: var(--text-tertiary); font-size: 10px; }
.row-main { display: flex; align-items: baseline; gap: 8px; }
.row-main b { color: var(--text-secondary); font-size: 12px; }
.row-main span { color: var(--text-secondary); font-size: 12px; overflow-wrap: anywhere; }
.row-meta { display: flex; flex-wrap: wrap; gap: 10px; color: var(--text-tertiary); font-size: 10px; }
.row-meta .err { color: var(--error); }
.xml-card { width: min(880px, 92vw); }
.xml { margin: 0; padding: 12px; border-radius: 10px; background: #071424; color: #cfe0f7; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.6; overflow: auto; max-height: 60vh; }
@media (max-width: 1360px) { .layout { grid-template-columns: minmax(0, 560px) minmax(0, 1fr); } }
@media (max-width: 1180px) { .layout { grid-template-columns: 1fr; } .history-panel { min-height: 360px; } }
@media (max-width: 640px) { .form-grid { grid-template-columns: 1fr; } .field.span-2 { grid-column: auto; } .reset-btn { margin-left: 0; } }
</style>
