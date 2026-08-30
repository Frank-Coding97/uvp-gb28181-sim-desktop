import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const app = await readFile(new URL("./App.vue", import.meta.url), "utf8");
const topbarEdit = app.match(/<n-button size="small" tertiary[^>]*>编辑<\/n-button>/)?.[0] ?? "";

assert.match(app, /await loadDesktopConfig\(\)/, "应用启动必须先读取 Rust 配置真相源");
assert.match(app, /await updateProfile\(/, "顶栏保存必须等待 Rust 返回");
assert.match(app, /passwordFor\(active\.value\.id\)/, "注册密码应从持久化平台档案读取");
assert.match(app, /startDevice\(active\.value,[\s\S]*passwordFor/, "顶栏注册应传已保存的平台密码");
assert.match(app, /active\?\.transport === 'TCP'/, "顶栏 TCP 注册按钮必须禁用");
assert.match(topbarEdit, /:disabled="deviceLive"/, "设备运行中必须锁定顶栏平台编辑入口");
assert.match(app, /resetDesktopConfig\(\)/, "配置损坏错误应提供显式重置入口");
