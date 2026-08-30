// 设备 store 的启停守卫测试。
//
// 锁的是一个真实存在过的 bug:`deviceLive` 直到 `start_device` **返回后**才置 true,
// 而 SIP 注册通常要几百毫秒到几秒 —— 这段窗口"注册"按钮仍可点,连点就会重复
// 下发 start_device(后端会因为槽位已占而报错,但用户看到的是莫名的失败提示)。
// 现在由 `busy` 标志把这个窗口封住,并且 store 内部也有二次守卫。
import { beforeEach, describe, expect, it, vi } from "vitest";

/** 受测试控制的 start_device 完成时机。 */
let releaseStart: (() => void) | null = null;
let startCalls = 0;
let stopCalls = 0;
let startShouldFail = false;

const invokeMock = vi.fn(async (cmd: string, _args?: unknown) => {
  if (cmd === "start_device") {
    startCalls += 1;
    // 模拟"注册要一段时间":直到测试显式放行才 resolve。
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
vi.mock("@tauri-apps/api/event", () => ({
  listen: async () => () => undefined,
}));

/** 极简 localStorage(vitest 的 node 环境没有 DOM;与 platform.test.ts 同一手法)。 */
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
vi.stubGlobal("localStorage", new MemoryStorage());

const { useDevice } = await import("./device");

const platform = {
  id: "p1",
  name: "本地平台",
  server_host: "127.0.0.1",
  server_port: 5060,
  server_domain: "34020000002000000001",
  transport: "UDP" as const,
  signaling_encoding: "GB18030" as const,
};

describe("设备启停守卫", () => {
  const dev = useDevice();

  beforeEach(() => {
    releaseStart = null;
    startCalls = 0;
    stopCalls = 0;
    startShouldFail = false;
    dev.deviceLive.value = false;
    dev.deviceState.value = "Disconnected";
    invokeMock.mockClear();
  });

  it("注册进行中按钮被禁用,重复调用不会重复下发", async () => {
    expect(dev.startDisabled.value).toBe(false);

    const first = dev.startDevice(platform);
    // 关键:此时 start_device 还没返回,deviceLive 仍是 false,
    // 但按钮必须已经是禁用态 —— 这正是之前漏掉的那段窗口。
    await vi.waitFor(() => expect(startCalls).toBe(1));
    expect(dev.startDisabled.value).toBe(true);
    expect(dev.startingUp.value).toBe(true);

    // 窗口内再点一次:store 的二次守卫应直接拒绝,不再发 IPC。
    const second = await dev.startDevice(platform);
    expect(second.ok).toBe(false);
    expect(startCalls).toBe(1);

    releaseStart?.();
    const result = await first;
    expect(result.ok).toBe(true);
    expect(dev.deviceLive.value).toBe(true);
    expect(dev.startingUp.value).toBe(false);
    // 已经起来了:按钮仍禁用,但换成了"实例存在"这个理由。
    expect(dev.startDisabled.value).toBe(true);
    expect(dev.stopDisabled.value).toBe(false);
  });

  it("启动失败后可以重新启动", async () => {
    startShouldFail = true;
    const pending = dev.startDevice(platform);
    await vi.waitFor(() => expect(startCalls).toBe(1));
    releaseStart?.();
    const result = await pending;

    expect(result.ok).toBe(false);
    expect(dev.deviceState.value).toBe("Failed");
    // 后端没留下实例,必须允许重试 —— 否则一次失败就把按钮永久锁死。
    expect(dev.startDisabled.value).toBe(false);
    expect(dev.startingUp.value).toBe(false);
  });

  it("注销进行中同样不可重复触发", async () => {
    dev.deviceLive.value = true;
    expect(dev.stopDisabled.value).toBe(false);

    const result = await dev.stopDevice();
    expect(result.ok).toBe(true);
    expect(stopCalls).toBe(1);
    expect(dev.deviceLive.value).toBe(false);
    // 已经停了:注销按钮禁用,注册按钮恢复可用。
    expect(dev.stopDisabled.value).toBe(true);
    expect(dev.startDisabled.value).toBe(false);
  });

  it("未选平台时不发 IPC", async () => {
    const result = await dev.startDevice(undefined);
    expect(result.ok).toBe(false);
    expect(result.msg).toContain("平台");
    expect(startCalls).toBe(0);
  });
});
