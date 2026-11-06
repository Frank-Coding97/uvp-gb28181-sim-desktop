// OSD 水印配置的纯函数：默认值、规范化、校验、预览排版。
// 规则与 Rust 侧 media_rtp::osd 保持一致(字号比例、32 字上限、#RRGGBB)。
import type { OsdLayer, OsdSettings, OsdSize } from "./platform";

export const OSD_TEXT_MAX_CHARS = 32;

export const OSD_SIZE_OPTIONS: { value: OsdSize; label: string; ratio: number }[] = [
  { value: "small", label: "小", ratio: 0.035 },
  { value: "medium", label: "中", ratio: 0.05 },
  { value: "large", label: "大", ratio: 0.07 },
];

const HEX = /^#[0-9a-fA-F]{6}$/;

function layer(enabled: boolean): OsdLayer {
  return { enabled, text: "", size: "medium", fill_color: "#FFFFFF", outline_color: "#000000" };
}

export function defaultOsdSettings(): OsdSettings {
  return { timestamp: layer(true), channel_name: layer(true), watermark: layer(false) };
}

function normalizeLayer(value: Partial<OsdLayer> | undefined, fallback: OsdLayer): OsdLayer {
  const size = OSD_SIZE_OPTIONS.some((option) => option.value === value?.size) ? value!.size! : fallback.size;
  return {
    enabled: typeof value?.enabled === "boolean" ? value.enabled : fallback.enabled,
    text: typeof value?.text === "string" ? value.text : fallback.text,
    size,
    fill_color: typeof value?.fill_color === "string" && HEX.test(value.fill_color) ? value.fill_color.toUpperCase() : fallback.fill_color,
    outline_color: typeof value?.outline_color === "string" && HEX.test(value.outline_color) ? value.outline_color.toUpperCase() : fallback.outline_color,
  };
}

/** 旧后端没有 osd 字段、或字段残缺时补齐默认值。 */
export function normalizeOsdSettings(value: Partial<OsdSettings> | undefined | null): OsdSettings {
  const base = defaultOsdSettings();
  return {
    timestamp: normalizeLayer(value?.timestamp, base.timestamp),
    channel_name: normalizeLayer(value?.channel_name, base.channel_name),
    watermark: normalizeLayer(value?.watermark, base.watermark),
  };
}

/** 返回第一条错误；合法时返回 null。 */
export function validateOsdSettings(value: OsdSettings): string | null {
  const layers: [string, OsdLayer][] = [["时间", value.timestamp], ["通道名", value.channel_name], ["水印", value.watermark]];
  for (const [label, item] of layers) {
    if (!HEX.test(item.fill_color)) return `${label}填充色必须是 #RRGGBB`;
    if (!HEX.test(item.outline_color)) return `${label}描边色必须是 #RRGGBB`;
    if ([...item.text].length > OSD_TEXT_MAX_CHARS) return `${label}文字不能超过 ${OSD_TEXT_MAX_CHARS} 个字符`;
  }
  if (value.watermark.enabled && !value.watermark.text.trim()) return "开启水印时必须填写水印文字";
  return null;
}

export function osdSettingsEqual(a: OsdSettings, b: OsdSettings): boolean {
  return JSON.stringify(normalizeOsdSettings(a)) === JSON.stringify(normalizeOsdSettings(b));
}

/** 字号像素 = max(16, floor(画面高 × 比例))。 */
export function osdPixelSize(size: OsdSize, frameHeight: number): number {
  const ratio = OSD_SIZE_OPTIONS.find((option) => option.value === size)?.ratio ?? 0.05;
  return Math.max(16, Math.floor(frameHeight * ratio));
}

/** 预览用时间文字，与码流里 drawtext 的格式一致。 */
export function formatOsdTime(date: Date, chinese = false): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  const y = date.getFullYear();
  const mo = pad(date.getMonth() + 1);
  const d = pad(date.getDate());
  const t = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  return chinese ? `${y}年${mo}月${d}日${t}` : `${y}-${mo}-${d} ${t}`;
}
