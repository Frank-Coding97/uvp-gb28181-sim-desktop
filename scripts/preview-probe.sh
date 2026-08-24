#!/usr/bin/env bash
set -euo pipefail

# 只读收集本机预览能力；不启动摄像头、不修改应用配置、不依赖 Tauri。
# Tauri/WebCodecs 的真实结果必须由桌面窗口内的诊断命令补充，不能用普通浏览器推断。

report_path="${1:-}"
if [[ -z "$report_path" ]]; then
  report_path="preview-capabilities-$(date +%Y%m%d-%H%M%S).json"
fi

mkdir -p "$(dirname "$report_path")"

find_ffmpeg() {
  if command -v ffmpeg >/dev/null 2>&1; then
    command -v ffmpeg
    return
  fi
  for candidate in \
    /opt/homebrew/bin/ffmpeg \
    /usr/local/bin/ffmpeg \
    /opt/local/bin/ffmpeg \
    /usr/bin/ffmpeg \
    "$PWD/target/debug/ffmpeg" \
    "$PWD/target/release/ffmpeg"; do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
  return 1
}

ffmpeg_path=""
hwaccels=""
decoders=""
ffmpeg_version=""
if ffmpeg_path=$(find_ffmpeg 2>/dev/null); then
  ffmpeg_version=$("$ffmpeg_path" -version 2>/dev/null | sed -n '1p' || true)
  hwaccels=$("$ffmpeg_path" -hide_banner -hwaccels 2>&1 || true)
  decoders=$("$ffmpeg_path" -hide_banner -decoders 2>&1 | grep -Ei 'h264|hevc|videotoolbox|d3d11va|dxva2' || true)
fi

python3 - "$report_path" "$ffmpeg_path" "$ffmpeg_version" "$hwaccels" "$decoders" <<'PY'
import json
import platform
import sys
from datetime import datetime, timezone

path, ffmpeg_path, version, hwaccels, decoders = sys.argv[1:]
report = {
    "schema_version": 1,
    "probed_at": datetime.now(timezone.utc).isoformat(),
    "tauri": False,
    "webcodecs": {"status": "requires_tauri_probe"},
    "host": {
        "os": platform.platform(),
        "machine": platform.machine(),
        "python": platform.python_version(),
    },
    "ffmpeg": {
        "path": ffmpeg_path or None,
        "available": bool(ffmpeg_path),
        "version": version or None,
        "hardware_accelerators": [line.strip() for line in hwaccels.splitlines() if line.strip() and not line.startswith("Hardware acceleration")],
        "matching_decoders": [line.strip() for line in decoders.splitlines() if line.strip()],
    },
    "latency_baseline": {
        "status": "requires_real_tauri_run",
        "source": "H.264 1280x720 30 FPS",
        "p50_ms": None,
        "p95_ms": None,
        "render_fps": None,
    },
}
with open(path, "w", encoding="utf-8") as handle:
    json.dump(report, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
print(json.dumps(report, ensure_ascii=False, indent=2))
PY
