/**
 * 在线升级的纯逻辑（不碰 Tauri API，便于单测）。
 *
 * 这里只负责「把升级过程翻译成用户看得懂的一句话」：
 * 版本提示、下载进度、体积格式。真正的检查/下载/安装/重启在 updater.ts。
 */

/** 清单地址与客户端同源校验的版本号格式：只接受 x.y.z，避免 "v1.2.3" 这类前缀进 UI 后重复。 */
export function normalizeVersion(version) {
  const text = String(version ?? "").trim();
  return text.replace(/^v/i, "");
}

/** 下载量 → 易读体积。未知总量时返回空串，UI 只显示已下载量。 */
export function formatBytes(bytes) {
  const value = Number(bytes);
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let size = value;
  let index = 0;
  while (size >= 1024 && index < units.length - 1) {
    size /= 1024;
    index += 1;
  }
  return `${size.toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

/**
 * 下载百分比。总量未知（服务端没给 contentLength）时返回 null，
 * 让界面退化成「已下载 12.3 MB」的不确定进度条，而不是卡在 0%。
 */
export function progressPercent(downloaded, total) {
  const done = Number(downloaded);
  const all = Number(total);
  if (!Number.isFinite(done) || !Number.isFinite(all) || all <= 0) return null;
  return Math.min(100, Math.max(0, Math.round((done / all) * 100)));
}

/** 进度条用于 aria-valuenow 的整数；未完成时不要伪装成 100。 */
export function clampedPercent(downloaded, total) {
  return progressPercent(downloaded, total) ?? 0;
}

/** 更新说明通常是一整段 markdown，通知里只取第一段有内容的行。 */
export function summarizeNotes(notes, max = 120) {
  const text = String(notes ?? "")
    .split(/\r?\n/)
    .map((line) => line.replace(/^[-*#>\s]+/, "").trim())
    .find((line) => line.length > 0);
  if (!text) return "";
  return text.length > max ? `${text.slice(0, max - 1)}…` : text;
}

/**
 * 启动时静默检查命中新版本 → 通知中心那一条。
 * 不带「立即升级」按钮：真正的安装入口固定在「关于」页，避免误点导致运行中的压测中断。
 */
export function updateNotice(version, notes) {
  const normalized = normalizeVersion(version);
  if (!normalized) return null;
  const summary = summarizeNotes(notes);
  return {
    level: "info",
    title: `发现新版本 v${normalized}`,
    detail: summary ? `${summary}（可在「关于」页升级）` : "可在「关于」页查看并升级",
  };
}

/** 同一版本只提示一次：否则每次启动都往通知中心塞一条重复消息。 */
export function shouldNotifyVersion(lastNotified, version) {
  const next = normalizeVersion(version);
  return next.length > 0 && normalizeVersion(lastNotified) !== next;
}
