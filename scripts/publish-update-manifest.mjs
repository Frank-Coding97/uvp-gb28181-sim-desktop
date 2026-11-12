#!/usr/bin/env node
// 把「桌面端的更新清单」写进官网仓库：docs/public/upgrades/desktop/stable/latest.json
//
// Tauri updater 读的就是这份静态 JSON（endpoints 里配置的地址），字段口径：
//   version 必填、pub_date RFC3339、platforms.<os-arch>.{url,signature} 必填。
// signature 是 .sig 文件的**内容**，不是路径 —— 路径或 URL 都不认。
//
// 用法：
//   node scripts/publish-update-manifest.mjs --version 1.4.0 --dry-run
//   node scripts/publish-update-manifest.mjs --version 1.4.0 --from-release
//   node scripts/publish-update-manifest.mjs --version 1.4.0 --sig-dir ./sigs
//
// 默认从官网仓库的 releases.json 取更新说明（--notes-from 可换），
// 默认用 GitHub Release 直链（--host gitee 可切 Gitee，但那边需要手工同步附件）。
import { readFile, writeFile, mkdir, readdir } from "node:fs/promises";
import { existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";

const REPO = "uvp-gb28181-sim-desktop";
// ⛔ 官网 data/releases.json 里的产品 id 是 uvp-28181-sim-desktop，和 GitHub 仓库名不同，
//    照着仓库名去找 notes 会永远取不到（下载页有内容、更新说明却是空）。
const WEBSITE_PRODUCT = "uvp-28181-sim-desktop";
const GITHUB_OWNER = "Frank-Coding97";
const GITHUB_REPO = `${GITHUB_OWNER}/${REPO}`;
const GITEE_REPO = "Frank-Coding/uvp-gb28181-sim-desktop";
const PUB_PREFIX = "https://uvplatform.com/upgrades/desktop";

const args = process.argv.slice(2);
function arg(name, fallback = "") {
  const index = args.indexOf(`--${name}`);
  return index >= 0 ? (args[index + 1] ?? fallback) : fallback;
}
const has = (name) => args.includes(`--${name}`);

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const tauriConfPath = join(root, "apps/desktop/src-tauri/tauri.conf.json");
const tauriConf = JSON.parse(await readFile(tauriConfPath, "utf8"));

const version = (arg("version", tauriConf.version) || "").replace(/^v/, "");
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`版本号不合法：${version}`);
  process.exit(1);
}

const siteDir = resolve(
  arg("site-dir", join(homedir(), "code/uvp/uvplatform-website/docs/public/upgrades/desktop")),
);
const host = arg("host", "github");
const dryRun = has("dry-run");
const checkUrls = !has("no-check");
const sigDir = arg("sig-dir", "");
const fromRelease = has("from-release");

const siteReleases = arg("notes-from", join(homedir(), "code/uvp/uvplatform-website/data/releases.json"));
const notesFile = arg("notes-file", "");
const inlineNotes = arg("notes", "");

/** macOS 的更新包 + Windows 的 NSIS 安装包，是这个版本必须齐的两份产物。 */
const TARGETS = [
  {
    key: "darwin-aarch64",
    label: "macOS arm64",
    file: `UVP-GB28181-Desktop_${version}_aarch64.app.tar.gz`,
    sigSuffix: ".app.tar.gz.sig",
  },
  {
    key: "windows-x86_64",
    label: "Windows x64",
    // ⛔ 文件名按 productName 原样来（`UVP GB28181 Desktop`，中间是空格，不是点）。
    //    写成 UVP.GB28181.Desktop 时清单里的直链会 404，客户端只会报「检查更新失败」。
    //    2026-11-12 在 52 上实跑到 bundle/nsis 才确认这个命名。
    file: `UVP GB28181 Desktop_${version}_x64-setup.exe`,
    sigSuffix: "-setup.exe.sig",
  },
];

function downloadUrl(file) {
  const tag = `v${version}`;
  // 安装包名里有空格，URL 必须编码（%20），否则 updater 拿到的地址不可用。
  const encoded = encodeURIComponent(file);
  return host === "gitee"
    ? `https://gitee.com/${GITEE_REPO}/releases/download/${tag}/${encoded}`
    : `https://github.com/${GITHUB_REPO}/releases/download/${tag}/${encoded}`;
}

async function readNotes() {
  if (inlineNotes) return inlineNotes;
  if (notesFile) return await readFile(resolve(notesFile), "utf8");
  try {
    const data = JSON.parse(await readFile(siteReleases, "utf8"));
    const entry = (Array.isArray(data) ? data : data.releases ?? []).find(
      (item) => item.productId === WEBSITE_PRODUCT && String(item.version) === version,
    );
    if (!entry) return "";
    const lines = Array.isArray(entry.notes) && entry.notes.length > 0
      ? entry.notes
      : [entry.note].filter(Boolean);
    return lines.map((line) => `- ${String(line).replace(/^[-*]\s*/, "")}`).join("\n");
  } catch {
    return "";
  }
}

/** 本地产物目录里找 .sig：macOS 在 bundle/macos，Windows 在 bundle/nsis。 */
async function readLocalSignature(target) {
  const candidates = [
    join(root, "target/release/bundle/macos"),
    join(root, `target/x86_64-pc-windows-msvc/release/bundle/nsis`),
    join(root, `target/release/bundle/nsis`),
    sigDir ? resolve(sigDir) : "",
  ].filter(Boolean);

  for (const dir of candidates) {
    if (!existsSync(dir)) continue;
    const files = await readdir(dir);
    const match = files.find((name) => name.endsWith(target.sigSuffix));
    if (match) {
      const text = (await readFile(join(dir, match), "utf8")).trim();
      if (text) return { signature: text, source: join(dir, match) };
    }
  }
  return null;
}

/** 构建在 CI 上跑，本地没有产物时直接从 Release 附件里把 .sig 取下来。 */
async function readReleaseSignature(target) {
  const api = `https://api.github.com/repos/${GITHUB_REPO}/releases/tags/v${version}`;
  const response = await fetch(api, { headers: { Accept: "application/vnd.github+json" } });
  if (!response.ok) {
    throw new Error(`读取 GitHub Release 失败：HTTP ${response.status}（${api}）`);
  }
  const release = await response.json();
  const asset = (release.assets ?? []).find((item) => item.name.endsWith(target.sigSuffix));
  if (!asset) return null;
  const sigResponse = await fetch(asset.browser_download_url);
  if (!sigResponse.ok) throw new Error(`下载签名失败：${asset.name} HTTP ${sigResponse.status}`);
  return { signature: (await sigResponse.text()).trim(), source: asset.browser_download_url };
}

async function main() {
  const notes = (await readNotes()).trim();
  if (!notes) {
    console.warn("提示：没有取到更新说明（notes 为空），客户端「更新说明」区会显示为空。");
  }

  const platforms = {};
  const missing = [];
  for (const target of TARGETS) {
    const found = (fromRelease ? await readReleaseSignature(target) : null)
      ?? (await readLocalSignature(target));
    if (!found?.signature) {
      missing.push(`${target.label}（需要 *${target.sigSuffix}）`);
      continue;
    }
    platforms[target.key] = { signature: found.signature, url: downloadUrl(target.file) };
    console.log(`签名就绪 ${target.label}: ${found.source}`);
  }

  if (missing.length > 0) {
    console.error("缺少签名文件，未写入清单：");
    for (const item of missing) console.error(`  - ${item}`);
    console.error("先完成对应平台的发布构建（UVP_UPDATER=1），或加 --from-release 从 Release 附件读取。");
    process.exit(1);
  }

  const manifest = {
    version,
    notes,
    pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
    platforms,
  };

  if (checkUrls) {
    for (const target of TARGETS) {
      const url = platforms[target.key].url;
      const response = await fetch(url, { headers: { Range: "bytes=0-1023" }, redirect: "follow" });
      if (!response.ok && response.status !== 206) {
        console.error(`更新包直链不可用（HTTP ${response.status}）：${url}`);
        process.exit(1);
      }
      console.log(`直链可用 ${target.label}: HTTP ${response.status}`);
    }
  }

  const text = `${JSON.stringify(manifest, null, 2)}\n`;
  const stablePath = join(siteDir, "stable/latest.json");
  const releasePath = join(siteDir, `releases/${version}/latest.json`);

  console.log(`\n版本 ${version} 清单：`);
  console.log(text);

  if (dryRun) {
    console.log(`[dry-run] 将写入：\n  ${stablePath}\n  ${releasePath}`);
    return;
  }

  await mkdir(dirname(stablePath), { recursive: true });
  await writeFile(stablePath, text, "utf8");
  await mkdir(dirname(releasePath), { recursive: true });
  await writeFile(releasePath, text, "utf8");

  console.log(`已写入：\n  ${stablePath}\n  ${releasePath}`);
  // 官网仓库根目录靠 data/releases.json 往上找，别用固定层数猜：临时目录演练时会算成 /。
  let websiteRoot = siteDir;
  let foundRoot = false;
  for (let i = 0; i < 6; i += 1) {
    if (existsSync(join(websiteRoot, "data/releases.json"))) {
      foundRoot = true;
      break;
    }
    websiteRoot = dirname(websiteRoot);
  }
  console.log(`\n下一步（官网由 Gitee 流水线构建，推上去约 1 分钟生效）：`);
  if (foundRoot) {
    console.log(`  cd ${websiteRoot} && git add docs/public/upgrades && \\`);
  } else {
    console.log(`  cd <官网仓库> && git add docs/public/upgrades && \\`);
  }
  console.log(`    git commit -m "chore(desktop): 发布 v${version} 在线升级清单" && git push`);
  console.log(`\n生效后验证（判据是 JSON 里的版本号，不是 HTTP 200）：`);
  console.log(`  curl -s ${PUB_PREFIX}/stable/latest.json | node -e 'let d="";process.stdin.on("data",c=>d+=c).on("end",()=>console.log(JSON.parse(d).version))'`);
}

await main();
