#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CHECK_SCRIPT="$SCRIPT_DIR/check-ffmpeg-capabilities.sh"
BUNDLE_SCRIPT="$SCRIPT_DIR/bundle-ffmpeg-macos.sh"
FFMPEG_BIN="${FFMPEG_BIN:-$(command -v ffmpeg || true)}"
FFPROBE_BIN="${FFPROBE_BIN:-$(command -v ffprobe || true)}"

fail() {
  echo "[T01][FAIL] $*" >&2
  exit 1
}

if [[ -z "$FFMPEG_BIN" || ! -x "$FFMPEG_BIN" ]]; then
  echo "[T01][SKIP] 未找到可执行 ffmpeg；没有把缺失环境标记为通过。" >&2
  exit 2
fi
if [[ -z "$FFPROBE_BIN" || ! -x "$FFPROBE_BIN" ]]; then
  echo "[T01][SKIP] 未找到可执行 ffprobe；没有把缺失环境标记为通过。" >&2
  exit 2
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/uvp-t01-probe.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

REPORT="$TMP_DIR/capabilities.json"
echo "[T01] probe: $FFMPEG_BIN"
REQUIRE_HW_ACCEL=1 FFPROBE_BIN="$FFPROBE_BIN" "$CHECK_SCRIPT" "$FFMPEG_BIN" "$REPORT" videotoolbox >"$TMP_DIR/probe.stdout"

python3 - "$REPORT" <<'PY'
import json
import pathlib
import sys

report_path = pathlib.Path(sys.argv[1])
report = json.loads(report_path.read_text(encoding="utf-8"))
assert report["schema_version"] >= 2, report
assert report["ffmpeg"]["path"]
assert report["ffmpeg"]["version"].startswith("ffmpeg version"), report
ffprobe = report["ffprobe"]
assert ffprobe["available"] is True, report
assert pathlib.Path(ffprobe["path"]).is_file(), report
assert ffprobe["version"].startswith("ffprobe version"), report
assert report["expected_backend"] == "videotoolbox", report
assert report["expected_backend_present"] is True, report
assert report["probe_input"]["devices_accessed"] is False, report

probes = report["probes"]
expected = {
    "h264": {"codec": "h264", "kind": "video"},
    "h265": {"codec": "hevc", "kind": "video"},
    "g711a": {"codec": "pcm_alaw", "sample_rate": 8000},
    "g711u": {"codec": "pcm_mulaw", "sample_rate": 8000},
    "aac_8k": {"codec": "aac", "sample_rate": 8000},
    "aac_16k": {"codec": "aac", "sample_rate": 16000},
}
assert set(probes) >= set(expected), probes
for name, fields in expected.items():
    probe = probes[name]
    assert probe["status"] == "passed", (name, probe)
    assert probe["encoded"] is True and probe["decoded"] is True, (name, probe)
    assert probe["ffprobe"]["codec_name"] == fields["codec"], (name, probe)
    if "sample_rate" in fields:
        assert int(probe["ffprobe"]["sample_rate"]) == fields["sample_rate"], (name, probe)
    if "kind" in fields:
        assert probe["ffprobe"]["codec_type"] == fields["kind"], (name, probe)
    assert probe["output_sha256"], (name, probe)
PY

echo "[T01] missing ffprobe must fail"
if FFPROBE_BIN="$TMP_DIR/missing-ffprobe" "$CHECK_SCRIPT" "$FFMPEG_BIN" "$TMP_DIR/missing.json" >"$TMP_DIR/missing.stdout" 2>"$TMP_DIR/missing.stderr"; then
  fail "ffprobe 缺失时探针返回成功"
fi
grep -q "ffprobe" "$TMP_DIR/missing.stderr" || fail "缺失 ffprobe 未给出明确错误"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[T01][SKIP] 当前不是 macOS，未执行签名打包夹具；系统包验证仍需在 macOS 执行。" >&2
  exit 2
fi

APP="$TMP_DIR/UVP Test.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp /usr/bin/true "$APP/Contents/MacOS/UVP Test"
cat >"$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>UVP Test</string>
<key>CFBundleIdentifier</key><string>com.uvp.test.t01</string>
<key>CFBundleName</key><string>UVP Test</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.0.0</string>
<key>CFBundleVersion</key><string>0.0.0</string>
</dict></plist>
PLIST

echo "[T01] bundle ffmpeg + ffprobe into temporary app"
FFPROBE_BIN="$FFPROBE_BIN" "$BUNDLE_SCRIPT" "$APP" - "$FFMPEG_BIN" "$FFPROBE_BIN" >"$TMP_DIR/bundle.stdout"

echo "[T01] probe bundled tools with the same synthetic media matrix"
FFPROBE_BIN="$APP/Contents/MacOS/ffprobe" "$CHECK_SCRIPT" \
  "$APP/Contents/MacOS/ffmpeg" "$TMP_DIR/bundled-capabilities.json" videotoolbox >"$TMP_DIR/bundled-probe.stdout"
python3 - "$TMP_DIR/bundled-capabilities.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    report = json.load(handle)
assert report["overall_status"] == "passed", report
assert all(item["encoded"] and item["decoded"] for item in report["probes"].values()), report
PY

for tool in ffmpeg ffprobe; do
  bundled="$APP/Contents/MacOS/$tool"
  [[ -x "$bundled" ]] || fail "包内缺少可执行 $tool"
  "$bundled" -version >"$TMP_DIR/$tool.version"
  grep -q "^$tool version" "$TMP_DIR/$tool.version" || fail "包内 $tool 无法运行"
  codesign --verify --strict --verbose "$bundled" >"$TMP_DIR/$tool.codesign" 2>&1 || fail "包内 $tool 签名校验失败"
  otool -L "$bundled" | grep -E '^\s+/opt/homebrew/|^\s+/usr/local/' && fail "包内 $tool 仍引用开发机绝对依赖"
done
codesign --verify --deep --strict --verbose "$APP" >"$TMP_DIR/app.codesign" 2>&1 || fail "临时 app 深度签名校验失败"
codesign -d --entitlements :- "$APP" >"$TMP_DIR/app.entitlements.plist" 2>/dev/null
[[ "$(/usr/libexec/PlistBuddy -c 'Print :com.apple.security.device.camera' "$TMP_DIR/app.entitlements.plist")" = true ]] || fail "主 app 重签丢失摄像头权限"

echo "[T01][PASS] capability probe and macOS bundle probe"
