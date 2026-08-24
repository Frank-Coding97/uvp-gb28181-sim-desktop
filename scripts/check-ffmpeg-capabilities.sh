#!/usr/bin/env bash
set -euo pipefail

FFMPEG="${1:?用法: check-ffmpeg-capabilities.sh <ffmpeg> <report> [expected backend] }"
REPORT="${2:?用法: check-ffmpeg-capabilities.sh <ffmpeg> <report> [expected backend] }"
EXPECTED="${3:-}"

[ -x "$FFMPEG" ] || { echo "FFmpeg 不可执行: $FFMPEG" >&2; exit 1; }
HWACCELS=$("$FFMPEG" -hide_banner -hwaccels 2>&1 || true)
DECODERS=$("$FFMPEG" -hide_banner -decoders 2>&1 | grep -Ei 'h264|hevc|videotoolbox|d3d11va|dxva2' || true)
VERSION=$("$FFMPEG" -version 2>/dev/null | sed -n '1p' || true)

mkdir -p "$(dirname "$REPORT")"
python3 - "$REPORT" "$FFMPEG" "$VERSION" "$HWACCELS" "$DECODERS" "$EXPECTED" <<'PY'
import json
import platform
import sys
from datetime import datetime, timezone

report_path, ffmpeg, version, hwaccels, decoders, expected = sys.argv[1:]
accels = [line.strip() for line in hwaccels.splitlines() if line.strip() and not line.startswith("Hardware acceleration")]
report = {
    "schema_version": 1,
    "checked_at": datetime.now(timezone.utc).isoformat(),
    "host": {"os": platform.platform(), "machine": platform.machine()},
    "ffmpeg": {"path": ffmpeg, "version": version, "hardware_accelerators": accels,
               "matching_decoders": [line.strip() for line in decoders.splitlines() if line.strip()]},
    "expected_backend": expected or None,
    "expected_backend_present": not expected or any(expected.lower() in item.lower() for item in accels + decoders.splitlines()),
}
with open(report_path, "w", encoding="utf-8") as handle:
    json.dump(report, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
print(json.dumps(report, ensure_ascii=False, indent=2))
if expected and not report["expected_backend_present"] and __import__("os").environ.get("REQUIRE_HW_ACCEL") == "1":
    raise SystemExit(f"缺少要求的硬件后端: {expected}")
PY
