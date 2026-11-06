import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, router, app, backend, config, clock, ntp] = await Promise.all([
  readFile(new URL("./Clock.vue", import.meta.url), "utf8"),
  readFile(new URL("../main.ts", import.meta.url), "utf8"),
  readFile(new URL("../App.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/config_store.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/common/src/clock.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/common/src/ntp.rs", import.meta.url), "utf8"),
]);

test("校时页展示平台基准、本机时钟、偏移与来源", () => {
  assert.match(view, /设备对外时间/);
  assert.match(view, /本机系统时间/);
  assert.match(view, /偏移（平台 − 本地）/);
  assert.match(view, /最近一次校时/);
  assert.match(view, /SIP Date 回退值/);
  assert.match(view, /来源凭证/);
  assert.match(view, /不修改操作系统时间/);
});

test("校时页提供 NTP 配置与手动校时", () => {
  assert.match(view, /NTP 服务器/);
  assert.match(view, /刷新间隔/);
  assert.match(view, /立即校时/);
  assert.match(view, /syncNtpNow/);
  assert.match(view, /saveTimeSync/);
  assert.match(view, /getClockState/);
});

test("校时页拥有一级菜单与独立路由", () => {
  assert.match(app, /label: "设备校时"[\s\S]{0,80}key: "\/clock"/);
  assert.match(router, /path: "\/clock"[\s\S]{0,80}component: Clock/);
});

test("后端注册校时命令并持久化 NTP 配置", () => {
  assert.match(backend, /get_clock_state/);
  assert.match(backend, /save_time_sync/);
  assert.match(backend, /sync_ntp_now/);
  assert.match(backend, /ntp_refresh_loop/);
  assert.match(config, /pub struct TimeSyncSettings/);
  assert.match(config, /pub fn save_time_sync/);
  assert.match(config, /refresh_interval_secs/);
});

test("NTP 生效期间不被 SIP Date 覆盖，失败回退 SIP 偏移", () => {
  assert.match(clock, /NTP_ACTIVE/);
  assert.match(clock, /pub fn apply_ntp/);
  assert.match(clock, /pub fn ntp_failed/);
  assert.match(clock, /pub fn clear_ntp/);
  assert.match(clock, /fn ntp生效期间sip_date不覆盖偏移/);
  assert.match(clock, /fn ntp失败回退sip_date/);
});

test("SNTP 客户端校验应答可信性", () => {
  assert.match(ntp, /pub fn query/);
  assert.match(ntp, /pub fn parse_response/);
  assert.match(ntp, /NTP 服务器自报未同步/);
  assert.match(ntp, /发起时间戳与本地请求不匹配/);
  assert.match(ntp, /fn 偏移与往返时延计算/);
});
