// 平台运行时视图(模块级单例):平台命令时间线、活跃订阅、长任务进度与平台下发的 OSD 配置。
// 全部来自后端语义事件,前端只做展示,不自己编造状态。
import { computed, ref } from "vue";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface CmdEntry {
  kind: string;
  summary: string;
  ts_ms: number;
}
export interface SubState {
  kind: string;
  active: boolean;
  notify_count: number;
}
export interface TaskProgress {
  kind: string;
  current: number;
  total: number;
  percent: number;
}
/** OSD 配置状态:反映平台下发的 OSD 配置命令(国标 A.2.3.2.11),设备已按其设置。 */
export interface OsdState {
  received: boolean;
  time_show: boolean;
  osd_show: boolean;
  at: string;
}

const MAX_CMD = 100;

export const kindLabel: Record<string, string> = {
  query: "查询",
  control: "控制",
  invite: "点播",
  broadcast: "广播",
  Catalog: "目录",
  Alarm: "报警",
  MobilePosition: "移动位置",
  PTZPosition: "PTZ精准位置",
  snapshot: "抓拍上传",
  upgrade: "在线升级",
};

const commands = ref<CmdEntry[]>([]);
const subs = ref<Record<string, SubState>>({});
const progress = ref<TaskProgress | null>(null);
const osd = ref<OsdState>({ received: false, time_show: false, osd_show: false, at: "" });

const subList = computed(() => Object.values(subs.value).filter((s) => s.active));

export function fmtCmdTs(ms: number): string {
  const d = new Date(ms);
  return (
    d.toLocaleTimeString("zh-CN", { hour12: false }) +
    "." +
    String(d.getMilliseconds()).padStart(3, "0")
  );
}

let unlistenCmd: UnlistenFn | null = null;
let unlistenSub: UnlistenFn | null = null;
let unlistenProg: UnlistenFn | null = null;
let unlistenOsd: UnlistenFn | null = null;
let progHideTimer: number | null = null;

async function startPlatformRuntime() {
  if (unlistenCmd) return;
  unlistenCmd = await listen<CmdEntry>("platform_command", (e) => {
    commands.value.unshift(e.payload);
    if (commands.value.length > MAX_CMD) commands.value.splice(MAX_CMD);
  });
  unlistenSub = await listen<SubState>("subscription_state", (e) => {
    subs.value = { ...subs.value, [e.payload.kind]: e.payload };
  });
  // 长任务进度(抓拍/升级);完成后 3s 自动隐藏。
  unlistenProg = await listen<TaskProgress>("task_progress", (e) => {
    progress.value = e.payload;
    if (progHideTimer) clearTimeout(progHideTimer);
    if (e.payload.percent >= 100) {
      progHideTimer = window.setTimeout(() => {
        progress.value = null;
      }, 3000);
    }
  });
  unlistenOsd = await listen<{ time_show: boolean; osd_show: boolean }>("osd_config", (e) => {
    osd.value = {
      received: true,
      time_show: e.payload.time_show,
      osd_show: e.payload.osd_show,
      at: new Date().toLocaleString("zh-CN", { hour12: false }),
    };
  });
}

function stopPlatformRuntime() {
  unlistenCmd?.();
  unlistenSub?.();
  unlistenProg?.();
  unlistenOsd?.();
  unlistenCmd = null;
  unlistenSub = null;
  unlistenProg = null;
  unlistenOsd = null;
  if (progHideTimer) {
    clearTimeout(progHideTimer);
    progHideTimer = null;
  }
}

export function usePlatformRuntime() {
  return {
    commands,
    subs,
    subList,
    progress,
    osd,
    startPlatformRuntime,
    stopPlatformRuntime,
  };
}
