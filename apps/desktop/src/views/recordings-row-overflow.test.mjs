import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("./Recordings.vue", import.meta.url), "utf8");

/** 取某个选择器的样式块内容，取不到返回空串。 */
function styleOf(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return source.match(new RegExp(`^${escaped} \\{([^}]*)\\}`, "m"))?.[1] ?? "";
}

// 回归背景：列表行原先直接渲染 ISO 原文（2026-11-18T10:13:37），右侧大小列又是 auto 宽度，
// 窗口一窄就把「时间 / 标签 / 大小 / 结束时间」挤成两行互相叠字。
// 现在：时间压短、每列可收缩、所有文字单行省略，谁都不许折行去压别人。

test("列表行不再直接渲染 ISO 时间串", () => {
  assert.match(source, /function shortTime\(iso: string\)/);
  assert.match(source, /\{\{ shortTime\(entry\.start_time\) \}\}/);
  assert.doesNotMatch(source, /<strong>\{\{ entry\.start_time \}\}<\/strong>/, "行内时间必须先压成短格式");
  assert.match(source, /function endLabel\(entry: RecordingEntry\)/);
  assert.match(source, /至 \{\{ endLabel\(entry\) \}\}/);
});

test("时间列改为定宽可收缩，不再让 auto 列抢光宽度", () => {
  const rowStyles = styleOf(".recording-row");
  assert.ok(rowStyles, "应存在列表行样式");
  assert.match(rowStyles, /grid-template-columns: 64px 34px minmax\(0, 1fr\) minmax\(0, 96px\) 34px/, "大小列须可收缩");
  assert.match(rowStyles, /overflow: hidden;/, "行内溢出一律裁掉，不允许压到相邻行");
});

test("行内文字单行省略，杜绝折行叠字", () => {
  for (const selector of [".recording-time", ".recording-sub", ".recording-meta strong", ".recording-meta small"]) {
    const styles = styleOf(selector);
    assert.ok(styles, `${selector} 应有独立样式块`);
    assert.match(styles, /white-space: nowrap;/, `${selector} 必须单行`);
    assert.match(styles, /text-overflow: ellipsis;/, `${selector} 超长须省略号`);
    assert.match(styles, /overflow: hidden;/, `${selector} 须裁掉溢出`);
  }
});

test("标签不参与收缩，避免被压成难看的窄条", () => {
  assert.match(source, /\.recording-line \.n-tag \{ flex: 0 0 auto; \}/);
});
