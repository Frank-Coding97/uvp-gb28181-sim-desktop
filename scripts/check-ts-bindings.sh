#!/usr/bin/env bash
# 校验 Rust→TS 类型生成物与 Rust 定义零漂移(④ IPC 契约类型化门禁)。
#
# 原理:DTO 上的 #[derive(ts_rs::TS)] + #[ts(export, export_to=…)] 会在 `cargo test`
# 时把 TypeScript 定义写到 apps/desktop/src/types/generated/。本脚本重新生成一次,
# 再用 git diff 校验生成物与已提交版本一致 —— Rust 侧改了 DTO 而没重新生成/提交,
# 或者有人手改了生成文件,都会在这里失败。
#
# 前端通过 `import type { X } from "../types/generated/X"` 消费这些类型,
# 手写副本(曾与 Rust 漂移过:transport 统计缺 3 个字段)已全部替换为再导出。
set -euo pipefail
cd "$(dirname "$0")/.."

GEN_DIR="apps/desktop/src/types/generated"

cargo test -p sip-core -p media-rtp -p gb28181-simulator -p stress-engine -p uvp-desktop --lib --quiet

if [ -n "$(git status --porcelain -- "$GEN_DIR")" ]; then
  echo "✗ TS 类型生成物与已提交版本不一致 —— 请运行 cargo test 重新生成并提交 $GEN_DIR:" >&2
  git status --porcelain -- "$GEN_DIR" >&2
  exit 1
fi
echo "✓ Rust→TS 类型生成物与 Rust 定义一致($GEN_DIR)"
