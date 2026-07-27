#!/usr/bin/env bash
# M1 WVP-Pro 验收 pre-flight 检查脚本
# 用法: bash apps/daemon/test/verify/preflight.sh
set -e

echo "=== M1 WVP-Pro 验收 pre-flight ==="

# 1. Docker
if ! command -v docker >/dev/null 2>&1; then
  echo "❌ Docker 未安装"
  exit 1
fi
echo "✅ Docker: $(docker --version)"

# 2. Go
if ! command -v go >/dev/null 2>&1; then
  echo "❌ Go 未安装"
  exit 1
fi
echo "✅ Go: $(go version | awk '{print $3}')"

# 3. 端口
if lsof -iUDP:5060 -sUDP:^ESTABLISHED 2>/dev/null | grep -qv "^COMMAND"; then
  echo "⚠️  5060/UDP 已被占用:"
  lsof -iUDP:5060 | head -5
else
  echo "✅ 5060/UDP 空闲"
fi

if lsof -iTCP:8080 -sTCP:LISTEN 2>/dev/null | grep -qv "^COMMAND"; then
  echo "⚠️  8080/TCP 已被占用:"
  lsof -iTCP:8080 -sTCP:LISTEN | head -5
else
  echo "✅ 8080/TCP 空闲"
fi

# 4. daemon 二进制
DAEMON_DIR="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$DAEMON_DIR/uvp-daemon"
if [ ! -x "$BIN" ]; then
  echo "⚠️  daemon 二进制未编译,正在编译..."
  (cd "$DAEMON_DIR" && go build -o uvp-daemon ./cmd/uvp-daemon)
fi
echo "✅ daemon: $($BIN --version)"

echo ""
echo "=== 就绪。下一步 ==="
echo "1. docker run -d --name wvp-pro-m1 -p 8080:8080 -p 5060:5060/udp 648540858/wvp-pro:latest"
echo "2. 等 30-60s 后访问 http://localhost:8080 (admin/admin)"
echo "3. 复制 sample-config.json → test-cfg-wvp.json 按 WVP 配置改 password"
echo "4. $BIN --mode once --config test-cfg-wvp.json --log-level debug"
echo "5. 验收:见 apps/daemon/test/verify/wvp-pro.md"
