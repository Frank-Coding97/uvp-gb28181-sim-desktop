// 压测场景 TOML 构造 —— **纯函数**,与 Vue 无关。
//
// 抽出来单测的理由:引号转义直接关系到 TOML 注入(设备名/路径里带引号或反斜杠时
// 能否把值"闭合"掉),而且**这里绝不能出现密码** —— 密码只存 OS 钥匙串,
// 由 Rust 侧按 profile_id 取。这两条都用测试锁住。

export interface ScenarioFormLike {
  base_device_id: string;
  heartbeat_interval: number;
  channels_per_device: number;
  gb_version: string;
  media_profile: string;
  bitrate_kbps: number;
  ramp_per_second: number;
  active_ratio: number;
  video_source: string;
}

export interface PlatformLike {
  server_host: string;
  server_port: number;
  server_domain: string;
  inbound_identity_mode: "compatible" | "strict";
  transport: string;
}

/**
 * 把字符串编码成 TOML 基本字符串。
 *
 * 用 `JSON.stringify`:它对 `"`、`\`、控制字符的转义规则与 TOML 基本字符串一致,
 * 因此值里带引号/反斜杠不会"闭合"出去。
 */
export function tomlString(value: string): string {
  return JSON.stringify(value);
}

/** 构造场景 TOML。注意:**不含 password 字段**。 */
export function buildScenarioToml(form: ScenarioFormLike, platform: PlatformLike): string {
  const lines = [
    `base_device_id = ${tomlString(form.base_device_id)}`,
    `server_host = ${tomlString(platform.server_host.trim())}`,
    `server_port = ${platform.server_port}`,
    `server_domain = ${tomlString(platform.server_domain.trim())}`,
    `inbound_identity_mode = ${tomlString(platform.inbound_identity_mode)}`,
    `transport = ${tomlString(platform.transport.toUpperCase())}`,
    `heartbeat_interval_secs = ${form.heartbeat_interval}`,
    `channels_per_device = ${form.channels_per_device}`,
    `gb_version = ${tomlString(form.gb_version)}`,
    `media_profile = ${tomlString(form.media_profile)}`,
    `bitrate_kbps = ${form.bitrate_kbps}`,
    `ramp_per_second = ${form.ramp_per_second}`,
    `active_ratio = ${(form.active_ratio / 100).toFixed(2)}`,
    "video_fps = 25",
  ];
  if (form.media_profile === "C") {
    lines.push(`video_source = ${tomlString(form.video_source.trim())}`);
  }
  lines.push(
    "[device_info]",
    `device_name = ${tomlString("UVP-Sim")}`,
    `manufacturer = ${tomlString("UVP")}`,
    `model = ${tomlString("Desktop-Sim")}`,
    `firmware = ${tomlString("0.1.2")}`,
  );
  return lines.join("\n");
}
