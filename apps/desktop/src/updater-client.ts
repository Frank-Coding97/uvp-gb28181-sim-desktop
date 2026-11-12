// 在线升级运行时：检查 → 下载 → 安装 → 重启。
//
// 产品口径（2026-11-12 定）：
// - 启动时**静默**查一次，只往通知中心放一条「发现新版本」，不弹窗、不打断压测；
// - 真正的升级入口固定在「关于」页，由用户手动点，因为安装完成需要重启应用，
//   正在跑的设备和压测会中断；
// - 开发态不查线上清单：dev 版本号永远落后于已发布版本，查了只会天天提示。
import { computed, reactive } from "vue";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { pushNotification } from "./notification-center";
import { formatBytes, normalizeVersion, shouldNotifyVersion, updateNotice } from "./updater.mjs";

/** 清单在官网，国内直连一般 1 秒内返回；超过 20 秒按网络异常处理，别让用户干等。 */
const CHECK_TIMEOUT_MS = 20_000;

/** 同一个版本只提示一次，避免每次启动都往通知中心塞重复消息。 */
const NOTIFIED_KEY = "uvp.update.notified";

export type UpdaterPhase =
  | "idle"
  | "checking"
  | "latest"
  | "available"
  | "downloading"
  | "installing"
  | "installed"
  | "error";

interface UpdaterState {
  phase: UpdaterPhase;
  /** 清单里公告的版本号（已去掉 v 前缀）。 */
  version: string | null;
  /** 清单里的更新说明原文。 */
  notes: string;
  /** 已下载字节数 / 总字节数（总数未知时为 0）。 */
  downloaded: number;
  total: number;
  error: string;
  /** 最近一次检查完成的时间戳，0 表示还没查过。 */
  checkedAt: number;
  /** 人工触发的那次检查（用来区分「静默失败」和「点了没反应」）。 */
  manual: boolean;
}

export const updaterState = reactive<UpdaterState>({
  phase: "idle",
  version: null,
  notes: "",
  downloaded: 0,
  total: 0,
  error: "",
  checkedAt: 0,
  manual: false,
});

/** 检查/下载/安装进行中：按钮要禁用，避免重复触发。 */
export const updaterBusy = computed(() =>
  updaterState.phase === "checking" ||
  updaterState.phase === "downloading" ||
  updaterState.phase === "installing",
);

export const updaterProgress = computed(() => ({
  downloaded: formatBytes(updaterState.downloaded),
  total: updaterState.total > 0 ? formatBytes(updaterState.total) : "",
}));

/** 下载好的更新包；确认 install 前先留着，失败可重试安装而不必重新下载。 */
let pending: Update | null = null;
let inflight: Promise<void> | null = null;

function isTauri() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function inDev() {
  return Boolean(import.meta.env?.DEV);
}

function lastNotified(): string {
  try {
    return window.localStorage.getItem(NOTIFIED_KEY) ?? "";
  } catch {
    return "";
  }
}

function rememberNotified(version: string) {
  try {
    window.localStorage.setItem(NOTIFIED_KEY, normalizeVersion(version));
  } catch {
    // 隐私模式/存储被禁用时只是会重复提示一次，不影响升级本身。
  }
}

/** 释放 Rust 侧的更新资源；不装就关掉，否则每次检查都在后端留一份句柄。 */
async function dropPending() {
  const current = pending;
  pending = null;
  if (!current) return;
  try {
    await current.close();
  } catch {
    // 资源已经释放过时 close 会报错，忽略即可。
  }
}

function errorText(err: unknown): string {
  const text = err instanceof Error ? err.message : String(err ?? "");
  if (/timeout|timed out|network|dns|connect|tls|certificate/i.test(text)) {
    return "无法连接更新服务器，请检查网络后重试。";
  }
  if (/signature|verify/i.test(text)) {
    return "更新包签名校验失败，已拒绝安装。请联系维护者确认发布产物。";
  }
  return text || "检查更新失败，请稍后重试。";
}

/**
 * 检查更新。
 * @param silent 启动时的后台检查：失败不打扰用户，命中新版本才进通知中心。
 */
export async function checkForUpdates({ silent = false } = {}): Promise<void> {
  if (inflight) return inflight;
  if (!isTauri()) {
    updaterState.phase = "error";
    updaterState.error = "当前不在桌面端运行，无法检查更新。";
    return;
  }
  if (inDev()) {
    updaterState.phase = "error";
    updaterState.manual = !silent;
    updaterState.error = "开发模式下不检查在线更新，请用发布包验证。";
    return;
  }

  updaterState.manual = !silent;
  updaterState.phase = "checking";
  updaterState.error = "";

  inflight = (async () => {
    try {
      const update = await check({ timeout: CHECK_TIMEOUT_MS });
      updaterState.checkedAt = Date.now();
      if (!update) {
        await dropPending();
        updaterState.phase = "latest";
        updaterState.version = null;
        updaterState.notes = "";
        return;
      }
      await dropPending();
      pending = update;
      updaterState.version = normalizeVersion(update.version);
      updaterState.notes = update.body ?? "";
      updaterState.phase = "available";
      if (silent && shouldNotifyVersion(lastNotified(), update.version)) {
        const notice = updateNotice(update.version, update.body);
        if (notice) {
          pushNotification(notice);
          rememberNotified(update.version);
        }
      }
    } catch (err) {
      updaterState.checkedAt = Date.now();
      updaterState.phase = "error";
      updaterState.error = errorText(err);
    } finally {
      inflight = null;
    }
  })();

  return inflight;
}

/**
 * 下载并安装，成功后重启到新版本。
 *
 * Windows 上 install 会直接退出应用（安装器接管），所以 relaunch 只在
 * macOS/Linux 有意义；统一调用也不会出问题——进程已经没了，调用不会执行。
 */
export async function installUpdate(): Promise<void> {
  if (!pending || updaterBusy.value) return;
  const update = pending;
  updaterState.phase = "downloading";
  updaterState.error = "";
  updaterState.downloaded = 0;
  updaterState.total = 0;

  try {
    await update.download((event) => {
      if (event.event === "Started") {
        updaterState.total = event.data.contentLength ?? 0;
      } else if (event.event === "Progress") {
        updaterState.downloaded += event.data.chunkLength;
      }
    });
    updaterState.phase = "installing";
    await update.install();
    updaterState.phase = "installed";
    await relaunch();
  } catch (err) {
    updaterState.phase = "error";
    updaterState.error = errorText(err);
  }
}
