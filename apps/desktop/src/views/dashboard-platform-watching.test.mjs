import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const watching = source.match(/const platformWatching = computed\(\(\) => ([^;]+)\);/)?.[1];
const stage = source.match(/<div class="preview-stage">([\s\S]*?)\n          <\/div>/)?.[1] ?? "";
const badge = source.match(/<div\s+v-if="platformWatching"([\s\S]*?)\n            <\/div>/)?.[0];
const badgeStyles = source.match(/^\.preview-watching \{([^}]*)\}/m)?.[1];

// 平台正在看 = 设备侧 StreamStart/StreamStop 折算出来的 InCall 状态。
// 这条只能有一个真源:再引入「观看人数」之类的事件,就会和推流生命周期打架。
assert.ok(watching, "首页应派生出平台观看状态");
assert.match(watching.trim(), /deviceState\.value === "InCall"/, "平台观看状态必须复用 device_state 的 InCall,不能另开事件真源");
assert.doesNotMatch(source, /listen<[^>]*>\("(viewer|watching)_state"/, "不应另建观看状态事件通道");

assert.ok(badge, "平台观看时应在预览画面上显示标识");
assert.ok(stage.includes("platformWatching"), "标识必须落在预览画面(.preview-stage)内部,而不是页面别处");
assert.match(badge, /role="status"/, "标识是状态变化,应对读屏软件播报");
assert.match(badge, />LIVE</, "标记视觉上只保留 LIVE 字样");
assert.match(badge, /aria-label="平台正在观看"/, "LIVE 之外必须给读屏软件补齐语义,不能只剩两个字母");

assert.ok(badgeStyles, "应存在平台观看标识样式");
assert.match(badgeStyles, /position:\s*absolute;/, "标识应压在画面上,不占布局");
assert.match(badgeStyles, /top:\s*\d+px;[\s\S]*right:\s*\d+px;/, "标识应固定在画面右上角");
assert.match(source, /\.preview-watching i \{[^}]*background:\s*#43d59d;/, "闪动点应为绿色,与画面左上角出图指示同色");
assert.match(source, /@keyframes preview-watch-pulse \{[^}]*rgba\(67,\s*213,\s*157/, "闪烁光晕用绿色,不能沿用红色告警色");
assert.match(
  source,
  /@media \(prefers-reduced-motion: reduce\) \{\s*\.preview-watching i \{ animation: none; \}/,
  "跟随系统的减少动效设置,关闭闪烁",
);
