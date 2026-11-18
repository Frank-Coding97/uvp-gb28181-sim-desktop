import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const source = await readFile(new URL("./App.vue", import.meta.url), "utf8");

// 活动标识必须跨页面常驻：顶栏由 App.vue 拥有，不能只在某个页面里显示。
assert.match(source, /const recordingActive = ref\(false\)/, "顶栏应持有录像活动标识");
assert.match(source, /invoke<\{ phase: RecordingPhase \}>\("get_recording_state"\)/, "录像标识应读后端真实状态，不做前端假态");
assert.match(source, /\["starting", "recording", "finalizing"\]\.includes\(state\.phase\)/, "录像起始/等待关键帧/收尾都算活动中");
assert.match(source, /const platformWatching = computed\(\(\) => deviceState\.value === "InCall"\)/, "平台观看必须复用 InCall 这一唯一真源");
assert.doesNotMatch(source, /listen<[^>]*>\("(viewer|watching)_state"/, "不应为顶栏另建观看状态事件通道");

assert.match(source, /if \(recordingActive\.value\)[\s\S]*?key: "recording"[\s\S]*?value: "REC"[\s\S]*?hint: "正在录像"/, "录像中应生成 REC 芯片并带中文语义");
assert.match(source, /if \(platformWatching\.value\)[\s\S]*?key: "watching"[\s\S]*?value: "LIVE"[\s\S]*?hint: "平台正在观看"/, "平台观看应生成 LIVE 芯片并带中文语义");
assert.match(source, /v-for="c in statusChips"/, "顶栏应渲染状态芯片列表");
assert.match(source, /class="chip-dot" :class="\{ pulse: c\.pulse \}"/, "仅活动芯片带闪烁点");
assert.match(source, /:aria-label="c\.hint \?\? c\.label \+ '：' \+ c\.value"/, "REC/LIVE 要给读屏软件补中文语义");
assert.match(source, /@media \(prefers-reduced-motion: reduce\) \{\s*\.chip-dot\.pulse \{ animation: none; \}/, "跟随系统关闭闪烁动画");

assert.match(source, /recordingTimer = window\.setInterval/, "录像状态需要定时回读");
assert.match(source, /if \(recordingTimer\) clearInterval\(recordingTimer\)/, "卸载时要清掉定时器");
