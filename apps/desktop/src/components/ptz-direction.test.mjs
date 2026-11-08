import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

// 云台方向通感回归：平台上按"右"，画面里的球机必须向右摆。
//
// 回归背景：三维盘面是第三视角（观察者在球机外部）。上一版按"俯视顺时针"定义 pan 正方向
// （`panGroup.rotation.y = -pan`）——俯视看确实是顺时针，但在默认机位里按"右"键镜头反而
// 向画面左侧摆，用户直连国标平台时反馈"云台方向是反的"。旧版 `ThreePtzCamera` 用的是
// `+pan`，两个组件对同一个 pan 值给出相反的画面，属于口径分裂。
//
// 本文件锁住统一口径（改前先用 three 投影复算一遍，不要凭直觉翻符号）：
//   pan 正 = 镜头朝画面右摆；tilt 正 = 抬头；
//   地面方位盘的 E 刻度落在 +X，与 pan=+90° 的朝向同向，读数和盘面不再打架。
const read = (path) => readFile(new URL(path, import.meta.url), "utf8");

const camera3d = await read("./PtzCamera3D.vue");
const legacy = await read("./ThreePtzCamera.vue");
const pad = await read("./PtzPad.vue");
const store = await read("../simulator/useSimulatorCenter.ts");
const page = await read("../views/Ptz.vue");

// ── 三维模型：pan / tilt 的符号 ────────────────────────────────────────────
assert.match(
  camera3d,
  /panGroup\.rotation\.y = THREE\.MathUtils\.degToRad\(panSpring\.value\);/,
  "pan 正必须映射到绕 +Y 正向旋转（镜头朝 +X = 画面右）",
);
assert.doesNotMatch(
  camera3d,
  /panGroup\.rotation\.y = -THREE\.MathUtils\.degToRad/,
  "不得再回到 -pan：默认机位下按键方向会与画面相反",
);
assert.match(
  camera3d,
  /tiltGroup\.rotation\.x = -THREE\.MathUtils\.degToRad\(tiltSpring\.value\);/,
  "tilt 正必须是抬头（子节点局部 +X 取负号）",
);

// ── 地面方位盘与刻度：必须跟着 pan 的口径一起镜像 ──────────────────────────
assert.match(
  camera3d,
  /return new THREE\.Vector3\(Math\.sin\(rad\) \* radius, y, Math\.cos\(rad\) \* radius\);/,
  "headingVector 的 sin 取正，否则盘面刻度会与镜头指向相差一个镜像",
);
assert.match(
  camera3d,
  /return \[c \+ Math\.sin\(rad\) \* r \* scale, c \+ Math\.cos\(rad\) \* r \* scale\];/,
  "方位盘 E 刻度需落在 +X，与 pan=+90° 同向",
);
assert.match(camera3d, /tick\.rotation\.y = THREE\.MathUtils\.degToRad\(deg\);/, "刻度朝向随盘面一起镜像");

// ── 新老两个球机组件口径一致 ───────────────────────────────────────────────
assert.match(
  legacy,
  /panGroup\.rotation\.y = THREE\.MathUtils\.degToRad\(props\.pan\);/,
  "旧版程序化球机本就按 +pan，两套模型对同一个 pan 值不能给出相反画面",
);

// ── 手操方盘：按"右"必须给出正的 dPan ─────────────────────────────────────
assert.match(pad, /\{ dPan: STEP, dTilt: 0, glyph: "→", label: "向右" \}/, "「向右」键需为 +dPan");
assert.match(pad, /\{ dPan: -STEP, dTilt: 0, glyph: "←", label: "向左" \}/, "「向左」键需为 -dPan");
assert.match(pad, /\{ dPan: 0, dTilt: STEP, glyph: "↑", label: "向上" \}/, "「向上」键需为 +dTilt");
assert.match(pad, /\{ dPan: 0, dTilt: -STEP, glyph: "↓", label: "向下" \}/, "「向下」键需为 -dTilt");

// ── 姿态积分：平台方向位 → 姿态增量 ───────────────────────────────────────
assert.match(store, /if \(ptz\.left\) pose\.pan = clamp\(pose\.pan - panStep/, "平台「左」应减小 pan");
assert.match(store, /if \(ptz\.right\) pose\.pan = clamp\(pose\.pan \+ panStep/, "平台「右」应增大 pan");
assert.match(store, /if \(ptz\.up\) pose\.tilt = clamp\(pose\.tilt \+ tiltStep/, "平台「上」应增大 tilt（抬头）");
assert.match(store, /if \(ptz\.down\) pose\.tilt = clamp\(pose\.tilt - tiltStep/, "平台「下」应减小 tilt（低头）");
assert.match(page, /if \(action\.left\) pan\.value -= panStep;/, "只读云台页的「左」同样减小 pan");
assert.match(page, /if \(action\.right\) pan\.value \+= panStep;/, "只读云台页的「右」同样增大 pan");
