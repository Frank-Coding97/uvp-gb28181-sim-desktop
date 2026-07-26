<script setup lang="ts">
// 设备模拟(首页):SIP 配置 + 画面源选择 + 预览占位 + 能力状态。
// 本版为静态页,不接后端 invoke;字段布局对齐手机端 uvp-gb28181-sim 主页。
import { computed, ref } from "vue";
import { NButton, NIcon } from "naive-ui";
import {
  VideocamOutline, DesktopOutline, FolderOpenOutline,
  EyeOutline, EyeOffOutline, PencilOutline,
} from "@vicons/ionicons5";

// ── SIP 配置(手机端同款字段:服务器/服务器 ID/服务器域/设备 ID/密码/信令传输/对讲传输) ──
const sip = ref({
  server_host: "192.168.10.222",
  server_port: 8160,
  server_id: "35020000002000000001",
  server_domain: "3502000000",
  device_id: "35020000001310000001",
  password: "12345678",
  transport: "UDP" as "UDP" | "TCP",
  talk_transport: "TCP_ACTIVE" as "UDP" | "TCP_ACTIVE" | "TCP_PASSIVE",
  gb_version: "2022" as "2022" | "2016",
});
const editing = ref(false);
const showPassword = ref(false);
const maskedPassword = computed(() => "•".repeat(sip.value.password.length || 8));

const talkOptions = [
  { label: "UDP", value: "UDP" },
  { label: "TCP 主动", value: "TCP_ACTIVE" },
  { label: "TCP 被动", value: "TCP_PASSIVE" },
] as const;

// ── 画面源:摄像头 / 屏幕 / 文件三选一 ──
type SourceKind = "camera" | "screen" | "file";
const sourceKind = ref<SourceKind>("camera");
const filePath = ref("");
const sources = [
  { kind: "camera" as SourceKind, icon: VideocamOutline, label: "摄像头", hint: "把电脑摄像头当作 IPC 镜头" },
  { kind: "screen" as SourceKind, icon: DesktopOutline, label: "屏幕", hint: "共享桌面画面作为视频源" },
  { kind: "file" as SourceKind, icon: FolderOpenOutline, label: "文件", hint: "循环推送本地 MP4 / H.264" },
];
const activeSource = computed(() => sources.find((s) => s.kind === sourceKind.value)!);

// ── 注册状态(静态占位:本地切换,不接后端) ──
type DState = "Disconnected" | "Registering" | "Registered";
const state = ref<DState>("Disconnected");
const stateMeta = computed(() => {
  switch (state.value) {
    case "Registering": return { text: "注册中", color: "var(--warning)" };
    case "Registered":  return { text: "已注册", color: "var(--success)" };
    default:            return { text: "未连接", color: "var(--text-tertiary)" };
  }
});
const registered = computed(() => state.value === "Registered");
function toggleRegister() {
  state.value = registered.value ? "Disconnected" : "Registered";
}

// ── 能力状态条(照搬手机端首页四项) ──
const capabilities = computed(() => [
  { label: "录像",     status: registered.value ? "就绪" : "未就绪" },
  { label: "报警",     status: registered.value ? "就绪" : "未就绪" },
  { label: "位置订阅", status: "未订阅" },
  { label: "目录订阅", status: "未订阅" },
]);

// ── 平台交互(静态样例数据,接后端后换为 platform_command / sip_trace 事件) ──
type TabKey = "command" | "trace";
const tab = ref<TabKey>("command");

const DEMO_COMMANDS = [
  { ts: "19:24:07.412", kind: "query",     summary: "查询设备目录 Catalog(SN=1)" },
  { ts: "19:24:07.088", kind: "query",     summary: "查询设备信息 DeviceInfo" },
  { ts: "19:24:06.735", kind: "subscribe", summary: "订阅目录变化 Catalog(有效期 3600s)" },
  { ts: "19:23:58.204", kind: "invite",    summary: "实时点播 通道 1 · TCP 被动 · SSRC 0100000001" },
  { ts: "19:23:41.667", kind: "control",   summary: "云台控制 向左 速度 128" },
  { ts: "19:23:40.902", kind: "control",   summary: "云台控制 停止" },
];
const DEMO_TRACES = [
  { ts: "19:24:07.410", dir: "out", summary: "200 OK (MESSAGE)",           cseq: "1 MESSAGE"  },
  { ts: "19:24:07.402", dir: "in",  summary: "MESSAGE Catalog Query",      cseq: "1 MESSAGE"  },
  { ts: "19:23:58.201", dir: "out", summary: "200 OK (INVITE) + SDP",      cseq: "20 INVITE"  },
  { ts: "19:23:58.106", dir: "in",  summary: "INVITE 通道 1",              cseq: "20 INVITE"  },
  { ts: "19:23:30.014", dir: "out", summary: "REGISTER (含 Authorization)", cseq: "2 REGISTER" },
  { ts: "19:23:29.887", dir: "in",  summary: "401 Unauthorized",           cseq: "1 REGISTER" },
];
const kindLabel: Record<string, string> = {
  query: "查询", control: "控制", invite: "点播", subscribe: "订阅", notify: "通知",
};
// 未注册时不造假交互记录 —— 空态才是真实情况。
const commands = computed(() => (registered.value ? DEMO_COMMANDS : []));
const traces = computed(() => (registered.value ? DEMO_TRACES : []));
</script>

<template>
  <div class="page">
    <div class="layout">
      <!-- ── 左:预览区(占位) ── -->
      <div class="col-left">
        <div class="glass-card preview-card">
          <div class="preview-stage">
            <template v-if="registered">
              <!-- 注册后:占位画面 + OSD 叠加(国标要求设备端叠加时间/通道名) -->
              <div class="osd osd-tl">2026-07-26 18:40:12</div>
              <div class="osd osd-tr">Camera-1</div>
              <div class="osd osd-bl">1920×1080 · 25fps</div>
              <div class="osd osd-br">H.264 · 2.1 Mbps</div>
              <!-- 录制角标(对齐手机端 RecordingBadge:红点闪烁 + 源名 + 计时) -->
              <div class="rec-badge">
                <span class="rec-dot" />
                <span class="rec-text">{{ activeSource.label }}  00:12</span>
              </div>
              <div class="stage-center">
                <n-icon :size="44" class="stage-icon"><component :is="activeSource.icon" /></n-icon>
                <div class="stage-label">{{ activeSource.label }}预览</div>
              </div>
            </template>
            <template v-else>
              <div class="stage-idle">
                <div class="idle-badge">GB/T 28181-{{ sip.gb_version }}</div>
                <div class="idle-brand">UVP</div>
                <div class="idle-hint">注册后开启预览</div>
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
          <div v-if="sourceKind === 'file'" class="file-row">
            <input v-model="filePath" class="inp" placeholder="选择本地 MP4 / H.264 文件" />
            <button class="file-btn">浏览…</button>
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

        <!-- 平台交互:注册后用户最想盯的东西,吃掉剩余高度 -->
        <div class="glass-card panel flow-card">
          <div class="flow-head">
            <div class="tabs">
              <button :class="{ on: tab === 'command' }" @click="tab = 'command'">平台命令</button>
              <button :class="{ on: tab === 'trace' }" @click="tab = 'trace'">SIP 信令</button>
            </div>
            <span class="flow-count" v-if="registered">
              {{ tab === "command" ? commands.length : traces.length }} 条
            </span>
          </div>

          <div class="flow-body">
            <div v-if="!registered" class="flow-empty">
              注册上线后,平台下发的命令与收发的 SIP 报文将实时显示在这里
            </div>

            <template v-else-if="tab === 'command'">
              <div v-for="(c, i) in commands" :key="i" class="cmd-item">
                <span class="mono-ts">{{ c.ts }}</span>
                <span class="cmd-tag" :class="'k-' + c.kind">{{ kindLabel[c.kind] ?? c.kind }}</span>
                <span class="cmd-sum">{{ c.summary }}</span>
              </div>
            </template>

            <template v-else>
              <div v-for="(t, i) in traces" :key="i" class="trace-item">
                <span class="mono-ts">{{ t.ts }}</span>
                <span class="trace-dir" :class="t.dir">{{ t.dir === "in" ? "◀ 收" : "▶ 发" }}</span>
                <span class="trace-sum">{{ t.summary }}</span>
                <span class="trace-cseq">{{ t.cseq }}</span>
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
          <input v-model="sip.server_id" class="inp" :disabled="!editing" placeholder="20 位平台 ID" />
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
            <button :class="{ on: sip.transport === 'UDP' }" @click="sip.transport = 'UDP'">UDP</button>
            <button :class="{ on: sip.transport === 'TCP' }" @click="sip.transport = 'TCP'">TCP</button>
          </div>
        </div>
        <div class="fg">
          <label>对讲传输</label>
          <div class="seg">
            <button
              v-for="t in talkOptions" :key="t.value"
              :class="{ on: sip.talk_transport === t.value }"
              @click="sip.talk_transport = t.value"
            >{{ t.label }}</button>
          </div>
        </div>
        <div class="fg">
          <label>国标版本</label>
          <div class="seg">
            <button :class="{ on: sip.gb_version === '2022' }" @click="sip.gb_version = '2022'">GB/T 2022</button>
            <button :class="{ on: sip.gb_version === '2016' }" @click="sip.gb_version = '2016'">GB/T 2016</button>
          </div>
        </div>

        <!-- 注册按钮 + 状态:填完表单紧接着注册,顺序自然 -->
        <div class="reg-row">
          <span class="reg-state">
            <span class="reg-dot" :style="{ background: stateMeta.color }" />{{ stateMeta.text }}
          </span>
          <n-button
            :type="registered ? 'default' : 'primary'"
            size="large" block @click="toggleRegister"
          >{{ registered ? "注 销" : "注 册" }}</n-button>
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

/* 左预览+交互 / 右配置。右栏定宽(输入框拉宽无意义),左栏吃掉余量 */
.layout {
  flex: 1; min-height: 0;
  display: grid; grid-template-columns: minmax(420px, 1fr) 360px;
  gap: 18px; align-items: stretch;
}
.col-left { display: flex; flex-direction: column; gap: 18px; min-width: 0; min-height: 0; }
/* 配置卡内容定高,超长时自身滚动,不撑破布局 */
.layout > .panel { overflow-y: auto; align-self: start; max-height: 100%; }
@media (max-width: 1080px) {
  .layout { grid-template-columns: 1fr; }
  .layout > .panel { max-height: none; }
}

/* ── 预览区 ── */
/* flex-shrink:0 保住 16:9,不被下方面板挤扁 */
.preview-card { padding: 18px; flex: 0 0 auto; }
/* 底色与手机端 CameraPreview.kt BrandCover 对齐(#0B1E3F→#0F2A57→#1A4480) */
.preview-stage {
  position: relative; aspect-ratio: 16 / 9; border-radius: var(--radius-md);
  /* 超宽屏限高并居中,避免画面拉成一条横带 */
  max-height: 52vh; margin: 0 auto; width: 100%;
  overflow: hidden; display: flex; align-items: center; justify-content: center;
  background: linear-gradient(135deg, #0b1e3f, #0f2a57 55%, #1a4480);
}

/* 未注册:品牌封面。字号/字距/渐变取自手机端(60sp Black·letterSpacing 10sp·#FFF→#7CC4FF) */
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
  /* letter-spacing 会在末字后多留一份间距,补偿使视觉居中 */
  text-indent: 13px;
  background: linear-gradient(90deg, #fff, #7cc4ff);
  -webkit-background-clip: text; background-clip: text; color: transparent;
}
.idle-hint {
  position: absolute; bottom: 12px; left: 0; right: 0; text-align: center;
  font-size: 12px; color: rgba(255, 255, 255, 0.4);
}

/* 已注册:占位画面 + OSD */
.stage-center { text-align: center; color: rgba(255, 255, 255, 0.62); }
.stage-icon { color: rgba(255, 255, 255, 0.5); }
.stage-label { font-size: 13px; margin-top: 8px; letter-spacing: 0.5px; }
.osd {
  position: absolute; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11.5px; color: rgba(255, 255, 255, 0.9);
  text-shadow: 0 1px 3px rgba(0, 0, 0, 0.6); pointer-events: none;
}
/* 录制角标:黑底半透明胶囊 + 呼吸红点(手机端同款) */
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
/* 吃掉左栏剩余高度:内容越多越长,全屏时正好填满 */
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
.flow-count { font-size: 11.5px; color: var(--text-tertiary); }
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
/* 平台命令行 */
.cmd-item, .trace-item {
  display: flex; align-items: center; gap: 10px; padding: 6px 2px;
  border-bottom: 1px solid rgba(120, 130, 150, 0.08); font-size: 12.5px; white-space: nowrap;
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
/* SIP 信令行 */
.trace-dir { flex: 0 0 auto; font-weight: 600; font-size: 12px; }
.trace-dir.in { color: #0891b2; }
.trace-dir.out { color: #7c3aed; }
.trace-cseq {
  flex: 0 0 auto; font-family: ui-monospace, "SF Mono", Menlo, monospace;
  font-size: 11px; color: var(--text-secondary);
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
</style>
