// 错误呈现的测试。
//
// 锁两条不变量:
// 1. **后端原文永不丢失** —— 它是排障唯一可靠的依据,包装层不能把它吃掉;
// 2. **匹配不到已知特征时不硬给建议** —— "请检查配置"这种话没有信息量,
//    给了反而让用户以为已经有人诊断过了。
import { describe, expect, it } from "vitest";

import { formatError, presentError } from "./ipcError";

describe("IPC 错误呈现", () => {
  it("端口占用给出可执行建议", () => {
    const p = presentError("网络错误: Address already in use (os error 48)");
    expect(p.title).toBe("本地端口被占用");
    expect(p.hint).toContain("重复实例");
    // 原文必须保留。
    expect(p.detail).toContain("os error 48");
  });

  it("权限不足指向具体的系统设置位置", () => {
    const p = presentError("媒体错误: Permission denied (os error 13)");
    expect(p.title).toBe("本地权限不足");
    expect(p.hint).toContain("隐私");
  });

  it("缺少密码指向顶栏平台设置", () => {
    const p = presentError("该平台档案尚未配置密码,请在顶栏平台设置中填写并保存");
    expect(p.title).toContain("密码");
    expect(p.hint).toContain("钥匙串");
  });

  it("超时与拒绝分别给出不同动作", () => {
    const timeout = presentError("SIP 事务超时: REGISTER 在 32.0s 内无最终响应(重传 6 次)");
    expect(timeout.title).toContain("超时");
    expect(timeout.hint).toContain("防火墙");

    const rejected = presentError("平台拒绝: 401 Unauthorized(注册)");
    expect(rejected.title).toContain("拒绝");
    expect(rejected.hint).toContain("密码");
    // 两者建议必须不同 —— 之前它们给的是同一句话。
    expect(rejected.hint).not.toBe(timeout.hint);
  });

  it("未知错误不编造建议但保留原文与兜底标题", () => {
    const p = presentError("某个我们没预料到的错误", "保存失败");
    expect(p.title).toBe("保存失败");
    expect(p.hint).toBe("");
    expect(p.detail).toBe("某个我们没预料到的错误");
  });

  it("能处理 Error 对象与非字符串抛出物", () => {
    expect(presentError(new Error("Address already in use")).title).toBe("本地端口被占用");
    expect(presentError({ code: 7 }).detail).toContain("7");
    expect(presentError(undefined).detail).toBe("undefined");
    expect(presentError(42).detail).toBe("42");
  });

  it("单行文案按 标题 → 建议 → 原文 排序", () => {
    const line = formatError("网络错误: Address already in use (os error 48)");
    const titleAt = line.indexOf("本地端口被占用");
    const hintAt = line.indexOf("重复实例");
    const detailAt = line.indexOf("os error 48");
    expect(titleAt).toBeGreaterThanOrEqual(0);
    expect(hintAt).toBeGreaterThan(titleAt);
    // 原文在最后:它是依据,不该抢在建议前面。
    expect(detailAt).toBeGreaterThan(hintAt);
  });

  it("标题与原文相同时不重复输出", () => {
    const line = formatError("某个未知错误", "某个未知错误");
    expect(line).toBe("某个未知错误");
  });
});
