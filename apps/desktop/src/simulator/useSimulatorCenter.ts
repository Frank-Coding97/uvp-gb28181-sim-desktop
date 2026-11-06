// 模拟器中心的功能状态。
//
// 设计原则：
// - 姿态由桌面端负责积分（和移动端一样，平台只下发速率/目标，不逐帧下发姿态）；
// - 平台命令从 Tauri 事件进入，纯展示状态保持“平台没下发就是空”的口径；
// - 真正缺字段的项（存储卡区域、遮挡区域、目标框等）不编造，页面明确写“待接入”。
//
// 这个 store 是模块级单例：切到别的页面再回来，姿态和预置位不丢。
import { computed, reactive, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type SimTab = "ptz" | "position" | "image" | "device";

export interface Pose {
  pan: number;
  tilt: number;
  zoom: number;
  focus: number;
  iris: number;
}

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

export interface SimPreset {
  id: number;
  name: string;
  pan: number;
  tilt: number;
  zoom: number;
}

export interface CruiseTrack {
  id: number;
  points: number[];
  speed: number | null;
  dwell: number | null;
  active: boolean;
}

export interface ScanGroup {
  id: number;
  left: number | null;
  right: number | null;
  speed: number | null;
  active: boolean;
}

export interface StorageCard {
  id: number;
  name: string;
  capacityMb: number;
  freeMb: number;
  status: string;
  progress: number | null;
}

export interface SimLastCommand {
  type: string;
  summary: string;
  at: number;
}

export interface RuntimeState {
  guarded: boolean;
  alarming: boolean;
  picture_mask_enabled: number | null;
  frame_mirror_mode: number | null;
  osd_time_show: number | null;
  osd_show: number | null;
  alarm_report_enabled: number | null;
}

interface ControlPresetDto {
  id: number;
  name: string;
  pan: number;
  tilt: number;
  zoom: number;
}

interface ControlCruiseDto {
  id: number;
  points: number[];
  speed: number | null;
  dwell: number | null;
  active: boolean;
}

interface ControlScanDto {
  id: number;
  left: [number, number, number] | null;
  right: [number, number, number] | null;
  speed: number | null;
  active: boolean;
}

interface ControlStorageCardDto {
  id: number;
  name: string;
  capacity_mb: number;
  free_mb: number;
  status: string;
  progress: number | null;
}

interface ControlSnapshot {
  pan: number;
  tilt: number;
  zoom: number;
  focus: number;
  iris: number;
  presets: ControlPresetDto[];
  home_enabled: boolean;
  home_preset_index: number | null;
  home_reset_time: number | null;
  cruise: ControlCruiseDto[];
  scan: ControlScanDto[];
  storage_queried: boolean;
  storage_cards: ControlStorageCardDto[];
  mask_configured: boolean;
  mask_on: boolean;
  mask_regions: Array<{
    seq: number;
    left: number;
    top: number;
    right: number;
    bottom: number;
  }>;
  osd_configured: boolean;
  osd_length: number;
  osd_width: number;
  osd_time_x: number;
  osd_time_y: number;
  osd_time_visible: boolean;
  osd_text_visible: boolean;
  osd_time_type: number | null;
  osd_items: Array<{ text: string; x: number; y: number }>;
  guarded: boolean;
  alarming: boolean;
  target_mode: string | null;
  target_box: [number, number, number, number] | null;
  drag_zoom_box: [number, number, number, number] | null;
}

const ZOOM_MAX = 20;
const PAN_RATE_MIN = 8;
const PAN_RATE_MAX = 120;
const TILT_RATE_MIN = 6;
const TILT_RATE_MAX = 80;

const FALLBACK_PRESETS: Record<number, [number, number, number]> = {
  1: [-120, 20, 1],
  2: [90, -15, 2],
  3: [0, 30, 1.5],
  4: [160, 0, 3],
  5: [-60, -30, 2.5],
};

// ---- 设备姿态 ----
const pose = reactive<Pose>({ pan: -36, tilt: 14, zoom: 3.2, focus: 0.5, iris: 0.5 });
const ptz = reactive<PtzAction>({
  up: false, down: false, left: false, right: false,
  zoom_in: false, zoom_out: false,
  pan_speed: 0, tilt_speed: 0, zoom_speed: 0,
});
const seeking = ref(false);
const seekLabel = ref("");
const activePreset = ref<number | null>(null);
const localPushing = ref(false);

// ---- 平台下发的只读状态 ----
const presets = ref<SimPreset[]>([]);
const home = reactive({
  configured: false,
  enabled: false,
  presetIndex: null as number | null,
  resetTime: null as number | null,
});
const cruiseTracks = ref<CruiseTrack[]>([]);
const scanGroups = ref<ScanGroup[]>([]);
const guarded = ref(false);
const alarming = ref(false);
const recording = ref(false);
const wiper = ref(false);
const targetActive = ref(false);
const targetMode = ref("");
const targetBox = ref<{ left: number; top: number; width: number; height: number } | null>(null);
const dragZoomBox = ref<{ left: number; top: number; width: number; height: number } | null>(null);
const keyframeAt = ref(0);
const snapshotAt = ref(0);
const formatAt = ref(0);
const dragZoomAt = ref(0);
const rebootUntil = ref(0);
const upgrade = ref<{ percent: number; result: string; firmware: string } | null>(null);
const storage = reactive({
  queried: false,
  queryAt: 0,
  cards: [] as StorageCard[],
});
const mask = reactive({
  configured: false,
  on: false,
  regions: [] as Array<{ seq: number; left: number; top: number; right: number; bottom: number }>,
});
const mirror = reactive({
  configured: false,
  mode: null as number | null,
});
const osd = reactive({
  configured: false,
  show: false,
  timeShow: false,
  length: 1920,
  width: 1080,
  timeX: 10,
  timeY: 10,
  timeType: null as number | null,
  items: [] as Array<{ text: string; x: number; y: number }>,
});
const lastCommand = ref<SimLastCommand | null>(null);
const nowMs = ref(Date.now());
const runtimeOnline = ref(false);
const autoFollow = ref(true);

let started = false;
const isTauriRuntime = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
let listeners: UnlistenFn[] = [];
let tickTimer = 0;
let pollTimer = 0;
let nowTimer = 0;
let lastTickAt = 0;
let lastPoseReportAt = 0;
let seekTarget: Pose | null = null;
let cruiseNextAt = 0;
let cruisePointCursor = 0;
let scanDirection = 1;
let lastActivityAt = Date.now();
let homeReturnArmed = true;
let ptzStopTimer = 0;
let lensPulse = { focus: 0, iris: 0, until: 0 };

function clamp(value: number, min: number, max: number) {
  return Math.min(max, Math.max(min, value));
}

function nearestPan(target: number): number {
  const delta = ((target - pose.pan + 540) % 360) - 180;
  return pose.pan + delta;
}

export function presetFallback(id: number): SimPreset {
  const fixed = FALLBACK_PRESETS[id];
  return {
    id,
    name: `预置位 ${id}`,
    pan: fixed?.[0] ?? (id * 53) % 360,
    tilt: fixed?.[1] ?? ((id * 37) % 180) - 90,
    zoom: fixed?.[2] ?? 1 + (id % 3),
  };
}

function upsertPreset(next: SimPreset) {
  const index = presets.value.findIndex((item) => item.id === next.id);
  if (index >= 0) presets.value.splice(index, 1, next);
  else presets.value.push(next);
  presets.value.sort((a, b) => a.id - b.id);
}

function removePreset(id: number) {
  presets.value = presets.value.filter((item) => item.id !== id);
  if (activePreset.value === id) activePreset.value = null;
}

function findPreset(id: number): SimPreset | undefined {
  return presets.value.find((item) => item.id === id);
}

export function seekToPreset(
  id: number,
  label: string,
  target?: { pan: number; tilt: number; zoom: number },
  rearm = true,
) {
  if (rearm) {
    lastActivityAt = Date.now();
    homeReturnArmed = true;
  }
  const preset = target ? { id, name: label, ...target } : findPreset(id) ?? presetFallback(id);
  seekTarget = {
    pan: nearestPan(preset.pan),
    tilt: clamp(preset.tilt, -90, 90),
    zoom: clamp(preset.zoom, 1, ZOOM_MAX),
    focus: pose.focus,
    iris: pose.iris,
  };
  activePreset.value = id;
  seekLabel.value = label;
  seeking.value = true;
  ptz.up = ptz.down = ptz.left = ptz.right = ptz.zoom_in = ptz.zoom_out = false;
  ptz.pan_speed = ptz.tilt_speed = ptz.zoom_speed = 0;
}

export function adjustPtz(dPan: number, dTilt: number, dZoom: number) {
  lastActivityAt = Date.now();
  homeReturnArmed = true;
  if (seeking.value) {
    seeking.value = false;
    seekTarget = null;
  }
  cruiseTracks.value.forEach((track) => { track.active = false; });
  activePreset.value = null;
  localPushing.value = true;
  window.setTimeout(() => { localPushing.value = false; }, 180);
  pose.pan = clamp(pose.pan + dPan, -180, 180);
  pose.tilt = clamp(pose.tilt + dTilt, -90, 90);
  pose.zoom = clamp(pose.zoom + dZoom, 1, ZOOM_MAX);
  ptz.up = ptz.down = ptz.left = ptz.right = ptz.zoom_in = ptz.zoom_out = false;
  ptz.pan_speed = ptz.tilt_speed = ptz.zoom_speed = 0;
}

export function adjustLens(dFocus: number, dIris: number) {
  lastActivityAt = Date.now();
  homeReturnArmed = true;
  localPushing.value = true;
  window.setTimeout(() => { localPushing.value = false; }, 180);
  pose.focus = clamp(pose.focus + dFocus, 0, 1);
  pose.iris = clamp(pose.iris + dIris, 0, 1);
}

function stopMotion() {
  ptz.up = ptz.down = ptz.left = ptz.right = ptz.zoom_in = ptz.zoom_out = false;
  ptz.pan_speed = ptz.tilt_speed = ptz.zoom_speed = 0;
}

function startCruise(trackId: number) {
  cruiseTracks.value.forEach((track) => { track.active = track.id === trackId; });
  scanGroups.value.forEach((group) => { group.active = false; });
  scanDirection = 1;
  cruiseNextAt = 0;
  cruisePointCursor = 0;
}

function stopCruise() {
  cruiseTracks.value.forEach((track) => { track.active = false; });
  cruiseNextAt = 0;
}

function activeCruise(): CruiseTrack | undefined {
  return cruiseTracks.value.find((track) => track.active);
}

function activeScan(): ScanGroup | undefined {
  return scanGroups.value.find((group) => group.active);
}

function tick() {
  const now = performance.now();
  const dt = lastTickAt === 0 ? 0.033 : Math.min(0.2, (now - lastTickAt) / 1000);
  lastTickAt = now;
  nowMs.value = Date.now();

  const busy =
    seeking.value ||
    activeCruise() !== undefined ||
    activeScan() !== undefined ||
    ptz.up || ptz.down || ptz.left || ptz.right || ptz.zoom_in || ptz.zoom_out;
  if (busy) {
    lastActivityAt = nowMs.value;
    homeReturnArmed = true;
  } else if (
    homeReturnArmed &&
    home.configured &&
    home.enabled &&
    home.presetIndex !== null &&
    home.resetTime !== null &&
    home.resetTime > 0 &&
    nowMs.value - lastActivityAt > home.resetTime * 1000 &&
    findPreset(home.presetIndex)
  ) {
    homeReturnArmed = false;
    seekToPreset(home.presetIndex, `看守位归位 P${home.presetIndex}`, undefined, false);
  }

  if (seeking.value && seekTarget) {
    const k = 1 - Math.exp(-dt * 3);
    pose.pan += (seekTarget.pan - pose.pan) * k;
    pose.tilt += (seekTarget.tilt - pose.tilt) * k;
    pose.zoom += (seekTarget.zoom - pose.zoom) * k;
    if (
      Math.abs(seekTarget.pan - pose.pan) < 0.4 &&
      Math.abs(seekTarget.tilt - pose.tilt) < 0.4 &&
      Math.abs(seekTarget.zoom - pose.zoom) < 0.02
    ) {
      pose.pan = seekTarget.pan;
      pose.tilt = seekTarget.tilt;
      pose.zoom = seekTarget.zoom;
      seeking.value = false;
      seekTarget = null;
    }
  } else {
    const scan = activeScan();
    if (scan) {
      // 自动扫描：在左右边界之间来回扫，速度是 12 位原值。
      if (scan.left !== null && scan.right !== null) {
        const left = Math.min(scan.left, scan.right);
        const right = Math.max(scan.left, scan.right);
        const rate = 4 + ((scan.speed ?? 2048) / 4095) * 36;
        let next = pose.pan + scanDirection * rate * dt;
        if (next >= right) {
          next = right;
          scanDirection = -1;
        } else if (next <= left) {
          next = left;
          scanDirection = 1;
        }
        pose.pan = clamp(next, -180, 180);
      }
    } else {
      const track = activeCruise();
      if (track) {
        if (nowMs.value >= cruiseNextAt && track.points.length > 0) {
          const point = track.points[cruisePointCursor % track.points.length];
          const preset = findPreset(point);
          if (preset) {
            seekToPreset(point, `巡航 #${track.id} → P${point}`);
            cruisePointCursor += 1;
            cruiseNextAt = nowMs.value + Math.max(1, track.dwell ?? 3) * 1000 + 1200;
          } else {
            cruiseNextAt = nowMs.value + 800;
          }
        }
      } else {
        const panStep = (PAN_RATE_MIN + (ptz.pan_speed / 255) * (PAN_RATE_MAX - PAN_RATE_MIN)) * dt;
        const tiltStep = (TILT_RATE_MIN + (ptz.tilt_speed / 255) * (TILT_RATE_MAX - TILT_RATE_MIN)) * dt;
        const zoomRate = 0.25 + (ptz.zoom_speed / 15) * 1.75;
        if (ptz.left) pose.pan = clamp(pose.pan - panStep, -180, 180);
        if (ptz.right) pose.pan = clamp(pose.pan + panStep, -180, 180);
        if (ptz.up) pose.tilt = clamp(pose.tilt + tiltStep, -90, 90);
      if (ptz.down) pose.tilt = clamp(pose.tilt - tiltStep, -90, 90);
      if (ptz.zoom_in) pose.zoom = clamp(pose.zoom * Math.exp(zoomRate * dt), 1, ZOOM_MAX);
      if (ptz.zoom_out) pose.zoom = clamp(pose.zoom * Math.exp(-zoomRate * dt), 1, ZOOM_MAX);
    }
  }
}

  // 平台 FI 命令只有方向、没有逐帧速率，按短脉冲处理。
  if (lensPulse.until > Date.now()) {
    pose.focus = clamp(pose.focus + lensPulse.focus * dt, 0, 1);
    pose.iris = clamp(pose.iris + lensPulse.iris * dt, 0, 1);
  } else if (lensPulse.focus !== 0 || lensPulse.iris !== 0) {
    lensPulse.focus = 0;
    lensPulse.iris = 0;
  }

  // 后端只存姿态、不做积分；约 8Hz 节流回写，供平台 SetPreset / 扫描边界取真值。
  if (isTauriRuntime && nowMs.value - lastPoseReportAt > 120) {
    lastPoseReportAt = nowMs.value;
    void invoke("report_ptz_pose", {
      pan: pose.pan,
      tilt: pose.tilt,
      zoom: pose.zoom,
      focus: pose.focus,
      iris: pose.iris,
    }).catch(() => undefined);
  }
}

function onPtzAction(action: PtzAction) {
  lastActivityAt = Date.now();
  homeReturnArmed = true;
  seeking.value = false;
  seekTarget = null;
  activePreset.value = null;
  ptz.up = action.up;
  ptz.down = action.down;
  ptz.left = action.left;
  ptz.right = action.right;
  ptz.zoom_in = action.zoom_in;
  ptz.zoom_out = action.zoom_out;
  ptz.pan_speed = action.pan_speed;
  ptz.tilt_speed = action.tilt_speed;
  ptz.zoom_speed = action.zoom_speed;
  lastCommand.value = { type: "ptz_action", summary: describeAction(action), at: Date.now() };
  window.clearTimeout(ptzStopTimer);
  if (action.up || action.down || action.left || action.right || action.zoom_in || action.zoom_out) {
    ptzStopTimer = window.setTimeout(() => {
      stopMotion();
      lastCommand.value = { type: "ptz_action", summary: "PTZ 停止（超时保护）", at: Date.now() };
    }, 3000);
  }
}

function describeAction(action: PtzAction): string {
  const parts: string[] = [];
  if (action.up) parts.push("上");
  if (action.down) parts.push("下");
  if (action.left) parts.push("左");
  if (action.right) parts.push("右");
  if (action.zoom_in) parts.push("放大");
  if (action.zoom_out) parts.push("缩小");
  return parts.length ? `PTZ ${parts.join("+")}` : "PTZ 停止";
}

function pulseLens(focus: number, iris: number) {
  lensPulse = { focus, iris, until: Date.now() + 900 };
}

function normalizeSummary(raw: string): string {
  // 后端控制类事件统一是「平台控制:<detail> → 已应答」；
  // 查询类没有这个前缀。归一化后再做语义解析，避免每个正则都带壳。
  return raw
    .replace(/^平台控制[:：]\s*/, "")
    .replace(/\s*→\s*已应答.*$/, "")
    .trim();
}

function applySummary(rawSummary: string) {
  const summary = normalizeSummary(rawSummary);
  lastCommand.value = { type: "platform_command", summary, at: Date.now() };

  let match = /^PTZ 设置预置位 (\d+)$/.exec(summary);
  if (match) {
    const id = Number(match[1]);
    upsertPreset({
      id,
      name: findPreset(id)?.name ?? `预置位 ${id}`,
      pan: pose.pan,
      tilt: pose.tilt,
      zoom: pose.zoom,
    });
    return;
  }

  match = /^PTZ 删除预置位 (\d+)$/.exec(summary);
  if (match) {
    removePreset(Number(match[1]));
    return;
  }

  match = /^PTZ 调用预置位 (\d+)$/.exec(summary);
  if (match) {
    const id = Number(match[1]);
    activePreset.value = id;
    // 正常平台会在应答后紧接一个 ptz_preset 事件；这里先动起来，
    // 事件到达后会再对齐一次真实位姿，避免“点了没反应”。
    seekToPreset(id, `预置位 ${id}`);
    return;
  }

  match = /^巡航(\d+)增点\(预置(\d+)\)$/.exec(summary);
  if (match) {
    const trackId = Number(match[1]);
    const point = Number(match[2]);
    let track = cruiseTracks.value.find((item) => item.id === trackId);
    if (!track) {
      track = { id: trackId, points: [], speed: null, dwell: null, active: false };
      cruiseTracks.value.push(track);
      cruiseTracks.value.sort((a, b) => a.id - b.id);
    }
    if (!track.points.includes(point)) track.points.push(point);
    return;
  }

  match = /^巡航(\d+)删点\(预置(\d+)\)$/.exec(summary);
  if (match) {
    const trackId = Number(match[1]);
    const point = Number(match[2]);
    const track = cruiseTracks.value.find((item) => item.id === trackId);
    if (track) track.points = track.points.filter((item) => item !== point);
    return;
  }

  match = /^巡航(\d+)设速(\d+)$/.exec(summary);
  if (match) {
    const trackId = Number(match[1]);
    const speed = Number(match[2]);
    const track = cruiseTracks.value.find((item) => item.id === trackId);
    if (track) track.speed = speed;
    return;
  }

  match = /^巡航(\d+)停留(\d+)s$/.exec(summary);
  if (match) {
    const trackId = Number(match[1]);
    const dwell = Number(match[2]);
    const track = cruiseTracks.value.find((item) => item.id === trackId);
    if (track) track.dwell = dwell;
    return;
  }

  match = /^巡航(\d+)启动$/.exec(summary);
  if (match) {
    startCruise(Number(match[1]));
    return;
  }
  if (summary === "巡航停止") {
    stopCruise();
    return;
  }

  match = /^扫描(\d+)启动$/.exec(summary);
  if (match) {
    const groupId = Number(match[1]);
    let group = scanGroups.value.find((item) => item.id === groupId);
    if (!group) {
      group = { id: groupId, left: null, right: null, speed: null, active: false };
      scanGroups.value.push(group);
    }
    scanGroups.value.forEach((item) => { item.active = item.id === groupId; });
    cruiseTracks.value.forEach((track) => { track.active = false; });
    scanDirection = 1;
    return;
  }
  match = /^扫描(\d+)设左边界$/.exec(summary);
  if (match) {
    const groupId = Number(match[1]);
    let group = scanGroups.value.find((item) => item.id === groupId);
    if (!group) {
      group = { id: groupId, left: null, right: null, speed: null, active: false };
      scanGroups.value.push(group);
    }
    group.left = pose.pan;
    return;
  }
  match = /^扫描(\d+)设右边界$/.exec(summary);
  if (match) {
    const groupId = Number(match[1]);
    let group = scanGroups.value.find((item) => item.id === groupId);
    if (!group) {
      group = { id: groupId, left: null, right: null, speed: null, active: false };
      scanGroups.value.push(group);
    }
    group.right = pose.pan;
    return;
  }
  match = /^扫描(\d+)设速(\d+)$/.exec(summary);
  if (match) {
    const groupId = Number(match[1]);
    let group = scanGroups.value.find((item) => item.id === groupId);
    if (!group) {
      group = { id: groupId, left: null, right: null, speed: null, active: false };
      scanGroups.value.push(group);
    }
    group.speed = Number(match[2]);
    return;
  }
  if (summary === "PTZ 停止") {
    stopMotion();
    stopCruise();
    scanGroups.value.forEach((group) => { group.active = false; });
    seeking.value = false;
    seekTarget = null;
    return;
  }

  if (summary === "辅助雨刷开") {
    wiper.value = true;
    return;
  }
  if (summary === "辅助雨刷关") {
    wiper.value = false;
    return;
  }

  match = /^布防\/撤防:(SetGuard|ResetGuard)$/.exec(summary);
  if (match) {
    guarded.value = match[1] === "SetGuard";
    return;
  }
  if (summary === "报警复位") {
    alarming.value = false;
    return;
  }

  match = /^录像控制:(Record|StopRecord)$/.exec(summary);
  if (match) {
    recording.value = match[1] === "Record";
    return;
  }

  if (summary === "强制关键帧") {
    keyframeAt.value = Date.now();
    return;
  }
  if (/^抓拍/.test(summary)) {
    snapshotAt.value = Date.now();
    return;
  }
  if (summary === "目标跟踪") {
    targetActive.value = true;
    targetMode.value = "平台已下发";
    targetBox.value = null;
    return;
  }
  match = /^目标跟踪:(Auto|Manual|Stop)$/.exec(summary);
  if (match) {
    const mode = match[1];
    targetActive.value = mode !== "Stop";
    targetMode.value = mode === "Stop" ? "" : mode === "Auto" ? "自动" : "手动";
    targetBox.value = null;
    if (mode !== "Stop") {
      // 框坐标只在后端快照里，下一拍拉一次。
      window.setTimeout(() => { void refreshControlState(); }, 200);
    }
    return;
  }
  match = /^看守位设置:Enabled(\d+) Preset(\d+) Reset(\d+)$/.exec(summary);
  if (match) {
    home.configured = true;
    home.enabled = Number(match[1]) === 1;
    home.presetIndex = Number(match[2]) || null;
    home.resetTime = Number(match[3]) || null;
    return;
  }
  if (summary === "看守位设置") {
    home.configured = true;
    return;
  }
  if (summary === "远程启动") {
    rebootUntil.value = Date.now() + 6500;
    return;
  }
  match = /^格式化SD卡(?::Disk(\d+))?$/.exec(summary);
  if (match) {
    formatAt.value = Date.now();
    return;
  }
  match = /^在线升级(?::(.+))?$/.exec(summary);
  if (match) {
    upgrade.value = { percent: 0, result: "平台已下发", firmware: match[1] ?? "" };
    return;
  }
  if (summary === "拉框放大" || summary === "拉框缩小") {
    dragZoomAt.value = Date.now();
    window.setTimeout(() => { void refreshControlState(); }, 120);
    return;
  }

  match = /^精确云台 Pan(-?\d+(?:\.\d+)?) Tilt(-?\d+(?:\.\d+)?) Zoom(-?\d+(?:\.\d+)?)$/.exec(summary);
  if (match) {
    seekTarget = {
      pan: nearestPan(Number(match[1])),
      tilt: clamp(Number(match[2]), -90, 90),
      zoom: clamp(Number(match[3]), 1, ZOOM_MAX),
      focus: pose.focus,
      iris: pose.iris,
    };
    seeking.value = true;
    activePreset.value = null;
    seekLabel.value = "精确控制";
    return;
  }

  if (/^光圈/.test(summary) || /^聚焦/.test(summary)) {
    if (summary.includes("光圈+")) pulseLens(0, 1);
    if (summary.includes("光圈") && summary.includes("−")) pulseLens(0, -1);
    if (summary.includes("聚焦近")) pulseLens(-1, 0);
    if (summary.includes("聚焦远")) pulseLens(1, 0);
    if (summary === "FI 停止") lensPulse = { focus: 0, iris: 0, until: 0 };
    return;
  }

  if (/^查询存储卡状态/.test(summary)) {
    storage.queried = true;
    storage.queryAt = Date.now();
    storage.cards = [{
      id: 1,
      name: "SD Card 1",
      capacityMb: 32768,
      freeMb: 24576,
      status: "正常",
      progress: null,
    }];
    return;
  }

  if (summary === "设备配置") {
    void refreshRuntime();
    void refreshControlState();
  }
}

function hydrateControl(snapshot: ControlSnapshot) {
  presets.value = snapshot.presets.map((preset) => ({
    id: preset.id,
    name: preset.name,
    pan: preset.pan,
    tilt: preset.tilt,
    zoom: preset.zoom,
  }));
  home.enabled = snapshot.home_enabled;
  home.presetIndex = snapshot.home_preset_index;
  home.resetTime = snapshot.home_reset_time;
  home.configured = snapshot.home_preset_index !== null || snapshot.home_reset_time !== null;
  cruiseTracks.value = snapshot.cruise.map((track) => ({
    id: track.id,
    points: [...track.points],
    speed: track.speed,
    dwell: track.dwell,
    active: track.active,
  }));
  scanGroups.value = snapshot.scan.map((group) => ({
    id: group.id,
    left: group.left?.[0] ?? null,
    right: group.right?.[0] ?? null,
    speed: group.speed,
    active: group.active,
  }));
  storage.queried = snapshot.storage_queried;
  storage.cards = snapshot.storage_cards.map((card) => ({
    id: card.id,
    name: card.name,
    capacityMb: card.capacity_mb,
    freeMb: card.free_mb,
    status: card.status,
    progress: card.progress,
  }));
  mask.configured = snapshot.mask_configured ?? false;
  mask.on = snapshot.mask_on ?? false;
  mask.regions = (snapshot.mask_regions ?? []).map((region) => ({ ...region }));
  osd.configured = snapshot.osd_configured ?? false;
  osd.length = snapshot.osd_length || 1920;
  osd.width = snapshot.osd_width || 1080;
  osd.timeX = snapshot.osd_time_x ?? 10;
  osd.timeY = snapshot.osd_time_y ?? 10;
  osd.timeShow = snapshot.osd_time_visible ?? false;
  osd.show = snapshot.osd_text_visible ?? false;
  osd.timeType = snapshot.osd_time_type ?? null;
  osd.items = (snapshot.osd_items ?? []).map((item) => ({ ...item }));
  guarded.value = snapshot.guarded;
  alarming.value = snapshot.alarming;
  targetActive.value = snapshot.target_mode !== null;
  targetMode.value = snapshot.target_mode === "Auto"
    ? "自动"
    : snapshot.target_mode === "Manual"
      ? "手动"
      : "";
  targetBox.value = snapshot.target_box
    ? {
        left: snapshot.target_box[0],
        top: snapshot.target_box[1],
        width: snapshot.target_box[2],
        height: snapshot.target_box[3],
      }
    : null;
  dragZoomBox.value = snapshot.drag_zoom_box
    ? {
        left: snapshot.drag_zoom_box[0],
        top: snapshot.drag_zoom_box[1],
        width: snapshot.drag_zoom_box[2],
        height: snapshot.drag_zoom_box[3],
      }
    : null;
  lastActivityAt = Date.now();
  homeReturnArmed = true;
}

async function refreshControlState() {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  try {
    const snapshot = await invoke<ControlSnapshot>("get_device_control_state");
    hydrateControl(snapshot);
  } catch {
    // 设备还没启动时该命令会报错；等注册成功事件再拉。
  }
}

async function refreshRuntime() {
  if (typeof window === "undefined" || !("__TAURI_INTERNALS__" in window)) return;
  try {
    const state = await invoke<RuntimeState>("get_device_runtime_state");
    runtimeOnline.value = true;
    guarded.value = state.guarded;
    alarming.value = state.alarming;
    mask.configured = state.picture_mask_enabled !== null;
    mask.on = state.picture_mask_enabled === 1;
    mirror.configured = state.frame_mirror_mode !== null;
    mirror.mode = state.frame_mirror_mode;
    osd.configured = state.osd_show !== null || state.osd_time_show !== null;
    osd.show = state.osd_show === 1;
    osd.timeShow = state.osd_time_show === 1;
  } catch {
    runtimeOnline.value = false;
  }
}

function onDeviceState(state: string) {
  if (state === "Disconnected" || state === "Failed") {
    stopMotion();
    stopCruise();
    scanGroups.value.forEach((group) => { group.active = false; });
    seeking.value = false;
    seekTarget = null;
    activePreset.value = null;
    guarded.value = false;
    alarming.value = false;
    recording.value = false;
    wiper.value = false;
    targetActive.value = false;
    targetMode.value = "";
    targetBox.value = null;
    dragZoomBox.value = null;
    rebootUntil.value = 0;
    upgrade.value = null;
    storage.queried = false;
    storage.cards = [];
    lastCommand.value = null;
    homeReturnArmed = false;
  }
}

function registerListeners() {
  const tasks: Array<Promise<UnlistenFn>> = [
    listen<PtzAction>("ptz_action", (event) => onPtzAction(event.payload)),
    listen<{ id: number; name: string; pose?: { pan: number; tilt: number; zoom: number } }>(
      "ptz_preset",
      (event) => {
        const { id, name, pose: target } = event.payload;
        upsertPreset({
          id,
          name: name || `预置位 ${id}`,
          pan: target?.pan ?? findPreset(id)?.pan ?? presetFallback(id).pan,
          tilt: target?.tilt ?? findPreset(id)?.tilt ?? presetFallback(id).tilt,
          zoom: target?.zoom ?? findPreset(id)?.zoom ?? presetFallback(id).zoom,
        });
        seekToPreset(id, `预置位 ${id}`, target);
        lastCommand.value = { type: "ptz_preset", summary: `调用预置位 ${id}`, at: Date.now() };
      },
    ),
    listen<{ kind: string; summary: string; ts_ms: number }>(
      "platform_command",
      (event) => {
        applySummary(event.payload.summary);
        void refreshRuntime();
      },
    ),
    listen<{ kind: string; current: number; total: number; percent: number }>(
      "task_progress",
      (event) => {
        if (event.payload.kind === "upgrade") {
          upgrade.value = {
            percent: event.payload.percent,
            result: event.payload.percent >= 100 ? "升级成功" : "升级中",
            firmware: "",
          };
        }
        if (event.payload.kind === "snapshot") {
          snapshotAt.value = Date.now();
        }
      },
    ),
    listen<string>("device_state", (event) => {
      onDeviceState(event.payload);
      if (event.payload === "Registered") void refreshControlState();
    }),
    listen<{ time_show: boolean; osd_show: boolean }>("osd_config", (event) => {
      osd.configured = true;
      osd.timeShow = event.payload.time_show;
      osd.show = event.payload.osd_show;
    }),
  ];
  void Promise.all(tasks).then((values) => { listeners = values; });
}

export function useSimulatorCenter() {
  const ptzActive = computed(() =>
    ptz.up || ptz.down || ptz.left || ptz.right || ptz.zoom_in || ptz.zoom_out,
  );
  const activeCruiseTrack = computed(() => activeCruise()?.id ?? null);
  const activeScanGroup = computed(() => activeScan()?.id ?? null);
  const recentCommand = computed(() => {
    const cmd = lastCommand.value;
    return cmd && nowMs.value - cmd.at < 3000 ? cmd : null;
  });
  const rebooting = computed(() => nowMs.value < rebootUntil.value);
  const keyframeActive = computed(() => nowMs.value - keyframeAt.value < 2000);
  const headline = computed(() => {
    if (rebooting.value) return { text: "远程重启中", color: "var(--warning)" };
    const track = activeCruise();
    if (track) return { text: `巡航 #${track.id} 运行中`, color: "var(--accent)" };
    const scan = activeScan();
    if (scan) {
      const ready = scan.left !== null && scan.right !== null;
      return {
        text: ready ? `扫描 #${scan.id} 运行中` : `扫描 #${scan.id} 已启动 · 缺边界`,
        color: "var(--accent)",
      };
    }
    if (targetActive.value) return { text: "目标跟踪中", color: "var(--warning)" };
    if (seeking.value) return { text: seekLabel.value || "云台转向中", color: "var(--accent)" };
    if (ptzActive.value) return { text: "PTZ 运动中", color: "var(--accent)" };
    if (recentCommand.value) return { text: `刚收到 ${recentCommand.value.summary}`, color: "var(--success)" };
    return { text: "等待平台下发控制指令", color: "var(--text-tertiary)" };
  });
  const normalizedPan = computed(() => ((pose.pan % 360) + 360) % 360);
  const directionText = computed(() => {
    const vertical = ptz.up ? "上" : ptz.down ? "下" : "";
    const horizontal = ptz.left ? "左" : ptz.right ? "右" : "";
    return vertical + horizontal || "—";
  });
  const speedText = computed(() => {
    if (ptz.zoom_in || ptz.zoom_out) return `变倍 ${ptz.zoom_speed}/15`;
    const parts: string[] = [];
    if (ptz.left || ptz.right) parts.push(`水平 ${ptz.pan_speed}`);
    if (ptz.up || ptz.down) parts.push(`垂直 ${ptz.tilt_speed}`);
    return parts.join(" · ") || "—";
  });

  function start() {
    if (started) return;
    started = true;
    lastTickAt = 0;
    tickTimer = window.setInterval(tick, 33);
    nowTimer = window.setInterval(() => { nowMs.value = Date.now(); }, 500);
    pollTimer = window.setInterval(() => { void refreshRuntime(); }, 1500);
    void refreshRuntime();
    void refreshControlState();
    if (isTauriRuntime) registerListeners();
  }

  function stop() {
    if (!started) return;
    started = false;
    listeners.forEach((unlisten) => unlisten());
    listeners = [];
    window.clearInterval(tickTimer);
    window.clearInterval(pollTimer);
    window.clearInterval(nowTimer);
    window.clearTimeout(ptzStopTimer);
    tickTimer = 0;
    pollTimer = 0;
    nowTimer = 0;
  }

  return {
    pose, ptz, presets, home, cruiseTracks, scanGroups,
    guarded, alarming, recording, wiper, targetActive,
    targetMode, targetBox, dragZoomBox,
    keyframeAt, snapshotAt, formatAt, dragZoomAt, rebootUntil, upgrade,
    storage, mask, mirror, osd, lastCommand, nowMs, runtimeOnline,
    autoFollow, seeking, activePreset, ptzActive, activeCruiseTrack,
    activeScanGroup,
    recentCommand, rebooting, keyframeActive, headline,
    normalizedPan, directionText, speedText, localPushing,
    start, stop, refreshRuntime, refreshControlState, hydrateControl,
    adjustPtz, adjustLens, seekToPreset,
    applySummary, presetFallback,
  };
}

export function tabForSummary(summary: string): SimTab | null {
  if (/预置位|巡航|扫描|看守位|HomePosition/.test(summary)) return "position";
  if (/关键帧|抓拍|目标跟踪|拉框|设备配置|OSD/.test(summary)) return "image";
  if (/录像|布防|撤防|报警|远程启动|升级|格式化|辅助|雨刷/.test(summary)) return "device";
  if (/PTZ|精确云台/.test(summary)) return "ptz";
  return null;
}
