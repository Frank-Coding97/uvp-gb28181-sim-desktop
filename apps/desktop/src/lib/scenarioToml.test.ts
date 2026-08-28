import { describe, expect, it } from "vitest";

import { buildScenarioToml, tomlString, type ScenarioFormLike } from "./scenarioToml";

const platform = {
  server_host: " 192.168.1.10 ",
  server_port: 5060,
  server_domain: " 34020000002000000001 ",
  transport: "udp",
};

function form(overrides: Partial<ScenarioFormLike> = {}): ScenarioFormLike {
  return {
    base_device_id: "34020000001320000001",
    heartbeat_interval: 60,
    channels_per_device: 1,
    gb_version: "V2022",
    media_profile: "A",
    bitrate_kbps: 512,
    ramp_per_second: 50,
    active_ratio: 100,
    video_source: "",
    ...overrides,
  };
}

describe("tomlString 转义", () => {
  it("引号与反斜杠被转义,值无法被闭合出去", () => {
    expect(tomlString('a"b')).toBe('"a\\"b"');
    expect(tomlString("a\\b")).toBe('"a\\\\b"');
    // 最恶劣的一种:试图闭合字符串并注入新键。
    const evil = '"\nmalicious = "x';
    const encoded = tomlString(evil);
    expect(encoded.startsWith('"')).toBe(true);
    expect(encoded.endsWith('"')).toBe(true);
    // 编码后不含裸换行,因此不可能构成新的一行 TOML。
    expect(encoded.includes("\n")).toBe(false);
  });

  it("中文原样保留", () => {
    expect(tomlString("前门相机")).toBe('"前门相机"');
  });
});

describe("buildScenarioToml", () => {
  it("绝不包含 password 字段", () => {
    const toml = buildScenarioToml(form(), platform);
    expect(toml).not.toContain("password");
  });

  it("平台字段被 trim 且 transport 大写", () => {
    const toml = buildScenarioToml(form(), platform);
    expect(toml).toContain('server_host = "192.168.1.10"');
    expect(toml).toContain('server_domain = "34020000002000000001"');
    expect(toml).toContain('transport = "UDP"');
    expect(toml).toContain("server_port = 5060");
  });

  it("推流占比按百分比折算成 0..1", () => {
    expect(buildScenarioToml(form({ active_ratio: 30 }), platform)).toContain(
      "active_ratio = 0.30",
    );
    expect(buildScenarioToml(form({ active_ratio: 100 }), platform)).toContain(
      "active_ratio = 1.00",
    );
  });

  it("只有 C 档才写 video_source", () => {
    expect(buildScenarioToml(form({ media_profile: "A" }), platform)).not.toContain(
      "video_source",
    );
    const c = buildScenarioToml(
      form({ media_profile: "C", video_source: "  /tmp/x.h264  " }),
      platform,
    );
    expect(c).toContain('video_source = "/tmp/x.h264"');
  });

  it("device_info 表在末尾,普通键不会落进表里", () => {
    const toml = buildScenarioToml(form(), platform);
    const tableIndex = toml.indexOf("[device_info]");
    expect(tableIndex).toBeGreaterThan(0);
    // 顶层键必须全部出现在 [device_info] 之前,否则会被解析成表内字段。
    for (const key of [
      "base_device_id",
      "server_host",
      "transport",
      "heartbeat_interval_secs",
      "ramp_per_second",
      "video_fps",
    ]) {
      expect(toml.indexOf(key)).toBeLessThan(tableIndex);
    }
  });

  it("TCP 平台原样写入 transport", () => {
    const toml = buildScenarioToml(form(), { ...platform, transport: "tcp" });
    expect(toml).toContain('transport = "TCP"');
  });
});
