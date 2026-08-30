import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const handler = source.match(/async function toggleRegistration\(\) \{([\s\S]*?)\n\}/)?.[1];
const button = source.match(/<n-button[^>]*@click="toggleRegistration"[^>]*>/s)?.[0];
const configPanelStyles = source.match(/^\.config-panel \{([^}]*)\}/m)?.[1];

assert.ok(handler, "应存在首页注册/注销处理函数");
assert.match(
  handler,
  /if \(registrationBusy\.value \|\| editing\.value\) return;/,
  "编辑 SIP 配置时，处理函数也必须拒绝注册动作",
);
assert.ok(button, "应存在首页注册/注销按钮");
assert.match(
  button,
  /:disabled="registrationBusy \|\| editing \|\| \(!deviceLive && !mediaSourceReady\) \|\| \(!deviceLive && active\?\.transport === 'Tcp'\)"/,
  "编辑配置、媒体未就绪或选择 TCP 时，注册按钮必须显示为禁用",
);
assert.match(handler, /active\.value\?\.transport === "Tcp"/, "处理函数也必须 fail-closed 拒绝 TCP 注册");
assert.match(source, /effectiveConfig\.value\?\.device \?\? config\.value\?\.device/, "运行时状态卡应优先展示 Rust 有效配置");
assert.match(source, /await saveDesktopConfig\(/, "配置保存必须等待 Rust 返回");
assert.match(source, /setSessionPassword\(profileId, draft\.password\)/, "密码只能写入当前会话状态");
assert.doesNotMatch(source, /active\.value\.password/, "不得从持久化平台档案回填密码");
assert.ok(configPanelStyles, "应存在 SIP 配置面板样式");
assert.match(
  configPanelStyles,
  /overflow-x:\s*hidden;/,
  "SIP 配置面板不应允许内容横向溢出",
);
assert.match(
  configPanelStyles,
  /overflow-y:\s*auto;/,
  "窗口高度不足时，SIP 配置面板应在内部滚动而不是向下溢出",
);
