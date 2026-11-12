// 在线升级的「接线契约」：配置、权限、脚本顺序任何一处被改坏，升级就会静默失效
// （清单读不到 / 安装被拒 / 升级后丢 ffmpeg），而这类问题只有发版当晚才会暴露。
// 所以这里把关键约定钉住，改之前先想清楚。
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (relative) => readFileSync(new URL(relative, import.meta.url), "utf8");

const tauriConf = JSON.parse(read("../src-tauri/tauri.conf.json"));
const capabilities = JSON.parse(read("../src-tauri/capabilities/default.json"));
const libRs = read("../src-tauri/src/lib.rs");
const appVue = read("./App.vue");
const settingsVue = read("./views/Settings.vue");
const macScript = read("../build-macos-dmg.sh");
const winScript = read("../build-windows.ps1");
const ci = read("../../../.github/workflows/ci.yml");

test("更新清单指向官网，且公钥是随包分发的 minisign 公钥", () => {
  const updater = tauriConf.plugins?.updater;
  assert.ok(updater, "tauri.conf.json 缺少 plugins.updater");
  assert.deepEqual(updater.endpoints, ["https://uvplatform.com/upgrades/desktop/stable/latest.json"]);
  assert.ok(String(updater.pubkey).length > 40, "公钥不能为空，否则所有更新都会被拒签");
  assert.match(String(updater.pubkey), /^[A-Za-z0-9+/=]+$/, "公钥应为 base64 字符串，不是文件路径");
});

test("updater 与重启权限必须授予主窗口，否则前端调用会被 ACL 拦下", () => {
  assert.ok(capabilities.permissions.includes("updater:default"));
  assert.ok(capabilities.permissions.includes("process:allow-restart"));
  assert.match(libRs, /tauri_plugin_updater::Builder::new\(\)\.build\(\)/);
  assert.match(libRs, /tauri_plugin_process::init\(\)/);
});

test("启动时静默检查，安装入口只在关于页手动触发", () => {
  assert.match(appVue, /checkForUpdates\(\{ silent: true \}\)/);
  assert.match(settingsVue, /installUpdate/);
  assert.match(settingsVue, /立即升级/);
  assert.match(settingsVue, /确认升级并重启/);
});

test("Windows 只出 NSIS：msi 装不了在线升级，双份包会装出两个应用", () => {
  assert.match(winScript, /--bundles 'nsis'/);
  assert.ok(!/--bundles\s+'nsis,msi'/.test(winScript), "禁止重新把 msi 加回打包列表");
  assert.match(winScript, /createUpdaterArtifacts/);
  assert.ok(!ci.includes("*.msi"), "CI 产物清单不应再包含 msi");
  assert.match(ci, /UVP_UPDATER/);
});

test("macOS 升级包必须在 FFmpeg / Swift runtime 内嵌之后才打，否则升级后没有 ffmpeg", () => {
  const ffmpegStep = macScript.indexOf("处理 FFmpeg 运行时");
  const updaterStep = macScript.indexOf("生成在线升级包");
  assert.ok(ffmpegStep > 0 && updaterStep > 0);
  assert.ok(
    updaterStep > ffmpegStep,
    "「生成在线升级包」必须排在「处理 FFmpeg 运行时」之后",
  );
  // updater 解包时会丢掉第一层目录名，归档根必须是 .app 本身。
  assert.match(macScript, /-C "\$BUNDLE_DIR" "\$\(basename "\$APP_DIR"\)"/);
  assert.match(macScript, /tauri signer sign/);
});
