import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const profileSwitcher = source.match(/<div class="sip-profile-switcher">([\s\S]*?)\n\s*<\/div>/)?.[1];
const beginEdit = source.match(/function beginEdit\(\) \{([\s\S]*?)\n\}/)?.[1];
const saveConfig = source.match(/function saveConfig\(\) \{([\s\S]*?)\n\}/)?.[1];

assert.ok(profileSwitcher, "应存在 SIP 配置切换区域");
assert.match(profileSwitcher, /<n-input[\s\S]*v-if="editing"[\s\S]*v-model:value="draft\.name"/, "编辑状态应在原下拉框位置直接修改配置名称");
assert.match(profileSwitcher, /<n-select[\s\S]*v-else[\s\S]*:value="activeId"/, "非编辑状态应在同一位置显示配置下拉框");
assert.equal(source.match(/配置名称/g)?.length, 1, "配置表单不应重复显示配置名称字段");
assert.ok(beginEdit, "应提供明确的编辑配置入口");
assert.match(beginEdit, /resetDraft\(\);[\s\S]*editing\.value = true;/, "进入编辑时应载入当前配置名称");
assert.match(source, /@click="beginEdit">编辑配置<\/n-button>/, "按钮文案应明确说明可编辑配置");
assert.match(saveConfig ?? "", /name:\s*draft\.name\.trim\(\)/, "保存时应持久化修改后的名称");
assert.match(source, /label:\s*profile\.name/, "保存后的名称应立即刷新到配置下拉框");
