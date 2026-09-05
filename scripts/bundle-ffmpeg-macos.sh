#!/usr/bin/env bash
# 把 ffmpeg、ffprobe + 其非系统依赖库打进主 app 的 Contents/MacOS/(普通可执行文件,非嵌套 .app)。
#
# 为什么这么放:Homebrew ffmpeg 在 /opt/homebrew/bin,不属任何 .app,采集时被 macOS TCC
# 直接 SIGABRT 崩掉。此前试过做成嵌套 FFmpegHelper.app,但它有独立 bundle id,macOS 把它当
# 成独立 TCC 主体 → 屏幕录制授权(授给主 app)覆盖不到它,仍失败。
# 现改为:ffmpeg 作为主 app 包内普通二进制,与主二进制同目录、同属 com.uvp.gb28181.desktop:
#   - 摄像头:归属 bundle=主 app,认主 app Info.plist 的 NSCameraUsageDescription;
#   - 屏幕录制:与主 app 同主体,用户在系统设置勾选主 app 即生效。
# 依赖库放 Contents/Frameworks/,install_name 重写为 @rpath。
#
# 用法: scripts/bundle-ffmpeg-macos.sh <主app路径> <签名身份> [源ffmpeg] [源ffprobe]
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_ENTITLEMENTS="$SCRIPT_DIR/../apps/desktop/src-tauri/Entitlements.plist"
/usr/libexec/PlistBuddy -c 'Print :com.apple.security.device.camera' "$APP_ENTITLEMENTS" | grep -qx true

APP_INPUT="${1:?需要主 app 路径}"
IDENTITY="${2:?需要签名身份(security find-identity 里的哈希或名称)}"
APP="$(cd "$(dirname "$APP_INPUT")" && pwd)/$(basename "$APP_INPUT")"
SRC_FFMPEG_INPUT="${3:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"
SRC_FFMPEG="$(cd "$(dirname "$SRC_FFMPEG_INPUT")" && pwd)/$(basename "$SRC_FFMPEG_INPUT")"
if [ -n "${4:-}" ]; then
  SRC_FFPROBE_INPUT="$4"
elif [ -n "${FFPROBE_BIN:-}" ]; then
  SRC_FFPROBE_INPUT="$FFPROBE_BIN"
elif [ -x "$(dirname "$SRC_FFMPEG_INPUT")/ffprobe" ]; then
  SRC_FFPROBE_INPUT="$(dirname "$SRC_FFMPEG_INPUT")/ffprobe"
else
  SRC_FFPROBE_INPUT="$(command -v ffprobe || echo /opt/homebrew/bin/ffprobe)"
fi
SRC_FFPROBE="$(cd "$(dirname "$SRC_FFPROBE_INPUT")" && pwd)/$(basename "$SRC_FFPROBE_INPUT")"

MACOS="$APP/Contents/MacOS"
LIBDIR="$APP/Contents/Frameworks"
FFBIN="$MACOS/ffmpeg"
FPBIN="$MACOS/ffprobe"

echo "[ffmpeg-bundle] 源 ffmpeg: $SRC_FFMPEG"
[ -x "$SRC_FFMPEG" ] || { echo "找不到可执行 ffmpeg: $SRC_FFMPEG"; exit 1; }
SRC_FFMPEG="$(readlink -f "$SRC_FFMPEG" 2>/dev/null || echo "$SRC_FFMPEG")"
echo "[ffmpeg-bundle] 源 ffprobe: $SRC_FFPROBE"
[ -x "$SRC_FFPROBE" ] || { echo "找不到可执行 ffprobe: $SRC_FFPROBE"; exit 1; }
SRC_FFPROBE="$(readlink -f "$SRC_FFPROBE" 2>/dev/null || echo "$SRC_FFPROBE")"
if ! "$SRC_FFMPEG" -version >/dev/null 2>&1; then
  echo "源 ffmpeg 启动失败，可能缺少动态库: $SRC_FFMPEG" >&2
  exit 1
fi
if ! "$SRC_FFPROBE" -version >/dev/null 2>&1; then
  echo "源 ffprobe 启动失败，可能缺少动态库: $SRC_FFPROBE" >&2
  exit 1
fi

for required_tool in otool install_name_tool codesign; do
  command -v "$required_tool" >/dev/null 2>&1 || {
    echo "缺少 macOS 打包工具: $required_tool" >&2
    exit 1
  }
done

mkdir -p "$LIBDIR"
rm -f "$FFBIN" "$FPBIN"
cp "$SRC_FFMPEG" "$FFBIN"
chmod +w "$FFBIN"
cp "$SRC_FFPROBE" "$FPBIN"
chmod +w "$FPBIN"

# 递归收集非系统依赖库(otool),拷进 Frameworks 并重写 install_name 为 @rpath。
# 兼容 macOS 自带 bash 3.2:用换行分隔的字符串做去重,不用关联数组。
DONE_LIST=""
collect() {
  local bin="$1"
  local deps
  deps=$(otool -L "$bin" 2>/dev/null | tail -n +2 | awk '{print $1}' \
         | grep -vE '^/usr/lib/|^/System/' || true)
  local d name
  for d in $deps; do
    name=$(basename "$d")
    install_name_tool -change "$d" "@rpath/$name" "$bin" 2>/dev/null || true
    case "$DONE_LIST" in
      *"|$name|"*) : ;;
      *)
        DONE_LIST="$DONE_LIST|$name|"
        if [ -f "$d" ]; then
          cp "$d" "$LIBDIR/$name"
          chmod +w "$LIBDIR/$name"
          install_name_tool -id "@rpath/$name" "$LIBDIR/$name" 2>/dev/null || true
          collect "$LIBDIR/$name"
        fi
        ;;
    esac
  done
}
collect "$FFBIN"
collect "$FPBIN"

# ffmpeg/ffprobe 从 Contents/MacOS 找 Contents/Frameworks 的库。
install_name_tool -add_rpath "@executable_path/../Frameworks" "$FFBIN" 2>/dev/null || true
install_name_tool -add_rpath "@executable_path/../Frameworks" "$FPBIN" 2>/dev/null || true

# 签名(由内到外):先库,再 ffmpeg/ffprobe(identifier 用主 app 的子标识,同 Team、同主体链),
# 最后重签外层主 app(内含新增文件,旧签名已失效)。正式 Developer ID 使用
# hardened runtime + secure timestamp；本地 ad-hoc 才禁用时间戳。
sign_code() {
  local target="$1"
  shift
  if [ "$IDENTITY" = "-" ]; then
    # Ad-hoc signatures have no stable Team ID. Enabling hardened runtime here
    # makes dyld reject the separately signed bundled dylibs as a different team.
    codesign --force --timestamp=none --sign - "$@" "$target"
  else
    codesign --force --timestamp --options runtime --sign "$IDENTITY" "$@" "$target"
  fi
}

echo "[ffmpeg-bundle] 签名依赖库…"
while IFS= read -r -d '' dylib; do
  sign_code "$dylib"
done < <(find "$LIBDIR" -type f -name '*.dylib' -print0)
for tool in ffmpeg ffprobe; do
  target="$MACOS/$tool"
  echo "[ffmpeg-bundle] 签名 $tool(归属主 app bundle)…"
  if [ "$IDENTITY" = "-" ]; then
    codesign --force --timestamp=none \
      --identifier "com.uvp.gb28181.desktop.$tool" --sign - "$target"
  else
    codesign --force --timestamp --options runtime \
      --identifier "com.uvp.gb28181.desktop.$tool" --sign "$IDENTITY" "$target"
  fi
done
echo "[ffmpeg-bundle] 重签外层主 app…"
rm -rf "$APP/Contents/_CodeSignature"
# Reuse Tauri's declared permissions; re-signing without this clears camera access.
sign_code "$APP" --entitlements "$APP_ENTITLEMENTS"

echo "[ffmpeg-bundle] 验证:"
for tool in ffmpeg ffprobe; do
  target="$MACOS/$tool"
  codesign -dv "$target" 2>&1 | grep -iE 'Identifier|TeamId' | head -3 || true
  otool -L "$target" | grep -c '@rpath' | xargs echo "  $tool @rpath 依赖数:"
  unresolved=$(otool -L "$target" | tail -n +2 | awk '{print $1}' \
    | grep -vE '^@|^/usr/lib/|^/System/' || true)
  if [ -n "$unresolved" ]; then
    echo "包内 $tool 仍包含未重写的绝对依赖:" >&2
    echo "$unresolved" >&2
    exit 1
  fi
  codesign --verify --strict --verbose "$target"
done
codesign --verify --deep --strict --verbose "$APP" 2>&1 | tail -2
SIGNED_ENTITLEMENTS="$(mktemp)"
trap 'rm -f "$SIGNED_ENTITLEMENTS"' EXIT
codesign -d --entitlements :- "$APP" >"$SIGNED_ENTITLEMENTS" 2>/dev/null
/usr/libexec/PlistBuddy -c 'Print :com.apple.security.device.camera' "$SIGNED_ENTITLEMENTS" | grep -qx true
echo "[ffmpeg-bundle] 完成 → $MACOS/ffmpeg, $MACOS/ffprobe"
