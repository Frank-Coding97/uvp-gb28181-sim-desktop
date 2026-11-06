import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [dashboard, simDevice, backend, protocolPtz, protocolManscdp] = await Promise.all([
  readFile(new URL("./Dashboard.vue", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-simulator/src/device.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/ptz.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/manscdp.rs", import.meta.url), "utf8"),
]);

test("订阅快照带运行详情而非只有活跃标记", () => {
  for (const field of [
    "pub lifecycle: String",
    "pub subscriber: Option<String>",
    "pub expires_secs: u32",
    "pub remaining_secs: u32",
    "pub last_notify_at_ms: Option<u64>",
    "pub last_notify_result: Option<String>",
    "pub last_error: Option<String>",
    "pub ptz_position: Option<PtzPositionSnapshot>",
  ]) {
    assert.ok(simDevice.includes(field), `订阅快照缺少字段：${field}`);
  }
});

test("生命周期区分未订阅/已订阅/已过期/已退订/异常", () => {
  for (const state of ['"subscribed"', '"expired"', '"exception"', '"cancelled"', '"not_subscribed"']) {
    assert.ok(simDevice.includes(state), `缺少生命周期取值：${state}`);
  }
  // 过期必须由「是否活跃」+「剩余时间」共同判定，否则到点未续订仍显示已订阅。
  assert.match(simDevice, /if active && remaining_secs > 0 \{/);
  assert.match(simDevice, /else if active \{\s*"expired"/);
});

test("剩余秒数饱和到 0，不会下溢", () => {
  assert.match(simDevice, /fn remaining_secs\(&self, now_ms: u64\) -> u32/);
  assert.match(simDevice, /expires_ms\.saturating_sub\(elapsed_ms\)/);
  assert.match(simDevice, /if self\.expires_secs == 0 \{\s*return 0;/);
});

test("订阅建立/退订/通知结果都落进运行详情", () => {
  assert.match(simDevice, /fn mark_subscribed\(&self, kind: &str, subscriber: String, expires_secs: u32\)/);
  assert.match(simDevice, /fn mark_unsubscribed\(&self, kind: &str\)/);
  assert.match(simDevice, /fn record_notify_outcome\(&self, kind: &str, outcome: &Result<u16>\)/);
  // 四类订阅都必须有钩子，不能只补一部分。
  const kinds = ["MobilePosition", "Alarm", "Catalog", "PTZPosition"];
  for (const kind of kinds) {
    assert.ok(simDevice.includes(`"${kind}"`), `缺少订阅类别：${kind}`);
  }
  // 成功时清旧错误；失败时写入错误，界面才能反映「已恢复」。
  assert.match(simDevice, /entry\.last_notify_result = Some\(status\.to_string\(\)\);\s*entry\.last_error = None;/);
});

test("设备下线清空订阅运行详情", () => {
  assert.match(simDevice, /for kind in TRACKED_SUBSCRIPTIONS \{\s*self\.mark_unsubscribed\(kind\);\s*\}/);
});

test("PTZ 六字段齐全且光学量按变焦换算", () => {
  for (const field of [
    "pub pan: f64",
    "pub tilt: f64",
    "pub zoom: f64",
    "pub horizontal_field_angle: f64",
    "pub vertical_field_angle: f64",
    "pub max_view_distance: f64",
  ]) {
    assert.ok(simDevice.includes(field), `六字段快照缺少：${field}`);
  }
  assert.match(protocolPtz, /BASE_HORIZONTAL_FIELD_ANGLE_DEG: f32 = 60\.0/);
  assert.match(protocolPtz, /BASE_VERTICAL_FIELD_ANGLE_DEG: f32 = 36\.0/);
  assert.match(protocolPtz, /BASE_MAX_VIEW_DISTANCE_M: f32 = 300\.0/);
  // 报文侧六字段必须都进 XML，不能只报三轴。
  assert.match(protocolManscdp, /rename = "HorizontalFieldAngle"/);
  assert.match(protocolManscdp, /rename = "VerticalFieldAngle"/);
  assert.match(protocolManscdp, /rename = "MaxViewDistance"/);
});

test("前端订阅卡片展示剩余时间、上报次数与悬停详情", () => {
  assert.match(dashboard, /function subscriptionLabel\(item: SubscriptionState\): string/);
  assert.match(dashboard, /function formatRemaining\(totalSecs: number\): string/);
  assert.match(dashboard, /function subscriptionTip\(item: SubscriptionState\): string/);
  assert.match(dashboard, /subscriptionLabel\(subscriptions\.MobilePosition\)/);
  assert.match(dashboard, /已上报 \{\{ subscriptions\.Catalog\.notify_count \}\} 次/);
  assert.match(dashboard, /最大可视距离/);
});

test("订阅事件只补活跃与计数，详情回读快照", () => {
  // 事件载荷只有 kind/active/notify_count，整体覆盖会把详情写成 undefined。
  assert.match(dashboard, /Pick<SubscriptionState, "kind" \| "active" \| "notify_count">/);
  assert.match(dashboard, /item\.active = event\.payload\.active;/);
  assert.match(dashboard, /scheduleSubscriptionRefresh\(\);/);
  assert.match(dashboard, /}, 300\);/);
});

test("本地秒级倒数让到期即时翻成已过期", () => {
  assert.match(dashboard, /function tickSubscriptionCountdown\(\)/);
  assert.match(dashboard, /item\.remaining_secs -= 1;/);
  assert.match(dashboard, /tickSubscriptionCountdown\(\);/);
});

test("后端订阅命令返回完整快照", () => {
  assert.match(backend, /get_device_subscriptions,/);
  assert.match(backend, /gb28181_simulator::SubscriptionSnapshot/);
});
