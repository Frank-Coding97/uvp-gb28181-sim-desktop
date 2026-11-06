import assert from "node:assert/strict";
import { test } from "node:test";
import {
  defaultOsdSettings,
  formatOsdTime,
  normalizeOsdSettings,
  osdPixelSize,
  osdSettingsEqual,
  validateOsdSettings,
} from "./osd-settings.ts";

test("默认值与移动端一致：时间、通道名开，水印关", () => {
  const value = defaultOsdSettings();
  assert.equal(value.timestamp.enabled, true);
  assert.equal(value.channel_name.enabled, true);
  assert.equal(value.watermark.enabled, false);
  assert.equal(value.timestamp.fill_color, "#FFFFFF");
  assert.equal(validateOsdSettings(value), null);
});

test("旧配置缺字段时补齐，非法值回落默认", () => {
  assert.deepEqual(normalizeOsdSettings(undefined), defaultOsdSettings());
  const value = normalizeOsdSettings({ watermark: { enabled: true, text: "测试", size: "huge", fill_color: "red" } });
  assert.equal(value.watermark.enabled, true);
  assert.equal(value.watermark.size, "medium");
  assert.equal(value.watermark.fill_color, "#FFFFFF");
  assert.equal(value.timestamp.enabled, true);
});

test("校验颜色、长度与水印文字", () => {
  const value = defaultOsdSettings();
  value.timestamp.outline_color = "#00";
  assert.match(validateOsdSettings(value), /时间描边色/);
  const long = defaultOsdSettings();
  long.watermark.text = "字".repeat(33);
  assert.match(validateOsdSettings(long), /32/);
  const empty = defaultOsdSettings();
  empty.watermark.enabled = true;
  assert.match(validateOsdSettings(empty), /水印文字/);
});

test("字号比例与 Rust 一致", () => {
  assert.equal(osdPixelSize("small", 720), 25);
  assert.equal(osdPixelSize("medium", 720), 36);
  assert.equal(osdPixelSize("large", 720), 50);
  assert.equal(osdPixelSize("small", 240), 16);
});

test("时间格式与 drawtext 输出一致", () => {
  const date = new Date(2026, 10, 6, 8, 5, 3);
  assert.equal(formatOsdTime(date), "2026-11-06 08:05:03");
  assert.equal(formatOsdTime(date, true), "2026年11月06日08:05:03");
});

test("比较时忽略颜色大小写差异", () => {
  const a = defaultOsdSettings();
  const b = defaultOsdSettings();
  b.timestamp.fill_color = "#ffffff";
  assert.ok(osdSettingsEqual(a, b));
});
