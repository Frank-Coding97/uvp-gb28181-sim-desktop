#!/usr/bin/env bash
# Bundle the Swift runtime needed by screencapturekit's Swift bridge.
# Usage: bundle-swift-runtime-macos.sh <app-path> <signing-identity>
set -euo pipefail

APP_INPUT="${1:?需要主 app 路径}"
IDENTITY="${2:?需要签名身份；本地 ad-hoc 可传 -}"
APP="$(cd "$(dirname "$APP_INPUT")" && pwd)/$(basename "$APP_INPUT")"
EXECUTABLE="$APP/Contents/MacOS/uvp-desktop"
FRAMEWORKS="$APP/Contents/Frameworks"
TOOL="$(xcrun --find swift-stdlib-tool)"

[ -x "$EXECUTABLE" ] || { echo "找不到应用主程序：$EXECUTABLE" >&2; exit 1; }
mkdir -p "$FRAMEWORKS"

# Xcode 26 may no longer ship a standalone libswift_Concurrency.dylib even
# though the generated bridge still references it through @rpath. Prefer the
# active Xcode runtime and fall back to Command Line Tools' ABI-stable runtime.
SWIFT_BIN="$(xcrun --find swift)"
XCODE_RUNTIME="$(cd "$(dirname "$SWIFT_BIN")/../lib/swift/macosx" 2>/dev/null && pwd || true)"
CLT_RUNTIME="/Library/Developer/CommandLineTools/usr/lib/swift-5.5/macosx"
if [ -n "$XCODE_RUNTIME" ] && [ -f "$XCODE_RUNTIME/libswift_Concurrency.dylib" ]; then
  SOURCE_RUNTIME="$XCODE_RUNTIME"
elif [ -f "$CLT_RUNTIME/libswift_Concurrency.dylib" ]; then
  SOURCE_RUNTIME="$CLT_RUNTIME"
else
  echo "找不到 libswift_Concurrency.dylib；请安装完整 Xcode 或 Command Line Tools。" >&2
  exit 1
fi

echo "[swift-runtime] 来源：$SOURCE_RUNTIME"
install_name_tool -add_rpath "@executable_path/../Frameworks" "$EXECUTABLE" 2>/dev/null || true
# swift-stdlib-tool on some Xcode/CLT combinations mishandles an app bundle
# destination and resolves it as /libswift_Concurrency.dylib. The bridge's
# non-system Swift dependency is explicit, so copy it directly and let dyld
# resolve its system Swift dependencies from /usr/lib/swift.
cp "$SOURCE_RUNTIME/libswift_Concurrency.dylib" "$FRAMEWORKS/libswift_Concurrency.dylib"

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

# install_name_tool changes the main executable, so renew inner and outer seals.
while IFS= read -r -d '' dylib; do
  sign_code "$dylib"
done < <(find "$FRAMEWORKS" -type f -name '*.dylib' -print0)
sign_code "$EXECUTABLE"
rm -rf "$APP/Contents/_CodeSignature"
sign_code "$APP"

if otool -L "$EXECUTABLE" | grep -q '@rpath/libswift_Concurrency.dylib'; then
  [ -f "$FRAMEWORKS/libswift_Concurrency.dylib" ] || {
    echo "Swift runtime 扫描完成但未复制 libswift_Concurrency.dylib。" >&2
    exit 1
  }
fi
codesign --verify --deep --verbose "$APP" >/dev/null
echo "[swift-runtime] 完成"
