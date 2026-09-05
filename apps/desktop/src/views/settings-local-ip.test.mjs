import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const settings = await readFile(new URL("./Settings.vue", import.meta.url), "utf8");
const platform = await readFile(new URL("../platform.ts", import.meta.url), "utf8");
const backend = await readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8");

assert.match(platform, /listLocalIpAddresses/, "前端平台层必须暴露本机 IP 枚举命令");
assert.match(platform, /invoke<LocalIpOption\[]>\("list_local_ip_addresses"\)/, "本机 IP 必须由 Rust 枚举而非浏览器猜测");
assert.match(settings, /<select[^>]*v-model="networkDraft\.bind_address"/, "指定本机 IP 必须使用下拉框");
assert.match(settings, /localIpOptions/, "下拉框必须绑定 Rust 返回的本机 IP 列表");
assert.doesNotMatch(settings, /placeholder="例如 192\.168\.1\.20"/, "不得继续显示自由输入 IP 的文本框");
assert.match(backend, /fn list_local_ip_addresses\(\)/, "Rust 必须注册本机 IP 枚举命令");
assert.match(backend, /if_addrs::get_if_addrs/, "Rust 必须从系统网卡读取地址");
