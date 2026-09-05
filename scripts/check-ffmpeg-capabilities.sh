#!/usr/bin/env bash
set -euo pipefail

FFMPEG="${1:?用法: check-ffmpeg-capabilities.sh <ffmpeg> <report> [expected backend] }"
REPORT="${2:?用法: check-ffmpeg-capabilities.sh <ffmpeg> <report> [expected backend] }"
EXPECTED="${3:-}"

[ -x "$FFMPEG" ] || { echo "FFmpeg 不可执行: $FFMPEG" >&2; exit 1; }

find_ffprobe() {
    if [ -n "${FFPROBE_BIN:-}" ]; then
        printf '%s\n' "$FFPROBE_BIN"
        return
    fi
    local sibling
    sibling="$(dirname "$FFMPEG")/ffprobe"
    if [ -x "$sibling" ]; then
        printf '%s\n' "$sibling"
        return
    fi
    if command -v ffprobe >/dev/null 2>&1; then
        command -v ffprobe
        return
    fi
    return 1
}

FFPROBE="$(find_ffprobe || true)"
[ -n "$FFPROBE" ] && [ -x "$FFPROBE" ] || {
    echo "ffprobe 不可执行或未找到: ${FFPROBE:-${FFPROBE_BIN:-ffprobe}}" >&2
    exit 1
}

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/uvp-ffmpeg-capabilities.XXXXXX")"
trap 'rm -rf "$TMP_DIR"' EXIT

if ! "$FFMPEG" -version >"$TMP_DIR/ffmpeg-version.txt" 2>"$TMP_DIR/ffmpeg-version.err"; then
    echo "FFmpeg 启动失败，可能缺少动态库: $FFMPEG" >&2
    sed -n '1,8p' "$TMP_DIR/ffmpeg-version.err" >&2 || true
    exit 1
fi
if ! "$FFPROBE" -version >"$TMP_DIR/ffprobe-version.txt" 2>"$TMP_DIR/ffprobe-version.err"; then
    echo "ffprobe 启动失败，可能缺少动态库: $FFPROBE" >&2
    sed -n '1,8p' "$TMP_DIR/ffprobe-version.err" >&2 || true
    exit 1
fi

"$FFMPEG" -hide_banner -hwaccels >"$TMP_DIR/hwaccels.txt" 2>&1 || true
"$FFMPEG" -hide_banner -decoders >"$TMP_DIR/decoders.txt" 2>&1 || true
"$FFMPEG" -hide_banner -encoders >"$TMP_DIR/encoders.txt" 2>&1 || true

mkdir -p "$(dirname "$REPORT")"
python3 - "$REPORT" "$FFMPEG" "$FFPROBE" "$TMP_DIR/ffmpeg-version.txt" "$TMP_DIR/ffprobe-version.txt" "$TMP_DIR/hwaccels.txt" "$TMP_DIR/decoders.txt" "$TMP_DIR/encoders.txt" "$EXPECTED" <<'PY'
import hashlib
import json
import os
import platform
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone

(
    report_path,
    ffmpeg,
    ffprobe,
    ffmpeg_version_path,
    ffprobe_version_path,
    hwaccels_path,
    decoders_path,
    encoders_path,
    expected,
) = sys.argv[1:]


def read_text(path):
    with open(path, encoding="utf-8", errors="replace") as handle:
        return handle.read()


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def run(command):
    return subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def short_error(result):
    message = (result.stderr or result.stdout or "").strip()
    return message[-4000:] if message else f"退出码 {result.returncode}"


def encoder_names(text):
    names = set()
    for line in text.splitlines():
        fields = line.split()
        if len(fields) >= 2 and re.fullmatch(r"[A-Z.]{6}", fields[0]):
            names.add(fields[1])
    return names


def probe_stream(path, selector):
    result = run([
        ffprobe,
        "-v",
        "error",
        "-select_streams",
        selector,
        "-show_entries",
        "stream=codec_name,codec_type,width,height,sample_rate,avg_frame_rate,duration",
        "-of",
        "json",
        path,
    ])
    if result.returncode != 0:
        return None, short_error(result)
    try:
        streams = json.loads(result.stdout).get("streams", [])
    except json.JSONDecodeError as exc:
        return None, f"ffprobe JSON 无法解析: {exc}"
    if not streams:
        return None, "ffprobe 未返回目标流"
    return streams[0], None


def base_probe(name, codec, kind, sample_rate=None):
    return {
        "status": "failed",
        "codec": codec,
        "kind": kind,
        "sample_rate": sample_rate,
        "encoder": None,
        "encoded": False,
        "decoded": False,
        "encode_exit_code": None,
        "decode_exit_code": None,
        "ffprobe": None,
        "output_bytes": 0,
        "output_sha256": None,
        "error": None,
        "input": {
            "duration_seconds": 5,
            "video": "lavfi:testsrc=size=320x180:rate=25" if kind == "video" else None,
            "audio": f"lavfi:sine=frequency=1000:sample_rate={sample_rate}" if kind == "audio" else None,
        },
    }


def run_video_probe(name, codec, candidates, output_dir):
    result = base_probe(name, codec, "video")
    available = [candidate for candidate in candidates if candidate in available_encoders]
    if not available:
        result["error"] = f"缺少 {codec} 编码器，候选: {', '.join(candidates)}"
        return result

    result["encoder"] = available[0]
    output = os.path.join(output_dir, f"{name}.mkv")
    encode = run([
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc=size=320x180:rate=25",
        "-t",
        "5",
        "-map",
        "0:v:0",
        "-an",
        "-c:v",
        available[0],
        "-pix_fmt",
        "yuv420p",
        "-g",
        "50",
        "-f",
        "matroska",
        output,
    ])
    result["encode_exit_code"] = encode.returncode
    if encode.returncode != 0 or not os.path.isfile(output) or os.path.getsize(output) == 0:
        result["error"] = f"{codec} 编码失败: {short_error(encode)}"
        return result
    result["encoded"] = True
    result["output_bytes"] = os.path.getsize(output)
    result["output_sha256"] = sha256(output)

    decoded = run([
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        output,
        "-map",
        "0:v:0",
        "-f",
        "null",
        "-",
    ])
    result["decode_exit_code"] = decoded.returncode
    if decoded.returncode != 0:
        result["error"] = f"{codec} 解码失败: {short_error(decoded)}"
        return result
    result["decoded"] = True
    stream, error = probe_stream(output, "v:0")
    if error:
        result["error"] = error
        return result
    result["ffprobe"] = stream
    if stream.get("codec_name") != codec or stream.get("codec_type") != "video":
        result["error"] = f"ffprobe 实际视频流不匹配: {stream}"
        return result
    result["status"] = "passed"
    return result


def run_audio_probe(name, codec, encoder_candidates, sample_rate, output_dir):
    result = base_probe(name, codec, "audio", sample_rate)
    available = [candidate for candidate in encoder_candidates if candidate in available_encoders]
    if not available:
        result["error"] = f"缺少 {codec} 编码器，候选: {', '.join(encoder_candidates)}"
        return result

    result["encoder"] = available[0]
    output_format = "wav" if codec in {"pcm_alaw", "pcm_mulaw"} else "adts"
    output = os.path.join(output_dir, f"{name}.{'wav' if output_format == 'wav' else 'aac'}")
    command = [
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        f"sine=frequency=1000:sample_rate={sample_rate}",
        "-t",
        "5",
        "-map",
        "0:a:0",
        "-c:a",
        available[0],
        "-ar",
        str(sample_rate),
        "-ac",
        "1",
    ]
    if codec == "aac":
        command.extend(["-b:a", "32k"])
    command.extend(["-f", output_format, output])
    encode = run(command)
    result["encode_exit_code"] = encode.returncode
    if encode.returncode != 0 or not os.path.isfile(output) or os.path.getsize(output) == 0:
        result["error"] = f"{codec}/{sample_rate}Hz 编码失败: {short_error(encode)}"
        return result
    result["encoded"] = True
    result["output_bytes"] = os.path.getsize(output)
    result["output_sha256"] = sha256(output)

    decoded = run([
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-i",
        output,
        "-map",
        "0:a:0",
        "-f",
        "null",
        "-",
    ])
    result["decode_exit_code"] = decoded.returncode
    if decoded.returncode != 0:
        result["error"] = f"{codec}/{sample_rate}Hz 解码失败: {short_error(decoded)}"
        return result
    result["decoded"] = True
    stream, error = probe_stream(output, "a:0")
    if error:
        result["error"] = error
        return result
    result["ffprobe"] = stream
    if stream.get("codec_name") != codec or stream.get("codec_type") != "audio":
        result["error"] = f"ffprobe 实际音频流不匹配: {stream}"
        return result
    if int(stream.get("sample_rate", 0)) != sample_rate:
        result["error"] = f"ffprobe 实际采样率不匹配: {stream.get('sample_rate')} != {sample_rate}"
        return result
    result["status"] = "passed"
    return result


hwaccels = read_text(hwaccels_path)
decoders = read_text(decoders_path)
encoders = read_text(encoders_path)
available_encoders = encoder_names(encoders)
accels = [line.strip() for line in hwaccels.splitlines() if line.strip() and not line.startswith("Hardware acceleration")]
matching_decoders = [
    line.strip()
    for line in decoders.splitlines()
    if line.strip() and re.search(r"\b(h264|hevc|videotoolbox|d3d11va|dxva2)\b", line, re.IGNORECASE)
]

with tempfile.TemporaryDirectory(prefix="uvp-ffmpeg-probes-") as probe_dir:
    probes = {
        "h264": run_video_probe("h264", "h264", ["libx264", "h264_videotoolbox", "h264_nvenc", "h264_qsv", "h264_amf"], probe_dir),
        "h265": run_video_probe("h265", "hevc", ["libx265", "hevc_videotoolbox", "hevc_nvenc", "hevc_qsv", "hevc_amf"], probe_dir),
        "g711a": run_audio_probe("g711a", "pcm_alaw", ["pcm_alaw", "pcm_alaw_at"], 8000, probe_dir),
        "g711u": run_audio_probe("g711u", "pcm_mulaw", ["pcm_mulaw", "pcm_mulaw_at"], 8000, probe_dir),
        "aac_8k": run_audio_probe("aac_8k", "aac", ["aac", "aac_at"], 8000, probe_dir),
        "aac_16k": run_audio_probe("aac_16k", "aac", ["aac", "aac_at"], 16000, probe_dir),
    }

report = {
    "schema_version": 2,
    "checked_at": datetime.now(timezone.utc).isoformat(),
    "host": {"os": platform.platform(), "machine": platform.machine()},
    "ffmpeg": {
        "path": os.path.abspath(ffmpeg),
        "version": read_text(ffmpeg_version_path).splitlines()[0],
        "sha256": sha256(ffmpeg),
        "hardware_accelerators": accels,
        "matching_decoders": matching_decoders,
    },
    "ffprobe": {
        "path": os.path.abspath(ffprobe),
        "available": True,
        "version": read_text(ffprobe_version_path).splitlines()[0],
        "sha256": sha256(ffprobe),
    },
    "probe_input": {
        "duration_seconds": 5,
        "video": "lavfi:testsrc=size=320x180:rate=25",
        "audio": "lavfi:sine=frequency=1000:sample_rate=<target>",
        "devices_accessed": False,
    },
    "probes": probes,
    "expected_backend": expected or None,
    "expected_backend_present": not expected or any(expected.lower() in item.lower() for item in accels + matching_decoders),
}
report["overall_status"] = "passed" if all(item["status"] == "passed" for item in probes.values()) else "failed"

with open(report_path, "w", encoding="utf-8") as handle:
    json.dump(report, handle, ensure_ascii=False, indent=2)
    handle.write("\n")
print(json.dumps(report, ensure_ascii=False, indent=2))

if expected and not report["expected_backend_present"] and os.environ.get("REQUIRE_HW_ACCEL") == "1":
    raise SystemExit(f"缺少要求的硬件后端: {expected}")
failures = [f"{name}: {item['error'] or '未知错误'}" for name, item in probes.items() if item["status"] != "passed"]
if failures:
    raise SystemExit("真实 FFmpeg 能力探针失败: " + "; ".join(failures))
PY
