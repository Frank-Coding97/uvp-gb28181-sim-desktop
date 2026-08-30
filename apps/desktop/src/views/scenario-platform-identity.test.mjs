import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Scenario.vue", import.meta.url), "utf8");
const validator = source.match(/function validateForm\(\): string \| null \{([\s\S]*?)\n\}/)?.[1] ?? "";
const builder = source.match(/function buildScenarioToml\(([\s\S]*?)\n\}/)?.[0] ?? "";

assert.match(validator, /passwordFor\(p\.id\)/, "启动前必须检查当前会话密码");
assert.match(validator, /请输入本次注册密码/, "空密码应给出明确提示");
assert.match(validator, /transport\.toUpperCase\(\) !== "UDP"/, "TCP 档案必须 fail-closed");
assert.match(builder, /server_id =/, "新 TOML 必须输出平台 ID");
assert.match(builder, /profile\.server_id/, "平台 ID 应取活动档案原值");
assert.match(builder, /server_domain =/, "新 TOML 必须独立输出平台域");
assert.match(builder, /profile\.server_domain/, "平台域不得再由平台 ID 覆盖");
assert.match(builder, /password/, "纯生成函数应显式消费会话密码参数");
assert.doesNotMatch(builder, /activePlatform|passwordFor|form\.value/, "TOML 生成函数不应读取组件隐式状态");
assert.match(source, /await invoke\("validate_scenario", \{ toml \}\)/, "启动前仍须走后端兼容校验");
