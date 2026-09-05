// 平台档案持久化的安全与迁移契约。
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

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "platform_secret_status") return {};
  if (cmd === "save_platform_secret") return { backend: "keyring", message: "已保存" };
  return undefined;
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

const STORE_KEY = "uvp_platform_profiles";
const ACTIVE_KEY = "uvp_platform_active";
const BACKUP_KEY = `${STORE_KEY}.corrupt_backup`;

function validProfile(overrides: Record<string, unknown> = {}) {
  return {
    id: "p-1",
    name: "平台一",
    server_host: "10.0.0.1",
    server_port: 5060,
    server_domain: "34020000002000000001",
    inbound_identity_mode: "compatible",
    transport: "UDP",
    signaling_encoding: "GB18030",
    ...overrides,
  };
}

function envelope(data: unknown, version = 1) {
  return JSON.stringify({
    kind: "uvp.desktop.config",
    schemaVersion: version,
    writtenAt: "2026-01-01T00:00:00.000Z",
    data,
  });
}

async function freshModule(seed?: string, active?: string, prepare?: (storage: MemoryStorage) => void) {
  const storage = new MemoryStorage();
  if (seed !== undefined) storage.map.set(STORE_KEY, seed);
  if (active !== undefined) storage.map.set(ACTIVE_KEY, active);
  prepare?.(storage);
  vi.stubGlobal("localStorage", storage);
  vi.resetModules();
  const mod = await import("./platform");
  return { mod, storage };
}

beforeEach(() => {
  invokeMock.mockClear();
});

describe("平台档案持久化", () => {
  it("空存储时给出无私密信息的本地示例", async () => {
    const { mod } = await freshModule();
    const { profiles } = mod.usePlatform();
    expect(profiles.value).toHaveLength(1);
    expect(profiles.value[0]).toMatchObject({
      server_host: "127.0.0.1",
      inbound_identity_mode: "compatible",
    });
    expect(profiles.value[0]).not.toHaveProperty("password");
  });

  it("迁移旧裸数组与 active key，写成合并后的 V1 envelope", async () => {
    const legacy = JSON.stringify([
      validProfile({ id: "p-1" }),
      validProfile({ id: "p-2", name: "平台二", server_host: "10.0.0.2" }),
    ]);
    const { mod, storage } = await freshModule(legacy, "p-2");

    expect(mod.usePlatform().activeId.value).toBe("p-2");
    expect(storage.getItem(ACTIVE_KEY)).toBeNull();
    expect(JSON.parse(storage.getItem(STORE_KEY)!)).toMatchObject({
      kind: "uvp.desktop.config",
      schemaVersion: 1,
      data: { activeProfileId: "p-2" },
    });
  });

  it("损坏数据先留脱敏备份再回退默认，备份不含可识别密码", async () => {
    const secret = "recognizable-password";
    const raw = `[{"id":"p-1","password":"${secret}"}, BROKEN]`;
    const { mod, storage } = await freshModule(raw);

    expect(mod.usePlatform().profiles.value).toHaveLength(1);
    const backup = storage.getItem(BACKUP_KEY)!;
    expect(backup).not.toContain(secret);
    expect(backup).toContain("[REDACTED]");
    expect(JSON.parse(storage.getItem(STORE_KEY)!)).toMatchObject({ schemaVersion: 1 });
  });

  it("损坏数据备份失败时不覆盖 primary", async () => {
    const raw = "{broken-primary";
    const { mod, storage } = await freshModule(raw, undefined, (target) => {
      target.failSetFor.set(BACKUP_KEY, new Error("backup unavailable"));
    });

    expect(mod.usePlatform().profiles.value[0].id).toBe("local");
    expect(storage.getItem(STORE_KEY)).toBe(raw);
  });

  it("含敏感或未知字段的可修复 V1 也必须备份成功后才覆盖 primary", async () => {
    const secret = "repair-path-secret";
    const raw = envelope({
      profiles: [validProfile({ password: secret, unknown: true })],
      activeProfileId: "missing",
    });
    const { mod, storage } = await freshModule(raw, undefined, (target) => {
      target.failSetFor.set(BACKUP_KEY, { name: "QuotaExceededError" });
    });

    expect(mod.usePlatform().profiles.value[0]).toEqual(validProfile());
    expect(mod.usePlatform().activeId.value).toBe("p-1");
    expect(storage.getItem(STORE_KEY)).toBe(raw);
    expect(storage.getItem(BACKUP_KEY)).toBeNull();
  });

  it("严格白名单解码、重复 ID 保留首个并修复 active", async () => {
    const first = validProfile({ unknown: "drop", password: "never-store" });
    const duplicate = validProfile({ name: "重复项", server_host: "10.0.0.9" });
    const invalid = validProfile({ id: "bad", server_host: "example.com" });
    const { mod, storage } = await freshModule(envelope({
      profiles: [first, duplicate, invalid],
      activeProfileId: "missing",
      topLevelUnknown: true,
    }));

    const state = mod.usePlatform();
    expect(state.profiles.value).toHaveLength(1);
    expect(state.profiles.value[0]).toEqual(validProfile());
    expect(state.activeId.value).toBe("p-1");
    const raw = storage.getItem(STORE_KEY)!;
    expect(raw).not.toContain("password");
    expect(raw).not.toContain("unknown");
    const backup = storage.getItem(BACKUP_KEY)!;
    expect(backup).toContain("[REDACTED]");
    expect(backup).not.toContain("never-store");
  });

  it("非法模式与编码修复为安全兼容值，无有效档案则回默认", async () => {
    const repaired = await freshModule(envelope({
      profiles: [validProfile({ inbound_identity_mode: "digest", transport: "SCTP", signaling_encoding: "BIG5" })],
      activeProfileId: "p-1",
    }));
    expect(repaired.mod.usePlatform().profiles.value[0]).toMatchObject({
      inbound_identity_mode: "compatible",
      transport: "UDP",
      signaling_encoding: "GB18030",
    });

    const fallback = await freshModule(envelope({
      profiles: [validProfile({ server_port: 70000 }), validProfile({ id: "bad", server_domain: "123" })],
      activeProfileId: "bad",
    }));
    expect(fallback.mod.usePlatform().profiles.value[0].id).toBe("local");
  });

  it("add/update 只持久化白名单，任何 password/未知字段均不会进入内存或磁盘", async () => {
    const { mod, storage } = await freshModule();
    const state = mod.usePlatform();
    const id = state.addProfile({
      name: "新平台",
      server_host: "1.2.3.4",
      server_port: 5060,
      server_domain: "34020000002000000001",
      inbound_identity_mode: "compatible",
      transport: "UDP",
      // @ts-expect-error 故意传入非法字段，验证运行时白名单
      password: "must-not-persist",
    });
    expect(id).not.toBe("");
    // @ts-expect-error 故意传入非法字段
    expect(state.updateProfile(id, { name: "改名", password: "nope", unknown: "drop" })).toBe(true);
    expect(state.profiles.value.find((profile) => profile.id === id)).not.toHaveProperty("password");
    expect(storage.getItem(STORE_KEY)).not.toContain("must-not-persist");
    expect(storage.getItem(STORE_KEY)).not.toContain("nope");
    expect(storage.getItem(STORE_KEY)).not.toContain("unknown");
  });

  it("写失败时 add/update 不先修改响应式状态", async () => {
    const { mod, storage } = await freshModule();
    const state = mod.usePlatform();
    const original = JSON.stringify(state.profiles.value);
    storage.failSetFor.set(STORE_KEY, { name: "QuotaExceededError" });

    expect(state.addProfile({
      name: "不会出现",
      server_host: "1.2.3.4",
      server_port: 5060,
      server_domain: "34020000002000000001",
      inbound_identity_mode: "strict",
      transport: "TCP",
    })).toBe("");
    expect(state.updateProfile(state.activeId.value, { name: "也不会出现" })).toBe(false);
    expect(JSON.stringify(state.profiles.value)).toBe(original);
  });

  it("未来版本不回写且所有保存操作被锁定", async () => {
    const raw = envelope({ profiles: [validProfile()], activeProfileId: "p-1" }, 9);
    const { mod, storage } = await freshModule(raw);
    const state = mod.usePlatform();

    expect(state.updateProfile("p-1", { name: "禁止覆盖" })).toBe(false);
    expect(state.addProfile({
      name: "禁止新增",
      server_host: "1.2.3.4",
      server_port: 5060,
      server_domain: "34020000002000000001",
      inbound_identity_mode: "strict",
      transport: "TCP",
    })).toBe("");
    expect(storage.getItem(STORE_KEY)).toBe(raw);
    expect(state.profiles.value[0].name).toBe("平台一");
  });

  it("迁移旧明文密码：模块 load 提取到内存，canonical 从不残留密码", async () => {
    const legacy = JSON.stringify([validProfile({ password: "leaked-secret" })]);
    const { mod, storage } = await freshModule(legacy);
    expect(storage.getItem(STORE_KEY)).not.toContain("leaked-secret");

    const migrated = await mod.usePlatform().migrateLegacyPasswords();
    expect(migrated).toEqual(["平台一"]);
    expect(invokeMock).toHaveBeenCalledWith("save_platform_secret", {
      profileId: "p-1",
      password: "leaked-secret",
    });
    expect(storage.getItem(STORE_KEY)).not.toContain("password");
  });

  it("钥匙串迁移成功但 canonical 清理失败时不谎报本地明文已清除", async () => {
    const legacy = JSON.stringify([validProfile({ password: "still-on-disk" })]);
    const { mod, storage } = await freshModule(legacy, undefined, (target) => {
      target.failSetFor.set(STORE_KEY, { name: "QuotaExceededError" });
    });

    expect(await mod.usePlatform().migrateLegacyPasswords()).toEqual([]);
    expect(invokeMock).toHaveBeenCalledWith("save_platform_secret", {
      profileId: "p-1",
      password: "still-on-disk",
    });
    expect(storage.getItem(STORE_KEY)).toBe(legacy);
  });

  it("没有旧密码时不做密码迁移", async () => {
    const { mod } = await freshModule();
    expect(await mod.usePlatform().migrateLegacyPasswords()).toEqual([]);
    expect(invokeMock).not.toHaveBeenCalledWith("save_platform_secret", expect.anything());
  });

  it("remove 保存成功后才更新 ref 并清理钥匙串", async () => {
    const { mod, storage } = await freshModule();
    const state = mod.usePlatform();
    const id = state.addProfile({
      name: "临时",
      server_host: "1.1.1.1",
      server_port: 5060,
      server_domain: "34020000002000000001",
      inbound_identity_mode: "strict",
      transport: "UDP",
    });
    storage.failSetFor.set(STORE_KEY, new Error("write failed"));
    expect(await state.removeProfile(id)).toBe(false);
    expect(state.profiles.value.some((profile) => profile.id === id)).toBe(true);
    expect(invokeMock).not.toHaveBeenCalledWith("delete_platform_secret", { profileId: id });

    storage.failSetFor.delete(STORE_KEY);
    expect(await state.removeProfile(id)).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("delete_platform_secret", { profileId: id });
  });

  it("至少保留一个档案", async () => {
    const { mod } = await freshModule();
    const state = mod.usePlatform();
    expect(await state.removeProfile(state.activeId.value)).toBe(false);
    expect(state.profiles.value).toHaveLength(1);
  });
});
