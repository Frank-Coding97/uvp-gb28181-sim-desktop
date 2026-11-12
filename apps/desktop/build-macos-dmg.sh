#!/usr/bin/env bash
# macOS 打包脚本：构建内嵌 FFmpeg 的 .app，最后生成无 Finder 依赖的 DMG。
#
# 用法：在 apps/desktop 目录运行 ./build-macos-dmg.sh
# 环境变量：
#   BUNDLE_FFMPEG=auto|1     默认 1；发布包必须内嵌 FFmpeg
#   FFMPEG_BIN=/path/ffmpeg  指定要内嵌的 FFmpeg（不指定时优先挑带 OSD 能力的）
#   FFPROBE_BIN=/path/ffprobe 指定配套 ffprobe；不指定时按 ffmpeg 同目录推断
#   REQUIRE_OSD_FFMPEG=1     缺 drawtext/drawbox 时直接失败，供 CI 防止静默降级
#   APPLE_SIGNING_IDENTITY   正式签名身份；未设置时使用 ad-hoc 签名“-”
#   APPLE_NOTARY_PROFILE     可选的 notarytool Keychain profile；设置后提交并装订 DMG
#   UVP_UPDATER=1            额外产出在线升级包（.app.tar.gz + .sig），发布版本必须开
#   TAURI_SIGNING_PRIVATE_KEY_PATH  升级签名私钥；默认 ~/.uvp-release-keys/desktop-updater.key
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"
. "$SCRIPT_DIR/../../scripts/select-ffmpeg-macos.sh"

# rustup 安装的 cargo 在非交互 shell 中可能不在 PATH。
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

VERSION="$(node -p "require('./src-tauri/tauri.conf.json').version")"
ARCH="$(uname -m | sed 's/x86_64/x64/;s/arm64/aarch64/')"
APP_DIR="../../target/release/bundle/macos/UVP GB28181 Desktop.app"
BUNDLE_DIR="../../target/release/bundle/macos"
DMG_DIR="../../target/release/bundle/dmg"
DMG="$DMG_DIR/UVP-GB28181-Desktop_${VERSION}_${ARCH}.dmg"
BUNDLE_MODE="${BUNDLE_FFMPEG:-1}"
BUNDLE_SCRIPT="../../scripts/bundle-ffmpeg-macos.sh"
CAPABILITY_SCRIPT="../../scripts/check-ffmpeg-capabilities.sh"
SWIFT_BUNDLE_SCRIPT="../../scripts/bundle-swift-runtime-macos.sh"
SIGN_IDENTITY="${APPLE_SIGNING_IDENTITY:--}"

echo "==> 1/5 构建 .app（Tauri app bundle）"
npm run tauri build -- --bundles app

if [ ! -d "$APP_DIR" ]; then
  echo "构建完成但未找到应用包：$APP_DIR" >&2
  exit 1
fi

if [ "$SIGN_IDENTITY" = "-" ]; then
  echo "提示：未设置 APPLE_SIGNING_IDENTITY，将使用 ad-hoc 签名；正式分发请配置 Developer ID Application 身份。"
fi

echo "==> 2/5 内嵌 ScreenCaptureKit 所需 Swift runtime"
"$SWIFT_BUNDLE_SCRIPT" "$APP_DIR" "$SIGN_IDENTITY"

echo "==> 3/5 处理 FFmpeg 运行时"
case "$BUNDLE_MODE" in
  auto|1|true|yes)
    if [ -n "${FFMPEG_BIN:-}" ] && [ ! -x "$FFMPEG_BIN" ]; then
      echo "错误：FFMPEG_BIN 指定的 FFmpeg 不可执行：$FFMPEG_BIN" >&2
      exit 1
    fi
    FFMPEG_SOURCE="$(uvp_find_ffmpeg || true)"
    if [ -z "$FFMPEG_SOURCE" ]; then
      echo "错误：发布包必须内嵌 FFmpeg，但当前构建机未找到。请安装构建依赖或设置 FFMPEG_BIN。" >&2
      exit 1
    fi
    uvp_check_osd_capability "$FFMPEG_SOURCE"
    FFPROBE_SOURCE="$(uvp_find_ffprobe "$FFMPEG_SOURCE" || true)"
    if [ -z "$FFPROBE_SOURCE" ]; then
      echo "错误：未找到与 $FFMPEG_SOURCE 配套的 ffprobe，请设置 FFPROBE_BIN。" >&2
      exit 1
    fi
    echo "内嵌 FFmpeg:  $FFMPEG_SOURCE"
    echo "内嵌 ffprobe: $FFPROBE_SOURCE"
    FFPROBE_BIN="$FFPROBE_SOURCE" "$CAPABILITY_SCRIPT" "$FFMPEG_SOURCE" \
      "../../target/release/preview-ffmpeg-capabilities.json" "videotoolbox"
    "$BUNDLE_SCRIPT" "$APP_DIR" "$SIGN_IDENTITY" "$FFMPEG_SOURCE" "$FFPROBE_SOURCE"
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

echo "==> 4/5 生成在线升级包（.app.tar.gz + minisign 签名）"
# ⛔ 顺序不能挪：必须等 Swift runtime 与 FFmpeg 都塞进 .app 之后才能打这个包。
#    官方 `tauri build --bundles app` 附带的 createUpdaterArtifacts 是在**塞 FFmpeg 之前**
#    打的 tar，拿它当更新包，升完级的用户会没有 ffmpeg —— 点播/转封装/OSD 全部失效。
UPDATER_ARCHIVE="$BUNDLE_DIR/UVP-GB28181-Desktop_${VERSION}_${ARCH}.app.tar.gz"
if [ "${UVP_UPDATER:-0}" = "1" ]; then
  # 私钥三种给法：CI 传内容（TAURI_SIGNING_PRIVATE_KEY）、本机传路径、都没有就用默认路径。
  # ⛔ 不要在「已经给了私钥内容」时再塞一个默认路径：两个环境变量同时存在时 CLI 会去读路径。
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ -z "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ]; then
    TAURI_SIGNING_PRIVATE_KEY_PATH="${UVP_UPDATER_KEY:-$HOME/.uvp-release-keys/desktop-updater.key}"
    export TAURI_SIGNING_PRIVATE_KEY_PATH
  fi
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ ! -f "${TAURI_SIGNING_PRIVATE_KEY_PATH:-}" ]; then
    echo "错误：未找到升级签名私钥：${TAURI_SIGNING_PRIVATE_KEY_PATH:-<未设置>}" >&2
    echo "      请设置 TAURI_SIGNING_PRIVATE_KEY_PATH，或 TAURI_SIGNING_PRIVATE_KEY（私钥内容）。" >&2
    exit 1
  fi

  rm -f "$UPDATER_ARCHIVE" "$UPDATER_ARCHIVE.sig"
  # 归档根目录必须是 .app 本身：updater 解包时会 skip(1) 丢掉第一层目录名。
  COPYFILE_DISABLE=1 tar -czf "$UPDATER_ARCHIVE" \
    --exclude '*.DS_Store' --exclude '._*' \
    -C "$BUNDLE_DIR" "$(basename "$APP_DIR")"

  # 安装更新需要整包替换，签名覆盖的是 tar.gz 的字节；改包后必须重签，不能沿用旧 .sig。
  npx tauri signer sign -p "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" "$UPDATER_ARCHIVE"
  if [ ! -s "$UPDATER_ARCHIVE.sig" ]; then
    echo "错误：升级包签名未生成：$UPDATER_ARCHIVE.sig" >&2
    exit 1
  fi
  # ⛔ 变量必须用 ${} 包住：紧跟中文全角括号时，bash 会把多字节字节并进变量名报 unbound variable。
  echo "升级包：${UPDATER_ARCHIVE}（$(du -h "$UPDATER_ARCHIVE" | cut -f1)）"
else
  echo "跳过：UVP_UPDATER 未设为 1（发带在线升级的版本时必须设 1）。"
fi

echo "==> 5/5 用 hdiutil 生成 DMG"
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
