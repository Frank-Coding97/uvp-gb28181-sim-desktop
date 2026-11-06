import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ENCODING_VALUES,
  defaultFor,
  effective,
  effectiveSummary,
  encodingLabel,
  isAvailableFor,
  normalizeForVersion,
  optionsFor,
  parseEncoding,
  xmlName,
} from "./signaling-charset.mjs";

test("默认字符集随国标版本走", () => {
  // 2016 §6.10「宜 GB2312」，2022 §6.10「应 GB18030」——默认值是版本的函数，不是常量。
  assert.equal(defaultFor("V2016"), "Gb2312");
  assert.equal(defaultFor("V2022"), "Gb18030");
});

test("字符集可选性按版本收窄", () => {
  assert.equal(isAvailableFor("Gb2312", "V2016"), true);
  assert.equal(isAvailableFor("Gb2312", "V2022"), false);
  assert.equal(isAvailableFor("Gb18030", "V2016"), false);
  assert.equal(isAvailableFor("Gb18030", "V2022"), true);
  // UTF-8 是两版共用的逃逸口。
  assert.equal(isAvailableFor("Utf8", "V2016"), true);
  assert.equal(isAvailableFor("Utf8", "V2022"), true);
});

test("不匹配的字符集按版本默认兜底", () => {
  assert.equal(effective("Gb18030", "V2016"), "Gb2312");
  assert.equal(effective("Gb2312", "V2022"), "Gb18030");
  assert.equal(effective("Gb2312", "V2016"), "Gb2312");
  assert.equal(effective("Utf8", "V2016"), "Utf8");
  assert.equal(effective("Utf8", "V2022"), "Utf8");
});

test("选项按版本禁用而不是隐藏", () => {
  // 隐藏会让用户以为这版本只有两项；禁用才能让版本约束在界面上可见。
  const v2016 = optionsFor("V2016");
  const v2022 = optionsFor("V2022");
  assert.equal(v2016.length, 3);
  assert.equal(v2022.length, 3);
  const pick = (list, value) => list.find((item) => item.value === value);
  assert.equal(pick(v2016, "Gb2312").disabled, false);
  assert.equal(pick(v2016, "Gb18030").disabled, true);
  assert.equal(pick(v2022, "Gb2312").disabled, true);
  assert.equal(pick(v2022, "Gb18030").disabled, false);
  assert.equal(pick(v2016, "Utf8").disabled, false);
  assert.equal(pick(v2022, "Utf8").disabled, false);
  // 每一项都要有解释，否则用户只能猜为什么某项是灰的。
  for (const item of [...v2016, ...v2022]) {
    assert.ok(item.hint && item.hint.length > 0, `${item.value} 缺少说明`);
  }
});

test("解析认得平台侧别名", () => {
  for (const label of ["GB2312", "gb2312", "GBK", "cp936", "MS936", "windows-936", "GB_2312-80"]) {
    assert.equal(parseEncoding(label), "Gb2312", `${label} 应解析为 GB2312`);
  }
  assert.equal(parseEncoding("utf8"), "Utf8");
  assert.equal(parseEncoding("UTF-8"), "Utf8");
  assert.equal(parseEncoding("GB18030"), "Gb18030");
  // 未知值回退 GB18030（2022 口径），而不是 UTF-8。
  assert.equal(parseEncoding("BIG5"), "Gb18030");
  assert.equal(parseEncoding(null), "Gb18030");
});

test("声明名与枚举名一致", () => {
  assert.equal(xmlName("Gb2312"), "GB2312");
  assert.equal(xmlName("Gb18030"), "GB18030");
  assert.equal(xmlName("Utf8"), "UTF-8");
  // 展示名复用声明名，避免界面上出现两套叫法。
  assert.equal(encodingLabel("Utf8"), xmlName("Utf8"));
  assert.deepEqual(ENCODING_VALUES, ["Gb2312", "Gb18030", "Utf8"]);
});

test("实际出站回显说明兜底原因", () => {
  // 匹配时不提兜底，只说实际编码与字节口径。
  assert.match(effectiveSummary("Gb2312", "V2016"), /实际出站 GB2312.*GBK/);
  assert.match(effectiveSummary("Gb18030", "V2022"), /实际出站 GB18030/);
  assert.match(effectiveSummary("Utf8", "V2022"), /实际出站 UTF-8.*非国标/);
  // 不匹配时必须点明"保存值与版本不符"，否则用户会以为保存没生效。
  const fallback = effectiveSummary("Gb18030", "V2016");
  assert.match(fallback, /实际出站 GB2312/);
  assert.match(fallback, /GB18030 与当前版本不符/);
  assert.match(fallback, /兜底/);
  assert.match(effectiveSummary("Gb2312", "V2022"), /2022 已由 GB18030 取代/);
});

test("保存前归一消掉无效值", () => {
  // 切版本后写回配置，避免长期躺着与版本不匹配的值。
  assert.equal(normalizeForVersion("Gb18030", "V2016"), "Gb2312");
  assert.equal(normalizeForVersion("Gb2312", "V2022"), "Gb18030");
  assert.equal(normalizeForVersion("Utf8", "V2022"), "Utf8");
});
