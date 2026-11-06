import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, filter, backend, thumb, conf] = await Promise.all([
  readFile(new URL("./Recordings.vue", import.meta.url), "utf8"),
  readFile(new URL("../recording-filter.mjs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/media-rtp/src/thumbnail.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
]);

test("录像列表按时间范围筛选，且跨夜录像按重叠命中", () => {
  assert.match(view, /rangeOptions/);
  assert.match(view, /activeRange/);
  assert.match(view, /filterRecordings\(recordings\.value/);
  // 重叠判定而非"开始时间落在窗内"，否则跨夜录像会被藏起来。
  assert.match(filter, /export function overlapsRange/);
  assert.match(filter, /if \(from !== null && end <= from\) return false;/);
  assert.match(filter, /export function quickRange/);
  assert.match(filter, /export function customRange/);
});

test("自定义时间范围用 datetime-local 输入且结束时间含当分钟", () => {
  assert.match(view, /v-model="customFrom" type="datetime-local"/);
  assert.match(view, /v-model="customTo" type="datetime-local"/);
  assert.match(filter, /if \(to !== null\) to \+= 60_000;/);
  // 升降序填反了要能用，不能筛出空列表。
  assert.match(filter, /if \(from !== null && to !== null && from > to\) \[from, to\] = \[to, from\];/);
});

test("切到自定义时用今天预填，避免空输入框让人以为坏了", () => {
  assert.match(view, /watch\(rangeKey, \(value\) => \{/);
  assert.match(view, /customFrom\.value = formatLocalInput\(today\.from\);/);
});

test("录像行显示缩略图，失败时给占位与原因", () => {
  assert.match(view, /class="recording-thumb"/);
  assert.match(view, /v-if="thumbs\[entry\.id\]"[\s\S]{0,120}loading="lazy"/);
  assert.match(view, /get_recording_thumbnail/);
  // 失败要能在悬停时看到原因，而不是永远一个灰框。
  assert.match(view, /:title="thumbErrors\[entry\.id\] \|\| entry\.path"/);
  assert.match(view, /thumbErrors\[entry\.id\] = String\(error\)/);
});

test("抽帧按需且串行，每段只请求一次", () => {
  assert.match(view, /const THUMB_LIMIT = 80;/);
  assert.match(view, /requestedThumbs\.has\(entry\.id\)/);
  assert.match(view, /thumbQueue = thumbQueue\.then\(async \(\) => \{/);
  // 删除录像要清掉缓存状态，否则 data URL 指向已删文件。
  assert.match(view, /requestedThumbs\.delete\(entry\.id\);/);
});

test("后端缩略图命令校验 ID 并缓存到数据目录", () => {
  assert.match(backend, /async fn get_recording_thumbnail/);
  assert.match(backend, /get_recording_thumbnail,/);
  assert.match(backend, /media_rtp::thumbnail::is_valid_id\(&id\)/);
  assert.match(backend, /media_rtp::thumbnail::THUMB_DIR/);
  // 缓存比源文件新才复用：录像被重写时要重新抽。
  assert.match(backend, /cached\.modified\(\)\? >= origin\.modified\(\)\?/);
  assert.match(backend, /data:image\/jpeg;base64/);
});

test("抽帧参数把定位放在输入之前，且挡住路径穿越", () => {
  assert.match(thumb, /pub fn extract_args/);
  assert.match(thumb, /pub fn cache_path/);
  // `-ss` 在 `-i` 之前才是关键帧级快速定位。
  assert.match(thumb, /fn 参数把定位放在输入之前且只要一帧/);
  assert.match(thumb, /fn 非法id被拒绝以阻断路径穿越/);
  assert.match(thumb, /pub fn seek_secs_for/);
});

test("缩略图走 data URL，不需要放宽 asset 协议作用域", () => {
  // 用 base64 而不是 convertFileSrc：数据根目录可能被 UVP_CONFIG_DIR 挪到作用域外，
  // 一旦改成 asset 协议，那些环境里缩略图会全部加载失败。
  assert.doesNotMatch(view, /convertFileSrc\([^)]*thumb/i);
  assert.match(conf, /"scope": \["\$APPDATA\/recordings\/\*\*\/\*"\]/);
});

test("时长列与播放面板一致", () => {
  assert.match(view, /formatDuration\(durationSeconds\(entry\)\)/);
  assert.match(view, /<dt>时长<\/dt>/);
});
