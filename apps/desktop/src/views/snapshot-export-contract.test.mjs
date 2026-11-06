import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [logs, snapshots, preview, router, app, backend, platform, redact, store] =
  await Promise.all([
    readFile(new URL("./System.vue", import.meta.url), "utf8"),
    readFile(new URL("./Snapshots.vue", import.meta.url), "utf8"),
    readFile(new URL("./Preview.vue", import.meta.url), "utf8"),
    readFile(new URL("../main.ts", import.meta.url), "utf8"),
    readFile(new URL("../App.vue", import.meta.url), "utf8"),
    readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
    readFile(new URL("../platform.ts", import.meta.url), "utf8"),
    readFile(new URL("../../../../crates/common/src/log_redact.rs", import.meta.url), "utf8"),
    readFile(new URL("../../../../crates/media-rtp/src/snapshot.rs", import.meta.url), "utf8"),
  ]);

test("日志页按当前页签导出，并提示脱敏结果", () => {
  assert.match(logs, /exportLogs\(activeTab\.value, blocks, target\)/);
  assert.match(logs, /exportCurrent/);
  assert.match(logs, /脱敏 \$\{result\.redacted\} 处/);
  assert.match(logs, /Authorization/);
});

test("导出内容与筛选后可见内容一致", () => {
  // 三个页签都从 filtered* 取值，避免导出「页面上看不到的行」。
  assert.match(logs, /filteredTraces\.value\.map/);
  assert.match(logs, /filteredCommands\.value\.map/);
  assert.match(logs, /filteredLogs\.value\.map/);
  assert.match(logs, /historicalSipLogs\.value/);
});

test("脱敏在 Rust 侧完成，前端不自行拼接抹除逻辑", () => {
  assert.match(redact, /fn redact_text/);
  assert.match(redact, /Authorization: \*\*\*\*\*\*/);
  assert.match(redact, /<\/password/);
  assert.match(backend, /common::log_redact::render_export|log_export::write_export/);
  // 前端只能把原始文本递进去，不能先替换再递。
  assert.doesNotMatch(logs, /replace\([^)]*Authorization/);
});

test("抓拍取自共享媒体当前关键帧，不依赖本地预览开启", () => {
  // 预览默认关闭(media-pipeline-v2 P4),抓拍不能再从预览总线取帧。
  assert.match(backend, /current_keyframe_jpeg\(\)/);
  assert.doesNotMatch(backend, /preview_bus\s*\.latest_frame\(\)/);
  assert.match(backend, /设备尚未启动，无法抓拍/);
  assert.match(store, /pub fn save\(&self, jpeg: &\[u8\]/);
  assert.match(preview, /takeSnapshot/);
  assert.match(preview, /:disabled="capturing"/);
  assert.doesNotMatch(preview, /:disabled="!hasFrame \|\| capturing"/);
});

test("抓拍 ID 只允许单段文件名字符，拒绝路径穿越", () => {
  assert.match(store, /非法抓拍 ID/);
  assert.match(store, /b == b'-' \|\| b == b'_'/);
  assert.match(store, /fn rejects_path_traversal_ids/);
  assert.match(store, /fn same_millisecond_capture_does_not_overwrite/);
});

test("图库页面具备缩略图、另存为、删除与独立路由菜单", () => {
  assert.match(snapshots, /listSnapshots/);
  assert.match(snapshots, /readSnapshot/);
  assert.match(snapshots, /exportSnapshot/);
  assert.match(snapshots, /deleteSnapshot/);
  assert.match(snapshots, /抓拍图库/);
  assert.match(router, /path: "\/snapshots"[\s\S]{0,80}component: Snapshots/);
  assert.match(app, /label: "抓拍图库"[\s\S]{0,80}key: "\/snapshots"/);
  assert.match(platform, /export interface SnapshotMeta/);
  assert.match(platform, /export async function takeSnapshot/);
});
