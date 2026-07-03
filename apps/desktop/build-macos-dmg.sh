#!/usr/bin/env bash
# macOS 打包脚本:产出 .app + 可分发 .dmg。
#
# 背景:tauri 内置的 dmg 打包(create-dmg / bundle_dmg.sh)会调 Finder AppleScript
# 设置窗口图标布局,在无 GUI 会话(SSH / CI 无桌面)下会失败。本脚本改用 hdiutil
# 直接打一个朴素 dmg(无花哨布局但可靠),规避该问题。
#
# 用法:在 apps/desktop 目录下运行 `./build-macos-dmg.sh`。
set -e
cd "$(dirname "$0")"

VERSION="0.1.0"
ARCH="$(uname -m | sed 's/x86_64/x64/;s/arm64/aarch64/')"
APP_DIR="../../target/release/bundle/macos/UVP GB28181 Desktop.app"
DMG_DIR="../../target/release/bundle/dmg"
DMG="$DMG_DIR/UVP-GB28181-Desktop_${VERSION}_${ARCH}.dmg"

echo "==> 1/2 构建 .app(tauri,仅 app 目标)"
npm run tauri build -- --bundles app

echo "==> 2/2 用 hdiutil 打 dmg"
mkdir -p "$DMG_DIR"
rm -f "$DMG"
hdiutil create -volname "UVP GB28181 Desktop" \
  -srcfolder "$APP_DIR" -ov -format UDZO "$DMG"

echo "完成:$DMG"
