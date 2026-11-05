import test from "node:test";
import assert from "node:assert/strict";

import {
  describeParseFailure,
  describeProvisionGaps,
  parseProvisionLink,
  readProvisionFailure,
  toProfilePatch,
} from "./provision-link.ts";

const VALID_TOKEN = "CjsyPp2aLZfT0yREqPlexA";
const VALID_LINK = `https://gb28181.uvplatform.com/gb28181/qr#t=${VALID_TOKEN}`;

test("解析平台真实链接", () => {
  const result = parseProvisionLink(VALID_LINK);
  assert.equal(result.ok, true);
  assert.deepEqual(result.value, {
    baseUrl: "https://gb28181.uvplatform.com",
    token: VALID_TOKEN,
  });
});

test("保留非默认端口", () => {
  const result = parseProvisionLink(`http://192.168.1.10:8080/gb28181/qr#t=${VALID_TOKEN}`);
  assert.equal(result.ok, true);
  assert.equal(result.value.baseUrl, "http://192.168.1.10:8080");
});

test("忽略链接上的其它 query 参数", () => {
  const result = parseProvisionLink(`https://demo.example.com/gb28181/qr?from=pc#t=${VALID_TOKEN}`);
  assert.equal(result.ok, true);
  assert.equal(result.value.token, VALID_TOKEN);
});

test("剥离复制时带入的空白与引号", () => {
  const result = parseProvisionLink(`  “${VALID_LINK}”\n`);
  assert.equal(result.ok, true);
  assert.equal(result.value.token, VALID_TOKEN);
});

test("裸接入码不带平台地址", () => {
  const result = parseProvisionLink(VALID_TOKEN);
  assert.equal(result.ok, true);
  assert.equal(result.value.baseUrl, "");
  assert.equal(result.value.token, VALID_TOKEN);
});

test("空输入单独归类", () => {
  assert.deepEqual(parseProvisionLink("   "), { ok: false, reason: "empty" });
});

test("非链接文本被拒绝", () => {
  assert.deepEqual(parseProvisionLink("扫描二维码接入"), { ok: false, reason: "not_url" });
});

test("拒绝非 http(s) 协议", () => {
  const result = parseProvisionLink(`javascript:alert(1)//#t=${VALID_TOKEN}`);
  assert.deepEqual(result, { ok: false, reason: "bad_scheme" });
});

test("链接缺少 fragment 时提示不完整", () => {
  assert.deepEqual(parseProvisionLink("https://gb28181.uvplatform.com/gb28181/qr"), {
    ok: false,
    reason: "no_token",
  });
});

test("token 放在 query 里不被接受", () => {
  // 平台刻意只用 fragment;query 形态说明链接被改写过或来自更早的版本。
  assert.deepEqual(parseProvisionLink(`https://example.com/gb28181/qr?t=${VALID_TOKEN}`), {
    ok: false,
    reason: "no_token",
  });
});

test("token 长度不符被拒绝", () => {
  assert.deepEqual(parseProvisionLink("https://example.com/gb28181/qr#t=short"), {
    ok: false,
    reason: "bad_token",
  });
});

test("token 含非法字符被拒绝", () => {
  assert.deepEqual(parseProvisionLink("https://example.com/gb28181/qr#t=abc$efghijk1234567890"), {
    ok: false,
    reason: "bad_token",
  });
});

test("解析失败都有可读文案", () => {
  for (const reason of ["empty", "not_url", "bad_scheme", "no_token", "bad_token"]) {
    assert.ok(describeParseFailure(reason).length > 0);
  }
});

test("六元组映射到平台配置字段", () => {
  const patch = toProfilePatch({
    serverId: "34020000002000000001",
    domain: "3402000000",
    ip: "192.168.1.100",
    port: 5060,
    transport: "udp",
    password: "uvp123456",
  });
  assert.deepEqual(patch, {
    server_host: "192.168.1.100",
    server_port: 5060,
    server_id: "34020000002000000001",
    server_domain: "3402000000",
    password: "uvp123456",
    transport: "UDP",
  });
});

test("TCP 传输归一为大写", () => {
  const patch = toProfilePatch({
    serverId: "34020000002000000001",
    domain: "3402000000",
    ip: "127.0.0.1",
    port: 5060,
    transport: "TCP",
    password: "",
  });
  assert.equal(patch.transport, "TCP");
});

test("非法端口回落默认值", () => {
  const base = {
    serverId: "34020000002000000001",
    domain: "3402000000",
    ip: "127.0.0.1",
    transport: "udp",
    password: "",
  };
  assert.equal(toProfilePatch({ ...base, port: 0 }).server_port, 5060);
  assert.equal(toProfilePatch({ ...base, port: 70000 }).server_port, 5060);
  assert.equal(toProfilePatch({ ...base, port: 8080 }).server_port, 8080);
});

test("合法六元组没有待补项", () => {
  const patch = toProfilePatch({
    serverId: "34020000002000000001",
    domain: "3402000000",
    ip: "10.0.0.5",
    port: 5060,
    transport: "udp",
    password: "x",
  });
  assert.deepEqual(describeProvisionGaps(patch), []);
});

test("异常 ID 在回填后立刻暴露", () => {
  const patch = toProfilePatch({
    serverId: "123",
    domain: "12345",
    ip: "10.0.0.5",
    port: 5060,
    transport: "udp",
    password: "",
  });
  assert.deepEqual(describeProvisionGaps(patch), ["服务器 ID 不是 20 位数字", "服务器域不是 10 位数字"]);
});

test("结构化失败原样透传", () => {
  assert.deepEqual(readProvisionFailure({ code: "expired", message: "接入码已失效" }), {
    code: "expired",
    message: "接入码已失效",
  });
});

test("非结构化失败也有兜底文案", () => {
  assert.deepEqual(readProvisionFailure("IPC 调用失败"), {
    code: "server",
    message: "IPC 调用失败",
  });
  assert.deepEqual(readProvisionFailure(null), { code: "server", message: "接入失败" });
});
