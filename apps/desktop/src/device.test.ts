import { beforeEach, describe, expect, it, vi } from "vitest";

let releaseStart: (() => void) | null = null;
let startCalls = 0;
let stopCalls = 0;
let startShouldFail = false;

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "start_device") {
    startCalls += 1;
    await new Promise<void>((resolve) => {
      releaseStart = resolve;
    });
    if (startShouldFail) throw new Error("平台不可达");
    return "设备已启动";
  }
  if (cmd === "stop_device") {
    stopCalls += 1;
    return "设备已停止";
  }
  if (cmd === "get_device_status") return { running: false };
  return undefined;
});

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => invokeMock(cmd, args),
}));

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

const FORM_KEY = "uvp_device_form";

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
  if (seed !== undefined) storage.map.set(FORM_KEY, seed);
  prepare?.(storage);
  vi.stubGlobal("localStorage", storage);
  vi.resetModules();
  const mod = await import("./device");
  return { mod, storage, dev: mod.useDevice() };
}

const platform = {
  id: "p1",
  name: "本地平台",
  server_host: "127.0.0.1",
  server_port: 5060,
  server_domain: "34020000002000000001",
  inbound_identity_mode: "strict" as const,
  transport: "UDP" as const,
  signaling_encoding: "GB18030" as const,
};

beforeEach(() => {
  releaseStart = null;
  startCalls = 0;
  stopCalls = 0;
  startShouldFail = false;
  invokeMock.mockClear();
});

describe("设备表单持久化", () => {
  it("迁移旧裸对象、丢弃未知字段并修复类型与边界", async () => {
    const legacy = JSON.stringify({
      device_id: "bad-id",
      gb_version: "2008",
      channel_name: "Legacy Camera",
      video_source: 123,
      catalog_template: "template",
      manufacturer: "Legacy",
      model: "M1",
      firmware: "F1",
      heartbeat_interval: 4,
      max_heartbeat_timeout: 11,
      expires: 59,
      video_fps: Number.NaN,
      password: "must-drop",
      unknown: true,
    });
    const { dev, storage } = await freshModule(legacy);

    expect(dev.form.value).toMatchObject({
      device_id: "35020000001310000001",
      gb_version: "2022",
      channel_name: "Legacy Camera",
      video_source: "",
      heartbeat_interval: 60,
      max_heartbeat_timeout: 3,
      expires: 3600,
      video_fps: 25,
    });
    const raw = storage.getItem(FORM_KEY)!;
    expect(raw).not.toContain("password");
    expect(raw).not.toContain("unknown");
    expect(JSON.parse(raw)).toMatchObject({ kind: "uvp.desktop.config", schemaVersion: 1 });
  });

  it("合法边界与 2016 版本可持久化", async () => {
    const { mod, dev, storage } = await freshModule();
    Object.assign(dev.form.value, {
      device_id: "34020000001310000001",
      gb_version: "2016",
      heartbeat_interval: 5,
      max_heartbeat_timeout: 10,
      expires: 86400,
      video_fps: 120,
    });
    expect(mod.persistForm()).toEqual({ ok: true });
    expect(JSON.parse(storage.getItem(FORM_KEY)!).data).toMatchObject({
      gb_version: "2016",
      heartbeat_interval: 5,
      max_heartbeat_timeout: 10,
      expires: 86400,
      video_fps: 120,
    });
  });

  it("quota 写失败返回结果且不抛", async () => {
    const { mod, storage } = await freshModule();
    storage.failSetFor.set(FORM_KEY, { name: "QuotaExceededError" });
    expect(mod.persistForm()).toEqual({ ok: false, reason: "storage", category: "quota" });
  });

  it("未来版本不回写并锁定保存", async () => {
    const raw = envelope({
      device_id: "34020000001310000001",
      gb_version: "2022",
      channel_name: "Future",
      video_source: "",
      catalog_template: "",
      manufacturer: "UVP",
      model: "M",
      firmware: "F",
      heartbeat_interval: 60,
      max_heartbeat_timeout: 3,
      expires: 3600,
      video_fps: 25,
    }, 8);
    const { mod, storage } = await freshModule(raw);
    expect(mod.persistForm()).toEqual({ ok: false, reason: "future-version" });
    expect(storage.getItem(FORM_KEY)).toBe(raw);
  });
});

describe("设备启停守卫", () => {
  it("注册进行中按钮被禁用，重复调用不会重复下发", async () => {
    const { dev } = await freshModule();
    const first = dev.startDevice(platform);
    await vi.waitFor(() => expect(startCalls).toBe(1));
    expect(invokeMock).toHaveBeenCalledWith("start_device", {
      config: expect.objectContaining({ inbound_identity_mode: "strict" }),
    });
    expect(dev.startDisabled.value).toBe(true);
    expect(dev.startingUp.value).toBe(true);

    const second = await dev.startDevice(platform);
    expect(second.ok).toBe(false);
    expect(startCalls).toBe(1);

    releaseStart?.();
    const result = await first;
    expect(result.ok).toBe(true);
    expect(dev.deviceLive.value).toBe(true);
    expect(dev.startingUp.value).toBe(false);
  });

  it("持久化失败明确返回失败、恢复 busy 且不发 IPC", async () => {
    const { dev, storage } = await freshModule(undefined, (target) => {
      target.failSetFor.set(FORM_KEY, { name: "SecurityError" });
    });
    // prepare 在模块加载前生效，不妨碍 load；失败只会发生在启动保存。
    storage.failSetFor.set(FORM_KEY, { name: "SecurityError" });

    const result = await dev.startDevice(platform);

    expect(result).toEqual({ ok: false, msg: "设备配置保存失败，未启动设备" });
    expect(startCalls).toBe(0);
    expect(dev.startingUp.value).toBe(false);
    expect(dev.startDisabled.value).toBe(false);
  });

  it("未来版本锁定时不发 IPC 且 busy 恢复", async () => {
    const raw = envelope({
      device_id: "34020000001310000001",
      gb_version: "2022",
      channel_name: "Future",
      video_source: "",
      catalog_template: "",
      manufacturer: "UVP",
      model: "M",
      firmware: "F",
      heartbeat_interval: 60,
      max_heartbeat_timeout: 3,
      expires: 3600,
      video_fps: 25,
    }, 2);
    const { dev, storage } = await freshModule(raw);
    expect((await dev.startDevice(platform)).ok).toBe(false);
    expect(startCalls).toBe(0);
    expect(dev.startingUp.value).toBe(false);
    expect(storage.getItem(FORM_KEY)).toBe(raw);
  });

  it("启动失败后可以重新启动", async () => {
    const { dev } = await freshModule();
    startShouldFail = true;
    const pending = dev.startDevice(platform);
    await vi.waitFor(() => expect(startCalls).toBe(1));
    releaseStart?.();
    const result = await pending;

    expect(result.ok).toBe(false);
    expect(dev.deviceState.value).toBe("Failed");
    expect(dev.startDisabled.value).toBe(false);
    expect(dev.startingUp.value).toBe(false);
  });

  it("注销后恢复按钮状态", async () => {
    const { dev } = await freshModule();
    dev.deviceLive.value = true;
    const result = await dev.stopDevice();
    expect(result.ok).toBe(true);
    expect(stopCalls).toBe(1);
    expect(dev.deviceLive.value).toBe(false);
    expect(dev.stopDisabled.value).toBe(true);
    expect(dev.startDisabled.value).toBe(false);
  });

  it("未选平台时不发 IPC", async () => {
    const { dev } = await freshModule();
    const result = await dev.startDevice(undefined);
    expect(result.ok).toBe(false);
    expect(result.msg).toContain("平台");
    expect(startCalls).toBe(0);
  });
});
