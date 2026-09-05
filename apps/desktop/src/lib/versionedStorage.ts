export const DESKTOP_CONFIG_KIND = "uvp.desktop.config" as const;

export type StorageFailureCategory = "get" | "set" | "quota" | "security";
export type StorageIssueType =
  | "recovery"
  | "migration"
  | "repair"
  | "future-version"
  | "storage-unavailable"
  | "save-failed";

export interface StorageIssue {
  id: number;
  key: string;
  type: StorageIssueType;
  summary: string;
  occurredAt: string;
  category?: StorageFailureCategory;
}

export interface DecodeResult<T> {
  data: T;
  repaired?: boolean;
  summary?: string;
}

export interface StorageSaveResult {
  ok: boolean;
  reason?: "future-version" | "storage";
  category?: StorageFailureCategory;
}

export interface StorageLoadResult<T> {
  data: T;
  locked: boolean;
  migrated: boolean;
  repaired: boolean;
  canonicalSaved: boolean;
}

interface ConfigEnvelope {
  kind: typeof DESKTOP_CONFIG_KIND;
  schemaVersion: number;
  writtenAt: string;
  data: unknown;
}

interface CorruptBackup {
  fingerprint: string;
  capturedAt: string;
  reason: string;
  snapshot: string;
  redacted: boolean;
}

export interface VersionedStorageOptions<T> {
  key: string;
  schemaVersion: number;
  defaultValue: () => T;
  decode: (value: unknown) => DecodeResult<T>;
  migrate?: (value: unknown, fromVersion: number | null) => DecodeResult<T>;
  /** 识别历史上不是 JSON 的精确裸值；不识别时返回 undefined。 */
  parseLegacyRaw?: (raw: string) => unknown | undefined;
  redactBackup?: (raw: string) => string;
  /** 修复可解码数据前也必须先完成备份，适用于可能含敏感字段的配置。 */
  backupBeforeRepair?: boolean;
}

const issueQueue: StorageIssue[] = [];
const issueListeners = new Set<(issue: StorageIssue) => void>();
let nextIssueId = 1;

function emitIssue(issue: Omit<StorageIssue, "id" | "occurredAt">): void {
  const complete: StorageIssue = {
    ...issue,
    id: nextIssueId++,
    occurredAt: new Date().toISOString(),
  };
  if (issueListeners.size === 0) {
    issueQueue.push(complete);
    if (issueQueue.length > 100) issueQueue.shift();
  }
  for (const listener of issueListeners) listener(complete);
}

/** 取走尚未展示的存储事件。事件只含非敏感摘要，不包含原始配置。 */
export function consumeStorageIssues(): StorageIssue[] {
  return issueQueue.splice(0, issueQueue.length);
}

/** 订阅后续存储事件；返回取消订阅函数。 */
export function subscribeStorageIssues(listener: (issue: StorageIssue) => void): () => void {
  issueListeners.add(listener);
  return () => issueListeners.delete(listener);
}

function classifyStorageError(error: unknown, operation: "get" | "set"): StorageFailureCategory {
  const candidate = error as { name?: unknown; code?: unknown } | null;
  const name = typeof candidate?.name === "string" ? candidate.name : "";
  if (name === "QuotaExceededError" || candidate?.code === 22 || candidate?.code === 1014) {
    return "quota";
  }
  if (name === "SecurityError") return "security";
  return operation;
}

function storageFailureSummary(category: StorageFailureCategory, operation: "get" | "set"): string {
  if (category === "quota") return "配置未保存：本地存储空间不足";
  if (category === "security") return "配置存储不可用：当前环境禁止访问本地存储";
  return operation === "get" ? "配置读取失败，已使用安全默认值" : "配置保存失败，当前更改未提交";
}

function safeGet(key: string): { ok: true; value: string | null } | { ok: false; category: StorageFailureCategory } {
  try {
    return { ok: true, value: localStorage.getItem(key) };
  } catch (error) {
    return { ok: false, category: classifyStorageError(error, "get") };
  }
}

function safeSet(key: string, value: string): StorageSaveResult {
  try {
    // 这是唯一提交点。localStorage 的单 key setItem 本身是原子的，不伪造 rename。
    localStorage.setItem(key, value);
    return { ok: true };
  } catch (error) {
    return { ok: false, reason: "storage", category: classifyStorageError(error, "set") };
  }
}

/** 尽力从 JSON 或损坏文本中移除敏感字段值，供损坏快照使用。 */
export function redactSensitiveText(raw: string, sensitiveKeys: readonly string[] = ["password"]): string {
  const sensitive = new Set(sensitiveKeys.map((key) => key.toLowerCase()));
  const visit = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(visit);
    if (value !== null && typeof value === "object") {
      const clean: Record<string, unknown> = {};
      for (const [key, child] of Object.entries(value)) {
        clean[key] = sensitive.has(key.toLowerCase()) ? "[REDACTED]" : visit(child);
      }
      return clean;
    }
    return value;
  };

  try {
    return JSON.stringify(visit(JSON.parse(raw)));
  } catch {
    // JSON 已损坏时无法可靠判定敏感值边界。只要出现敏感 key，就丢弃整份文本，
    // 避免截断引号、转义或混合大小写令密码漏进长期备份。
    const containsSensitiveKey = sensitiveKeys.some((key) => {
      const escaped = key.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      return new RegExp(`(?:"${escaped}"|\\b${escaped}\\b)\\s*[:=]`, "i").test(raw);
    });
    return containsSensitiveKey ? "[REDACTED] (corrupt sensitive data omitted)" : raw;
  }
}

function fingerprint(raw: string): string {
  // FNV-1a 32-bit：只用于稳定去重，不用于安全判定。
  let hash = 0x811c9dc5;
  for (let i = 0; i < raw.length; i += 1) {
    hash ^= raw.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16).padStart(8, "0");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isEnvelope(value: unknown): value is ConfigEnvelope {
  return isRecord(value)
    && value.kind === DESKTOP_CONFIG_KIND
    && Number.isInteger(value.schemaVersion)
    && typeof value.writtenAt === "string"
    && Object.prototype.hasOwnProperty.call(value, "data");
}

function serializeEnvelope<T>(schemaVersion: number, data: T): string {
  return JSON.stringify({
    kind: DESKTOP_CONFIG_KIND,
    schemaVersion,
    writtenAt: new Date().toISOString(),
    data,
  });
}

export function createVersionedStorage<T>(options: VersionedStorageOptions<T>) {
  const backupKey = `${options.key}.corrupt_backup`;
  let futureLocked = false;

  const recordWriteFailure = (result: StorageSaveResult): void => {
    if (result.ok) return;
    const category = result.category ?? "set";
    emitIssue({
      key: options.key,
      type: "save-failed",
      category,
      summary: storageFailureSummary(category, "set"),
    });
  };

  const commit = (data: T): StorageSaveResult => {
    let serialized: string;
    try {
      serialized = serializeEnvelope(options.schemaVersion, data);
    } catch {
      const result: StorageSaveResult = { ok: false, reason: "storage", category: "set" };
      recordWriteFailure(result);
      return result;
    }
    const result = safeSet(options.key, serialized);
    recordWriteFailure(result);
    return result;
  };

  const backupCorruptValue = (raw: string, reason: string): boolean => {
    const existingResult = safeGet(backupKey);
    if (!existingResult.ok) {
      emitIssue({
        key: options.key,
        type: "storage-unavailable",
        category: existingResult.category,
        summary: "损坏配置无法安全备份，因此未覆盖原配置",
      });
      return false;
    }

    let entries: CorruptBackup[] = [];
    if (existingResult.value) {
      try {
        const parsed = JSON.parse(existingResult.value) as unknown;
        if (Array.isArray(parsed)) {
          entries = parsed.filter((entry): entry is CorruptBackup =>
            isRecord(entry)
            && typeof entry.fingerprint === "string"
            && typeof entry.capturedAt === "string"
            && typeof entry.reason === "string"
            && typeof entry.snapshot === "string"
            && entry.redacted === true,
          );
        }
      } catch {
        entries = [];
      }
    }

    const redact = (value: string): string => {
      try {
        return (options.redactBackup ?? redactSensitiveText)(value);
      } catch {
        return redactSensitiveText(value);
      }
    };
    let existingChanged = false;
    entries = entries.map((entry) => {
      const snapshot = redact(entry.snapshot);
      if (snapshot === entry.snapshot) return entry;
      existingChanged = true;
      return { ...entry, snapshot, redacted: true };
    });

    const valueFingerprint = fingerprint(raw);
    if (entries.some((entry) => entry.fingerprint === valueFingerprint)) {
      if (!existingChanged) return true;
      const scrubResult = safeSet(backupKey, JSON.stringify(entries.slice(-3)));
      if (scrubResult.ok) return true;
      emitIssue({
        key: options.key,
        type: "storage-unavailable",
        category: scrubResult.category,
        summary: "损坏配置无法安全备份，因此未覆盖原配置",
      });
      return false;
    }

    const snapshot = redact(raw);
    entries.push({
      fingerprint: valueFingerprint,
      capturedAt: new Date().toISOString(),
      reason,
      snapshot,
      redacted: true,
    });
    const result = safeSet(backupKey, JSON.stringify(entries.slice(-3)));
    if (!result.ok) {
      emitIssue({
        key: options.key,
        type: "storage-unavailable",
        category: result.category,
        summary: "损坏配置无法安全备份，因此未覆盖原配置",
      });
      return false;
    }
    return true;
  };

  const recover = (raw: string, reason: string): StorageLoadResult<T> => {
    const fallback = options.defaultValue();
    const backedUp = backupCorruptValue(raw, reason);
    const saveResult = backedUp ? commit(fallback) : { ok: false };
    emitIssue({
      key: options.key,
      type: "recovery",
      summary: backedUp
        ? "配置异常，已保存脱敏备份并恢复安全默认值"
        : "配置异常，已在内存中恢复安全默认值；原配置因备份失败而保留",
    });
    return {
      data: fallback,
      locked: false,
      migrated: false,
      repaired: true,
      canonicalSaved: saveResult.ok,
    };
  };

  const load = (): StorageLoadResult<T> => {
    futureLocked = false;
    const readResult = safeGet(options.key);
    if (!readResult.ok) {
      emitIssue({
        key: options.key,
        type: "storage-unavailable",
        category: readResult.category,
        summary: storageFailureSummary(readResult.category, "get"),
      });
      return {
        data: options.defaultValue(),
        locked: false,
        migrated: false,
        repaired: false,
        canonicalSaved: false,
      };
    }
    if (readResult.value === null) {
      return {
        data: options.defaultValue(),
        locked: false,
        migrated: false,
        repaired: false,
        canonicalSaved: false,
      };
    }

    const raw = readResult.value;
    let parsed: unknown;
    try {
      parsed = JSON.parse(raw);
    } catch {
      try {
        parsed = options.parseLegacyRaw?.(raw);
      } catch {
        parsed = undefined;
      }
      if (parsed === undefined) return recover(raw, "JSON 解析失败");
    }

    if (isEnvelope(parsed)) {
      if (parsed.schemaVersion > options.schemaVersion) {
        futureLocked = true;
        let data = options.defaultValue();
        try {
          data = options.decode(parsed.data).data;
        } catch {
          // 未来版本的数据不做恢复或回写，只以内存默认值继续运行。
        }
        emitIssue({
          key: options.key,
          type: "future-version",
          summary: "检测到更高版本配置，已锁定保存以避免覆盖",
        });
        return {
          data,
          locked: true,
          migrated: false,
          repaired: false,
          canonicalSaved: false,
        };
      }

      if (parsed.schemaVersion < options.schemaVersion) {
        if (!options.migrate) return recover(raw, "缺少版本迁移器");
        try {
          const migrated = options.migrate(parsed.data, parsed.schemaVersion);
          const result = commit(migrated.data);
          emitIssue({ key: options.key, type: "migration", summary: migrated.summary ?? "配置已迁移到当前版本" });
          return {
            data: migrated.data,
            locked: false,
            migrated: true,
            repaired: !!migrated.repaired,
            canonicalSaved: result.ok,
          };
        } catch {
          return recover(raw, "版本迁移失败");
        }
      }

      try {
        const decoded = options.decode(parsed.data);
        let canonicalSaved = false;
        if (decoded.repaired) {
          const backedUp = !options.backupBeforeRepair
            || backupCorruptValue(raw, "配置字段修复前备份");
          canonicalSaved = backedUp && commit(decoded.data).ok;
          const defaultSummary = decoded.summary ?? "配置字段已自动修复";
          emitIssue({
            key: options.key,
            type: "repair",
            summary: options.backupBeforeRepair
              ? backedUp
                ? `${defaultSummary}；原配置已保存脱敏备份`
                : `${defaultSummary}；备份失败，原配置未被覆盖`
              : defaultSummary,
          });
        }
        return {
          data: decoded.data,
          locked: false,
          migrated: false,
          repaired: !!decoded.repaired,
          canonicalSaved,
        };
      } catch {
        return recover(raw, "配置结构校验失败");
      }
    }

    if (!options.migrate) return recover(raw, "配置结构校验失败");
    try {
      const migrated = options.migrate(parsed, null);
      const result = commit(migrated.data);
      emitIssue({ key: options.key, type: "migration", summary: migrated.summary ?? "旧版配置已迁移" });
      return {
        data: migrated.data,
        locked: false,
        migrated: true,
        repaired: !!migrated.repaired,
        canonicalSaved: result.ok,
      };
    } catch {
      return recover(raw, "旧版配置迁移失败");
    }
  };

  const save = (data: T): StorageSaveResult => {
    if (futureLocked) {
      emitIssue({
        key: options.key,
        type: "future-version",
        summary: "配置来自更高版本，本次更改未保存",
      });
      return { ok: false, reason: "future-version" };
    }
    return commit(data);
  };

  return {
    key: options.key,
    backupKey,
    load,
    save,
    isFutureLocked: () => futureLocked,
  };
}
