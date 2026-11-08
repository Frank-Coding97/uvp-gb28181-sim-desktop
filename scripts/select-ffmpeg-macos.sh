#!/usr/bin/env bash
# macOS 发布包内嵌 FFmpeg 的选择逻辑，供 build-macos-dmg.sh 与
# bundle-ffmpeg-macos.sh 共用；被 source 时只定义函数，直接执行时打印选中的路径。
#
# 为什么不能只挑「第一个能跑的 ffmpeg」：macOS 摄像头采集会把 OSD 水印
# (drawtext 画字 + drawbox 遮挡) 烧进码流，而 Homebrew 默认的 ffmpeg 编译时
# 不带 libfreetype，只有 drawbox。若内嵌了它，发布包会静默降级成「画面无字」，
# 设置页也只显示「缺少 drawtext」，联调时极易误判成烧录坏了。
set -euo pipefail

# 判断某个 ffmpeg 是否具备 OSD 烧录所需的两种滤镜。
ffmpeg_has_osd() {
  local bin="$1" line name has_text=0 has_box=0 filters
  [ -x "$bin" ] || return 1
  filters="$("$bin" -hide_banner -filters 2>/dev/null || true)"
  while IFS= read -r line; do
    name="$(printf '%s\n' "$line" | awk '{print $2}')"
    case "$name" in
      drawtext) has_text=1 ;;
      drawbox) has_box=1 ;;
    esac
  done <<<"$filters"
  [ "$has_text" -eq 1 ] && [ "$has_box" -eq 1 ]
}

# 按优先级挑 FFmpeg：显式 FFMPEG_BIN > 已知带 OSD 能力的位置 > PATH > 常见路径。
# 同一档里优先返回带 OSD 的；全都缺 OSD 时退回第一个可用的，不阻断其它构建机。
uvp_find_ffmpeg() {
  local candidate fallback=""
  local -a candidates=()
  if [ -n "${FFMPEG_BIN:-}" ]; then
    candidates+=("$FFMPEG_BIN")
  else
    candidates+=(
      "$HOME/.local/bin/ffmpeg-osd"
      /opt/homebrew/opt/ffmpeg-full/bin/ffmpeg
      /usr/local/opt/ffmpeg-full/bin/ffmpeg
    )
    if command -v ffmpeg >/dev/null 2>&1; then
      candidates+=("$(command -v ffmpeg)")
    fi
    candidates+=(/opt/homebrew/bin/ffmpeg /usr/local/bin/ffmpeg /opt/local/bin/ffmpeg)
  fi
  for candidate in "${candidates[@]}"; do
    [ -x "$candidate" ] || continue
    [ -n "$fallback" ] || fallback="$candidate"
    if ffmpeg_has_osd "$candidate"; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  if [ -n "$fallback" ]; then
    printf '%s\n' "$fallback"
    return 0
  fi
  return 1
}

# 与选中的 ffmpeg 配套的 ffprobe：显式 FFPROBE_BIN > 同目录同后缀 > PATH。
# 例如 ffmpeg-osd 对应 ffprobe-osd，避免 9.x ffmpeg 配 8.x ffprobe。
uvp_find_ffprobe() {
  local ffmpeg_bin="${1:?需要 ffmpeg 路径}" dir name sibling
  if [ -n "${FFPROBE_BIN:-}" ] && [ -x "$FFPROBE_BIN" ]; then
    printf '%s\n' "$FFPROBE_BIN"
    return 0
  fi
  dir="$(dirname "$ffmpeg_bin")"
  name="$(basename "$ffmpeg_bin")"
  case "$name" in
    ffmpeg-*) sibling="ffprobe-${name#ffmpeg-}" ;;
    *) sibling="ffprobe" ;;
  esac
  if [ -x "$dir/$sibling" ]; then
    printf '%s\n' "$dir/$sibling"
    return 0
  fi
  if [ -x "$dir/ffprobe" ]; then
    printf '%s\n' "$dir/ffprobe"
    return 0
  fi
  if command -v ffprobe >/dev/null 2>&1; then
    command -v ffprobe
    return 0
  fi
  return 1
}

# 打包前对 OSD 能力做一次显式判定：缺能力时给出可执行的修复提示；
# 设 REQUIRE_OSD_FFMPEG=1 可让 CI 把「静默降级」变成硬失败。
uvp_check_osd_capability() {
  local bin="$1"
  if ffmpeg_has_osd "$bin"; then
    return 0
  fi
  echo "警告：内嵌的 FFmpeg 缺少 drawtext/drawbox，发布包里 macOS 摄像头 OSD 水印不会烧进码流。" >&2
  echo "       请改用带 libfreetype 的 FFmpeg：FFMPEG_BIN=/path/to/ffmpeg $0" >&2
  if [ "${REQUIRE_OSD_FFMPEG:-0}" = "1" ]; then
    echo "错误：REQUIRE_OSD_FFMPEG=1 要求 OSD 能力，已终止打包。" >&2
    return 1
  fi
  return 0
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  uvp_find_ffmpeg
fi
