<script setup lang="ts">
// 单设备联调控制台(UC-1),高保真对齐参考原型 frost-blue:
// 顶部 4 指标卡 + 双栏配置卡(SIP 服务器 / 设备身份)。
import { ref, onMounted, onUnmounted, computed } from "vue";
import { NButton, NSpace, useMessage } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const message = useMessage();

const form = ref({
  server_host: "192.168.10.222",
  server_port: 8160,
  server_domain: "3502000000",
  device_id: "35020000001310000001",
  password: "wvp_sip_password",
  transport: "UDP",
  gb_version: "2022",
  channel_name: "Camera-1",
  video_source: "",
});

// 设备状态(device_state 事件驱动)。
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const deviceState = ref<DState>("Disconnected");
const startedAt = ref<number | null>(null);
const uptime = ref("--:--:--");
let timer: number | null = null;

const stateMeta = computed(() => {
  switch (deviceState.value) {
    case "Registering": return { text: "注册中", color: "var(--warning)" };
    case "Registered":  return { text: "已注册", color: "var(--success)" };
    case "InCall":      return { text: "推流中", color: "var(--accent)" };
    case "Failed":      return { text: "注册失败", color: "var(--error)" };
    default:            return { text: "未连接", color: "var(--text-tertiary)" };
  }
});
const running = computed(() => deviceState.value !== "Disconnected" && deviceState.value !== "Failed");
const canReport = computed(() => deviceState.value === "Registered" || deviceState.value === "InCall");

function fmtUptime() {
  if (!startedAt.value) { uptime.value = "--:--:--"; return; }
  const s = Math.floor((Date.now() - startedAt.value) / 1000);
  const h = String(Math.floor(s / 3600)).padStart(2, "0");
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const ss = String(s % 60).padStart(2, "0");
  uptime.value = `${h}:${m}:${ss}`;
}

async function startDevice() {
  try {
    const config = { ...form.value, video_source: form.value.video_source.trim() || null };
    const msg = await invoke<string>("start_device", { config });
    message.success(msg);
  } catch (e) {
    message.error(String(e));
    deviceState.value = "Failed";
  }
}
async function stopDevice() {
  try {
    await invoke<string>("stop_device");
    deviceState.value = "Disconnected";
    startedAt.value = null;
    message.info("设备已停止");
  } catch (e) { message.error(String(e)); }
}
async function fireAlarm() {
  try { message.success(await invoke<string>("fire_alarm", { description: "移动侦测报警" })); }
  catch (e) { message.error(String(e)); }
}
async function firePosition() {
  try { message.success(await invoke<string>("fire_position", { longitude: 116.397, latitude: 39.908 })); }
  catch (e) { message.error(String(e)); }
}

// SIP 信令追踪(FR-43):订阅 sip_trace 事件,滚动展示最近 N 条。
interface TraceEntry {
  ts_ms: number; direction: "in" | "out"; method: string;
  status?: number; cseq?: string; call_id?: string; peer: string; summary: string;
}
const traces = ref<TraceEntry[]>([]);
const traceOn = ref(true);
const MAX_TRACE = 200;
function fmtTs(ms: number) {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2,"0")}:${String(d.getMinutes()).padStart(2,"0")}:${String(d.getSeconds()).padStart(2,"0")}`;
}
async function toggleTrace() {
  try {
    await invoke<string>("set_sip_trace", { enabled: traceOn.value });
  } catch (e) { message.error(String(e)); }
}
function clearTraces() { traces.value = []; }

let unlisten: UnlistenFn | null = null;
let unlistenTrace: UnlistenFn | null = null;
onMounted(async () => {
  unlisten = await listen<string>("device_state", (e) => {
    const s = e.payload as DState;
    deviceState.value = s;
    if (s === "Registered" && !startedAt.value) startedAt.value = Date.now();
    if (s === "Disconnected" || s === "Failed") startedAt.value = null;
  });
  unlistenTrace = await listen<TraceEntry>("sip_trace", (e) => {
    if (!traceOn.value) return;
    traces.value.push(e.payload);
    if (traces.value.length > MAX_TRACE) traces.value.splice(0, traces.value.length - MAX_TRACE);
  });
  timer = window.setInterval(fmtUptime, 1000);
});
onUnmounted(() => { unlisten?.(); unlistenTrace?.(); if (timer) clearInterval(timer); });

const metrics = computed(() => [
  { label: "注册状态", value: stateMeta.value.text, color: stateMeta.value.color, dot: true },
  { label: "在线时长", value: uptime.value, mono: true },
  { label: "国标版本", value: "GB/T " + form.value.gb_version },
  { label: "传输协议", value: form.value.transport },
]);
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div class="page-title">连接配置</div>
      <div class="page-sub">配置 SIP 服务器连接参数和设备身份信息</div>
    </div>

    <!-- 顶部指标卡 -->
    <div class="metrics">
      <div v-for="m in metrics" :key="m.label" class="glass-card metric">
        <div class="metric-label">{{ m.label }}</div>
        <div class="metric-value" :class="{ mono: m.mono }" :style="{ color: m.color }">
          <span v-if="m.dot" class="mdot" :style="{ background: m.color }" />
          {{ m.value }}
        </div>
      </div>
    </div>

    <!-- 双栏配置 -->
    <div class="cols">
      <!-- SIP 服务器 -->
      <div class="glass-card panel">
        <div class="panel-title">SIP 服务器</div>
        <div class="fg">
          <label>服务器地址</label>
          <input v-model="form.server_host" class="inp" placeholder="WVP 主机 IP" />
        </div>
        <div class="fg-row">
          <div class="fg">
            <label>SIP 端口</label>
            <input v-model.number="form.server_port" class="inp" type="number" />
          </div>
          <div class="fg">
            <label>服务器域</label>
            <input v-model="form.server_domain" class="inp" placeholder="如 3502000000" />
          </div>
        </div>
        <div class="fg">
          <label>传输协议</label>
          <div class="seg">
            <button :class="{ on: form.transport === 'UDP' }" @click="form.transport = 'UDP'">UDP</button>
            <button :class="{ on: form.transport === 'TCP' }" @click="form.transport = 'TCP'">TCP</button>
          </div>
        </div>
        <div class="fg">
          <label>国标版本</label>
          <div class="seg">
            <button :class="{ on: form.gb_version === '2022' }" @click="form.gb_version = '2022'">2022</button>
            <button :class="{ on: form.gb_version === '2016' }" @click="form.gb_version = '2016'">2016</button>
          </div>
        </div>
      </div>

      <!-- 设备身份 -->
      <div class="glass-card panel">
        <div class="panel-title">设备身份</div>
        <div class="fg">
          <label>设备编号</label>
          <input v-model="form.device_id" class="inp" placeholder="20 位国标 ID" />
        </div>
        <div class="fg">
          <label>认证密码</label>
          <input v-model="form.password" class="inp" type="password" />
        </div>
        <div class="fg-row">
          <div class="fg">
            <label>通道名称</label>
            <input v-model="form.channel_name" class="inp" />
          </div>
          <div class="fg">
            <label>视频源(可选)</label>
            <input v-model="form.video_source" class="inp" placeholder="H.264 文件路径" />
          </div>
        </div>

        <n-space style="margin-top: 18px">
          <n-button type="primary" :disabled="running" @click="startDevice">注册上线</n-button>
          <n-button :disabled="!running" @click="stopDevice">注销</n-button>
          <n-button :disabled="!canReport" @click="fireAlarm">上报报警</n-button>
          <n-button :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
        </n-space>
      </div>
    </div>

    <!-- SIP 信令实时追踪(FR-43) -->
    <div class="glass-card panel trace-panel">
      <div class="trace-head">
        <div class="panel-title" style="margin: 0">SIP 信令追踪</div>
        <div class="trace-ctl">
          <label class="trace-toggle">
            <input type="checkbox" v-model="traceOn" @change="toggleTrace" /> 追踪
          </label>
          <button class="trace-clear" @click="clearTraces">清空</button>
        </div>
      </div>
      <div class="trace-log">
        <div v-if="!traces.length" class="trace-empty">注册上线后,收发的 SIP 报文将实时显示在这里</div>
        <div v-for="(t, i) in traces" :key="i" class="trace-row" :class="t.direction">
          <span class="trace-ts">{{ fmtTs(t.ts_ms) }}</span>
          <span class="trace-dir" :class="t.direction">{{ t.direction === "in" ? "◀ 收" : "▶ 发" }}</span>
          <span class="trace-sum">{{ t.summary }}</span>
          <span class="trace-cseq" v-if="t.cseq">{{ t.cseq }}</span>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1100px; }
.page-header { margin-bottom: 20px; }
.page-title { font-size: 24px; font-weight: 700; color: var(--text-primary); }
.page-sub { font-size: 13px; color: var(--text-tertiary); margin-top: 6px; }

.metrics { display: grid; grid-template-columns: repeat(4, 1fr); gap: 16px; margin-bottom: 20px; }
.metric { padding: 16px 18px; }
.metric-label { font-size: 12px; color: var(--text-tertiary); }
.metric-value {
  font-size: 20px; font-weight: 700; margin-top: 8px; color: var(--text-primary);
  display: flex; align-items: center; gap: 8px;
}
.metric-value.mono { font-family: "SF Mono", Menlo, monospace; letter-spacing: 1px; }
.mdot { width: 8px; height: 8px; border-radius: 50%; }

.cols { display: grid; grid-template-columns: 1fr 1fr; gap: 18px; align-items: start; }
.panel { padding: 22px 22px 24px; }
.panel-title {
  font-size: 12px; font-weight: 600; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: .5px; margin-bottom: 16px;
}
.fg { margin-bottom: 14px; }
.fg-row { display: grid; grid-template-columns: 1fr 1fr; gap: 14px; }
.fg label { display: block; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 6px; }
.inp {
  width: 100%; box-sizing: border-box; height: 38px; padding: 0 12px;
  border: 1px solid var(--border-default); border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.7); color: var(--text-primary);
  font-size: 13px; font-family: "SF Mono", Menlo, monospace;
  transition: border-color var(--transition), box-shadow var(--transition); outline: none;
}
.inp:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-dim); }
.seg {
  display: inline-flex; padding: 3px; gap: 3px; border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--border-default);
}
.seg button {
  border: none; background: transparent; padding: 5px 18px; border-radius: 6px;
  font-size: 13px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.seg button.on { background: var(--accent); color: #fff; box-shadow: 0 2px 6px var(--accent-glow); }

/* SIP 信令追踪面板 */
.trace-panel { margin-top: 18px; }
.trace-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
.trace-ctl { display: inline-flex; align-items: center; gap: 14px; }
.trace-toggle { font-size: 13px; color: var(--text-secondary); cursor: pointer; user-select: none; }
.trace-clear {
  border: 1px solid var(--border-default); background: rgba(255,255,255,0.5);
  border-radius: 6px; padding: 3px 12px; font-size: 12px; color: var(--text-secondary); cursor: pointer;
}
.trace-log {
  height: 240px; overflow-y: auto; border-radius: var(--radius-sm);
  background: rgba(15, 23, 42, 0.03); border: 1px solid var(--border-default);
  padding: 8px 10px; font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px;
}
.trace-empty { color: var(--text-secondary); padding: 12px; text-align: center; }
.trace-row {
  display: flex; align-items: baseline; gap: 8px; padding: 2px 4px;
  border-bottom: 1px solid rgba(15,23,42,0.04); white-space: nowrap;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }
</style>
