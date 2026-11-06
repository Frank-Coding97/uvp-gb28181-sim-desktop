import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const [view, router, app, backend, config, crate] = await Promise.all([
  readFile(new URL("./RecordPlan.vue", import.meta.url), "utf8"),
  readFile(new URL("../main.ts", import.meta.url), "utf8"),
  readFile(new URL("../App.vue", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/lib.rs", import.meta.url), "utf8"),
  readFile(new URL("../../src-tauri/src/config_store.rs", import.meta.url), "utf8"),
  readFile(new URL("../../../../crates/record-plan/src/lib.rs", import.meta.url), "utf8"),
]);

test("录像计划页具备开关、周选择、30 分钟网格与秒级编辑", () => {
  assert.match(view, /启用定时录像/);
  assert.match(view, /WEEKDAY_LABELS/);
  assert.match(view, /toggle\(block - 1\)/);
  assert.match(view, /formatHms\(segment\.start_sec\)/);
  assert.match(view, /parseHms\(text\)/);
  assert.match(view, /复制到全周/);
});

test("录像计划页显示运行时判定与调度状态", () => {
  assert.match(view, /在计划时段内/);
  assert.match(view, /调度任务/);
  assert.match(view, /10s 判定一次/);
  assert.match(view, /getRecordPlan/);
  assert.match(view, /saveRecordPlan/);
  assert.match(view, /今日计划/);
});

test("录像计划拥有一级菜单与独立路由", () => {
  assert.match(app, /label: "录像计划"[\s\S]{0,80}key: "\/record-plan"/);
  assert.match(router, /path: "\/record-plan"[\s\S]{0,80}component: RecordPlan/);
});

test("后端注册计划命令并启动 10s 调度", () => {
  assert.match(backend, /get_record_plan,/);
  assert.match(backend, /save_record_plan,/);
  assert.match(backend, /async fn record_plan_loop/);
  assert.match(backend, /Duration::from_secs\(10\)/);
  assert.match(backend, /start_plan_scheduler/);
  assert.match(backend, /stop_plan_scheduler/);
  // 停止设备必须先取消调度，再收尾录像。
  const stopIndex = backend.indexOf("state.stop_plan_scheduler().await;");
  const takeIndex = backend.indexOf("match guard.take() {", stopIndex);
  assert.ok(stopIndex > 0 && takeIndex > stopIndex, "停止顺序应为：先取消调度再收尾");
});

test("调度只停自己起的录像，不打断手动录像", () => {
  assert.match(backend, /let mut plan_owned = false;/);
  assert.match(backend, /plan_owned = true;/);
  assert.match(backend, /Some\(RecordingPhase::Recording\) if !in_window && plan_owned/);
  assert.match(backend, /RecordingKind::Time, RecordingSource::Local/);
});

test("计划持久化且默认关闭（升级不改变既有行为）", () => {
  assert.match(config, /pub struct RecordPlanSettings/);
  assert.match(config, /pub fn save_record_plan/);
  assert.match(config, /self\.record_plan\.validate\(\)\?/);
  // Default 派生 → enabled=false，避免升级后突然开始自动录像。
  assert.match(config, /#\[derive\(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default\)\]\s*pub struct RecordPlanSettings/);
});

test("ConfigDownload 回读与设备实际行为对齐", () => {
  assert.match(backend, /set_record_plan_enabled/);
  assert.match(backend, /fn plan_effective/);
});

test("计划模型单测覆盖合并、切分、边界与周几换算", () => {
  assert.match(crate, /fn adjacent_segments_merge/);
  assert.match(crate, /fn subtract_splits_segment/);
  assert.match(crate, /fn toggle_block_paints_and_erases/);
  assert.match(crate, /fn weekday_mapping_matches_calendar/);
  assert.match(crate, /fn validates_day_count_and_segment_cap/);
  assert.match(crate, /fn rejects_overlapping_raw_segments/);
});
