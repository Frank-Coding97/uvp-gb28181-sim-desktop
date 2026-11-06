/**
 * 信令字符集(GB/T 28181 §6.10)的纯函数，与 Rust 侧 `common::SignalingEncoding` 同语义。
 *
 * 两版口径不同，不是"换个默认值"：
 *   - 2016 §6.10「SIP 信令字符集**宜**采用 GB 2312」—— 推荐项。
 *   - 2022 §6.10「SIP 信令字符集**应**采用 GB 18030」—— 由「宜」升「应」。
 *
 * 因此界面不能只给一个下拉让用户随便选：GB2312 在 2022 下是没有位置的
 * （GB18030 是它的超集，常用字字节完全一致），GB18030 在 2016 下则是不认得的声明名。
 * 选项按版本收窄，并把"实际出站"单独回显 —— 用户改完版本后能立刻看到真实生效值。
 */

/** 三个候选值的内部标识，与 Rust 枚举变体名、配置落盘值一致。 */
export const ENCODING_VALUES = ["Gb2312", "Gb18030", "Utf8"];

/** XML 声明里写的名字。注意 GB2312 声明对应的实际字节是 GBK(兼容超集)。 */
const XML_NAMES = { Gb2312: "GB2312", Gb18030: "GB18030", Utf8: "UTF-8" };

/** 该版本下 §6.10 的默认字符集。 */
export function defaultFor(version) {
  return version === "V2016" ? "Gb2312" : "Gb18030";
}

/** 该字符集在指定版本下是否可选。UTF-8 是两版共用的联调逃逸口。 */
export function isAvailableFor(encoding, version) {
  if (encoding === "Gb2312") return version === "V2016";
  if (encoding === "Gb18030") return version === "V2022";
  if (encoding === "Utf8") return true;
  return false;
}

/**
 * 实际出站字符集 = 保存值(该版本下可用时)或版本默认。
 *
 * 保存值与版本不匹配时按版本默认兜底，而不是照发：否则切到 2016 后仍会带着
 * GB18030 声明出站，而那是 2016 平台不认的编码名。兜底方向与 `defaultFor` 一致，
 * 所以"跟随版本"不需要额外开关。
 */
export function effective(stored, version) {
  return isAvailableFor(stored, version) ? stored : defaultFor(version);
}

/** 宽松解析：认平台侧常用别名，未知回退 GB18030（2022 口径）。 */
export function parseEncoding(raw) {
  const key = String(raw ?? "").toUpperCase().replace(/[-_\s]/g, "");
  if (["GB2312", "GB231280", "GB23121980", "GBK", "CP936", "MS936", "WINDOWS936"].includes(key)) {
    return "Gb2312";
  }
  if (["UTF8", "UNICODE11UTF8"].includes(key)) return "Utf8";
  return "Gb18030";
}

/** XML 声明里的编码名。 */
export function xmlName(encoding) {
  return XML_NAMES[encoding] ?? XML_NAMES.Gb18030;
}

/** 展示用的字符集名（与声明名一致，避免出现两套叫法）。 */
export function encodingLabel(encoding) {
  return xmlName(encoding);
}

/**
 * 按当前版本生成选项，不可用项 disabled 而不是隐藏。
 *
 * 隐藏会让用户以为"这版本只有两项"，禁用才能让「2016 没有 GB18030、2022 没有 GB2312」
 * 这层约束在界面上可见，而不是让用户去猜为什么选项变了。
 */
export function optionsFor(version) {
  const v2016 = version === "V2016";
  return [
    {
      value: "Gb2312",
      label: "GB2312",
      disabled: !v2016,
      hint: v2016 ? "2016 §6.10「宜」；字节实际用 GBK 兼容超集" : "2022 下由 GB18030 取代",
    },
    {
      value: "Gb18030",
      label: "GB18030",
      disabled: v2016,
      hint: v2016 ? "2016 平台不认这个声明名" : "2022 §6.10「应」；GB2312 的超集",
    },
    {
      value: "Utf8",
      label: "UTF-8",
      disabled: false,
      hint: "非国标；存量厂商常用，选它即放弃 §6.10 合规",
    },
  ];
}

/** 「实际出站」的一行回显文案。 */
export function effectiveSummary(stored, version) {
  const actual = effective(stored, version);
  const name = encodingLabel(actual);
  if (!isAvailableFor(stored, version)) {
    const why = stored === "Gb2312" ? "2022 已由 GB18030 取代" : "2016 不认 GB18030 声明名";
    return `实际出站 ${name}（保存的 ${encodingLabel(stored)} 与当前版本不符：${why}，已按版本默认兜底）`;
  }
  if (actual === "Gb2312") return "实际出站 GB2312（字节按 GBK 编码，声明仍写 GB2312）";
  if (actual === "Gb18030") return "实际出站 GB18030（§6.10 要求）";
  return "实际出站 UTF-8（非国标，仅联调兼容）";
}

/** 保存前归一：版本切换后把不匹配的保存值纠到版本默认，避免配置里长期躺着无效值。 */
export function normalizeForVersion(stored, version) {
  return effective(stored, version);
}
