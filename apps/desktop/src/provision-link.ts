// 粘贴接入链接 —— 解析与字段映射的纯逻辑。
//
// 链接由平台 QRProvisionCard 生成:`https://<平台>/gb28181/qr#t=<token>`。
// token 刻意放在 fragment 而不是 query:query 会随 GET 进平台访问日志,
// 通用扫码 App 一扫就当场消费掉一次性 token 并在浏览器里明文显示密码。
// 所以这里必须读 hash —— `searchParams` 永远拿不到它。

export type ProvisionParseFailure =
  | "empty"
  | "not_url"
  | "bad_scheme"
  | "no_token"
  | "bad_token";

export interface ProvisionLink {
  /** 平台 origin。粘贴的是裸接入码时为空,由调用方决定回落到哪个平台。 */
  baseUrl: string;
  token: string;
}

export type ProvisionParseResult =
  | { ok: true; value: ProvisionLink }
  | { ok: false; reason: ProvisionParseFailure };

/** 与平台 isWellFormedQRToken 一致:128 bit 的 base64url 无填充编码。 */
const TOKEN_PATTERN = /^[A-Za-z0-9_-]{22}$/;

/** 从微信/QQ 复制常带回车、零宽字符和成对引号,统一剥掉。 */
const WRAPPING = /^[\s"'`<>“”‘’]+|[\s"'`<>“”‘’]+$/g;

/**
 * 解析粘贴进来的接入链接。
 *
 * 宁可在这里判死,也不要带着一个形似 token 的字符串去请求平台:
 * 平台的兑换端点免鉴权且有速率限制,无差别输入会挤占正常兑换的配额。
 */
export function parseProvisionLink(raw: string): ProvisionParseResult {
  const text = String(raw ?? "").replace(WRAPPING, "");
  if (!text) return { ok: false, reason: "empty" };

  // 管理员可能只把接入码发过来。裸 token 不含平台地址,
  // 这里不替调用方猜地址 —— baseUrl 留空,由上层决定。
  if (TOKEN_PATTERN.test(text)) {
    return { ok: true, value: { baseUrl: "", token: text } };
  }

  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false, reason: "not_url" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { ok: false, reason: "bad_scheme" };
  }

  const hash = url.hash.startsWith("#") ? url.hash.slice(1) : url.hash;
  const token = (new URLSearchParams(hash).get("t") ?? "").trim();
  if (!token) return { ok: false, reason: "no_token" };
  if (!TOKEN_PATTERN.test(token)) return { ok: false, reason: "bad_token" };

  return { ok: true, value: { baseUrl: url.origin, token } };
}

/** 平台兑换端点返回的 SIP 六元组(字段名与 Go 侧 json tag 一致)。 */
export interface ProvisionPayload {
  serverId: string;
  domain: string;
  ip: string;
  port: number;
  transport: string;
  password: string;
}

/** 回填到 `PlatformProfile` 的那部分字段。 */
export interface ProvisionProfilePatch {
  server_host: string;
  server_port: number;
  server_id: string;
  server_domain: string;
  password: string;
  transport: "UDP" | "TCP";
}

export function toProfilePatch(payload: ProvisionPayload): ProvisionProfilePatch {
  const port = Number(payload.port);
  return {
    server_host: (payload.ip ?? "").trim(),
    // 端口异常时回落 5060 而不是留 0:后者会让 platformValid 直接拦住保存,
    // 用户却看不出是哪一项不对。
    server_port: Number.isInteger(port) && port > 0 && port <= 65535 ? port : 5060,
    server_id: (payload.serverId ?? "").trim(),
    server_domain: (payload.domain ?? "").trim(),
    password: payload.password ?? "",
    // 平台下发的是小写 udp/tcp,桌面端的联合类型是大写。
    transport: /tcp/i.test(payload.transport ?? "") ? "TCP" : "UDP",
  };
}

/**
 * 回填后仍不满足保存条件的项。
 *
 * 平台侧六元组理论上都是合法值,但把判断显式暴露出来,
 * 比让用户点保存后被 `platformValid` 静默拦住要好。
 */
export function describeProvisionGaps(patch: ProvisionProfilePatch): string[] {
  const gaps: string[] = [];
  if (!patch.server_host) gaps.push("服务器地址为空");
  if (!/^\d{20}$/.test(patch.server_id)) gaps.push("服务器 ID 不是 20 位数字");
  if (patch.server_domain && !/^\d{10}$/.test(patch.server_domain)) {
    gaps.push("服务器域不是 10 位数字");
  }
  return gaps;
}

export interface ProvisionFailure {
  code: string;
  message: string;
}

/**
 * 归一化 Tauri 的 reject 值。
 *
 * 命令返回的是结构化 `{code,message}`,但 invoke 也可能抛出字符串
 * (例如 IPC 序列化失败),两种情况都要能渲染。
 */
export function readProvisionFailure(cause: unknown): ProvisionFailure {
  if (cause && typeof cause === "object") {
    const record = cause as Record<string, unknown>;
    if (typeof record.code === "string" && typeof record.message === "string") {
      return { code: record.code, message: record.message };
    }
  }
  const text = String(cause ?? "").trim();
  return { code: "server", message: text || "接入失败" };
}

/** 解析失败的中文提示。 */
export function describeParseFailure(reason: ProvisionParseFailure): string {
  const text: Record<ProvisionParseFailure, string> = {
    empty: "请先粘贴接入链接",
    not_url: "这不是一个有效的链接,请粘贴平台「扫码接入」里的完整链接",
    bad_scheme: "只支持 http:// 或 https:// 开头的接入链接",
    no_token: "链接里没有接入码,请确认复制的是完整链接(含 #t= 部分)",
    bad_token: "链接中的接入码不完整,请重新复制",
  };
  return text[reason];
}
