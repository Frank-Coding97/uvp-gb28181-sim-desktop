import assert from "node:assert/strict";
import test from "node:test";
import {
  formatBytes,
  normalizeVersion,
  progressPercent,
  shouldNotifyVersion,
  summarizeNotes,
  updateNotice,
} from "./updater.mjs";

test("版本号统一去掉 v 前缀，避免界面出现 vv1.4.0", () => {
  assert.equal(normalizeVersion("v1.4.0"), "1.4.0");
  assert.equal(normalizeVersion(" 1.4.0 "), "1.4.0");
  assert.equal(normalizeVersion(undefined), "");
});

test("体积按 1024 进位，0 与非法值不显示成 NaN", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(-5), "0 B");
  assert.equal(formatBytes(512), "512 B");
  assert.equal(formatBytes(1536), "1.5 KB");
  assert.equal(formatBytes(58 * 1024 * 1024), "58.0 MB");
});

test("总量未知时进度返回 null，界面退化成不确定进度而不是卡 0%", () => {
  assert.equal(progressPercent(0, 0), null);
  assert.equal(progressPercent(10, undefined), null);
  assert.equal(progressPercent(50, 100), 50);
  assert.equal(progressPercent(150, 100), 100);
  assert.equal(progressPercent(-1, 100), 0);
});

test("更新说明只取第一行有内容的文字并截断", () => {
  assert.equal(summarizeNotes("\n\n- 新增在线升级\n- 修复别的问题"), "新增在线升级");
  assert.equal(summarizeNotes(""), "");
  assert.equal(summarizeNotes("x".repeat(200), 10), "xxxxxxxxx…");
});

test("静默检查命中新版本 → 通知中心一条 info，不提供一键安装", () => {
  const notice = updateNotice("v1.4.0", "- 新增在线升级");
  assert.equal(notice.level, "info");
  assert.equal(notice.title, "发现新版本 v1.4.0");
  assert.match(notice.detail, /新增在线升级/);
  assert.match(notice.detail, /关于/);
  // 没有版本号就不该产生通知，否则会推出标题为空的记录。
  assert.equal(updateNotice(""), null);
});

test("同一版本只提示一次，升级到新版本后重新提示", () => {
  assert.equal(shouldNotifyVersion("", "1.4.0"), true);
  assert.equal(shouldNotifyVersion("1.4.0", "v1.4.0"), false);
  assert.equal(shouldNotifyVersion("1.3.0", "1.4.0"), true);
  assert.equal(shouldNotifyVersion("1.4.0", ""), false);
});
