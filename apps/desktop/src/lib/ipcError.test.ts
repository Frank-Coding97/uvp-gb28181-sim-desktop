import { describe, expect, it } from "vitest";

import {
  formatError,
  normalizeIpcError,
  presentError,
  type IpcErrorDto,
} from "./ipcError";

describe("IPC 错误归一化", () => {
  it("接受原生 IpcError 对象并忽略错误类型的可选字段", () => {
    const error = normalizeIpcError({
      code: "sip.timeout",
      message: "SIP 事务超时: REGISTER",
      details: 7,
      retryable: "yes",
      action: false,
    });
    expect(error).toEqual({
      code: "sip.timeout",
      message: "SIP 事务超时: REGISTER",
    });
  });

  it("接受 JSON 字符串 DTO", () => {
    const dto: IpcErrorDto = {
      code: "device.not_running",
      message: "设备未启动",
      retryable: false,
      action: "先启动设备",
    };
    expect(normalizeIpcError(JSON.stringify(dto))).toEqual(dto);
  });

  it("兼容 legacy string、Error、普通对象与 null", () => {
    expect(normalizeIpcError("旧错误").message).toBe("旧错误");
    expect(normalizeIpcError(new Error("异常对象")).message).toBe("异常对象");
    expect(normalizeIpcError({ reason: "bad" }).message).toContain("bad");
    expect(normalizeIpcError(null).message).toBe("null");
  });

  it("循环引用不会让归一化再次抛错", () => {
    const cyclic: Record<string, unknown> = { reason: "bad" };
    cyclic.self = cyclic;
    const normalized = normalizeIpcError(cyclic);
    expect(normalized.message).toContain("bad");
    expect(normalized.message).toContain("[Circular]");
  });

  it("message 与 details 都保留且相同内容只出现一次", () => {
    const same = presentError({ code: "internal.error", message: "失败", details: "失败" });
    expect(same.detail).toBe("失败");

    const distinct = presentError({
      code: "internal.error",
      message: "操作失败",
      details: "文件不可读",
    });
    expect(distinct.detail).toBe("操作失败；文件不可读");
    expect(formatError(distinct.error, "保存失败")).toContain("操作失败；文件不可读");
  });
});

describe("IPC 错误呈现", () => {
  it("稳定 code 优先于文案规则", () => {
    const p = presentError({
      code: "device.not_running",
      message: "Address already in use",
    });
    expect(p.title).toBe("设备未启动");
    expect(p.hint).toContain("注册");
  });

  it("DTO action 优先于 code 与旧文本建议", () => {
    const p = presentError({
      code: "sip.timeout",
      message: "SIP 事务超时",
      action: "联系专用网络管理员",
    });
    expect(p.title).toContain("超时");
    expect(p.hint).toBe("联系专用网络管理员");
  });

  it("端口占用旧字符串仍给出可执行建议", () => {
    const p = presentError("网络错误: Address already in use (os error 48)");
    expect(p.title).toBe("本地端口被占用");
    expect(p.hint).toContain("重复实例");
    expect(p.detail).toContain("os error 48");
  });

  it("超时与鉴权拒绝按 code 给出不同动作", () => {
    const timeout = presentError({ code: "sip.timeout", message: "REGISTER 超时" });
    const rejected = presentError({
      code: "sip.rejected.auth",
      message: "平台拒绝: 401 Unauthorized",
    });
    expect(timeout.hint).toContain("防火墙");
    expect(rejected.hint).toContain("密码");
    expect(rejected.hint).not.toBe(timeout.hint);
  });

  it("未知 code 与未知字符串都不编造建议", () => {
    const object = presentError(
      { code: "future.new_code", message: "Permission denied by remote policy" },
      "保存失败",
    );
    expect(object.title).toBe("保存失败");
    expect(object.hint).toBe("");
    expect(object.detail).toContain("Permission denied");

    const text = presentError("某个我们没预料到的错误", "保存失败");
    expect(text.title).toBe("保存失败");
    expect(text.hint).toBe("");
  });

  it("单行文案按标题、建议、原文排序", () => {
    const line = formatError("网络错误: Address already in use (os error 48)");
    const titleAt = line.indexOf("本地端口被占用");
    const hintAt = line.indexOf("重复实例");
    const detailAt = line.indexOf("os error 48");
    expect(titleAt).toBeGreaterThanOrEqual(0);
    expect(hintAt).toBeGreaterThan(titleAt);
    expect(detailAt).toBeGreaterThan(hintAt);
  });

  it("标题与原文相同或原文已带标题前缀时不重复", () => {
    expect(formatError("某个未知错误", "某个未知错误")).toBe("某个未知错误");
    expect(formatError("保存失败：字段非法", "保存失败")).toBe("保存失败：字段非法");
    expect(formatError("保存失败: 字段非法", "保存失败")).toBe("保存失败: 字段非法");
  });

  it("多行错误会格式化为单行但内容不丢失", () => {
    const line = formatError({
      code: "internal.error",
      message: "第一行\n第二行",
      details: "第三行",
    });
    expect(line).not.toContain("\n");
    expect(line).toContain("第一行 第二行");
    expect(line).toContain("第三行");
  });
});
