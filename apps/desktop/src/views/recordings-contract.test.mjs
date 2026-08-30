import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const [page, dashboard, backend, tauriConfig] = await Promise.all([
  readFile(new URL("./Recordings.vue", import.meta.url), "utf8"),
  readFile(new URL("./Dashboard.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
]);

for (const command of ["get_recordings", "get_recording_state", "start_recording", "stop_recording", "delete_recording"]) {
  assert.match(page, new RegExp(`invoke(?:<[^>]+>)?\\(\"${command}\"`), `录像页必须调用 ${command}`);
  assert.match(backend, new RegExp(`async fn ${command}\\b`), `Tauri 必须实现 ${command}`);
}
assert.match(page, /convertFileSrc\(selected\.value\.path\)/, "本地播放 URL 必须仅由文件路径派生");
assert.match(tauriConfig, /media-src[^;]*asset:[^;]*http:\/\/asset\.localhost/, "CSP 必须允许 Tauri asset 协议播放本地录像");
const assetProtocol = JSON.parse(tauriConfig).app.security.assetProtocol;
assert.equal(assetProtocol?.enable, true, "Tauri asset 协议必须显式启用");
assert.deepEqual(assetProtocol?.scope, ["$APPDATA/recordings/**/*"], "asset 协议只允许读取录像目录");
assert.match(page, /<n-popconfirm[\s\S]*@positive-click="remove\(entry\)"/, "删除录像必须二次确认");
assert.match(page, /deviceLive\.value && !isActive\.value/, "设备未运行时必须禁止开始录像");
assert.match(dashboard, /get_recording_state/, "首页录像卡必须回读 Rust 真实状态");
assert.match(dashboard, /router\.push\('\/recordings'\)/, "首页录像卡必须跳转录像中心");
assert.doesNotMatch(dashboard, /桌面端录像能力尚未接入|<span>未就绪<\/span>/, "首页不应继续伪装录像未实现");
