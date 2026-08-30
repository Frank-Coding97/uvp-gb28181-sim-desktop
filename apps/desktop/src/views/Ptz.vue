<script setup lang="ts">
import { computed, onActivated, onMounted, onUnmounted, ref, watch } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import ThreePtzCamera from "../components/ThreePtzCamera.vue";
import { useDevice } from "../device";

interface PtzAction {
  up: boolean;
  down: boolean;
  left: boolean;
  right: boolean;
  zoom_in: boolean;
  zoom_out: boolean;
  pan_speed: number;
  tilt_speed: number;
  zoom_speed: number;
}

interface PtzPresetPayload { id: number; name: string; }
interface SubscriptionState { kind: string; active: boolean; notify_count: number; }

const { deviceLive, reconcile } = useDevice();
const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const stopped: PtzAction = {
  up: false, down: false, left: false, right: false,
  zoom_in: false, zoom_out: false,
  pan_speed: 0, tilt_speed: 0, zoom_speed: 0,
};
const ptz = ref<PtzAction>({ ...stopped });
const pan = ref(0);
const tilt = ref(0);
const zoom = ref(1);
const target = ref<{ pan: number; tilt: number; zoom: number } | null>(null);
const seeking = ref(false);
const activePreset = ref<number | null>(null);
const activePresetName = ref("");
const lastAction = ref("STOP");
const lastActionAt = ref("尚未收到平台 PTZ 命令");
const subscriptions = ref<Record<string, SubscriptionState>>({});

const PRESET_IDS = [1, 2, 3, 4, 5] as const;
const presetNames = ref<Record<number, string>>({
  1: "大门入口", 2: "停车场", 3: "接待大厅", 4: "东侧通道", 5: "西侧通道",
});
const PRESET_POS: Record<number, { pan: number; tilt: number; zoom: number }> = {
  1: { pan: -120, tilt: 20, zoom: 1 },
  2: { pan: 90, tilt: -15, zoom: 2 },
  3: { pan: 0, tilt: 30, zoom: 1.5 },
  4: { pan: 160, tilt: 0, zoom: 3 },
  5: { pan: -60, tilt: -30, zoom: 2.5 },
};

const ptzActive = computed(() => Object.entries(ptz.value)
  .some(([key, value]) => !key.endsWith("_speed") && value === true));
const moving = computed(() => ptz.value.up || ptz.value.down || ptz.value.left || ptz.value.right);
const normalizedPan = computed(() => ((pan.value % 360) + 360) % 360);
const statusText = computed(() => {
  if (!deviceLive.value) return "设备未运行";
  if (seeking.value) return "巡航中";
  if (moving.value) return "转动中";
  if (ptz.value.zoom_in || ptz.value.zoom_out) return "变焦中";
  return "STOP · 静止";
});
const directionText = computed(() => {
  if (seeking.value && activePreset.value) return `转向 ${activePresetName.value}`;
  const vertical = ptz.value.up ? "上" : ptz.value.down ? "下" : "";
  const horizontal = ptz.value.left ? "左" : ptz.value.right ? "右" : "";
  return vertical + horizontal || "—";
});
const speedText = computed(() => {
  if (ptz.value.zoom_in || ptz.value.zoom_out) return `变焦 ${ptz.value.zoom_speed}/15`;
  const parts: string[] = [];
  if (ptz.value.left || ptz.value.right) parts.push(`水平 ${ptz.value.pan_speed}`);
  if (ptz.value.up || ptz.value.down) parts.push(`垂直 ${ptz.value.tilt_speed}`);
  return parts.join(" · ") || "—";
});
const poseText = computed(() =>
  `方位 ${normalizedPan.value.toFixed(0).padStart(3, "0")}° · 累计 ${pan.value.toFixed(0)}° · 俯仰 ${tilt.value.toFixed(0)}° · ${zoom.value.toFixed(1)}×`);
const compassDialStyle = computed(() => ({ transform: `rotate(${(-normalizedPan.value).toFixed(1)}deg)` }));
const activeSubscriptions = computed(() => Object.values(subscriptions.value).filter((item) => item.active));
const kindLabel: Record<string, string> = {
  Catalog: "目录", Alarm: "报警", MobilePosition: "移动位置", PTZPosition: "PTZ 精准位置",
};

function timestamp() {
  return new Date().toLocaleTimeString("zh-CN", { hour12: false });
}

function describe(action: PtzAction) {
  const labels: string[] = [];
  if (action.up) labels.push("上");
  if (action.down) labels.push("下");
  if (action.left) labels.push("左");
  if (action.right) labels.push("右");
  if (action.zoom_in) labels.push("放大");
  if (action.zoom_out) labels.push("缩小");
  return labels.join(" + ") || "STOP";
}

function nearestEquivalentPan(heading: number) {
  const normalizedTarget = ((heading % 360) + 360) % 360;
  const delta = ((normalizedTarget - normalizedPan.value + 540) % 360) - 180;
  return pan.value + delta;
}

function resetTransientState() {
  ptz.value = { ...stopped };
  target.value = null;
  seeking.value = false;
  activePreset.value = null;
  activePresetName.value = "";
  subscriptions.value = {};
  lastAction.value = "STOP";
  lastActionAt.value = "设备已停止，等待重新注册";
}

let poseTimer: number | null = null;
let ptzStopTimer: number | null = null;
let unlistenPtz: UnlistenFn | null = null;
let unlistenPreset: UnlistenFn | null = null;
let unlistenSubscription: UnlistenFn | null = null;

function poseTick() {
  if (seeking.value && target.value) {
    const next = target.value;
    pan.value += (next.pan - pan.value) * 0.08;
    tilt.value += (next.tilt - tilt.value) * 0.08;
    zoom.value += (next.zoom - zoom.value) * 0.08;
    if (Math.abs(next.pan - pan.value) < 0.5 && Math.abs(next.tilt - tilt.value) < 0.5 && Math.abs(next.zoom - zoom.value) < 0.02) {
      pan.value = next.pan;
      tilt.value = next.tilt;
      zoom.value = next.zoom;
      seeking.value = false;
      target.value = null;
    }
    return;
  }
  const action = ptz.value;
  const panStep = (action.pan_speed / 255) * 4 + 1.5;
  const tiltStep = (action.tilt_speed / 255) * 4 + 1.5;
  if (action.left) pan.value -= panStep;
  if (action.right) pan.value += panStep;
  if (action.up) tilt.value = Math.min(90, tilt.value + tiltStep);
  if (action.down) tilt.value = Math.max(-90, tilt.value - tiltStep);
  if (action.zoom_in) zoom.value = Math.min(4, zoom.value + 0.03);
  if (action.zoom_out) zoom.value = Math.max(1, zoom.value - 0.03);
}

watch(deviceLive, (running) => {
  if (!running) resetTransientState();
});

onMounted(async () => {
  poseTimer = window.setInterval(poseTick, 60);
  if (!isTauri) return;
  unlistenPtz = await listen<PtzAction>("ptz_action", (event) => {
    seeking.value = false;
    target.value = null;
    activePreset.value = null;
    activePresetName.value = "";
    ptz.value = event.payload;
    lastAction.value = describe(event.payload);
    lastActionAt.value = timestamp();
    if (ptzStopTimer) clearTimeout(ptzStopTimer);
    if (ptzActive.value) {
      ptzStopTimer = window.setTimeout(() => {
        ptz.value = { ...stopped };
        lastAction.value = "STOP（超时保护）";
        lastActionAt.value = timestamp();
      }, 3000);
    }
  });
  unlistenPreset = await listen<PtzPresetPayload>("ptz_preset", (event) => {
    if (ptzStopTimer) clearTimeout(ptzStopTimer);
    ptz.value = { ...stopped };
    const { id, name } = event.payload;
    activePreset.value = id;
    activePresetName.value = name || `预置位 ${id}`;
    presetNames.value = { ...presetNames.value, [id]: activePresetName.value };
    const preset = PRESET_POS[id] ?? { pan: (id * 53) % 360, tilt: ((id * 37) % 180) - 90, zoom: 1 + id % 3 };
    target.value = { ...preset, pan: nearestEquivalentPan(preset.pan) };
    seeking.value = true;
    lastAction.value = `调用 ${activePresetName.value}（${id}）`;
    lastActionAt.value = timestamp();
  });
  unlistenSubscription = await listen<SubscriptionState>("subscription_state", (event) => {
    subscriptions.value = { ...subscriptions.value, [event.payload.kind]: event.payload };
  });
  await reconcile();
});

onActivated(() => { if (isTauri) void reconcile(); });
onUnmounted(() => {
  unlistenPtz?.();
  unlistenPreset?.();
  unlistenSubscription?.();
  if (poseTimer) clearInterval(poseTimer);
  if (ptzStopTimer) clearTimeout(ptzStopTimer);
});
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">云台控制</div>
        <div class="page-sub">显示上级平台下发的方向、变倍和预置位命令；本页不主动控制平台</div>
      </div>
      <span class="device-pill" :class="{ online: deviceLive }">{{ deviceLive ? "设备运行中" : "设备未运行" }}</span>
    </div>

    <div class="ptz-layout">
      <section class="glass-card stage-card">
        <div class="stage" :class="{ active: ptzActive, seeking, offline: !deviceLive }">
          <div class="stage-grid" />
          <div class="stage-caption"><i /><b>PTZ LIVE</b><span>{{ statusText }}</span></div>
          <div class="compass">
            <div class="dial" :style="compassDialStyle"><span>N</span><span>E</span><span>S</span><span>W</span></div>
            <b>{{ normalizedPan.toFixed(0).padStart(3, "0") }}°</b>
          </div>
          <ThreePtzCamera :pan="pan" :tilt="tilt" :zoom="zoom" :active="ptzActive" :seeking="seeking" />
          <div v-if="!deviceLive" class="offline-cover"><b>等待设备注册</b><span>注册后平台 PTZ 命令会在这里实时驱动球机</span></div>
        </div>
      </section>

      <aside class="glass-card info-card">
        <div class="panel-title">实时姿态</div>
        <div class="pose">{{ poseText }}</div>
        <div class="stat-grid">
          <div><span>方向</span><b>{{ directionText }}</b></div>
          <div><span>变倍</span><b>{{ zoom.toFixed(1) }}×</b></div>
          <div><span>速度</span><b>{{ speedText }}</b></div>
          <div><span>状态</span><b :class="{ hot: ptzActive || seeking }">{{ statusText }}</b></div>
        </div>
        <div class="last-action"><span>最近命令 · {{ lastActionAt }}</span><b>{{ lastAction }}</b></div>

        <div class="panel-title section-title">预置位</div>
        <div class="presets">
          <div v-for="id in PRESET_IDS" :key="id" :class="{ active: activePreset === id && seeking }">
            <small>{{ id }}</small><span>{{ presetNames[id] }}</span>
          </div>
        </div>

        <div class="panel-title section-title">活跃订阅</div>
        <div v-if="activeSubscriptions.length" class="subscriptions">
          <div v-for="item in activeSubscriptions" :key="item.kind">
            <i /><span>{{ kindLabel[item.kind] ?? item.kind }}</span><b>NOTIFY {{ item.notify_count }}</b>
          </div>
        </div>
        <div v-else class="empty">无活跃订阅</div>
      </aside>
    </div>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.page-header { display: flex; align-items: center; justify-content: space-between; gap: 20px; margin-bottom: 18px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.device-pill { padding: 6px 11px; border: 1px solid var(--border-default); border-radius: 999px; color: var(--text-tertiary); background: rgba(255,255,255,.48); font-size: 12px; }
.device-pill.online { border-color: rgba(24,160,88,.25); color: var(--success); background: rgba(24,160,88,.08); }
.ptz-layout { display: grid; grid-template-columns: minmax(0, 1.6fr) minmax(320px, .72fr); gap: 16px; min-height: 620px; }
.stage-card, .info-card { min-width: 0; padding: 18px; }
.stage { position: relative; height: 100%; min-height: 580px; overflow: hidden; border-radius: 12px; background: radial-gradient(circle at 50% 44%, #15345a, #071523 68%); }
.stage-grid { position: absolute; inset: 0; opacity: .22; background-image: linear-gradient(rgba(108,162,221,.16) 1px, transparent 1px), linear-gradient(90deg, rgba(108,162,221,.16) 1px, transparent 1px); background-size: 42px 42px; }
.stage-caption { position: absolute; z-index: 4; top: 18px; left: 20px; display: flex; align-items: center; gap: 8px; color: rgba(218,232,251,.72); font-size: 11px; letter-spacing: .08em; }
.stage-caption i { width: 7px; height: 7px; border-radius: 50%; background: #7b8ca3; }
.stage.active .stage-caption i, .stage.seeking .stage-caption i { background: #45d483; box-shadow: 0 0 10px #45d483; }
.stage-caption span { color: #fff; letter-spacing: 0; }
.compass { position: absolute; z-index: 4; top: 18px; right: 20px; width: 94px; height: 94px; border: 1px solid rgba(163,196,235,.24); border-radius: 50%; }
.dial { position: absolute; inset: 8px; transition: transform .1s linear; }
.dial span { position: absolute; color: rgba(212,230,251,.55); font-size: 10px; }
.dial span:nth-child(1) { top: 0; left: 50%; transform: translateX(-50%); }
.dial span:nth-child(2) { top: 50%; right: 0; transform: translateY(-50%); }
.dial span:nth-child(3) { bottom: 0; left: 50%; transform: translateX(-50%); }
.dial span:nth-child(4) { top: 50%; left: 0; transform: translateY(-50%); }
.compass > b { position: absolute; inset: 0; display: grid; place-items: center; color: #fff; font: 700 17px "SF Mono", monospace; }
.offline-cover { position: absolute; z-index: 6; inset: 0; display: grid; place-content: center; gap: 8px; text-align: center; color: rgba(223,234,249,.72); background: rgba(5,16,29,.42); backdrop-filter: blur(2px); }
.offline-cover b { color: #fff; font-size: 18px; }
.offline-cover span { font-size: 12px; }
.panel-title { color: var(--text-tertiary); font-size: 11px; font-weight: 700; letter-spacing: .08em; text-transform: uppercase; }
.pose { margin-top: 10px; padding: 13px; border-radius: 9px; color: var(--accent); background: rgba(56,132,255,.07); font: 650 12px "SF Mono", monospace; line-height: 1.6; }
.stat-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 9px; margin-top: 12px; }
.stat-grid > div { padding: 12px; border: 1px solid var(--border-default); border-radius: 9px; background: rgba(255,255,255,.3); }
.stat-grid span, .last-action span { display: block; margin-bottom: 5px; color: var(--text-tertiary); font-size: 11px; }
.stat-grid b { color: var(--text-primary); font-size: 13px; }
.stat-grid b.hot { color: var(--accent); }
.last-action { margin-top: 10px; padding: 12px; border-radius: 9px; background: rgba(15,23,42,.035); }
.last-action b { color: var(--text-primary); font-size: 12px; }
.section-title { margin-top: 22px; margin-bottom: 10px; }
.presets { display: grid; gap: 7px; }
.presets > div { display: flex; align-items: center; gap: 9px; padding: 9px 10px; border: 1px solid var(--border-default); border-radius: 8px; color: var(--text-secondary); font-size: 12px; }
.presets > div.active { border-color: rgba(56,132,255,.4); color: var(--accent); background: rgba(56,132,255,.07); }
.presets small { display: grid; width: 21px; height: 21px; place-items: center; border-radius: 6px; color: #fff; background: var(--accent); }
.subscriptions { display: grid; gap: 7px; }
.subscriptions > div { display: grid; grid-template-columns: auto 1fr auto; align-items: center; gap: 8px; font-size: 12px; }
.subscriptions i { width: 7px; height: 7px; border-radius: 50%; background: var(--success); }
.subscriptions b { color: var(--text-tertiary); font: 600 10px "SF Mono", monospace; }
.empty { padding: 18px; border: 1px dashed var(--border-default); border-radius: 8px; color: var(--text-tertiary); text-align: center; font-size: 12px; }
@media (max-width: 960px) {
  .ptz-layout { grid-template-columns: 1fr; }
  .stage { min-height: 480px; }
}
</style>
