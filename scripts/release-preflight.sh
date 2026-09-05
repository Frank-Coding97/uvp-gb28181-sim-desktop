#!/usr/bin/env bash
# 发布前置质量门与一致性完整性检验脚本
#
# 在打 tag 或触发发布打包流水线前，执行全方位检查：
# 1. 版本号跨文件一致性 (scripts/check-version.sh)
# 2. Git 格式与冲突标记检查 (git diff --check)
# 3. Rust workspace 测试与 Clippy 零警告
# 4. 前端 ESLint + TypeScript + Vitest + Vite 打包
# 5. Gitleaks 敏感凭据防泄漏扫描（若存在工具）

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "========================================="
echo "=== 1. 检查跨模块版本一致性 ==="
echo "========================================="
./scripts/check-version.sh

echo ""
echo "========================================="
echo "=== 2. 检查 Git 文本格式与冲突标记 ==="
echo "========================================="
git diff --check

echo ""
echo "========================================="
echo "=== 3. 运行 Rust Workspace 测试 ==="
echo "========================================="
cargo test --workspace

echo ""
echo "========================================="
echo "=== 4. 运行 Rust Workspace Clippy 门禁 ==="
echo "========================================="
cargo clippy --workspace --all-targets -- -D warnings

echo ""
echo "========================================="
echo "=== 5. 运行前端构建、类型与单元测试门禁 ==="
echo "========================================="
(
  cd apps/desktop
  npm run build
)

echo ""
echo "========================================="
echo "=== 6. 安全敏感凭据扫描 ==="
echo "========================================="
if command -v gitleaks &> /dev/null; then
  echo "执行 gitleaks dir 扫描..."
  gitleaks dir --config .gitleaks.toml .
  echo "Gitleaks 扫描通过 ✓"
else
  echo "提示: 本地未安装 gitleaks，跳过本地扫描（CI 将在云端执行完整门禁）"
fi

echo ""
echo "========================================="
echo "🎉 所有发布前置门禁检查全部通过！准备就绪。"
echo "========================================="
