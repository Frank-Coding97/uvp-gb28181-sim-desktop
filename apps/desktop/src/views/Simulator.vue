<script setup lang="ts">
// 设备模拟(首页):SIP 配置 + 画面源选择 + 预览 + 平台命令 / SIP trace。
// 用户在这里填 SIP → 点"注册" → 后端 start_device → device_state / sip_trace 事件回流。
// SIP 配置持久化到 localStorage 单 key,重开应用保留上次填的值。
import { computed, inject, onMounted, onUnmounted, ref, watch, type Ref } from "vue";
import { NButton, NIcon, useMessage } from "naive-ui";
import { Channel as TauriChannel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  VideocamOutline, FolderOpenOutline, RefreshOutline,
  EyeOutline, EyeOffOutline, PencilOutline,
} from "@vicons/ionicons5";

const message = useMessage();

// ── SIP 配置(持久化到 localStorage) ──
interface SipConfig {
  server_host: string;
  server_port: number;
  server_id: string;        // 20 位平台 ID(REGISTER Request-URI 的 user)
  server_domain: string;    // 10 位域(设备 AOR 的 host)
  device_id: string;        // 20 位设备国标 ID
  password: string;
  transport: "UDP" | "TCP";
  signaling_encoding: "GB18030" | "UTF-8";
  gb_version: "2022" | "2016";
}
const STORE_KEY = "uvp_sip_config";
function defaults(): SipConfig {
  return {
    server_host: "192.168.10.222",
    server_port: 8160,
    server_id: "35020000002000000001",
    server_domain: "3502000000",
    device_id: "35020000001310000001",
    password: "12345678",
    transport: "UDP",
    signaling_encoding: "GB18030",
    gb_version: "2022",
  };
}
function load(): SipConfig {
  try {
    const raw = localStorage.getItem(STORE_KEY);
    if (raw) return { ...defaults(), ...JSON.parse(raw) };
  } catch { /* 损坏数据回退默认 */ }
  return defaults();
}
const sip = ref<SipConfig>(load());
// 每次配置变更即持久化(表单编辑边填边存,避免忘记保存)。
watch(sip, (v) => localStorage.setItem(STORE_KEY, JSON.stringify(v)), { deep: true });

const editing = ref(false);
const showPassword = ref(false);
const maskedPassword = computed(() => "•".repeat(sip.value.password.length || 8));

// ── 画面源:摄像头 / 文件二选一 ──
// (窗口/屏幕采集因 mac 上 ffmpeg 不支持窗口级采集,已决定不做。)
type SourceKind = "camera" | "file";
const sourceKind = ref<SourceKind>("camera");
const filePath = ref("");
const sources = [
  { kind: "camera" as SourceKind, icon: VideocamOutline, label: "摄像头", hint: "把电脑摄像头当作 IPC 镜头" },
  { kind: "file" as SourceKind, icon: FolderOpenOutline, label: "文件", hint: "循环推送本地 MP4 / H.264" },
];
const activeSource = computed(() => sources.find((s) => s.kind === sourceKind.value)!);

// 摄像头设备列表(切到"摄像头"时首次拉取,缓存)。
interface CameraDevice { id: string; name: string; }
const cameras = ref<CameraDevice[]>([]);
const selectedCameraId = ref<string>("");
const camerasLoaded = ref(false);
async function loadCameras() {
  // M2 阶段:后端 list_cameras command 未实现(视频源在 M3+ 点播时才需要)。
  // 静默失败,让 UI 显示"未检测到摄像头"占位符,不打断用户注册流程。
  try {
    cameras.value = await invoke<CameraDevice[]>("list_cameras");
    if (cameras.value.length && !selectedCameraId.value) {
      selectedCameraId.value = cameras.value[0].id;
    }
  } catch {
    cameras.value = [];
  } finally {
    camerasLoaded.value = true;
  }
}
// 切到摄像头时按需拉一次;点"刷新"重拉。
watch(sourceKind, (v) => {
  if (v === "camera" && !camerasLoaded.value) loadCameras();
});
async function refreshCameras() {
  camerasLoaded.value = false;
  await loadCameras();
}

async function pickVideoFile() {
  try {
    const picked = await openDialog({
      multiple: false, directory: false,
      filters: [{ name: "视频", extensions: ["h264", "264", "h265", "hevc", "mp4", "flv", "mkv", "mov"] }],
    });
    if (typeof picked === "string") filePath.value = picked;
  } catch (e) { message.error("选择文件失败:" + String(e)); }
}

// ── 注册状态:App.vue 统一订阅 device_state,inject 共享 ──
type DState = "Disconnected" | "Registering" | "Registered" | "InCall" | "Failed";
const deviceState = inject<Ref<DState>>("deviceState", ref<DState>("Disconnected"));
const startedAt = inject<Ref<number | null>>("deviceStartedAt", ref<number | null>(null));
const stateMeta = computed(() => {
  switch (deviceState.value) {
    case "Registering": return { text: "注册中", color: "var(--warning)" };
    case "Registered":  return { text: "已注册", color: "var(--success)" };
    case "InCall":      return { text: "推流中", color: "var(--accent)" };
    case "Failed":      return { text: "注册失败", color: "var(--error)" };
    default:            return { text: "未连接", color: "var(--text-tertiary)" };
  }
});
const registered = computed(() =>
  deviceState.value === "Registered" || deviceState.value === "InCall");
// 后端是否有活跃设备实例(允许"注销"停止它,即使还在重试注册)。
const deviceLive = ref(false);

async function toggleRegister() {
  if (deviceLive.value) {
    await stopDevice();
  } else {
    await startDevice();
  }
}

async function startDevice() {
  const s = sip.value;
  // 前端粗校验:关键字段非空,避免把明显无效值扔给后端。
  if (!s.server_host.trim() || !s.server_port) { message.error("请填服务器地址与端口"); return; }
  if (!s.device_id.trim()) { message.error("请填设备 ID"); return; }
  try {
    // M2 daemon 参数(与 Go 侧 ipc.StartDeviceParams 严格对齐)。
    // v1 的 channel_name / video_source / catalog_template 归 M3+(点播/目录)。
    const config = {
      device_id: s.device_id.trim(),
      server_host: s.server_host.trim(),
      server_port: s.server_port,
      server_id: s.server_id.trim(), // 留空则后端回退 server_domain
      server_domain: s.server_domain.trim(),
      password: s.password,
      transport: s.transport.toLowerCase(), // daemon 期望 udp/tcp 小写
    };
    // daemon 返 {request_id, started: true}(handler 立即返, REGISTER 走异步 device_state 事件)
    const resp = await invoke<{ request_id: string; started: boolean }>(
      "start_device",
      { config },
    );
    deviceLive.value = true;
    message.success(`已提交注册请求 (${resp.request_id.slice(0, 8)})`);
    editing.value = false;
  } catch (e) {
    message.error(String(e));
    deviceLive.value = false;
  }
}

async function stopDevice() {
  try {
    // daemon 返 {stopped: true}, 具体状态由 device_state 事件回流。
    await invoke<{ stopped: boolean }>("stop_device");
    deviceLive.value = false;
    message.info("已停止设备");
  } catch (e) { message.error(String(e)); }
}

// ── 能力状态条 ──
const capabilities = computed(() => [
  { label: "录像",     status: registered.value ? "就绪" : "未就绪" },
  { label: "报警",     status: registered.value ? "就绪" : "未就绪" },
  { label: "位置订阅", status: "未订阅" },
  { label: "目录订阅", status: "未订阅" },
]);

// ── 平台交互:命令时间线 + SIP trace ──
type TabKey = "command" | "trace";
const tab = ref<TabKey>("command");

interface CmdEntry { kind: string; summary: string; ts_ms: number; }
// M2 阶段:平台命令订阅暂未实现(daemon 侧的 sip_message / catalog 事件归 M3)。
// commands 数组保留供 UI 展示"暂无命令"占位符。
const commands = ref<CmdEntry[]>([]);

interface TraceEntry {
  ts_ms: number; direction: "in" | "out"; method: string;
  status?: number; cseq?: string; call_id?: string; peer: string; summary: string; raw?: string;
}
const traces = ref<TraceEntry[]>([]);
const MAX_TRACE = 200;
const expandedTrace = ref<number | null>(null);
function toggleTraceRow(i: number) {
  expandedTrace.value = expandedTrace.value === i ? null : i;
}

const kindLabel: Record<string, string> = {
  query: "查询", control: "控制", invite: "点播", subscribe: "订阅", notify: "通知",
  broadcast: "广播", Catalog: "目录", Alarm: "报警", MobilePosition: "移动位置",
  PTZPosition: "PTZ精准位置", snapshot: "抓拍上传", upgrade: "在线升级",
};
function fmtTs(ms: number): string {
  const d = new Date(ms);
  return d.toLocaleTimeString("zh-CN", { hour12: false }) + "." + String(d.getMilliseconds()).padStart(3, "0");
}

// ── 实时预览(占位符, M2 阶段不启用) ──
// M2 仅打通注册, 视频预览归 M3+ INVITE 点播实现。
// 保留 canvasRef / previewLive / previewChannel 是为了让 template 与 subscribe/unsubscribe
// helper 编译通过, 后续 M3 补 WebCodecs decode 逻辑。
const canvasRef = ref<HTMLCanvasElement | null>(null);
const previewLive = ref(false);
let previewChannel: TauriChannel<unknown> | null = null;

function resetDecoder() {
  previewLive.value = false;
}

async function subscribePreview() {
  // M2 阶段:预览通道未实现(点播视频归 M3+ INVITE)。
  // 保留占位符函数让 watch(deviceLive) 不报错。
  if (previewChannel) return;
  previewChannel = null; // 明确置空
}

async function unsubscribePreview() {
  previewChannel = null;
  resetDecoder();
}

// 设备启动 → 订阅预览;停止 → 退订。
watch(deviceLive, async (live) => {
  if (live) {
    await subscribePreview();
  } else {
    await unsubscribePreview();
  }
});

// ── M3 心跳事件订阅 ──
// heartbeat_result payload:{ ok: bool, consecutive_fails: number, error?: string, ts_ms: number }
interface HeartbeatEvent {
  ok: boolean;
  consecutive_fails: number;
  error?: string;
  ts_ms: number;
}
const heartbeat = ref<HeartbeatEvent | null>(null);
const heartbeatCount = ref(0);
// UI 显示的心跳状态文案 + 色调 (spec R4: 3 次内不告警,只累计;3 次时降级触发 device_state:Failed)
const heartbeatMeta = computed(() => {
  // 未注册或未收到心跳事件:不显示
  if (!registered.value && heartbeat.value == null) {
    return null;
  }
  if (heartbeat.value == null) {
    return { text: "心跳等待", color: "var(--text-tertiary)" };
  }
  if (heartbeat.value.ok) {
    return { text: `心跳正常 (${heartbeatCount.value})`, color: "var(--success)" };
  }
  const n = heartbeat.value.consecutive_fails;
  if (n >= 3) {
    return { text: `心跳超限 (${n}/3)`, color: "var(--error)" };
  }
  return { text: `心跳异常 (${n}/3)`, color: "var(--warning)" };
});

// M3 platform_command 事件订阅归 M4,当前只订 sip_trace + heartbeat_result。
let unlistenTrace: UnlistenFn | null = null;
let unlistenHeartbeat: UnlistenFn | null = null;
onMounted(async () => {
  // canvas 2D ctx 在 M3 预览接入时才用。M2 阶段留空引用避免类型报错。

  // 与后端对账一次:app 重启后仍能反映后台是否在跑。
  // M2 daemon 返 { state, registered_expires_secs }, deviceLive = 状态不是 Disconnected。
  try {
    const st = await invoke<{ state: string; registered_expires_secs: number }>(
      "get_device_status",
    );
    deviceLive.value = st.state !== "Disconnected";
  } catch { /* 忽略 */ }

  // sip_trace 事件订阅:daemon 每收发一条 SIP 报文推一次。
  // payload shape 见 daemon tracerToIPC.Emit:
  //   { direction, method, status_code, cseq, call_id, peer, summary, raw, seq, ts_ms }
  // heartbeat_result 事件订阅 (M3)
  unlistenHeartbeat = await listen<any>("heartbeat_result", (e) => {
    const p = e.payload || {};
    heartbeat.value = {
      ok: p.ok === true,
      consecutive_fails: typeof p.consecutive_fails === "number" ? p.consecutive_fails : 0,
      error: p.error,
      ts_ms: typeof p.ts_ms === "number" ? p.ts_ms : Date.now(),
    };
    if (heartbeat.value.ok) heartbeatCount.value += 1;
  });

  unlistenTrace = await listen<any>("sip_trace", (e) => {
    const p = e.payload || {};
    const entry: TraceEntry = {
      ts_ms: typeof p.ts_ms === "number" ? p.ts_ms : Date.now(),
      direction: p.direction === "in" ? "in" : "out",
      method: p.method ?? "",
      status: typeof p.status_code === "number" && p.status_code > 0 ? p.status_code : undefined,
      cseq: p.cseq != null ? String(p.cseq) : undefined,
      call_id: p.call_id ?? undefined,
      peer: p.peer ?? "",
      summary: p.summary ?? "",
      raw: p.raw ?? undefined,
    };
    traces.value.unshift(entry);
    if (traces.value.length > MAX_TRACE) traces.value.splice(MAX_TRACE);
    if (expandedTrace.value !== null) expandedTrace.value += 1;
  });
});
onUnmounted(() => {
  unlistenTrace?.();
  unlistenHeartbeat?.();
  // 预览取消订阅(M2 是 no-op, M3 真接入)。
  unsubscribePreview();
});

// 在线时长(OSD 里显示,以及未来指标用)。
const now = ref(new Date().toLocaleString("zh-CN", { hour12: false }));
const uptime = ref("--:--:--");
let osdTimer: number | null = null;
onMounted(() => {
  osdTimer = window.setInterval(() => {
    now.value = new Date().toLocaleString("zh-CN", { hour12: false });
    if (startedAt.value) {
      const s = Math.floor((Date.now() - startedAt.value) / 1000);
      const h = String(Math.floor(s / 3600)).padStart(2, "0");
      const m = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
      const ss = String(s % 60).padStart(2, "0");
      uptime.value = `${h}:${m}:${ss}`;
    } else {
      uptime.value = "--:--:--";
    }
  }, 1000);
});
onUnmounted(() => { if (osdTimer) clearInterval(osdTimer); });
</script>

<template>
  <div class="page">
    <div class="layout">
      <!-- ── 左:预览区 + 能力条 + 平台交互 ── -->
      <div class="col-left">
        <div class="glass-card preview-card">
          <div class="preview-stage">
            <!-- 真实推流预览 canvas:平台点播后开始有画面(未点播时空)。 -->
            <canvas ref="canvasRef" class="preview-canvas" :class="{ live: previewLive }" />

            <!-- 已注册但还没画面(未点播 / 首帧未到):OSD 叠加 + 等待提示。 -->
            <template v-if="registered && !previewLive">
              <div class="osd osd-tl">{{ now }}</div>
              <div class="osd osd-tr">Camera-1</div>
              <div class="osd osd-bl">在线 {{ uptime }}</div>
              <div class="osd osd-br">{{ stateMeta.text }}</div>
              <div class="stage-center">
                <n-icon :size="44" class="stage-icon"><component :is="activeSource.icon" /></n-icon>
                <div class="stage-label">已注册,等待平台点播…</div>
              </div>
            </template>

            <!-- 直播画面就位后仍显示 OSD(时间/名字/状态),但不再显示"等待"文案。 -->
            <template v-if="previewLive">
              <div class="osd osd-tl">{{ now }}</div>
              <div class="osd osd-tr">Camera-1</div>
              <div class="osd osd-bl">在线 {{ uptime }}</div>
              <div class="osd osd-br">推流中</div>
              <div class="rec-badge">
                <span class="rec-dot" />
                <span class="rec-text">{{ activeSource.label }}</span>
              </div>
            </template>

            <!-- 未注册:品牌封面。 -->
            <template v-if="!registered">
              <div class="stage-idle">
                <div class="idle-badge">GB/T 28181-{{ sip.gb_version }}</div>
                <div class="idle-brand">UVP</div>
                <div class="idle-hint">
                  {{ deviceState === "Registering" ? "注册中…" :
                     deviceState === "Failed" ? "注册失败,请检查配置" : "填写 SIP 配置后点击注册" }}
                </div>
              </div>
            </template>
          </div>

          <!-- 画面源切换 -->
          <div class="src-row">
            <button
              v-for="s in sources" :key="s.kind"
              class="src-btn" :class="{ on: sourceKind === s.kind }"
              @click="sourceKind = s.kind"
            >
              <n-icon :size="17"><component :is="s.icon" /></n-icon>
              <span>{{ s.label }}</span>
            </button>
            <span class="src-hint">{{ activeSource.hint }}</span>
          </div>
          <div v-if="sourceKind === 'camera'" class="file-row">
            <select v-model="selectedCameraId" class="inp cam-select">
              <option v-if="!cameras.length" value="" disabled>
                {{ camerasLoaded ? "未检测到摄像头" : "加载中…" }}
              </option>
              <option v-for="c in cameras" :key="c.id" :value="c.id">
                [{{ c.id }}] {{ c.name }}
              </option>
            </select>
            <button class="file-btn" @click="refreshCameras" title="刷新设备列表">
              <n-icon :size="14"><RefreshOutline /></n-icon>
            </button>
          </div>
          <div v-if="sourceKind === 'file'" class="file-row">
            <input v-model="filePath" class="inp" placeholder="选择本地 MP4 / H.264 文件" />
            <button class="file-btn" @click="pickVideoFile">浏览…</button>
          </div>
        </div>

        <!-- 能力状态条 -->
        <div class="cap-row">
          <div v-for="c in capabilities" :key="c.label" class="glass-card cap-item">
            <div class="cap-label">{{ c.label }}</div>
            <div class="cap-status" :class="{ ready: c.status === '就绪' }">
              <span class="cap-dot" />{{ c.status }}
            </div>
          </div>
        </div>

        <!-- 平台交互 -->
        <div class="glass-card panel flow-card">
          <div class="flow-head">
            <div class="tabs">
              <button :class="{ on: tab === 'command' }" @click="tab = 'command'">
                平台命令 <span class="tab-count" v-if="commands.length">{{ commands.length }}</span>
              </button>
              <button :class="{ on: tab === 'trace' }" @click="tab = 'trace'">
                SIP 信令 <span class="tab-count" v-if="traces.length">{{ traces.length }}</span>
              </button>
            </div>
          </div>

          <div class="flow-body">
            <div v-if="tab === 'command' && commands.length === 0" class="flow-empty">
              {{ registered
                ? "等待平台下发命令…"
                : "注册上线后,平台下发的命令将实时显示在这里" }}
            </div>
            <div v-else-if="tab === 'trace' && traces.length === 0" class="flow-empty">
              {{ deviceLive
                ? "等待 SIP 报文…"
                : "点击注册后,收发的 SIP 报文将实时显示在这里" }}
            </div>

            <template v-else-if="tab === 'command'">
              <div v-for="(c, i) in commands" :key="i" class="cmd-item">
                <span class="mono-ts">{{ fmtTs(c.ts_ms) }}</span>
                <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
                <span class="cmd-sum">{{ c.summary }}</span>
              </div>
            </template>

            <template v-else>
              <div v-for="(t, i) in traces" :key="i">
                <div class="trace-item" @click="toggleTraceRow(i)">
                  <span class="mono-ts">{{ fmtTs(t.ts_ms) }}</span>
                  <span class="trace-dir" :class="t.direction">
                    {{ t.direction === "in" ? "◀ 收" : "▶ 发" }}
                  </span>
                  <span class="trace-sum">{{ t.summary }}</span>
                  <span class="trace-cseq">{{ t.cseq ?? "" }}</span>
                </div>
                <pre v-if="expandedTrace === i && t.raw" class="trace-raw">{{ t.raw }}</pre>
              </div>
            </template>
          </div>
        </div>
      </div>

      <!-- ── 右:SIP 配置 ── -->
      <div class="glass-card panel">
        <div class="panel-head">
          <div class="panel-title">SIP 配置</div>
          <button class="edit-btn" @click="editing = !editing">
            <n-icon :size="14"><PencilOutline /></n-icon>
            {{ editing ? "完成" : "编辑" }}
          </button>
        </div>

        <div class="fg">
          <label>服务器</label>
          <div class="host-row">
            <input v-model="sip.server_host" class="inp" :disabled="!editing" placeholder="平台 IP" />
            <span class="colon">:</span>
            <input v-model.number="sip.server_port" class="inp port" :disabled="!editing" />
          </div>
        </div>
        <div class="fg">
          <label>服务器 ID</label>
          <input v-model="sip.server_id" class="inp" :disabled="!editing" placeholder="20 位平台 ID(留空则用域 ID)" />
        </div>
        <div class="fg">
          <label>服务器域</label>
          <input v-model="sip.server_domain" class="inp" :disabled="!editing" placeholder="10 位域" />
        </div>
        <div class="fg">
          <label>设备 ID</label>
          <input v-model="sip.device_id" class="inp" :disabled="!editing" placeholder="20 位国标 ID" />
        </div>
        <div class="fg">
          <label>注册密码</label>
          <div class="pwd-row">
            <input
              v-if="showPassword || editing"
              v-model="sip.password" class="inp" :disabled="!editing" type="text"
            />
            <input v-else :value="maskedPassword" class="inp" disabled />
            <button class="pwd-eye" @click="showPassword = !showPassword">
              <n-icon :size="16"><component :is="showPassword ? EyeOffOutline : EyeOutline" /></n-icon>
            </button>
          </div>
        </div>
        <div class="fg">
          <label>信令传输</label>
          <div class="seg">
            <button :class="{ on: sip.transport === 'UDP' }" :disabled="!editing" @click="sip.transport = 'UDP'">UDP</button>
            <button :class="{ on: sip.transport === 'TCP' }" :disabled="!editing" @click="sip.transport = 'TCP'">TCP</button>
          </div>
        </div>
        <div class="fg">
          <label>信令编码</label>
          <div class="seg">
            <button :class="{ on: sip.signaling_encoding === 'GB18030' }" :disabled="!editing"
                    @click="sip.signaling_encoding = 'GB18030'">GB18030</button>
            <button :class="{ on: sip.signaling_encoding === 'UTF-8' }" :disabled="!editing"
                    @click="sip.signaling_encoding = 'UTF-8'">UTF-8</button>
          </div>
        </div>
        <div class="fg">
          <label>国标版本</label>
          <div class="seg">
            <button :class="{ on: sip.gb_version === '2022' }" :disabled="!editing" @click="sip.gb_version = '2022'">GB/T 2022</button>
            <button :class="{ on: sip.gb_version === '2016' }" :disabled="!editing" @click="sip.gb_version = '2016'">GB/T 2016</button>
          </div>
        </div>

        <!-- 注册按钮 + 状态 -->
        <div class="reg-row">
          <span class="reg-state">
            <span class="reg-dot" :style="{ background: stateMeta.color }" />{{ stateMeta.text }}
          </span>
          <!-- 心跳状态 (M3):注册后每 60s 一次 tick,失败降级前显示 N/3 -->
          <span class="reg-state" v-if="heartbeatMeta">
            <span class="reg-dot" :style="{ background: heartbeatMeta.color }" />{{ heartbeatMeta.text }}
          </span>
          <n-button
            :type="deviceLive ? 'default' : 'primary'"
            size="large" block
            :loading="deviceState === 'Registering'"
            @click="toggleRegister"
          >{{ deviceLive ? "注 销" : "注 册" }}</n-button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* 撑满内容区高度:让平台交互面板吃掉剩余空间,全屏时不留死白 */
.page { height: 100%; display: flex; flex-direction: column; min-height: 0; }

/* 注册行:状态在上,按钮撑满 */
.reg-row { margin-top: 20px; }
.reg-state {
  display: flex; align-items: center; gap: 7px; margin-bottom: 10px;
  font-size: 12.5px; color: var(--text-secondary);
}
.reg-dot { width: 8px; height: 8px; border-radius: 50%; transition: background var(--transition); }

/* 左预览+交互 / 右配置。右栏定宽,左栏吃余量 */
.layout {
  flex: 1; min-height: 0;
  display: grid; grid-template-columns: minmax(420px, 1fr) 360px;
  gap: 18px; align-items: stretch;
}
.col-left { display: flex; flex-direction: column; gap: 18px; min-width: 0; min-height: 0; }
.layout > .panel { overflow-y: auto; align-self: start; max-height: 100%; }
@media (max-width: 1080px) {
  .layout { grid-template-columns: 1fr; }
  .layout > .panel { max-height: none; }
}

/* ── 预览区 ── */
.preview-card { padding: 18px; flex: 0 0 auto; }
.preview-stage {
  position: relative; aspect-ratio: 16 / 9; border-radius: var(--radius-md);
  max-height: 52vh; margin: 0 auto; width: 100%;
  overflow: hidden; display: flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #0b1e3f, #0f2a57 55%, #1a4480);
}
/* canvas 覆盖整个 stage,object-fit contain 让真实分辨率居中不拉伸;
   未有画面时透明,让底层封面/OSD 露出。 */
.preview-canvas {
  position: absolute; inset: 0; width: 100%; height: 100%;
  object-fit: contain;
  opacity: 0; transition: opacity var(--transition);
}
.preview-canvas.live { opacity: 1; }
.stage-idle {
  position: absolute; inset: 0;
  display: flex; flex-direction: column; align-items: center; justify-content: center;
}
.idle-badge {
  padding: 4px 12px; border-radius: 10px;
  background: rgba(255, 255, 255, 0.08);
  border: 1px solid rgba(255, 255, 255, 0.18);
  color: rgba(255, 255, 255, 0.7); font-size: 12px; letter-spacing: 1.4px;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
}
.idle-brand {
  font-size: 76px; font-weight: 900; letter-spacing: 13px; margin: 14px 0 0;
  text-indent: 13px;
  background: linear-gradient(90deg, #fff, #7cc4ff);
  -webkit-background-clip: text; background-clip: text; color: transparent;
}
.idle-hint {
  position: absolute; bottom: 12px; left: 0; right: 0; text-align: center;
  font-size: 12px; color: rgba(255, 255, 255, 0.4);
}
.stage-center { text-align: center; color: rgba(255, 255, 255, 0.62); }
.stage-icon { color: rgba(255, 255, 255, 0.5); }
.stage-label { font-size: 13px; margin-top: 8px; letter-spacing: 0.5px; }
.osd {
  position: absolute; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11.5px; color: rgba(255, 255, 255, 0.9);
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.6); pointer-events: none;
}
.rec-badge {
  position: absolute; top: 34px; left: 12px;
  display: inline-flex; align-items: center; gap: 5px;
  padding: 3px 8px; border-radius: 4px;
  background: rgba(0, 0, 0, 0.55);
}
.rec-dot {
  width: 8px; height: 8px; border-radius: 50%;
  background: var(--error); animation: recPulse 1.4s ease-in-out infinite;
}
@keyframes recPulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.3; } }
.rec-text {
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 10.5px; font-weight: 500; color: #fff; white-space: pre;
}
.osd-tl { top: 10px; left: 12px; }
.osd-tr { top: 10px; right: 12px; }
.osd-bl { bottom: 10px; left: 12px; }
.osd-br { bottom: 10px; right: 12px; }

/* 画面源切换 */
.src-row { display: flex; align-items: center; gap: 8px; margin-top: 14px; }
.src-btn {
  display: inline-flex; align-items: center; gap: 6px;
  padding: 7px 14px; border-radius: var(--radius-sm); cursor: pointer;
  background: rgba(255, 255, 255, 0.55); border: 1px solid var(--border-default);
  color: var(--text-secondary); font-size: 13px; transition: all var(--transition);
}
.src-btn:hover { border-color: var(--border-accent); color: var(--accent); }
.src-btn.on {
  background: var(--accent); border-color: var(--accent); color: #fff;
  box-shadow: 0 2px 8px var(--accent-glow);
}
.src-hint { font-size: 11.5px; color: var(--text-tertiary); margin-left: auto; }
.file-row { display: flex; gap: 8px; margin-top: 10px; }
.file-row .inp { flex: 1; }
.file-btn {
  flex: 0 0 auto; padding: 0 16px; border-radius: var(--radius-sm); cursor: pointer;
  background: rgba(255, 255, 255, 0.6); border: 1px solid var(--border-default);
  color: var(--text-secondary); font-size: 13px;
}
.file-btn:hover { border-color: var(--accent); color: var(--accent); }
.cam-select { cursor: pointer; padding-right: 28px; appearance: menulist; }

/* 能力状态条 */
.cap-row { display: grid; grid-template-columns: repeat(4, 1fr); gap: 12px; }
.cap-item { padding: 14px 16px; }
.cap-label { font-size: 13px; font-weight: 600; color: var(--text-primary); }
.cap-status {
  display: flex; align-items: center; gap: 6px;
  font-size: 11.5px; color: var(--text-tertiary); margin-top: 6px;
}
.cap-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--text-tertiary); }
.cap-status.ready { color: var(--success); }
.cap-status.ready .cap-dot { background: var(--success); }

/* ── 平台交互 ── */
.flow-card {
  flex: 1; min-height: 180px;
  display: flex; flex-direction: column; overflow: hidden;
}
.flow-head {
  display: flex; align-items: center; justify-content: space-between;
  margin-bottom: 12px; flex: 0 0 auto;
}
.tabs {
  display: inline-flex; padding: 3px; gap: 3px; border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--border-default);
}
.tabs button {
  border: none; background: transparent; padding: 5px 16px; border-radius: 6px;
  font-size: 12.5px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.tabs button.on { background: var(--accent); color: #fff; box-shadow: 0 2px 6px var(--accent-glow); }
.tab-count {
  display: inline-block; min-width: 18px; margin-left: 4px; padding: 0 5px;
  font-size: 10.5px; border-radius: 9px; background: rgba(255,255,255,0.2);
}
.flow-body {
  flex: 1; min-height: 0; overflow-y: auto;
  border-radius: var(--radius-sm); border: 1px solid var(--border-default);
  background: rgba(15, 23, 42, 0.03); padding: 6px 10px;
}
.flow-empty {
  height: 100%; display: flex; align-items: center; justify-content: center;
  text-align: center; font-size: 12.5px; color: var(--text-tertiary); padding: 0 20px;
}
.mono-ts {
  flex: 0 0 auto; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11px; color: var(--text-tertiary);
}
.cmd-item, .trace-item {
  display: flex; align-items: center; gap: 10px; padding: 6px 2px;
  border-bottom: 1px solid rgba(120, 130, 150, 0.08); font-size: 12.5px; white-space: nowrap;
  cursor: pointer;
}
.cmd-item:last-child, .trace-item:last-child { border-bottom: none; }
.cmd-tag {
  flex: 0 0 auto; font-size: 11px; font-weight: 600; padding: 1px 8px; border-radius: 8px;
  background: rgba(56, 132, 255, 0.12); color: var(--accent);
}
.cmd-tag.k-control   { background: rgba(234, 88, 12, 0.12);  color: #ea580c; }
.cmd-tag.k-invite    { background: rgba(5, 150, 105, 0.12);  color: #059669; }
.cmd-tag.k-subscribe { background: rgba(139, 92, 246, 0.12); color: #8b5cf6; }
.cmd-sum, .trace-sum {
  flex: 1 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis;
  color: var(--text-primary);
}
.trace-dir { flex: 0 0 auto; font-weight: 600; font-size: 12px; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-cseq {
  flex: 0 0 auto; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11px; color: var(--text-secondary);
}
.trace-raw {
  margin: 0 2px 8px 78px; padding: 8px 10px;
  background: rgba(15, 23, 42, 0.06); border-radius: 6px;
  font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11px; color: var(--text-secondary); line-height: 1.5;
  white-space: pre-wrap; word-break: break-all;
}

/* ── SIP 配置 ── */
.panel { padding: 20px 22px 22px; }
.panel-head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; }
.panel-title {
  font-size: 12px; font-weight: 600; color: var(--text-tertiary);
  text-transform: uppercase; letter-spacing: 0.5px;
}
.edit-btn {
  display: inline-flex; align-items: center; gap: 4px;
  border: none; background: transparent; cursor: pointer;
  color: var(--accent); font-size: 12.5px; padding: 2px 4px;
}
.fg { margin-bottom: 13px; }
.fg label { display: block; font-size: 12.5px; color: var(--text-secondary); margin-bottom: 6px; }
.inp {
  width: 100%; box-sizing: border-box; height: 36px; padding: 0 12px;
  border: 1px solid var(--border-default); border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.7); color: var(--text-primary);
  font-size: 13px; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  transition: border-color var(--transition), box-shadow var(--transition); outline: none;
}
.inp:focus { border-color: var(--accent); box-shadow: 0 0 0 3px var(--accent-dim); }
.inp:disabled { background: rgba(255, 255, 255, 0.4); color: var(--text-secondary); cursor: default; }
.host-row { display: flex; align-items: center; gap: 6px; }
.colon { color: var(--text-tertiary); }
.port { width: 76px; flex: 0 0 auto; text-align: center; }
.pwd-row { position: relative; }
.pwd-row .inp { padding-right: 38px; }
.pwd-eye {
  position: absolute; top: 50%; right: 8px; transform: translateY(-50%);
  display: inline-flex; border: none; background: transparent; cursor: pointer;
  color: var(--text-tertiary); padding: 2px;
}
.pwd-eye:hover { color: var(--accent); }
.seg {
  display: inline-flex; padding: 3px; gap: 3px; border-radius: var(--radius-sm);
  background: rgba(255, 255, 255, 0.5); border: 1px solid var(--border-default);
}
.seg button {
  border: none; background: transparent; padding: 5px 14px; border-radius: 6px;
  font-size: 12.5px; color: var(--text-secondary); cursor: pointer; transition: all var(--transition);
}
.seg button.on { background: var(--accent); color: #fff; box-shadow: 0 2px 6px var(--accent-glow); }
.seg button:disabled { cursor: default; }
</style>
