#!/usr/bin/env bash
# macOS 打包脚本：构建内嵌 FFmpeg 的 .app，最后生成无 Finder 依赖的 DMG。
#
# 用法：在 apps/desktop 目录运行 ./build-macos-dmg.sh
# 环境变量：
#   BUNDLE_FFMPEG=auto|1     默认 1；发布包必须内嵌 FFmpeg
#   FFMPEG_BIN=/path/ffmpeg  指定要内嵌的 FFmpeg
#   APPLE_SIGNING_IDENTITY   正式签名身份；未设置时使用 ad-hoc 签名“-”
#   APPLE_NOTARY_PROFILE     可选的 notarytool Keychain profile；设置后提交并装订 DMG
set -euo pipefail
cd "$(dirname "$0")"

# rustup 安装的 cargo 在非交互 shell 中可能不在 PATH。
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version")"
ARCH="$(uname -m | sed 's/x86_64/x64/;s/arm64/aarch64/')"
APP_DIR="../../target/release/bundle/macos/UVP GB28181 Desktop.app"
DMG_DIR="../../target/release/bundle/dmg"
DMG="$DMG_DIR/UVP-GB28181-Desktop_${VERSION}_${ARCH}.dmg"
BUNDLE_MODE="${BUNDLE_FFMPEG:-1}"
BUNDLE_SCRIPT="../../scripts/bundle-ffmpeg-macos.sh"
CAPABILITY_SCRIPT="../../scripts/check-ffmpeg-capabilities.sh"
SWIFT_BUNDLE_SCRIPT="../../scripts/bundle-swift-runtime-macos.sh"
SIGN_IDENTITY="${APPLE_SIGNING_IDENTITY:--}"

find_ffmpeg() {
  if [ -n "${FFMPEG_BIN:-}" ] && [ -x "$FFMPEG_BIN" ]; then
    printf '%s\n' "$FFMPEG_BIN"
    return
  fi
  if command -v ffmpeg >/dev/null 2>&1; then
    command -v ffmpeg
    return
  fi
  for candidate in /opt/homebrew/bin/ffmpeg /usr/local/bin/ffmpeg /opt/local/bin/ffmpeg; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return
    fi
  done
}

echo "==> 1/4 构建 .app（Tauri app bundle）"
npm run tauri build -- --bundles app

if [ ! -d "$APP_DIR" ]; then
  echo "构建完成但未找到应用包：$APP_DIR" >&2
  exit 1
fi

if [ "$SIGN_IDENTITY" = "-" ]; then
  echo "提示：未设置 APPLE_SIGNING_IDENTITY，将使用 ad-hoc 签名；正式分发请配置 Developer ID Application 身份。"
fi

echo "==> 2/4 内嵌 ScreenCaptureKit 所需 Swift runtime"
"$SWIFT_BUNDLE_SCRIPT" "$APP_DIR" "$SIGN_IDENTITY"

echo "==> 3/4 处理 FFmpeg 运行时"
case "$BUNDLE_MODE" in
  auto|1|true|yes)
    FFMPEG_SOURCE="$(find_ffmpeg || true)"
    if [ -z "$FFMPEG_SOURCE" ]; then
      echo "错误：发布包必须内嵌 FFmpeg，但当前构建机未找到。请安装构建依赖或设置 FFMPEG_BIN。" >&2
      exit 1
    else
      "$CAPABILITY_SCRIPT" "$FFMPEG_SOURCE" "../../target/release/preview-ffmpeg-capabilities.json" "videotoolbox"
      "$BUNDLE_SCRIPT" "$APP_DIR" "$SIGN_IDENTITY" "$FFMPEG_SOURCE"
    fi
    ;;
  0|false|no)
    echo "错误：禁止生成不含 FFmpeg 的用户发布包（BUNDLE_FFMPEG=$BUNDLE_MODE）。" >&2
    exit 1
    ;;
  *)
    echo "无效的 BUNDLE_FFMPEG=$BUNDLE_MODE；允许值为 auto 或 1。" >&2
    exit 1
    ;;
esac

echo "==> 4/4 用 hdiutil 生成 DMG"
mkdir -p "$DMG_DIR"
rm -f "$DMG"
hdiutil create -volname "UVP GB28181 Desktop" \
  -srcfolder "$APP_DIR" -ov -format UDZO "$DMG"

if [ -n "${APPLE_NOTARY_PROFILE:-}" ]; then
  echo "==> 提交 Apple 公证并装订票据"
  xcrun notarytool submit "$DMG" --keychain-profile "$APPLE_NOTARY_PROFILE" --wait
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
elif [ "$SIGN_IDENTITY" != "-" ]; then
  echo "警告：已使用正式签名但未设置 APPLE_NOTARY_PROFILE，DMG 尚未公证。" >&2
fi

echo "完成：$DMG"
