#!/usr/bin/env node
// 应用版本号单源设置：一条命令把版本号同步到所有「会被读」的位置。
//
// 真源是 apps/desktop/src-tauri/tauri.conf.json 的 version —— Tauri 打包实际读它，
// 安装包/产物文件名、安装器里的版本、.app 的 Info.plist 全都来自这里。
// 其它位置都只是它的副本，不允许反向修改。
//
// 用法：
//   node scripts/set-version.mjs 1.0.0     # 改版本
//   node scripts/set-version.mjs           # 只看当前值
//
// 为什么要有这个脚本：v0.1.1/v0.1.2/v0.1.3 三个 Release 全都出现过
// 「tag 是新号、包里文件名还是旧号」——因为版本号散在 4 处，靠手改必漏。

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const TAURI_CONF = resolve(ROOT, "apps/desktop/src-tauri/tauri.conf.json");
const PKG_JSON = resolve(ROOT, "apps/desktop/package.json");
const ROOT_CARGO = resolve(ROOT, "Cargo.toml");
const ROOT_CARGO_LOCK = resolve(ROOT, "Cargo.lock");
const UI_SRC = resolve(ROOT, "apps/desktop/src");

const SEMVER = /^\d+\.\d+\.\d+$/;

const read = (path) => readFileSync(path, "utf8");
const write = (path, text) => writeFileSync(path, text, "utf8");

const current = JSON.parse(read(TAURI_CONF)).version ?? "?";
const raw = process.argv[2];

if (!raw || raw === "-h" || raw === "--help") {
  console.log(`当前版本：${current}`);
  console.log("真源：apps/desktop/src-tauri/tauri.conf.json");
  console.log(`用法：node scripts/set-version.mjs <x.y.z>

同步范围：
  - apps/desktop/src-tauri/tauri.conf.json   Tauri 打包读它：产物名 / 安装器 / Info.plist
  - apps/desktop/package.json                前端构建元数据
  - Cargo.toml [workspace.package].version   env!("CARGO_PKG_VERSION") 读它
  - Cargo.lock                               workspace 成员版本（跑 cargo metadata 刷新）

UI 里的版本号由 vite.config.ts 构建期注入 __APP_VERSION__，不用手工改。`);
  process.exit(0);
}

const next = raw.replace(/^v/, "");
if (!SEMVER.test(next)) {
  console.error(`✗ 版本号格式不对：${raw}（要 x.y.z，不带 v 前缀）`);
  process.exit(1);
}
if (next === current) {
  console.log(`= 已经是 ${next}，无需改动`);
  process.exit(0);
}

const changed = [];
const skipped = [];

// ① tauri.conf.json —— 只换顶层 version 那一行（文件里仅此一处 "version"）。
{
  const before = read(TAURI_CONF);
  const after = before.replace(/^(\s{2}"version":\s*")[^"]+(")/m, `$1${next}$2`);
  if (after !== before) {
    write(TAURI_CONF, after);
    changed.push(`apps/desktop/src-tauri/tauri.conf.json  ${current} → ${next}`);
  } else {
    skipped.push("apps/desktop/src-tauri/tauri.conf.json（没匹配到顶层 version）");
  }
}

// ② apps/desktop/package.json
{
  const before = read(PKG_JSON);
  const after = before.replace(/^(\s{2}"version":\s*")[^"]+(")/m, `$1${next}$2`);
  if (after !== before) {
    write(PKG_JSON, after);
    changed.push("apps/desktop/package.json");
  } else {
    skipped.push("apps/desktop/package.json（没匹配到 version）");
  }
}

// ③ Cargo.toml 的 [workspace.package] 段。只改这一段，别碰 [workspace.dependencies] 里的版本。
{
  const before = read(ROOT_CARGO);
  const sectionMatch = before.match(/(\[workspace\.package\][\s\S]*?)(?=\n\[|$)/);
  if (!sectionMatch) {
    skipped.push("Cargo.toml（找不到 [workspace.package]）");
  } else {
    const section = sectionMatch[1];
    const patched = section.replace(/^version\s*=\s*"[^"]+"/m, `version = "${next}"`);
    if (patched !== section) {
      write(ROOT_CARGO, before.replace(section, patched));
      changed.push("Cargo.toml [workspace.package].version");
    } else {
      skipped.push("Cargo.toml（[workspace.package] 里没有 version）");
    }
  }
}

// ④ Cargo.lock：workspace 成员版本记在里面，不刷新会和 Cargo.toml 不一致。
//    ⛔ 必须是 `cargo update --workspace`：`cargo metadata --no-deps` 只读不写，
//    lock 里的版本号会一直停在旧值（实测过）。
if (existsSync(ROOT_CARGO_LOCK)) {
  try {
    execFileSync("cargo", ["update", "--workspace", "--offline"], {
      cwd: ROOT,
      stdio: "ignore",
    });
    changed.push("Cargo.lock（已刷新 workspace 成员版本）");
  } catch {
    skipped.push("Cargo.lock（cargo update 失败，下次 cargo build 会自动补）");
  }
}

// ⑤ 收尾自检：UI 里不该再写死版本串，否则改了也白改。
const stale = [];
const walk = (dir) => {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.name === "node_modules" || entry.name === "dist") continue;
    const path = resolve(dir, entry.name);
    if (entry.isDirectory()) {
      walk(path);
      continue;
    }
    if (!/\.(vue|ts|mjs)$/.test(entry.name)) continue;
    if (read(path).includes(current)) stale.push(path.slice(ROOT.length + 1));
  }
};
if (existsSync(UI_SRC)) walk(UI_SRC);

console.log(`\n✓ 版本 ${current} → ${next}\n`);
for (const line of changed) console.log(`  改了 ${line}`);
for (const line of skipped) console.log(`  跳过 ${line}`);

if (stale.length > 0) {
  console.log(`\n⚠ 下面这些文件里还留着旧版本串 "${current}"，确认它们不是「应用版本」：`);
  for (const file of stale) console.log(`  - ${file}`);
}

console.log(`\n下一步：git diff 核对 → 提交 → git tag v${next}`);
