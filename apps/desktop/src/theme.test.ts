import { beforeEach, describe, expect, it, vi } from "vitest";

class MemoryStorage {
  map = new Map<string, string>();
  failSetFor = new Map<string, unknown>();

  getItem(key: string) {
    return this.map.get(key) ?? null;
  }
  setItem(key: string, value: string) {
    const failure = this.failSetFor.get(key);
    if (failure) throw failure;
    this.map.set(key, value);
  }
  removeItem(key: string) {
    this.map.delete(key);
  }
  clear() {
    this.map.clear();
  }
}

const STORE_KEY = "uvp_theme";

function envelope(data: unknown, version = 1) {
  return JSON.stringify({
    kind: "uvp.desktop.config",
    schemaVersion: version,
    writtenAt: "2026-01-01T00:00:00.000Z",
    data,
  });
}

async function freshModule(seed?: string, prepare?: (storage: MemoryStorage) => void) {
  const storage = new MemoryStorage();
  if (seed !== undefined) storage.map.set(STORE_KEY, seed);
  prepare?.(storage);
  vi.stubGlobal("localStorage", storage);
  vi.resetModules();
  const theme = await import("./theme");
  const issues = await import("./lib/versionedStorage");
  return { theme, issues, storage };
}

beforeEach(() => {
  vi.unstubAllGlobals();
});

describe("主题持久化", () => {
  it.each(["light", "dark", "system"] as const)("迁移旧字符串 %s 到 V1 envelope", async (legacy) => {
    const { theme, issues, storage } = await freshModule(legacy);
    expect(theme.useTheme().preference.value).toBe(legacy);
    expect(JSON.parse(storage.getItem(STORE_KEY)!)).toMatchObject({
      kind: "uvp.desktop.config",
      schemaVersion: 1,
      data: { preference: legacy },
    });
    expect(issues.consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "migration", summary: "旧版主题偏好已迁移" }),
    ]);
  });

  it("保存失败时不先改变内存 preference，并产生可观测事件", async () => {
    const { theme, issues, storage } = await freshModule(envelope({ preference: "light" }));
    const state = theme.useTheme();
    storage.failSetFor.set(STORE_KEY, { name: "QuotaExceededError" });

    expect(state.setPreference("dark")).toMatchObject({ ok: false, category: "quota" });
    expect(state.preference.value).toBe("light");
    expect(issues.consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "save-failed", category: "quota" }),
    ]);
  });

  it("损坏配置恢复 system 并产生持久恢复事件", async () => {
    const { theme, issues, storage } = await freshModule("{broken");
    expect(theme.useTheme().preference.value).toBe("system");
    expect(storage.getItem(`${STORE_KEY}.corrupt_backup`)).not.toBeNull();
    expect(issues.consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "recovery", summary: expect.stringContaining("脱敏备份") }),
    ]);
  });

  it("未来版本锁定且 toggle 不改变当前 preference", async () => {
    const raw = envelope({ preference: "dark" }, 7);
    const { theme, issues, storage } = await freshModule(raw);
    const state = theme.useTheme();
    expect(state.preference.value).toBe("dark");
    expect(state.toggle()).toEqual({ ok: false, reason: "future-version" });
    expect(state.preference.value).toBe("dark");
    expect(storage.getItem(STORE_KEY)).toBe(raw);
    expect(issues.consumeStorageIssues()).toEqual([
      expect.objectContaining({ type: "future-version" }),
      expect.objectContaining({ type: "future-version" }),
    ]);
  });
});
