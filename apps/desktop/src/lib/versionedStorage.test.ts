import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  DESKTOP_CONFIG_KIND,
  consumeStorageIssues,
  createVersionedStorage,
  redactSensitiveText,
} from "./versionedStorage";

class MemoryStorage {
  map = new Map<string, string>();
  setCalls: string[] = [];
  failGet: unknown = null;
  failSetFor = new Map<string, unknown>();

  getItem(key: string) {
    if (this.failGet) throw this.failGet;
    return this.map.get(key) ?? null;
  }

  setItem(key: string, value: string) {
    const failure = this.failSetFor.get(key);
    if (failure) throw failure;
    this.setCalls.push(key);
    this.map.set(key, value);
  }

  removeItem(key: string) {
    this.map.delete(key);
  }

  clear() {
    this.map.clear();
  }
}

interface ExampleData { value: string }

function envelope(version: number, data: unknown): string {
  return JSON.stringify({
    kind: DESKTOP_CONFIG_KIND,
    schemaVersion: version,
    writtenAt: "2026-01-01T00:00:00.000Z",
    data,
  });
}

function exampleStorage() {
  return createVersionedStorage<ExampleData>({
    key: "example",
    schemaVersion: 1,
    defaultValue: () => ({ value: "safe" }),
    decode(value) {
      if (!value || typeof value !== "object" || typeof (value as { value?: unknown }).value !== "string") {
        throw new Error("bad shape");
      }
      return { data: { value: (value as { value: string }).value } };
    },
    migrate(value, fromVersion) {
      if (fromVersion !== null || typeof value !== "string") throw new Error("bad legacy");
      return { data: { value }, summary: "已迁移测试配置" };
    },
  });
}

let storage: MemoryStorage;

beforeEach(() => {
  storage = new MemoryStorage();
  vi.stubGlobal("localStorage", storage);
  consumeStorageIssues();
});

describe("versionedStorage", () => {
  it("迁移旧数据并以单 key setItem 提交 envelope", () => {
    storage.map.set("example", JSON.stringify("legacy"));
    const store = exampleStorage();

    const loaded = store.load();

    expect(loaded.data).toEqual({ value: "legacy" });
    expect(loaded.migrated).toBe(true);
    expect(loaded.canonicalSaved).toBe(true);
    expect(storage.setCalls).toEqual(["example"]);
    expect(JSON.parse(storage.map.get("example")!)).toMatchObject({
      kind: DESKTOP_CONFIG_KIND,
      schemaVersion: 1,
      data: { value: "legacy" },
    });
    expect(consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "migration", summary: "已迁移测试配置" }),
    ]);
  });

  it("损坏数据备份按 fingerprint 去重且只保留最近三条", () => {
    const store = exampleStorage();
    for (const raw of ["{bad-1", "{bad-1", "{bad-2", "{bad-3", "{bad-4"]) {
      storage.map.set("example", raw);
      store.load();
    }

    const backups = JSON.parse(storage.map.get("example.corrupt_backup")!) as Array<{
      fingerprint: string;
      snapshot: string;
      redacted: boolean;
    }>;
    expect(backups).toHaveLength(3);
    expect(new Set(backups.map((entry) => entry.fingerprint))).toHaveLength(3);
    expect(backups.every((entry) => entry.redacted)).toBe(true);
    expect(backups.map((entry) => entry.snapshot)).toEqual(["{bad-2", "{bad-3", "{bad-4"]);
  });

  it("备份写入失败时使用内存默认值但绝不覆盖 primary", () => {
    storage.map.set("example", "{broken-primary");
    storage.failSetFor.set("example.corrupt_backup", new Error("disk unavailable"));
    const loaded = exampleStorage().load();

    expect(loaded.data).toEqual({ value: "safe" });
    expect(loaded.canonicalSaved).toBe(false);
    expect(storage.map.get("example")).toBe("{broken-primary");
    expect(consumeStorageIssues()).toEqual(expect.arrayContaining([
      expect.objectContaining({ type: "storage-unavailable" }),
      expect.objectContaining({ type: "recovery", summary: expect.stringContaining("原配置") }),
    ]));
  });

  it("未来版本可读取已知字段但锁定回写", () => {
    const raw = envelope(99, { value: "future" });
    storage.map.set("example", raw);
    const store = exampleStorage();

    const loaded = store.load();
    const saved = store.save({ value: "changed" });

    expect(loaded).toMatchObject({ data: { value: "future" }, locked: true });
    expect(saved).toEqual({ ok: false, reason: "future-version" });
    expect(storage.map.get("example")).toBe(raw);
    expect(storage.setCalls).toEqual([]);
  });

  it("将 quota 与 security 异常分类并暴露非敏感事件", () => {
    const store = exampleStorage();
    storage.failSetFor.set("example", { name: "QuotaExceededError" });
    expect(store.save({ value: "x" })).toMatchObject({ ok: false, category: "quota" });
    expect(consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "save-failed", category: "quota" }),
    ]);

    storage.failGet = { name: "SecurityError" };
    expect(store.load().data).toEqual({ value: "safe" });
    expect(consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "storage-unavailable", category: "security" }),
    ]);
  });

  it("损坏及截断文本中的 password 值在备份前 fail-closed 脱敏", () => {
    const sampleValue = "test-placeholder-token";
    for (const raw of [
      `{"nested":{"password":"${sampleValue}"}, broken`,
      `[{"password":"${sampleValue}`,
      `{PaSsWoRd=${sampleValue}`,
    ]) {
      const redacted = redactSensitiveText(raw);
      expect(redacted).not.toContain(sampleValue);
      expect(redacted).toContain("[REDACTED]");
    }
  });
});
