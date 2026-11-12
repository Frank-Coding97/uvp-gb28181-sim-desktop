import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, router, app, backend, platform, alarm, device, manscdp] = await Promise.all([
  readFile(new URL("./Alarms.vue", import.meta.url), "utf8"),
  readFile(new URL("../main.ts", import.meta.url), "utf8"),
  readFile(new URL("../App.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../platform.ts", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/alarm.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-simulator/src/device.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/gb28181-protocol/src/manscdp.rs", import.meta.url), "utf8"),
]);

test("报警页覆盖等级、方式、类型、描述、时间与经纬度", () => {
  for (const label of ["AlarmPriority", "AlarmMethod", "AlarmType", "报警描述", "报警时间", "经纬度"]) {
    assert.ok(view.includes(label), `缺少字段: ${label}`);
  }
  assert.match(view, /固定单/);
  assert.match(view, /随机发一条/);
  assert.match(view, /复位本机报警/);
  assert.match(view, /预览 XML/);
});

test("报警历史展示送达结果与发送路径", () => {
  assert.match(view, /报警历史/);
  assert.match(view, /via_notify \? "订阅 NOTIFY" : "独立 MESSAGE"/);
  assert.match(view, /statusText\(record\)/);
  assert.match(view, /listAlarmHistory/);
});

test("报警页拥有一级菜单与独立路由", () => {
  assert.match(app, /label: "报警管理"[\s\S]{0,80}key: "\/alarms"/);
  assert.match(router, /path: "\/alarms"[\s\S]{0,80}component: Alarms/);
  assert.match(platform, /export async function reportAlarm/);
  assert.match(platform, /export async function previewAlarmXml/);
});

test("后端注册报警命令并把失败也写进历史", () => {
  assert.match(backend, /report_alarm,/);
  assert.match(backend, /preview_alarm_xml/);
  assert.match(backend, /list_alarm_history/);
  assert.match(backend, /reset_alarm/);
  assert.match(backend, /ALARM_HISTORY_LIMIT/);
  // 失败路径必须落库：先 push_front 再按结果返回。
  assert.match(backend, /history\.push_front\(record\.clone\(\)\)/);
  assert.match(backend, /Err\(error\) => Err\(error\)/);
});

test("报警单在协议层预检，非法值不进入 XML", () => {
  assert.match(alarm, /pub fn validate/);
  assert.match(alarm, /报警级别必须是 1-4/);
  assert.match(alarm, /经度必须在 -180~180/);
  assert.match(alarm, /纬度必须在 -90~90/);
  assert.match(alarm, /fn invalid_report_never_reaches_xml/);
  assert.match(alarm, /pub fn to_notify/);
});

test("类型下拉按报警方式联动，标准类型表只有一个来源", () => {
  // 方式/类型不再各自写死在页面上，统一取协议层的 get_alarm_capability。
  assert.match(view, /listAlarmCapability/);
  assert.match(view, /const typeOptions = computed/);
  assert.doesNotMatch(view, /const TYPES = \[/, "页面不应再维护写死的类型数组");
  assert.doesNotMatch(view, /const METHODS = \[/, "方式名称应由协议层提供");
  assert.match(platform, /export async function listAlarmCapability/);
  assert.match(backend, /get_alarm_capability,/);
  assert.match(alarm, /pub fn alarm_type_names/);
  assert.match(alarm, /pub fn alarm_type_range/);
});

test("固定单给出的方式/类型组合必须在标准里成立", () => {
  // 独立于源码的一份标准区间：方式 2 → 1-5，方式 5 → 1-13，方式 6 → 1-2，其余不带。
  const ranges = { 2: [1, 5], 5: [1, 13], 6: [1, 2] };
  const presets = [
    ...view.matchAll(/\{ label: "([^"]+)", report: \{ priority: \d+, method: (\d+), alarm_type: (null|\d+)/g),
  ];
  assert.ok(presets.length >= 5, `固定单应至少 5 条，实际 ${presets.length}`);
  for (const [, label, method, type] of presets) {
    const code = Number(method);
    if (type === "null") {
      assert.equal(ranges[code], undefined, `${label}: 方式 ${code} 不应携带类型`);
      continue;
    }
    const range = ranges[code];
    assert.ok(range, `${label}: 方式 ${code} 在标准里没有类型表，不能携带类型`);
    const value = Number(type);
    assert.ok(
      value >= range[0] && value <= range[1],
      `${label}: 方式 ${code} 的类型 ${value} 超出 ${range[0]}-${range[1]}`,
    );
  }
});

test("AlarmType 位于 Info 容器，可选字段缺省时不输出", () => {
  assert.match(manscdp, /pub struct AlarmNotifyInfo/);
  assert.match(manscdp, /#\[serde\(rename = "Info", skip_serializing_if = "Option::is_none"\)\]/);
  assert.match(manscdp, /fn 自定义报警通知输出等级方式经纬度与类型/);
  assert.match(manscdp, /fn 自定义报警不带类型时不输出info/);
});

test("上报路径与复位语义集中在模拟器", () => {
  assert.match(device, /pub async fn report_alarm_report/);
  assert.match(device, /pub fn reset_alarm/);
  assert.match(device, /pub async fn has_alarm_subscription/);
  // 旧入口必须委托新路径，避免两条分支各自演化。
  assert.match(device, /let report = AlarmReport::video\(description\);/);
});
