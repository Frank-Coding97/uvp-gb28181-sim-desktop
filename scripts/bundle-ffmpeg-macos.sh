#!/usr/bin/env bash
# 把 ffmpeg + 其非系统依赖库打进主 app 的 Contents/MacOS/(普通可执行文件,非嵌套 .app)。
#
# 为什么这么放:Homebrew ffmpeg 在 /opt/homebrew/bin,不属任何 .app,采集时被 macOS TCC
# 直接 SIGABRT 崩掉。此前试过做成嵌套 FFmpegHelper.app,但它有独立 bundle id,macOS 把它当
# 成独立 TCC 主体 → 屏幕录制授权(授给主 app)覆盖不到它,仍失败。
# 现改为:ffmpeg 作为主 app 包内普通二进制,与主二进制同目录、同属 com.uvp.gb28181.desktop:
#   - 摄像头:归属 bundle=主 app,认主 app Info.plist 的 NSCameraUsageDescription;
#   - 屏幕录制:与主 app 同主体,用户在系统设置勾选主 app 即生效。
# 依赖库放 Contents/Frameworks/,install_name 重写为 @rpath。
#
# 用法: scripts/bundle-ffmpeg-macos.sh <主app路径> <签名身份> [源ffmpeg]
set -euo pipefail

APP_INPUT="${1:?需要主 app 路径}"
IDENTITY="${2:?需要签名身份(security find-identity 里的哈希或名称)}"
APP="$(cd "$(dirname "$APP_INPUT")" && pwd)/$(basename "$APP_INPUT")"
SRC_FFMPEG_INPUT="${3:-$(command -v ffmpeg || echo /opt/homebrew/bin/ffmpeg)}"
SRC_FFMPEG="$(cd "$(dirname "$SRC_FFMPEG_INPUT")" && pwd)/$(basename "$SRC_FFMPEG_INPUT")"

MACOS="$APP/Contents/MacOS"
LIBDIR="$APP/Contents/Frameworks"
FFBIN="$MACOS/ffmpeg"

echo "[ffmpeg-bundle] 源 ffmpeg: $SRC_FFMPEG"
[ -x "$SRC_FFMPEG" ] || { echo "找不到可执行 ffmpeg: $SRC_FFMPEG"; exit 1; }
SRC_FFMPEG="$(readlink -f "$SRC_FFMPEG" 2>/dev/null || echo "$SRC_FFMPEG")"

mkdir -p "$LIBDIR"
rm -f "$FFBIN"
cp "$SRC_FFMPEG" "$FFBIN"
chmod +w "$FFBIN"

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

# ffmpeg 从 Contents/MacOS 找 Contents/Frameworks 的库。
install_name_tool -add_rpath "@executable_path/../Frameworks" "$FFBIN" 2>/dev/null || true

# 签名(由内到外):先库,再 ffmpeg(identifier 用主 app 的子标识,同 Team、同主体链),
# 最后重签外层主 app(内含新增文件,旧签名已失效)。正式 Developer ID 使用
# hardened runtime + secure timestamp；本地 ad-hoc 才禁用时间戳。
sign_code() {
  local target="$1"
  if [ "$IDENTITY" = "-" ]; then
    # Ad-hoc signatures have no stable Team ID. Enabling hardened runtime here
    # makes dyld reject the separately signed bundled dylibs as a different team.
    codesign --force --timestamp=none --sign - "$target"
  else
    codesign --force --timestamp --options runtime --sign "$IDENTITY" "$target"
  fi
}

echo "[ffmpeg-bundle] 签名依赖库…"
while IFS= read -r -d '' dylib; do
  sign_code "$dylib"
done < <(find "$LIBDIR" -type f -name '*.dylib' -print0)
echo "[ffmpeg-bundle] 签名 ffmpeg(归属主 app bundle)…"
if [ "$IDENTITY" = "-" ]; then
  codesign --force --timestamp=none \
    --identifier "com.uvp.gb28181.desktop.ffmpeg" --sign - "$FFBIN"
else
  codesign --force --timestamp --options runtime \
    --identifier "com.uvp.gb28181.desktop.ffmpeg" --sign "$IDENTITY" "$FFBIN"
fi
echo "[ffmpeg-bundle] 重签外层主 app…"
rm -rf "$APP/Contents/_CodeSignature"
sign_code "$APP"

echo "[ffmpeg-bundle] 验证:"
codesign -dv "$FFBIN" 2>&1 | grep -iE 'Identifier|TeamId' | head -3 || true
otool -L "$FFBIN" | grep -c '@rpath' | xargs echo "  @rpath 依赖数:"
codesign --verify --deep --strict --verbose "$APP" 2>&1 | tail -2
echo "[ffmpeg-bundle] 完成 → $FFBIN"
