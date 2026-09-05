// Tauri IPC 错误的统一兼容与呈现层。

/** Rust `IpcError` 的前端投影。未知/旧错误会归一为同一形状。 */
export interface IpcErrorDto {
  code: string;
  message: string;
  details?: string;
  retryable?: boolean;
  action?: string;
}

/** 一条已归类的错误呈现。 */
export interface PresentedError {
  title: string;
  hint: string;
  /** message 与 details 去重后的完整排障文本。 */
  detail: string;
  error: IpcErrorDto;
}

interface ErrorRule {
  title: string;
  hint: string;
}

/** 稳定 code 优先于旧文案规则。未知 code 不猜建议。 */
const CODE_RULES: Record<string, ErrorRule> = {
  "config.invalid": {
    title: "配置无效",
    hint: "请按错误详情修正配置后重试。",
  },
  "credential.missing": {
    title: "该平台档案还没有密码",
    hint: "在顶栏平台设置里填写 SIP 密码并保存（密码存入系统钥匙串，不落配置文件）。",
  },
  "credential.invalid": {
    title: "凭据配置无效",
    hint: "请检查平台档案与 SIP 密码后重试。",
  },
  "credential.store": {
    title: "凭据存储不可用",
    hint: "请重启应用后重试；若仍失败，请检查系统钥匙串是否可用。",
  },
  "device.not_running": {
    title: "设备未启动",
    hint: "先到「单设备联调」页点「注册」启动设备，再执行此操作。",
  },
  "device.already_running": {
    title: "设备已在运行",
    hint: "请先停止当前设备实例后重试。",
  },
  "stress.not_running": {
    title: "没有正在运行的压测",
    hint: "请先启动压测后再执行此操作。",
  },
  "stress.already_running": {
    title: "已有压测正在运行",
    hint: "等待当前压测完全退出后重试。",
  },
  "report.no_data": {
    title: "暂无压测数据可导出",
    hint: "请先完成一次压测。",
  },
  "report.export_failed": {
    title: "报告导出失败",
    hint: "请检查磁盘空间与应用数据目录写入权限。",
  },
  "diagnostics.export_failed": {
    title: "诊断包导出失败",
    hint: "请检查磁盘空间与应用数据目录写入权限。",
  },
  "sip.timeout": {
    title: "与平台通信超时",
    hint: "检查平台 IP/端口是否正确、本机到平台的网络是否可达，以及防火墙是否放行 SIP 端口。",
  },
  "sip.rejected.auth": {
    title: "平台拒绝了认证",
    hint: "核对 SIP 用户名/密码、平台域 ID，以及该设备编号是否已在平台注册。",
  },
  "sip.rejected.busy": {
    title: "平台繁忙或过载",
    hint: "降低设备并发或放慢爬坡速率后重试。",
  },
  "sip.rejected": {
    title: "平台拒绝了请求",
    hint: "请根据响应状态和系统日志核对平台策略。",
  },
  "sip.protocol": {
    title: "SIP 协议错误",
    hint: "请核对平台协议配置并查看信令追踪。",
  },
  "gb28181.protocol": {
    title: "GB28181 协议错误",
    hint: "请核对设备编号、目录数据与平台协议版本。",
  },
  "media.error": {
    title: "媒体处理失败",
    hint: "请检查媒体源、采集权限和 FFmpeg 可用性。",
  },
  "media.access": {
    title: "媒体文件不可访问",
    hint: "请重新选择媒体文件，并确认文件仍存在且有读取权限。",
  },
  "media.preview_denied": {
    title: "预览未授权",
    hint: "点「选择文件」重新选取该视频；预览授权只能来自系统文件对话框。",
  },
  "media.dialog_failed": {
    title: "文件选择失败",
    hint: "请重新打开文件选择器后重试。",
  },
  "io.address_in_use": {
    title: "本地端口被占用",
    hint: "可能已有另一个实例在运行，或端口被其它程序占用。关闭重复实例后重试。",
  },
  "io.permission_denied": {
    title: "本地权限不足",
    hint: "检查系统网络、文件、摄像头或屏幕录制权限后重试。",
  },
  "io.not_found": {
    title: "文件或依赖不存在",
    hint: "确认文件路径与运行时依赖仍然存在。",
  },
  "io.storage_full": {
    title: "磁盘空间不足",
    hint: "清理磁盘后重试。日志与报告位于应用数据目录。",
  },
};

/** 旧字符串错误的一个版本兼容规则。 */
const RULES: Array<{ match: RegExp; title: string; hint: string }> = [
  {
    match: /Address already in use|已在使用|EADDRINUSE|os error 48|os error 98/i,
    title: "本地端口被占用",
    hint: "可能已有另一个实例在运行，或该端口被其它程序占用。关闭重复实例后重试。",
  },
  {
    match: /Permission denied|权限|os error 13|os error 1/i,
    title: "本地权限不足",
    hint: "绑定端口或访问文件被系统拒绝。请检查系统网络、文件、摄像头或屏幕录制权限。",
  },
  {
    match: /尚未配置密码|未配置 SIP 密码/,
    title: "该平台档案还没有密码",
    hint: "在顶栏平台设置里填写 SIP 密码并保存（密码存入系统钥匙串，不落配置文件）。",
  },
  {
    match: /设备未启动|没有正在运行的设备/,
    title: "设备未启动",
    hint: "先到「单设备联调」页点「注册」启动设备，再执行此操作。",
  },
  {
    match: /已有压测正在运行|没有正在运行的压测/,
    title: "压测状态不匹配",
    hint: "等待上一次压测完全退出后重试；停止过程可能仍在收敛设备任务。",
  },
  {
    match: /必须是有效的 IP|不支持域名/,
    title: "平台地址格式不对",
    hint: "当前只支持 IP:端口形式。若平台使用域名，请先解析成 IP 填入。",
  },
  {
    match: /未找到 ffmpeg|需要 ffmpeg/i,
    title: "缺少 FFmpeg",
    hint: "容器转封装与实时采集依赖 FFmpeg；请安装 FFmpeg，或改用 H.264/H.265 裸流文件。",
  },
  {
    match: /尚未经你选择授权|放行媒体预览失败/,
    title: "预览未授权",
    hint: "点「选择文件」重新选取该视频；预览授权只能来自系统文件对话框。",
  },
  {
    match: /连接超时|事务超时|timed? ?out/i,
    title: "与平台通信超时",
    hint: "检查平台 IP/端口是否正确、本机到平台的网络是否可达，以及防火墙是否放行 SIP 端口。",
  },
  {
    match: /平台拒绝|401|403/,
    title: "平台拒绝了请求",
    hint: "核对 SIP 用户名/密码、平台域 ID，以及该设备编号是否已在平台注册。",
  },
  {
    match: /磁盘|No space left|os error 28/i,
    title: "磁盘空间不足",
    hint: "清理磁盘后重试。日志与报告位于应用数据目录。",
  },
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function fromObject(value: Record<string, unknown>): IpcErrorDto | null {
  const message = optionalString(value.message);
  if (!message) return null;
  const code = optionalString(value.code) ?? "legacy.error";
  const details = optionalString(value.details);
  const action = optionalString(value.action);
  return {
    code,
    message,
    ...(details && details !== message ? { details } : {}),
    ...(typeof value.retryable === "boolean" ? { retryable: value.retryable } : {}),
    ...(action ? { action } : {}),
  };
}

function safeStringify(value: object): string {
  const seen = new WeakSet<object>();
  try {
    const encoded = JSON.stringify(value, (_key, child: unknown) => {
      if (child && typeof child === "object") {
        if (seen.has(child)) return "[Circular]";
        seen.add(child);
      }
      return child;
    });
    return encoded ?? Object.prototype.toString.call(value);
  } catch {
    try {
      return String(value);
    } catch {
      return "[无法读取的错误对象]";
    }
  }
}

/**
 * 把 Tauri 新错误对象及一个版本内的旧错误形态归一为稳定 DTO。
 * 可选字段类型不合法时忽略，不让坏 payload 覆盖可用的 message。
 */
export function normalizeIpcError(error: unknown): IpcErrorDto {
  if (typeof error === "string") {
    const message = error.trim();
    if (message.startsWith("{") && message.endsWith("}")) {
      try {
        const parsed: unknown = JSON.parse(message);
        if (isRecord(parsed)) {
          const dto = fromObject(parsed);
          if (dto) return dto;
        }
      } catch {
        // 不是合法 JSON DTO，按旧字符串原样处理。
      }
    }
    return { code: "legacy.error", message };
  }
  if (error instanceof Error) {
    return { code: "legacy.error", message: error.message.trim() || error.name };
  }
  if (isRecord(error)) {
    return fromObject(error) ?? { code: "legacy.error", message: safeStringify(error) };
  }
  return { code: "legacy.error", message: String(error) };
}

function combinedDetail(error: IpcErrorDto): string {
  if (!error.details || error.details === error.message) return error.message;
  return `${error.message}；${error.details}`;
}

/** 把任意抛出物归类成可呈现的错误。 */
export function presentError(error: unknown, fallbackTitle = "操作失败"): PresentedError {
  const normalized = normalizeIpcError(error);
  const detail = combinedDetail(normalized);
  const codeRule = CODE_RULES[normalized.code];
  const legacyRule = normalized.code === "legacy.error"
    ? RULES.find((rule) => rule.match.test(detail))
    : undefined;
  return {
    title: codeRule?.title ?? legacyRule?.title ?? fallbackTitle,
    // 后端 action 最具体；其次稳定 code；最后才匹配旧文本。
    hint: normalized.action ?? codeRule?.hint ?? legacyRule?.hint ?? "",
    detail,
    error: normalized,
  };
}

function startsWithTitle(detail: string, title: string): boolean {
  if (!detail.startsWith(title)) return false;
  const next = detail.slice(title.length, title.length + 1);
  return !next || /[：:，,。；;\s]/.test(next);
}

/** 拼成一条适合放进 message/toast 的单行文案。 */
export function formatError(error: unknown, fallbackTitle = "操作失败"): string {
  const { title, hint, detail } = presentError(error, fallbackTitle);
  const parts: string[] = [];
  // 旧错误常已带“保存失败：”前缀；完整原文只出现一次，避免双前缀。
  if (detail && startsWithTitle(detail, title)) {
    parts.push(detail);
    if (hint) parts.push(hint);
  } else {
    parts.push(title);
    if (hint) parts.push(hint);
    if (detail && detail !== title) parts.push(`(${detail})`);
  }
  return parts.join(" ").replace(/\s+/g, " ").trim();
}
