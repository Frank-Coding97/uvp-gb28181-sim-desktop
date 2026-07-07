<script setup lang="ts">
// 单设备联调控制台(UC-1),高保真对齐参考原型 frost-blue:
// 顶部 4 指标卡 + 双栏配置卡(SIP 服务器 / 设备身份)。
import { ref, onMounted, onUnmounted, onActivated, computed, inject, type Ref } from "vue";
import { NButton, NSpace, useMessage } from "naive-ui";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";

const message = useMessage();

// 选择本地 H.264 文件作视频源(FR-8:C 档真实码流)。
async function pickVideoSource() {
  try {
    const picked = await openDialog({
      multiple: false,
      directory: false,
      filters: [
        { name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] },
      ],
    });
    if (typeof picked === "string") form.value.video_source = picked;
  } catch (e) {
    message.error("选择文件失败:" + String(e));
  }
}

// 平台连接参数(server_host/port/domain/password/transport)来自顶栏全局平台档案,
// 本页只管设备自身参数(设备 ID/版本/通道/视频源/模板)。
import { usePlatform } from "../platform";
const { active: activePlatform } = usePlatform();

const form = ref({
  device_id: "35020000001310000001",
  gb_version: "2022",
  channel_name: "Camera-1",
  video_source: "",
  catalog_template: "",
});

// 设备状态由常驻的 App.vue 统一维护并 provide,这里 inject 共享同一份,
// 避免路由切换导致本页状态与顶栏胶囊分叉(注册后离开再回来两处状态不一致)。
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const deviceState = inject<Ref<DState>>("deviceState", ref<DState>("Disconnected"));
const startedAt = inject<Ref<number | null>>("deviceStartedAt", ref<number | null>(null));
const uptime = ref("--:--:--");
let timer: number | null = null;


// OSD 配置状态:反映**平台下发的 OSD 配置命令**(国标 A.2.3.2.11),设备已按其设置。
// 非本地随意填写——osd_config 事件由后端在收到平台 DeviceConfig+OSDConfig 时推来。
const osd = ref<{ received: boolean; time_show: boolean; osd_show: boolean; at: string }>({
  received: false, time_show: false, osd_show: false, at: "",
});
const now = ref(new Date().toLocaleString("zh-CN", { hour12: false }));
let osdTimer: number | null = null;
let unlistenOsd: (() => void) | null = null;

const stateMeta = computed(() => {
  switch (deviceState.value) {
    case "Registering": return { text: "注册中", color: "var(--warning)" };
    case "Registered":  return { text: "已注册", color: "var(--success)" };
    case "InCall":      return { text: "推流中", color: "var(--accent)" };
    case "Failed":      return { text: "注册失败", color: "var(--error)" };
    default:            return { text: "未连接", color: "var(--text-tertiary)" };
  }
});
// 引擎里是否存在设备实例(与状态灯解耦):只要设备后台在跑(哪怕在重试注册),
// 就应允许"注销"停止它。启动成功即置 true,stop/引擎对账为空时置 false。
const deviceLive = ref(false);
// "注册上线"禁用:设备实例存在时禁用(避免重复启动)。
const startDisabled = computed(() => deviceLive.value);
// "注销"禁用:设备实例不存在时禁用。注册失败/重试中设备仍在跑,注销可点。
const stopDisabled = computed(() => !deviceLive.value);
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
    const p = activePlatform.value;
    if (!p) { message.error("请先在顶栏配置目标平台"); return; }
    // 合并全局平台参数 + 本页设备参数。
    const config = {
      server_host: p.server_host, server_port: p.server_port,
      server_domain: p.server_domain, password: p.password, transport: p.transport,
      signaling_encoding: p.signaling_encoding ?? "GB18030",
      ...form.value,
      video_source: form.value.video_source.trim() || null,
    };
    const msg = await invoke<string>("start_device", { config });
    // 设备实例已在后台运行(可能仍在注册/重试),允许注销。
    deviceLive.value = true;
    message.success(msg);
  } catch (e) {
    // 启动本身失败(如配置非法):后端未留下实例,保持可重新启动。
    message.error(String(e));
    deviceLive.value = false;
    deviceState.value = "Failed";
  }
}
async function stopDevice() {
  try {
    await invoke<string>("stop_device");
    deviceLive.value = false;
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

// 平台命令时间线 / 活跃订阅 / 长任务进度(展示后端语义事件)。
interface CmdEntry { kind: string; summary: string; ts_ms: number; }
const commands = ref<CmdEntry[]>([]);
const MAX_CMD = 100;
let unlistenCmd: (() => void) | null = null;

interface SubState { kind: string; active: boolean; notify_count: number; }
const subs = ref<Record<string, SubState>>({});
let unlistenSub: (() => void) | null = null;

interface TaskProgress { kind: string; current: number; total: number; percent: number; }
const progress = ref<TaskProgress | null>(null);
let unlistenProg: (() => void) | null = null;
let progHideTimer: number | null = null;

const subList = computed(() => Object.values(subs.value).filter((s) => s.active));
const kindLabel: Record<string, string> = {
  query: "查询", control: "控制", invite: "点播", broadcast: "广播",
  Catalog: "目录", Alarm: "报警", MobilePosition: "移动位置", PTZPosition: "PTZ精准位置",
  snapshot: "抓拍上传", upgrade: "在线升级",
};
function fmtCmdTs(ms: number): string {
  const d = new Date(ms);
  return d.toLocaleTimeString("zh-CN", { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0");
}

// SIP 信令追踪(FR-43):订阅 sip_trace 事件,滚动展示最近 N 条。
interface TraceEntry {
  ts_ms: number; direction: "in" | "out"; method: string;
  status?: number; cseq?: string; call_id?: string; peer: string; summary: string; raw?: string;
}
const traces = ref<TraceEntry[]>([]);
const traceOn = ref(true);
const MAX_TRACE = 200;
const expandedTrace = ref<number | null>(null);
function toggleTraceRow(i: number) {
  expandedTrace.value = expandedTrace.value === i ? null : i;
}
function fmtTs(ms: number) {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2,"0")}:${String(d.getMinutes()).padStart(2,"0")}:${String(d.getSeconds()).padStart(2,"0")}`;
}
async function toggleTrace() {
  try {
    await invoke<string>("set_sip_trace", { enabled: traceOn.value });
  } catch (e) { message.error(String(e)); }
}
function clearTraces() { traces.value = []; expandedTrace.value = null; }

// 云台控制可视化:订阅 ptz_action 事件,累积转动角度驱动 3D 球机。
interface PtzAction {
  up: boolean; down: boolean; left: boolean; right: boolean;
  zoom_in: boolean; zoom_out: boolean;
  pan_speed: number; tilt_speed: number; zoom_speed: number;
}
const ptz = ref<PtzAction>({
  up: false, down: false, left: false, right: false,
  zoom_in: false, zoom_out: false, pan_speed: 0, tilt_speed: 0, zoom_speed: 0,
});
const ptzActive = computed(() =>
  ptz.value.up || ptz.value.down || ptz.value.left || ptz.value.right ||
  ptz.value.zoom_in || ptz.value.zoom_out);

// 云台连续姿态:pan/tilt 角度(度)+ zoom 倍数。手动 PTZ 实时改,预置位调用平滑转到目标。
const pan = ref(0);    // 水平 -180~180
const tilt = ref(0);   // 俯仰 -60~60
const zoom = ref(1);   // 变倍 1~4
// 预置位目标位(演示用固定映射:每个预置位一个 pan/tilt/zoom)。
const PRESET_POS: Record<number, { pan: number; tilt: number; zoom: number }> = {
  1: { pan: -120, tilt: 20, zoom: 1 },
  2: { pan: 90, tilt: -15, zoom: 2 },
  3: { pan: 0, tilt: 30, zoom: 1.5 },
  4: { pan: 160, tilt: 0, zoom: 3 },
  5: { pan: -60, tilt: -30, zoom: 2.5 },
};
// 预置位巡航(转到目标)状态:目标 + 是否在补间。
const target = ref<{ pan: number; tilt: number; zoom: number } | null>(null);
const seeking = ref(false);
const activePreset = ref<number | null>(null);

// 是否在方向转动(不含变焦,变焦不算"转动")。
const moving = computed(() => {
  const m = ptz.value;
  return m.up || m.down || m.left || m.right;
});
const zoomText = computed(() =>
  ptz.value.zoom_in ? "放大 +" : ptz.value.zoom_out ? "缩小 −" : "—");
const dirText = computed(() => {
  if (seeking.value && activePreset.value) return `转向预置位 ${activePreset.value}`;
  const m = ptz.value;
  const v = m.up ? "上" : m.down ? "下" : "";
  const h = m.left ? "左" : m.right ? "右" : "";
  return (v + h) || "—";
});
// 速度:方向转动时显示水平/垂直速度(0-255),变焦时显示变焦速度(0-15)。
const speedText = computed(() => {
  const m = ptz.value;
  if (m.zoom_in || m.zoom_out) return `变焦 ${m.zoom_speed}/15`;
  if (moving.value) {
    const parts: string[] = [];
    if (m.left || m.right) parts.push(`水平 ${m.pan_speed}`);
    if (m.up || m.down) parts.push(`垂直 ${m.tilt_speed}`);
    return parts.join(" · ") || "—";
  }
  return "—";
});
// 状态:变焦单独显示"变焦中",不与转动混淆。
const statusText = computed(() => {
  if (seeking.value) return "巡航中";
  if (moving.value) return "转动中";
  if (ptz.value.zoom_in || ptz.value.zoom_out) return "变焦中";
  return "静止";
});
// 摇杆偏移:反映当前 pan/tilt(相对目标视角),像真摇杆推向运动方向。
const knobStyle = computed(() => {
  const m = ptz.value;
  let dx = 0, dy = 0;
  if (seeking.value) {
    // 巡航中:摇杆指向目标方向。
    const t = target.value!;
    dx = Math.max(-1, Math.min(1, (t.pan - pan.value) / 30)) * 22;
    dy = Math.max(-1, Math.min(1, (t.tilt - tilt.value) / 20)) * -22;
  } else {
    dx = (m.left ? -22 : 0) + (m.right ? 22 : 0);
    dy = (m.up ? -22 : 0) + (m.down ? 22 : 0);
  }
  const z = 0.9 + (zoom.value - 1) * 0.06 + (m.zoom_in ? 0.04 : m.zoom_out ? -0.04 : 0);
  return { transform: `translate(${dx.toFixed(1)}px, ${dy.toFixed(1)}px) scale(${z.toFixed(2)})` };
});

// 视野方位角文本(演示"摄像头当前朝向")。
const poseText = computed(() =>
  `方位 ${pan.value.toFixed(0)}° · 俯仰 ${tilt.value.toFixed(0)}° · ${zoom.value.toFixed(1)}×`);

// 60ms 动画帧:手动 PTZ 按速度连续改 pan/tilt/zoom;预置位巡航平滑补间到目标。
let poseTimer: number | null = null;
let ptzStopTimer: number | null = null;
function poseTick() {
  if (seeking.value && target.value) {
    const t = target.value;
    const ease = 0.08;
    pan.value += (t.pan - pan.value) * ease;
    tilt.value += (t.tilt - tilt.value) * ease;
    zoom.value += (t.zoom - zoom.value) * ease;
    if (Math.abs(t.pan - pan.value) < 0.5 && Math.abs(t.tilt - tilt.value) < 0.5 && Math.abs(t.zoom - zoom.value) < 0.02) {
      pan.value = t.pan; tilt.value = t.tilt; zoom.value = t.zoom;
      seeking.value = false; target.value = null;
    }
    return;
  }
  const m = ptz.value;
  const ps = (m.pan_speed / 255) * 4 + 1.5;
  const ts = (m.tilt_speed / 255) * 4 + 1.5;
  if (m.left)  pan.value = Math.max(-180, pan.value - ps);
  if (m.right) pan.value = Math.min(180, pan.value + ps);
  if (m.up)    tilt.value = Math.min(60, tilt.value + ts);
  if (m.down)  tilt.value = Math.max(-60, tilt.value - ts);
  if (m.zoom_in)  zoom.value = Math.min(4, zoom.value + 0.03);
  if (m.zoom_out) zoom.value = Math.max(1, zoom.value - 0.03);
}

// device_state 由 App.vue 统一订阅并写入共享状态,本页订阅 sip_trace + ptz_action + ptz_preset。
let unlistenTrace: UnlistenFn | null = null;
let unlistenPtz: UnlistenFn | null = null;
let unlistenPreset: UnlistenFn | null = null;
onMounted(async () => {
  unlistenPtz = await listen<PtzAction>("ptz_action", (e) => {
    ptz.value = e.payload;
    // 自动停止兜底:部分平台(如 WVP)松手时不发显式停止命令,持续运动命令后
    // 若 1.2s 内无新命令,自动归零,避免 UI 一直显示"转动中"。收到显式停止(全 false)则不再计时。
    if (ptzStopTimer) { clearTimeout(ptzStopTimer); ptzStopTimer = null; }
    const m = e.payload;
    const active = m.up || m.down || m.left || m.right || m.zoom_in || m.zoom_out;
    if (active) {
      ptzStopTimer = window.setTimeout(() => {
        ptz.value = { up: false, down: false, left: false, right: false,
          zoom_in: false, zoom_out: false, pan_speed: 0, tilt_speed: 0, zoom_speed: 0 };
      }, 1200);
    }
  });
  // 预置位调用:平滑巡航到目标位(未登记的预置位随机造一个目标演示)。
  unlistenPreset = await listen<number>("ptz_preset", (e) => {
    const id = e.payload;
    activePreset.value = id;
    target.value = PRESET_POS[id] ?? {
      pan: ((id * 53) % 360) - 180, tilt: ((id * 37) % 100) - 50, zoom: 1 + (id % 3),
    };
    seeking.value = true;
  });
  unlistenTrace = await listen<TraceEntry>("sip_trace", (e) => {
    if (!traceOn.value) return;
    traces.value.unshift(e.payload); // 新的在最上(倒序)
    if (traces.value.length > MAX_TRACE) traces.value.splice(MAX_TRACE);
    // 展开态锚在原条目上:新条目插到头部后,展开索引下移一位。
    if (expandedTrace.value !== null) expandedTrace.value += 1;
  });
  timer = window.setInterval(fmtUptime, 1000);
  poseTimer = window.setInterval(poseTick, 60);
  osdTimer = window.setInterval(() => { now.value = new Date().toLocaleString("zh-CN", { hour12: false }); }, 1000);
  // 订阅平台下发的 OSD 配置命令(国标 A.2.3.2.11)。
  unlistenOsd = await listen<{ time_show: boolean; osd_show: boolean }>("osd_config", (e) => {
    osd.value = {
      received: true,
      time_show: e.payload.time_show,
      osd_show: e.payload.osd_show,
      at: new Date().toLocaleString("zh-CN", { hour12: false }),
    };
  });
  // 平台命令时间线。
  unlistenCmd = await listen<CmdEntry>("platform_command", (e) => {
    commands.value.unshift(e.payload);
    if (commands.value.length > MAX_CMD) commands.value.splice(MAX_CMD);
  });
  // 活跃订阅。
  unlistenSub = await listen<SubState>("subscription_state", (e) => {
    subs.value = { ...subs.value, [e.payload.kind]: e.payload };
  });
  // 长任务进度(抓拍/升级);完成后 3s 自动隐藏。
  unlistenProg = await listen<TaskProgress>("task_progress", (e) => {
    progress.value = e.payload;
    if (progHideTimer) clearTimeout(progHideTimer);
    if (e.payload.percent >= 100) {
      progHideTimer = window.setTimeout(() => { progress.value = null; }, 3000);
    }
  });
});
onUnmounted(() => {
  unlistenTrace?.();
  unlistenPtz?.();
  unlistenCmd?.();
  unlistenSub?.();
  unlistenProg?.();
  if (progHideTimer) clearTimeout(progHideTimer);
  unlistenPreset?.();
  unlistenOsd?.();
  if (timer) clearInterval(timer);
  if (poseTimer) clearInterval(poseTimer);
  if (osdTimer) clearInterval(osdTimer);
  if (ptzStopTimer) clearTimeout(ptzStopTimer);
});

// keep-alive 激活时与引擎对账:以引擎真实状态为准同步 deviceLive(避免切页后按钮态错乱)。
onActivated(async () => {
  try {
    const st = await invoke<{ running: boolean }>("get_device_status");
    deviceLive.value = st.running;
    if (!st.running && deviceState.value !== "Disconnected") {
      deviceState.value = "Disconnected";
      startedAt.value = null;
    }
  } catch { /* 忽略 */ }
});
// 首次挂载也对账一次(app 重启后仍能反映后台是否在跑)。
onMounted(async () => {
  try {
    const st = await invoke<{ running: boolean }>("get_device_status");
    deviceLive.value = st.running;
  } catch { /* 忽略 */ }
});

const metrics = computed(() => [
  { label: "注册状态", value: stateMeta.value.text, color: stateMeta.value.color, dot: true },
  { label: "在线时长", value: uptime.value, mono: true },
  { label: "国标版本", value: "GB/T " + form.value.gb_version },
  { label: "传输协议", value: activePlatform.value?.transport ?? "—" },
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
      <!-- 目标平台(来自顶栏全局档案,只读回显;改在顶栏"编辑") -->
      <div class="glass-card panel">
        <div class="panel-title">目标平台</div>
        <div class="plat-readonly" v-if="activePlatform">
          <div class="pr-name">{{ activePlatform.name }}</div>
          <div class="pr-row"><span>地址</span><b>{{ activePlatform.server_host }}:{{ activePlatform.server_port }}</b></div>
          <div class="pr-row"><span>平台域</span><b>{{ activePlatform.server_domain }}</b></div>
          <div class="pr-row"><span>传输</span><b>{{ activePlatform.transport }}</b></div>
          <div class="fg-hint">平台参数在顶栏"目标平台"处切换/编辑,单设备与压测共用。</div>
        </div>
        <div class="fg" style="margin-top: 12px">
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
          <div class="fg-hint">SIP 认证密码用顶栏目标平台的密码。</div>
        </div>
        <div class="fg-row">
          <div class="fg">
            <label>通道名称</label>
            <input v-model="form.channel_name" class="inp" />
          </div>
          <div class="fg">
            <label>视频源(可选)</label>
            <div class="file-row">
              <input v-model="form.video_source" class="inp" placeholder="H.264/MP4 文件路径(点右侧选择)" />
              <button class="file-btn" @click="pickVideoSource">选择文件</button>
            </div>
            <div class="fg-hint">
              留空则只做信令联调(注册/目录/心跳),平台点播会收流超时;要点播出画面请选一个 H.264/MP4 文件。
            </div>
          </div>
        </div>
        <div class="fg-row">
          <div class="fg">
            <label>目录模板</label>
            <select v-model="form.catalog_template" class="inp">
              <option value="">单通道(默认)</option>
              <option value="nvr-8ch">8 通道 NVR</option>
              <option value="civil-3x2">跨区划(3 区 × 2 通道)</option>
              <option value="large-16ch">16 通道大型监控</option>
            </select>
            <div class="fg-hint">启动时载入模板,注册后平台即可同步多通道目录。留空为单通道兼容模式。</div>
          </div>
        </div>

        <n-space style="margin-top: 18px">
          <n-button type="primary" :disabled="startDisabled" @click="startDevice">注册上线</n-button>
          <n-button :disabled="stopDisabled" @click="stopDevice">注销</n-button>
          <n-button :disabled="!canReport" @click="fireAlarm">上报报警</n-button>
          <n-button :disabled="!canReport" @click="firePosition">上报 GPS</n-button>
        </n-space>
      </div>
    </div>

    <!-- 云台控制可视化:平台下发 PTZ 时,球机随命令转动 -->
    <div class="glass-card panel ptz-panel">
      <div class="panel-title" style="margin: 0 0 4px">云台控制</div>
      <div class="ptz-sub">平台下发 PTZ 命令时,下方球机实时演示转动方向与变倍(本设备为被控端)</div>
      <div class="ptz-body">
        <!-- 拟态摇杆:凹陷底盘 + 悬浮摇杆,随命令向对应方向偏移 -->
        <div class="stick-wrap" :class="{ active: ptzActive, seeking }">
          <div class="stick-base">
            <span class="arr arr-u" :class="{ on: ptz.up }">▲</span>
            <span class="arr arr-d" :class="{ on: ptz.down }">▼</span>
            <span class="arr arr-l" :class="{ on: ptz.left }">◀</span>
            <span class="arr arr-r" :class="{ on: ptz.right }">▶</span>
            <div class="stick-knob" :style="knobStyle">
              <div class="knob-face">
                <div class="knob-dot" :class="{ zoom: ptz.zoom_in || ptz.zoom_out }"></div>
              </div>
            </div>
          </div>
        </div>
        <!-- 状态 -->
        <div class="ptz-info">
          <div class="ptz-stats">
            <div class="ptz-stat"><span>当前朝向</span><b class="pose">{{ poseText }}</b></div>
            <div class="ptz-stat"><span>方向</span><b :class="{ hot: seeking || moving }">{{ dirText }}</b></div>
            <div class="ptz-stat"><span>变倍</span><b :class="{ hot: ptz.zoom_in || ptz.zoom_out }">{{ zoomText }}</b></div>
            <div class="ptz-stat"><span>速度</span><b :class="{ hot: moving || ptz.zoom_in || ptz.zoom_out }">{{ speedText }}</b></div>
            <div class="ptz-stat"><span>状态</span><b :class="{ hot: ptzActive || seeking }">{{ statusText }}</b></div>
            <div class="ptz-stat presets">
              <span>预置位</span>
              <span class="preset-chips">
                <b v-for="id in [1,2,3,4,5]" :key="id" class="pchip" :class="{ on: activePreset === id && seeking }">{{ id }}</b>
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>

    <!-- OSD 配置(国标 A.2.3.2.11):展示平台下发的 OSD 设置命令,设备已按其应用 -->
    <div class="glass-card panel osd-panel">
      <div class="panel-title">OSD 设置(平台下发)</div>
      <div v-if="!osd.received" class="osd-empty">
        等待平台下发 OSD 配置命令(DeviceConfig + OSDConfig)…
        <div class="fg-hint" style="margin-top:6px">
          国标中 OSD 是平台→设备的配置命令(时间/信息显示开关)。平台下发后此处显示设备已应用的设置。
        </div>
      </div>
      <div v-else class="osd-applied">
        <div class="osd-row">
          <span class="osd-label">时间叠加显示</span>
          <span class="osd-badge" :class="osd.time_show ? 'on' : 'off'">{{ osd.time_show ? '开启' : '关闭' }}</span>
          <span v-if="osd.time_show" class="osd-sample">{{ now }}</span>
        </div>
        <div class="osd-row">
          <span class="osd-label">OSD 信息显示</span>
          <span class="osd-badge" :class="osd.osd_show ? 'on' : 'off'">{{ osd.osd_show ? '开启' : '关闭' }}</span>
          <span v-if="osd.osd_show" class="osd-sample">{{ form.channel_name || 'Camera-1' }}</span>
        </div>
        <div class="fg-hint">✓ 已应用平台 OSD 配置 · {{ osd.at }} · 设备已回 DeviceConfig 应答</div>
      </div>
    </div>

    <!-- 长任务进度(抓拍上传 / 在线升级) -->
    <div class="glass-card panel" v-if="progress">
      <div class="panel-title">{{ kindLabel[progress.kind] ?? progress.kind }}进度</div>
      <div class="prog-wrap">
        <div class="prog-bar"><div class="prog-fill" :style="{ width: progress.percent + '%' }" /></div>
        <div class="prog-txt">{{ progress.current }}/{{ progress.total }} · {{ progress.percent }}%</div>
      </div>
    </div>

    <!-- 活跃订阅面板 -->
    <div class="glass-card panel" v-if="subList.length">
      <div class="panel-title">活跃订阅</div>
      <div class="sub-list">
        <div v-for="s in subList" :key="s.kind" class="sub-item">
          <span class="sub-dot" />
          <span class="sub-kind">{{ kindLabel[s.kind] ?? s.kind }}</span>
          <span class="sub-count">已发 NOTIFY {{ s.notify_count }} 次</span>
        </div>
      </div>
    </div>

    <!-- 平台命令时间线:平台下发了什么、设备回了什么 -->
    <div class="glass-card panel">
      <div class="panel-title">平台命令时间线</div>
      <div v-if="commands.length === 0" class="cmd-empty">
        等待平台下发命令(注册后平台会查目录、下发控制等)…
      </div>
      <div v-else class="cmd-list">
        <div v-for="(c, i) in commands" :key="i" class="cmd-item">
          <span class="cmd-ts">{{ fmtCmdTs(c.ts_ms) }}</span>
          <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
          <span class="cmd-sum">{{ c.summary }}</span>
        </div>
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
        <div v-if="!traces.length" class="trace-empty">注册上线后,收发的 SIP 报文将实时显示在这里(新的在最上,点击展开详情)</div>
        <template v-for="(t, i) in traces" :key="i">
          <div class="trace-row" :class="[t.direction, { open: expandedTrace === i }]" @click="toggleTraceRow(i)">
            <span class="trace-caret">{{ expandedTrace === i ? "▾" : "▸" }}</span>
            <span class="trace-ts">{{ fmtTs(t.ts_ms) }}</span>
            <span class="trace-dir" :class="t.direction">{{ t.direction === "in" ? "◀ 收" : "▶ 发" }}</span>
            <span class="trace-sum">{{ t.summary }}</span>
            <span class="trace-cseq" v-if="t.cseq">{{ t.cseq }}</span>
          </div>
          <pre v-if="expandedTrace === i" class="trace-raw">{{ t.raw }}</pre>
        </template>
      </div>
    </div>
  </div>
</template>

<style scoped>
.page { max-width: 1100px; }
.page-header { margin-bottom: 20px; }
.plat-readonly { font-size: 13px; }
.pr-name { font-weight: 600; color: var(--text-primary); margin-bottom: 6px; }
.pr-row { display: flex; gap: 10px; padding: 3px 0; color: var(--text-secondary); }
.pr-row span { width: 56px; }
.pr-row b { color: var(--text-primary); font-weight: 500; }
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

/* 密码显示/隐藏:框内右侧眼睛图标(与平台配置页交互一致) */
.pwd-wrap { position: relative; }
.pwd-wrap .inp { width: 100%; padding-right: 40px; }
.pwd-eye {
  position: absolute; top: 50%; right: 10px; transform: translateY(-50%);
  display: inline-flex; align-items: center; justify-content: center;
  border: none; background: transparent; padding: 2px; cursor: pointer;
  color: var(--text-tertiary);
}
.pwd-eye:hover { color: var(--accent); }

/* 视频源文件选择 */
.file-row { display: flex; gap: 8px; align-items: stretch; }
.file-row .inp { flex: 1 1 auto; }
.fg-hint { font-size: 11.5px; color: var(--text-tertiary); margin-top: 6px; line-height: 1.5; }
.file-btn {
  flex: 0 0 auto; border: 1px solid var(--border-default); background: rgba(255,255,255,0.6);
  border-radius: var(--radius-sm); padding: 0 14px; font-size: 13px; color: var(--text-secondary);
  cursor: pointer; white-space: nowrap;
}
.file-btn:hover { border-color: var(--accent); color: var(--accent); }

/* 云台控制可视化 */
.ptz-panel { margin-top: 18px; }
.ptz-sub { font-size: 12px; color: var(--text-tertiary); margin-bottom: 16px; }
.ptz-body { display: flex; gap: 40px; align-items: center; }

/* 拟态摇杆(neumorphism):凹陷底盘 + 悬浮圆钮 */
.stick-wrap { flex: 0 0 auto; padding: 6px; }
.stick-base {
  position: relative;
  width: 176px; height: 176px; border-radius: 50%;
  background: linear-gradient(145deg, #e4e9ef, #c8cfd8);
  /* 外凸底座 + 内凹碗:双向阴影营造立体 */
  box-shadow:
    8px 8px 20px rgba(163, 177, 198, 0.65),
    -8px -8px 20px rgba(255, 255, 255, 0.9),
    inset 3px 3px 8px rgba(163, 177, 198, 0.5),
    inset -3px -3px 8px rgba(255, 255, 255, 0.7);
  display: flex; align-items: center; justify-content: center;
}
/* 碗内深凹槽 */
.stick-base::before {
  content: ""; position: absolute; width: 128px; height: 128px; border-radius: 50%;
  background: linear-gradient(145deg, #cfd6de, #eef2f6);
  box-shadow:
    inset 6px 6px 14px rgba(163, 177, 198, 0.7),
    inset -6px -6px 14px rgba(255, 255, 255, 0.85);
}
/* 方向箭头(碗沿) */
.arr {
  position: absolute; font-size: 12px; color: #9aa5b1; z-index: 3;
  transition: color 0.15s, text-shadow 0.15s, transform 0.15s;
}
.arr-u { top: 12px; left: 50%; transform: translateX(-50%); }
.arr-d { bottom: 12px; left: 50%; transform: translateX(-50%); }
.arr-l { left: 12px; top: 50%; transform: translateY(-50%); }
.arr-r { right: 12px; top: 50%; transform: translateY(-50%); }
.arr.on { color: var(--accent); text-shadow: 0 0 8px var(--accent-glow); transform: scale(1.35) translate(0,0); }
.arr-u.on { transform: translateX(-50%) scale(1.35); }
.arr-d.on { transform: translateX(-50%) scale(1.35); }
.arr-l.on { transform: translateY(-50%) scale(1.35); }
.arr-r.on { transform: translateY(-50%) scale(1.35); }
/* 悬浮摇杆钮 */
.stick-knob {
  position: relative; z-index: 2;
  width: 92px; height: 92px; border-radius: 50%;
  background: linear-gradient(145deg, #f4f7fa, #d2d9e1);
  box-shadow:
    5px 5px 14px rgba(163, 177, 198, 0.75),
    -4px -4px 12px rgba(255, 255, 255, 0.95);
  transition: transform 0.18s cubic-bezier(.34,1.56,.64,1);
  display: flex; align-items: center; justify-content: center;
}
.knob-face {
  width: 66px; height: 66px; border-radius: 50%;
  background: linear-gradient(145deg, #eaeef3, #ffffff);
  box-shadow: inset 2px 2px 6px rgba(163,177,198,0.5), inset -2px -2px 6px rgba(255,255,255,0.9);
  display: flex; align-items: center; justify-content: center;
}
.knob-dot {
  width: 14px; height: 14px; border-radius: 50%;
  background: radial-gradient(circle at 35% 30%, #9aa5b1, #64748b);
  box-shadow: inset 0 1px 2px rgba(255,255,255,0.4);
  transition: all 0.15s;
}
.knob-dot.zoom { background: radial-gradient(circle at 35% 30%, #7dd3fc, var(--accent)); box-shadow: 0 0 10px var(--accent-glow); }
.stick-wrap.active .stick-knob { box-shadow: 6px 6px 16px rgba(163,177,198,0.85), -4px -4px 12px rgba(255,255,255,0.95), 0 0 0 2px rgba(37,99,235,0.15); }

.ptz-info { flex: 1 1 auto; display: flex; gap: 28px; align-items: center; }
.ptz-stats { display: flex; flex-direction: column; gap: 10px; }
.ptz-stat { display: flex; gap: 10px; align-items: baseline; }
.ptz-stat span { font-size: 12px; color: var(--text-tertiary); width: 56px; }
.ptz-stat b { font-size: 14px; color: var(--text-primary); }
.ptz-stat b.hot { color: var(--accent); }
.ptz-stat b.pose { font-variant-numeric: tabular-nums; font-size: 13px; }
.ptz-stat.presets { align-items: center; }
.preset-chips { display: inline-flex; gap: 6px; }
.pchip {
  display: inline-flex; align-items: center; justify-content: center;
  width: 22px; height: 22px; border-radius: 6px; font-size: 12px; font-weight: 600;
  color: var(--text-tertiary); background: rgba(255,255,255,0.5);
  border: 1px solid var(--border-default); transition: all 0.15s;
}
.pchip.on { color: #fff; background: var(--accent); box-shadow: 0 0 10px var(--accent-glow); transform: scale(1.12); }
/* 巡航中底盘泛蓝光 */
.stick-wrap.seeking .stick-base { box-shadow:
  8px 8px 20px rgba(163,177,198,0.65), -8px -8px 20px rgba(255,255,255,0.9),
  inset 3px 3px 8px rgba(163,177,198,0.5), inset -3px -3px 8px rgba(255,255,255,0.7),
  0 0 0 3px rgba(37,99,235,0.25); }

/* 长任务进度 */
.prog-wrap { display: flex; align-items: center; gap: 12px; margin-top: 8px; }
.prog-bar { flex: 1; height: 8px; background: rgba(120,120,120,0.15); border-radius: 4px; overflow: hidden; }
.prog-fill { height: 100%; background: var(--accent); border-radius: 4px; transition: width 0.4s; }
.prog-txt { font-size: 12.5px; color: var(--text-secondary); font-family: monospace; }

/* 活跃订阅面板 */
.sub-list { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 8px; }
.sub-item { display: inline-flex; align-items: center; gap: 8px; padding: 6px 12px;
  background: rgba(5,150,105,0.08); border-radius: 8px; font-size: 13px; }
.sub-dot { width: 8px; height: 8px; border-radius: 50%; background: #059669;
  box-shadow: 0 0 0 3px rgba(5,150,105,0.15); }
.sub-kind { font-weight: 600; color: var(--text-primary); }
.sub-count { color: var(--text-tertiary); font-size: 12px; }

/* 平台命令时间线 */
.cmd-empty { color: var(--text-tertiary); font-size: 13px; padding: 12px 0; }
.cmd-list { margin-top: 8px; max-height: 260px; overflow-y: auto; }
.cmd-item { display: flex; align-items: center; gap: 10px; padding: 5px 0;
  border-bottom: 1px solid rgba(120,120,120,0.08); font-size: 12.5px; }
.cmd-ts { color: var(--text-tertiary); font-family: monospace; font-size: 11.5px; flex-shrink: 0; }
.cmd-tag { flex-shrink: 0; font-size: 11px; font-weight: 600; padding: 1px 8px; border-radius: 8px;
  background: rgba(56,132,255,0.12); color: var(--accent); }
.cmd-tag.k-control { background: rgba(234,88,12,0.12); color: #ea580c; }
.cmd-tag.k-invite { background: rgba(5,150,105,0.12); color: #059669; }
.cmd-tag.k-broadcast { background: rgba(139,92,246,0.12); color: #8b5cf6; }
.cmd-sum { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

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
  display: flex; align-items: baseline; gap: 8px; padding: 3px 4px;
  border-bottom: 1px solid rgba(15,23,42,0.04); white-space: nowrap;
  cursor: pointer; transition: background 0.12s;
}
.trace-row:hover { background: rgba(56,132,255,0.05); }
.trace-row.open { background: rgba(56,132,255,0.08); }
.trace-caret { flex: 0 0 auto; color: var(--text-tertiary); font-size: 10px; width: 10px; }
.trace-raw {
  margin: 0 0 6px 22px; padding: 10px 12px; background: #0f172a; color: #cbd5e1;
  border-radius: 6px; font-size: 11.5px; line-height: 1.5; white-space: pre-wrap;
  word-break: break-all; max-height: 300px; overflow-y: auto;
}
.trace-ts { color: var(--text-secondary); flex: 0 0 auto; }
.trace-dir { flex: 0 0 auto; font-weight: 600; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-sum { flex: 1 1 auto; overflow: hidden; text-overflow: ellipsis; color: var(--text-primary); }
.trace-cseq { flex: 0 0 auto; color: var(--text-secondary); }

/* OSD 设置(平台下发) */
.osd-panel { margin-top: 18px; }
.osd-empty { color: var(--text-tertiary); font-size: 13px; padding: 8px 0; }
.osd-applied { padding: 6px 0; }
.osd-row { display: flex; align-items: center; gap: 12px; padding: 8px 0; }
.osd-label { width: 110px; color: var(--text-secondary); font-size: 13px; }
.osd-badge {
  font-size: 12px; font-weight: 600; padding: 2px 10px; border-radius: 10px;
}
.osd-badge.on { color: #059669; background: rgba(5,150,105,0.12); }
.osd-badge.off { color: var(--text-tertiary); background: rgba(120,120,120,0.12); }
.osd-sample {
  font-family: monospace; font-size: 13px; color: var(--text-primary);
  background: #0f172a; color: #fff; padding: 2px 10px; border-radius: 4px;
}
</style>
