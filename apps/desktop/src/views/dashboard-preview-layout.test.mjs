import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./Dashboard.vue", import.meta.url), "utf8");
const previewSlotStyles = source.match(/^\.preview-slot \{([^}]*)\}/m)?.[1];
const previewStageStyles = source.match(/^\.preview-stage \{([^}]*)\}/m)?.[1];
const previewCanvasStyles = source.match(/^\.preview-stage > img, \.preview-stage > canvas \{([^}]*)\}/m)?.[1];

assert.ok(previewSlotStyles, "应存在约束预览舞台的比例容器");
assert.match(
  previewSlotStyles,
  /container-type:\s*size;/,
  "比例容器必须同时提供可用宽度和高度",
);
assert.match(
  previewSlotStyles,
  /place-items:\s*center;/,
  "预览舞台应在比例容器中居中",
);
assert.ok(previewStageStyles, "应存在首页预览舞台样式");
assert.match(
  previewStageStyles,
  /aspect-ratio:\s*16\s*\/\s*9;/,
  "首页预览舞台必须保持 16:9",
);
assert.match(
  previewStageStyles,
  /width:\s*min\(100cqw,\s*calc\(100cqh\s*\*\s*16\s*\/\s*9\)\);/,
  "预览舞台宽度必须同时受容器宽度和高度约束",
);
assert.doesNotMatch(
  previewStageStyles,
  /min-height:/,
  "固定最小高度会破坏矮窗口下的 16:9 自适应",
);
assert.ok(previewCanvasStyles, "应存在首页预览画布样式");
assert.match(
  previewCanvasStyles,
  /object-fit:\s*contain;/,
  "首页预览必须完整显示画面，不能用裁剪铺满代替比例修复",
);
assert.doesNotMatch(
  source,
  /1280x720\s*·\s*H\.264/,
  "首页不应把推流分辨率硬编码成预览分辨率",
);
assert.match(
  source,
  /本地预览.*previewWidth.*previewHeight/s,
  "首页收到画面后应显示真实本地预览尺寸",
);
