// 云台姿态仿真(模块级单例):订阅后端 ptz_action / ptz_preset 事件,
// 把平台下发的方向+速度积分成连续姿态(pan 累计角、tilt ±90°、zoom 1~4×),驱动 3D 球机。
import { computed, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface PtzAction {
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
export interface PtzPresetPayload {
  id: number;
  name: string;
}

export const PRESET_IDS = [1, 2, 3, 4, 5] as const;
const DEFAULT_PRESET_NAMES: Record<number, string> = {
  1: "大门入口",
  2: "停车场",
  3: "接待大厅",
  4: "东侧通道",
  5: "西侧通道",
};
// 预置位目标位(演示用固定映射:每个预置位一个 pan/tilt/zoom)。
const PRESET_POS: Record<number, { pan: number; tilt: number; zoom: number }> = {
  1: { pan: -120, tilt: 20, zoom: 1 },
  2: { pan: 90, tilt: -15, zoom: 2 },
  3: { pan: 0, tilt: 30, zoom: 1.5 },
  4: { pan: 160, tilt: 0, zoom: 3 },
  5: { pan: -60, tilt: -30, zoom: 2.5 },
};
const IDLE_ACTION: PtzAction = {
  up: false,
  down: false,
  left: false,
  right: false,
  zoom_in: false,
  zoom_out: false,
  pan_speed: 0,
  tilt_speed: 0,
  zoom_speed: 0,
};

const ptz = ref<PtzAction>({ ...IDLE_ACTION });
const lastPtzAction = ref("STOP");
const lastPtzAt = ref("尚未收到平台 PTZ 命令");
// 云台连续姿态:pan 保留累计角度,可连续跨越任意圈;tilt/zoom 保持物理边界。
const pan = ref(0);
const tilt = ref(0); // 俯仰 -90~90(UI 模拟物理范围,并非国标强制机械限位)
const zoom = ref(1); // 变倍 1~4
// 预置位巡航(转到目标)状态:目标 + 是否在补间。
const target = ref<{ pan: number; tilt: number; zoom: number } | null>(null);
const seeking = ref(false);
const activePreset = ref<number | null>(null);
const activePresetName = ref("");
const presetNames = ref<Record<number, string>>({ ...DEFAULT_PRESET_NAMES });

const ptzActive = computed(
  () =>
    ptz.value.up ||
    ptz.value.down ||
    ptz.value.left ||
    ptz.value.right ||
    ptz.value.zoom_in ||
    ptz.value.zoom_out
);
// 是否在方向转动(不含变焦,变焦不算"转动")。
const moving = computed(() => {
  const m = ptz.value;
  return m.up || m.down || m.left || m.right;
});
const ptzButtons = computed(() => [
  { key: "up", label: "上", active: ptz.value.up },
  { key: "down", label: "下", active: ptz.value.down },
  { key: "left", label: "左", active: ptz.value.left },
  { key: "right", label: "右", active: ptz.value.right },
  { key: "zoom-in", label: "放大", active: ptz.value.zoom_in },
  { key: "zoom-out", label: "缩小", active: ptz.value.zoom_out },
  { key: "stop", label: "STOP", active: !ptzActive.value && !seeking.value },
]);
const zoomText = computed(() => {
  const value = `${zoom.value.toFixed(1)}×`;
  if (ptz.value.zoom_in) return `${value} · 放大中`;
  if (ptz.value.zoom_out) return `${value} · 缩小中`;
  return value;
});
const dirText = computed(() => {
  if (seeking.value && activePreset.value) {
    return `转向 ${activePresetName.value || `预置位 ${activePreset.value}`}`;
  }
  const m = ptz.value;
  const v = m.up ? "上" : m.down ? "下" : "";
  const h = m.left ? "左" : m.right ? "右" : "";
  return v + h || "—";
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
  return "STOP · 静止";
});
const normalizedPan = computed(() => ((pan.value % 360) + 360) % 360);
// 刻度盘反向旋转,保持固定指针显示当前绝对方位。
const compassDialStyle = computed(() => ({
  transform: `rotate(${(-normalizedPan.value).toFixed(1)}deg)`,
}));
const tiltMarkerStyle = computed(() => ({
  transform: `translateY(${((-tilt.value / 90) * 30).toFixed(1)}px)`,
}));
// 视野方位角文本(演示"摄像头当前朝向")。
const poseText = computed(
  () =>
    `方位 ${normalizedPan.value.toFixed(0).padStart(3, "0")}° · 累计 ${pan.value.toFixed(0)}° · 俯仰 ${tilt.value.toFixed(0)}° · ${zoom.value.toFixed(1)}×`
);

function ptzTimestamp(): string {
  return new Date().toLocaleTimeString("zh-CN", { hour12: false });
}

function describePtzAction(action: PtzAction): string {
  const labels: string[] = [];
  if (action.up) labels.push("上");
  if (action.down) labels.push("下");
  if (action.left) labels.push("左");
  if (action.right) labels.push("右");
  if (action.zoom_in) labels.push("放大");
  if (action.zoom_out) labels.push("缩小");
  return labels.length ? labels.join(" + ") : "STOP";
}

/** 把目标方位折算到"离当前累计角最近"的等价角,避免绕远路。 */
function nearestEquivalentPan(heading: number): number {
  const normalizedTarget = ((heading % 360) + 360) % 360;
  const delta = ((normalizedTarget - normalizedPan.value + 540) % 360) - 180;
  return pan.value + delta;
}

// 60ms 动画帧:手动 PTZ 按速度连续改 pan/tilt/zoom;预置位巡航平滑补间到目标。
function poseTick() {
  if (seeking.value && target.value) {
    const t = target.value;
    const ease = 0.08;
    pan.value += (t.pan - pan.value) * ease;
    tilt.value += (t.tilt - tilt.value) * ease;
    zoom.value += (t.zoom - zoom.value) * ease;
    if (
      Math.abs(t.pan - pan.value) < 0.5 &&
      Math.abs(t.tilt - tilt.value) < 0.5 &&
      Math.abs(t.zoom - zoom.value) < 0.02
    ) {
      pan.value = t.pan;
      tilt.value = t.tilt;
      zoom.value = t.zoom;
      seeking.value = false;
      target.value = null;
    }
    return;
  }
  const m = ptz.value;
  const ps = (m.pan_speed / 255) * 4 + 1.5;
  const ts = (m.tilt_speed / 255) * 4 + 1.5;
  if (m.left) pan.value -= ps;
  if (m.right) pan.value += ps;
  if (m.up) tilt.value = Math.min(90, tilt.value + ts);
  if (m.down) tilt.value = Math.max(-90, tilt.value - ts);
  if (m.zoom_in) zoom.value = Math.min(4, zoom.value + 0.03);
  if (m.zoom_out) zoom.value = Math.max(1, zoom.value - 0.03);
}

let poseTimer: number | null = null;
let ptzStopTimer: number | null = null;
let unlistenPtz: UnlistenFn | null = null;
let unlistenPreset: UnlistenFn | null = null;

/** 订阅 PTZ 事件并启动姿态动画帧(由 PtzConsole 在 onMounted 调用)。 */
async function startPtzTracking() {
  if (unlistenPtz) return;
  unlistenPtz = await listen<PtzAction>("ptz_action", (e) => {
    const m = e.payload;
    const active = m.up || m.down || m.left || m.right || m.zoom_in || m.zoom_out;
    // 手动命令(包括显式 STOP)立即抢占预置位巡航,避免姿态补间吞掉平台控制。
    seeking.value = false;
    target.value = null;
    activePreset.value = null;
    activePresetName.value = "";
    // 收到显式停止(全 false)立即归零(WVP 松手会发 0x00 停止命令,设备已实时应答)。
    ptz.value = m;
    lastPtzAction.value = describePtzAction(m);
    lastPtzAt.value = ptzTimestamp();
    if (ptzStopTimer) {
      clearTimeout(ptzStopTimer);
      ptzStopTimer = null;
    }
    // 仅作安全兜底:极少数平台松手不发停止命令时,3s 无新命令才自动归零。
    // 放宽到 3s 避免"按住时因平台重发间隔较长被误判停止"。
    if (active) {
      ptzStopTimer = window.setTimeout(() => {
        ptz.value = { ...IDLE_ACTION };
        lastPtzAction.value = "STOP（超时保护）";
        lastPtzAt.value = ptzTimestamp();
      }, 3000);
    }
  });
  // 预置位调用:使用设备按国标 PresetName 保存的名称,并平滑巡航到演示姿态。
  unlistenPreset = await listen<PtzPresetPayload>("ptz_preset", (e) => {
    const { id, name } = e.payload;
    if (ptzStopTimer) {
      clearTimeout(ptzStopTimer);
      ptzStopTimer = null;
    }
    ptz.value = { ...IDLE_ACTION };
    activePreset.value = id;
    activePresetName.value = name || `预置位${id}`;
    presetNames.value = { ...presetNames.value, [id]: activePresetName.value };
    const preset =
      PRESET_POS[id] ?? {
        pan: (id * 53) % 360,
        tilt: ((id * 37) % 180) - 90,
        zoom: 1 + (id % 3),
      };
    target.value = { ...preset, pan: nearestEquivalentPan(preset.pan) };
    lastPtzAction.value = `调用 ${activePresetName.value}（${id}）`;
    lastPtzAt.value = ptzTimestamp();
    seeking.value = true;
  });
  poseTimer = window.setInterval(poseTick, 60);
}

function stopPtzTracking() {
  unlistenPtz?.();
  unlistenPreset?.();
  unlistenPtz = null;
  unlistenPreset = null;
  if (poseTimer) {
    clearInterval(poseTimer);
    poseTimer = null;
  }
  if (ptzStopTimer) {
    clearTimeout(ptzStopTimer);
    ptzStopTimer = null;
  }
}

function gotoPreset(id: number) {
  if (ptzStopTimer) {
    clearTimeout(ptzStopTimer);
    ptzStopTimer = null;
  }
  ptz.value = { ...IDLE_ACTION };
  activePreset.value = id;
  activePresetName.value = presetNames.value[id] || `预置位${id}`;
  const preset =
    PRESET_POS[id] ?? {
      pan: (id * 53) % 360,
      tilt: ((id * 37) % 180) - 90,
      zoom: 1 + (id % 3),
    };
  target.value = { ...preset, pan: nearestEquivalentPan(preset.pan) };
  lastPtzAction.value = `测试调用 ${activePresetName.value}（${id}）`;
  lastPtzAt.value = ptzTimestamp();
  seeking.value = true;
}

function triggerManualPtz(action: string) {
  if (action === "stop") {
    seeking.value = false;
    target.value = null;
    ptz.value = { ...IDLE_ACTION };
    lastPtzAction.value = "STOP";
    lastPtzAt.value = ptzTimestamp();
    return;
  }
  seeking.value = false;
  target.value = null;
  const newAction: PtzAction = { ...IDLE_ACTION };
  if (action === "up") { newAction.up = true; newAction.tilt_speed = 120; }
  else if (action === "down") { newAction.down = true; newAction.tilt_speed = 120; }
  else if (action === "left") { newAction.left = true; newAction.pan_speed = 120; }
  else if (action === "right") { newAction.right = true; newAction.pan_speed = 120; }
  else if (action === "zoom-in") { newAction.zoom_in = true; newAction.zoom_speed = 8; }
  else if (action === "zoom-out") { newAction.zoom_out = true; newAction.zoom_speed = 8; }
  ptz.value = newAction;
  lastPtzAction.value = describePtzAction(newAction);
  lastPtzAt.value = ptzTimestamp();
  if (ptzStopTimer) clearTimeout(ptzStopTimer);
  ptzStopTimer = window.setTimeout(() => {
    ptz.value = { ...IDLE_ACTION };
    lastPtzAction.value = "STOP（自停）";
    lastPtzAt.value = ptzTimestamp();
  }, 1200);
}

export function usePtzPose() {
  return {
    ptz,
    pan,
    tilt,
    zoom,
    seeking,
    activePreset,
    presetNames,
    lastPtzAction,
    lastPtzAt,
    ptzActive,
    moving,
    ptzButtons,
    zoomText,
    dirText,
    speedText,
    statusText,
    normalizedPan,
    compassDialStyle,
    tiltMarkerStyle,
    poseText,
    gotoPreset,
    triggerManualPtz,
    startPtzTracking,
    stopPtzTracking,
  };
}
