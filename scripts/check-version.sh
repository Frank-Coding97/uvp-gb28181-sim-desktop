#!/usr/bin/env bash
# 校验版本号只有一个来源。
#
# 唯一来源是根 `Cargo.toml` 的 `[workspace.package] version`:
#   · 所有 crate 用 `version.workspace = true` 继承它;
#   · `tauri.conf.json` **不写** version —— Tauri 留空时取 crate 版本;
#   · `apps/desktop/package.json` 因为 npm 要求必须有 version 字段,
#     无法继承,所以在这里校验它与 Cargo.toml 一致。
#
# 打 tag 时额外校验 `vX.Y.Z` 与文件里的版本一致 —— 否则会出现
# "tag 是 v0.1.3 但安装包标 0.1.2" 这种没法追溯的发布。
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

fail() {
  echo "版本校验失败: $*" >&2
  exit 1
}

# 只取 [workspace.package] 段里的 version,避免匹配到依赖项的 version。
cargo_version="$(
  awk '
    /^\[workspace\.package\]/ { in_section = 1; next }
    /^\[/ { in_section = 0 }
    in_section && /^version *=/ {
      gsub(/^version *= *"/, ""); gsub(/".*$/, ""); print; exit
    }
  ' Cargo.toml
)"
[ -n "$cargo_version" ] || fail "无法从 Cargo.toml 读出 [workspace.package] version"

pkg_version="$(
  awk -F'"' '/^[[:space:]]*"version"[[:space:]]*:/ { print $4; exit }' apps/desktop/package.json
)"
[ -n "$pkg_version" ] || fail "无法从 apps/desktop/package.json 读出 version"

echo "Cargo.toml       : $cargo_version"
echo "package.json     : $pkg_version"

if [ "$cargo_version" != "$pkg_version" ]; then
  fail "Cargo.toml($cargo_version) 与 package.json($pkg_version) 不一致"
fi

# tauri.conf.json 必须**不含** version:写了就等于又多出一个来源。
if grep -Eq '^[[:space:]]*"version"[[:space:]]*:' apps/desktop/src-tauri/tauri.conf.json; then
  fail "tauri.conf.json 不应包含 version 字段(留空即继承 crate 版本)"
fi
echo "tauri.conf.json  : 未写 version(继承 crate 版本)✓"

# tag 构建:tag 必须等于文件里的版本。
ref="${GITHUB_REF:-}"
case "$ref" in
  refs/tags/v*)
    tag_version="${ref#refs/tags/v}"
    echo "git tag          : $tag_version"
    if [ "$tag_version" != "$cargo_version" ]; then
      fail "tag(v$tag_version) 与 Cargo.toml($cargo_version) 不一致"
    fi
    ;;
esac

echo "版本号一致 ✓"
