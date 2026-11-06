// 信令字符集(§6.10)两端一致性契约。
// 纯函数的行为测试在 ../signaling-charset.test.mjs；这里只锁"接线没被改回去"：
// 选项必须按版本算、保存前必须归一、Rust 侧必须有同一套语义。
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const read = (relative) => readFile(new URL(relative, import.meta.url), "utf8");

const settings = await read("./Settings.vue");
const app = await read("../App.vue");
const platform = await read("../platform.ts");
const rustTypes = await read("../../../../crates/common/src/types.rs");
const builder = await read("../../../../crates/gb28181-simulator/src/builder.rs");
const libRs = await read("../../src-tauri/src/lib.rs");

// ---- 设备设置页：三项可选 + 按版本禁用 + 实际出站回显 ----

assert.match(settings, /optionsFor\(profileVersion\.value\)/, "字符集选项必须按当前国标版本计算");
assert.match(settings, /effectiveSummary\(profileEncoding\.value, profileVersion\.value\)/,
  "必须回显实际出站字符集，否则用户不知保存值与生效值的差别");
assert.match(settings, /pickVersion\('V2016'\)/, "切 2016 必须走版本切换函数（顺带纠正字符集）");
assert.match(settings, /pickVersion\('V2022'\)/, "切 2022 必须走版本切换函数");
assert.match(settings, /normalizeForVersion\(profileEncoding\.value, profileVersion\.value\)/,
  "保存前必须把字符集归一到版本默认，配置里不留无效值");
assert.match(settings, /signal.*char|charset|字符集/i, "设备设置页必须有字符集区块");
// 禁用态来自选项本身，不能在模板里另写一套版本判断（两处判断迟早不一致）。
assert.match(settings, /item\.disabled/, "不可用项必须由选项的 disabled 驱动");
assert.doesNotMatch(settings, /profileVersion === 'V2016' \? false/, "不要另写一套版本判断");

// ---- 顶栏档案弹窗：同样按版本收窄，不能只留两项 ----

assert.match(app, /optionsFor\(editForm\.value\.gb_version\)/, "档案编辑的字符集选项也要按版本计算");
assert.match(app, /pickEditVersion/, "档案编辑切版本时要纠正字符集");
assert.doesNotMatch(app, /\{ label: "UTF-8", value: "Utf8" \}/,
  "编码选项不能是写死的两项清单（会漏掉 GB2312）");

// ---- 前端类型与迁移 ----

assert.match(platform, /export type SignalingEncoding = "Gb2312" \| "Gb18030" \| "Utf8"/,
  "类型必须包含 GB2312");
assert.match(platform, /signaling_encoding: effective\(parseEncoding\(raw\.signaling_encoding\), version\)/,
  "旧配置迁移必须按版本归一，不能直接落到 GB18030");

// ---- Rust 侧：枚举、版本约束、实际生效 ----

assert.match(rustTypes, /Gb2312,/, "SignalingEncoding 必须有 GB2312 变体");
assert.match(rustTypes, /pub fn default_for\(version: GbVersion\)/, "必须有随版本变化的默认值");
assert.match(rustTypes, /pub fn is_available_for\(&self, version: GbVersion\)/, "必须有版本可选性判断");
assert.match(rustTypes, /pub fn effective\(self, version: GbVersion\)/, "必须有实际生效计算");
assert.match(rustTypes, /"GB2312" \| "GB231280" \| "GB23121980" \| "GBK"/,
  "解析要认平台侧别名，否则会出现声明名对不上的哑火");

// ---- 出站/入站编码 ----

assert.match(builder, /SignalingEncoding::Gb2312 => \{[\s\S]{0,200}encoding_rs::GBK\.encode/,
  "GB2312 出站必须用 GBK 兼容超集编码");
assert.match(builder, /declared_xml_encoding\(bytes\)\.unwrap_or\(encoding\)/,
  "入站必须按报文自身声明解码，配置只作兜底");
assert.match(builder, /fn declared_xml_encoding/, "必须有声明嗅探函数");
// 声明改写只能落在 prolog：全局首个 encoding=" 会误改正文里的同名属性。
assert.match(builder, /只在 prolog 内替换|只动 prolog/, "声明改写必须限定在 prolog 内");

// ---- 后端接线 ----

assert.match(libRs, /\.signaling_encoding\s*\.effective\(resolved\.profile\.gb_version\)/,
  "构建 DeviceConfig 时必须把字符集按有效版本归一");
