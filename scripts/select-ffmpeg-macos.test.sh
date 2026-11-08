#!/usr/bin/env bash
# 发布包 FFmpeg 选择逻辑的防回归测试：用桩脚本模拟不同能力的 ffmpeg，
# 不依赖构建机是否真的装了带 libfreetype 的 FFmpeg。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
. "$SCRIPT_DIR/select-ffmpeg-macos.sh"

REAL_HOME="$HOME"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/uvp-ffmpeg-select.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT
STUB_DIR="$TMP_DIR/path-bin"
mkdir -p "$STUB_DIR"
SAFE_PATH="$STUB_DIR:/usr/bin:/bin:/usr/sbin:/sbin"

fail() {
  echo "[ffmpeg-select][FAIL] $*" >&2
  exit 1
}

pass() {
  echo "[ffmpeg-select][PASS] $*"
}

# 桩：模拟 `ffmpeg -hide_banner -filters` 的输出。kind=osd 时多输出 drawtext。
write_stub() {
  local path="$1" kind="$2"
  mkdir -p "$(dirname "$path")"
  cat >"$path" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' 'Filters:' '  T.. = Timeline support' \
  ' T. drawbox           V->V       Draw a colored box on the input video.'
STUB
  if [ "$kind" = osd ]; then
    cat >>"$path" <<'STUB'
printf '%s\n' ' T. drawtext          V->V       Draw text with libfreetype.'
STUB
  fi
  chmod +x "$path"
}

expect_selected() {
  local label="$1" expected="$2" actual
  actual="$(uvp_find_ffmpeg)" || fail "$label：uvp_find_ffmpeg 意外失败"
  [ "$actual" = "$expected" ] || fail "$label：期望 $expected，实际 $actual"
  pass "$label"
}

expect_probe() {
  local label="$1" ffmpeg_bin="$2" expected="$3" actual
  actual="$(uvp_find_ffprobe "$ffmpeg_bin")" || fail "$label：uvp_find_ffprobe 意外失败"
  [ "$actual" = "$expected" ] || fail "$label：期望 $expected，实际 $actual"
  pass "$label"
}

write_stub "$STUB_DIR/ffmpeg" plain
write_stub "$STUB_DIR/ffprobe" plain

echo "[ffmpeg-select] T1 优先选带 OSD 能力的 ffmpeg，而不是 PATH 里第一个"
(
  HOME="$TMP_DIR/home1"
  write_stub "$HOME/.local/bin/ffmpeg-osd" osd
  PATH="$SAFE_PATH"
  unset FFMPEG_BIN
  expect_selected "T1" "$HOME/.local/bin/ffmpeg-osd"
)

echo "[ffmpeg-select] T2 显式 FFMPEG_BIN 优先级最高，仍然尊重调用方"
(
  HOME="$TMP_DIR/home2"
  write_stub "$HOME/.local/bin/ffmpeg-osd" osd
  PATH="$SAFE_PATH"
  FFMPEG_BIN="$STUB_DIR/ffmpeg"
  expect_selected "T2" "$STUB_DIR/ffmpeg"
)

echo "[ffmpeg-select] T3 全都缺 OSD 时退回第一个可用的，不阻断其它构建机"
(
  HOME="$TMP_DIR/home3"
  write_stub "$HOME/.local/bin/ffmpeg-osd" plain
  PATH="$SAFE_PATH"
  unset FFMPEG_BIN
  expect_selected "T3" "$HOME/.local/bin/ffmpeg-osd"
)

echo "[ffmpeg-select] T4 FFMPEG_BIN 指向不存在的路径时不静默改用别的 ffmpeg"
(
  HOME="$TMP_DIR/home4"
  write_stub "$HOME/.local/bin/ffmpeg-osd" osd
  PATH="$SAFE_PATH"
  FFMPEG_BIN="$TMP_DIR/does-not-exist"
  if uvp_find_ffmpeg >/dev/null 2>&1; then
    fail "T4：指定了不存在的 FFMPEG_BIN 却仍然返回了别的 ffmpeg"
  fi
  pass "T4"
)

echo "[ffmpeg-select] T5 OSD 能力判定与 REQUIRE_OSD_FFMPEG 强制失败"
write_stub "$TMP_DIR/osd-ffmpeg" osd
write_stub "$TMP_DIR/plain-ffmpeg" plain
ffmpeg_has_osd "$TMP_DIR/osd-ffmpeg" || fail "T5：带 drawtext/drawbox 的 ffmpeg 被误判为不支持 OSD"
if ffmpeg_has_osd "$TMP_DIR/plain-ffmpeg"; then
  fail "T5：只有 drawbox 的 ffmpeg 被误判为支持 OSD"
fi
if ffmpeg_has_osd "$TMP_DIR/no-such-ffmpeg"; then
  fail "T5：不存在的 ffmpeg 被误判为支持 OSD"
fi
(
  unset REQUIRE_OSD_FFMPEG
  if ! uvp_check_osd_capability "$TMP_DIR/plain-ffmpeg" 2>"$TMP_DIR/warn.txt"; then
    fail "T5：未开启强制要求时不应失败"
  fi
  grep -q "OSD" "$TMP_DIR/warn.txt" || fail "T5：缺能力时没有给出告警"
)
(
  REQUIRE_OSD_FFMPEG=1
  if uvp_check_osd_capability "$TMP_DIR/plain-ffmpeg" >/dev/null 2>&1; then
    fail "T5：REQUIRE_OSD_FFMPEG=1 时缺能力却放行了"
  fi
  uvp_check_osd_capability "$TMP_DIR/osd-ffmpeg" >/dev/null 2>&1 \
    || fail "T5：REQUIRE_OSD_FFMPEG=1 时具备能力却被拦下"
)
pass "T5"

echo "[ffmpeg-select] T6 ffprobe 与 ffmpeg 配套（含 -osd 后缀与显式覆盖）"
write_stub "$STUB_DIR/ffmpeg-osd" osd
write_stub "$STUB_DIR/ffprobe-osd" plain
(
  unset FFPROBE_BIN
  PATH="$SAFE_PATH"
  expect_probe "T6a ffmpeg-osd -> ffprobe-osd" "$STUB_DIR/ffmpeg-osd" "$STUB_DIR/ffprobe-osd"
  expect_probe "T6b ffmpeg -> ffprobe" "$STUB_DIR/ffmpeg" "$STUB_DIR/ffprobe"
)
(
  FFPROBE_BIN="$TMP_DIR/custom-ffprobe"
  write_stub "$FFPROBE_BIN" plain
  PATH="$SAFE_PATH"
  expect_probe "T6c 显式 FFPROBE_BIN" "$STUB_DIR/ffmpeg-osd" "$FFPROBE_BIN"
)
pass "T6"

echo "[ffmpeg-select] T7 语法检查"
for script in \
  "$SCRIPT_DIR/select-ffmpeg-macos.sh" \
  "$SCRIPT_DIR/bundle-ffmpeg-macos.sh" \
  "$SCRIPT_DIR/../apps/desktop/build-macos-dmg.sh"; do
  bash -n "$script" || fail "T7：$script 语法错误"
done
grep -q 'uvp_find_ffmpeg' "$SCRIPT_DIR/../apps/desktop/build-macos-dmg.sh" \
  || fail "T7：build-macos-dmg.sh 没有使用共享选择逻辑"
pass "T7"

echo "[ffmpeg-select] T8 真机校验（本机装了独立 FFmpeg 时才跑）"
if [ -x "$REAL_HOME/.local/bin/ffmpeg-osd" ]; then
  ffmpeg_has_osd "$REAL_HOME/.local/bin/ffmpeg-osd" \
    || fail "T8：本机 ffmpeg-osd 实际不具备 OSD 能力"
  (
    HOME="$REAL_HOME"
    unset FFMPEG_BIN
    actual="$(uvp_find_ffmpeg)"
    ffmpeg_has_osd "$actual" || fail "T8：默认选中的 $actual 不具备 OSD 能力"
    pass "T8 默认选中 $actual"
  )
else
  echo "[ffmpeg-select][SKIP] T8：本机没有 ~/.local/bin/ffmpeg-osd，跳过真机校验"
fi

echo "[ffmpeg-select] 全部通过"
