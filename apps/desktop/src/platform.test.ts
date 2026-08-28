// 平台档案 store 的持久化行为。重点锁住一条安全不变量:
// **localStorage 里永远不能出现 password**(密码只存 OS 钥匙串)。
import { beforeEach, describe, expect, it, vi } from "vitest";

/** 极简 localStorage 实现(vitest 的 node 环境没有 DOM)。 */
class MemoryStorage {
  private map = new Map<string, string>();
  getItem(key: string) {
    return this.map.has(key) ? this.map.get(key)! : null;
  }
  setItem(key: string, value: string) {
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

async function freshModule(seed?: string) {
  const storage = new MemoryStorage();
  if (seed !== undefined) storage.setItem(STORE_KEY, seed);
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
    expect(profiles.value[0].server_host).toBe("127.0.0.1");
    expect(profiles.value[0]).not.toHaveProperty("password");
  });

  it("损坏数据回退默认而不是抛异常", async () => {
    const { mod } = await freshModule("{not json");
    expect(mod.usePlatform().profiles.value).toHaveLength(1);
  });

  it("加载旧数据时剥掉残留的 password 字段", async () => {
    const legacy = JSON.stringify([
      {
        id: "p-1",
        name: "旧档案",
        server_host: "10.0.0.1",
        server_port: 5060,
        server_domain: "34020000002000000001",
        transport: "UDP",
        password: "leaked-secret",
      },
    ]);
    const { mod } = await freshModule(legacy);
    const { profiles } = mod.usePlatform();
    expect(profiles.value[0]).not.toHaveProperty("password");
    expect(profiles.value[0].server_host).toBe("10.0.0.1");
  });

  it("写回 localStorage 时不含 password", async () => {
    const { mod, storage } = await freshModule();
    const { addProfile } = mod.usePlatform();
    // 即使调用方(错误地)塞了 password,也不该被持久化。
    addProfile({
      name: "新平台",
      server_host: "1.2.3.4",
      server_port: 5060,
      server_domain: "34020000002000000001",
      transport: "UDP",
      // @ts-expect-error 故意传入非法字段,验证被剥离
      password: "must-not-persist",
    });
    const raw = storage.getItem(STORE_KEY)!;
    expect(raw).not.toContain("password");
    expect(raw).not.toContain("must-not-persist");
  });

  it("updateProfile 不会把 password 写进档案", async () => {
    const { mod, storage } = await freshModule();
    const { updateProfile, activeId, profiles } = mod.usePlatform();
    // @ts-expect-error 故意传入非法字段
    updateProfile(activeId.value, { name: "改名", password: "nope" });
    expect(profiles.value[0].name).toBe("改名");
    expect(profiles.value[0]).not.toHaveProperty("password");
    expect(storage.getItem(STORE_KEY)!).not.toContain("nope");
  });

  it("迁移旧明文密码:写进钥匙串并抹掉本地明文", async () => {
    const legacy = JSON.stringify([
      {
        id: "p-1",
        name: "旧档案",
        server_host: "10.0.0.1",
        server_port: 5060,
        server_domain: "34020000002000000001",
        transport: "UDP",
        password: "leaked-secret",
      },
    ]);
    const { mod, storage } = await freshModule(legacy);
    const { migrateLegacyPasswords } = mod.usePlatform();
    const migrated = await migrateLegacyPasswords();
    expect(migrated).toEqual(["旧档案"]);
    expect(invokeMock).toHaveBeenCalledWith("save_platform_secret", {
      profileId: "p-1",
      password: "leaked-secret",
    });
    expect(storage.getItem(STORE_KEY)!).not.toContain("leaked-secret");
  });

  it("没有旧密码时不做迁移", async () => {
    const { mod } = await freshModule();
    expect(await mod.usePlatform().migrateLegacyPasswords()).toEqual([]);
    expect(invokeMock).not.toHaveBeenCalledWith(
      "save_platform_secret",
      expect.anything(),
    );
  });

  it("删除档案会一并清理钥匙串条目", async () => {
    const { mod } = await freshModule();
    const { addProfile, removeProfile, profiles } = mod.usePlatform();
    const id = addProfile({
      name: "临时",
      server_host: "1.1.1.1",
      server_port: 5060,
      server_domain: "34020000002000000001",
      transport: "UDP",
    });
    expect(profiles.value).toHaveLength(2);
    await removeProfile(id);
    expect(profiles.value).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledWith("delete_platform_secret", { profileId: id });
  });

  it("至少保留一个档案", async () => {
    const { mod } = await freshModule();
    const { removeProfile, profiles, activeId } = mod.usePlatform();
    await removeProfile(activeId.value);
    expect(profiles.value).toHaveLength(1);
  });
});
