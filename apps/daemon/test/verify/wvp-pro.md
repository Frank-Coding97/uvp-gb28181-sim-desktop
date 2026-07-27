# M1 真实平台验收: WVP-Pro Docker

> 目的: 用真实 WVP-Pro 平台跑通 UDP REGISTER,证明 M1 成果不是仅在 raw mock 上"看起来对"。
>
> M1 只做 UDP + one-shot 注册,TCP 归 M4,LiveGBS 跨平台验证归 M4。

## 前置检查

```bash
# 1. Docker 已装且能跑
docker --version

# 2. 5060 UDP 端口空闲(会跟系统 SIP 服务冲突)
lsof -iUDP:5060 || echo "5060 空闲"

# 3. 8080 TCP 端口空闲(WVP 管理面)
lsof -iTCP:8080 -sTCP:LISTEN || echo "8080 空闲"

# 4. daemon 已构建
cd apps/daemon && go build -o uvp-daemon ./cmd/uvp-daemon
```

## 步骤

### 1. 起 WVP-Pro Docker

```bash
docker run -d --name wvp-pro-m1 \
  -p 8080:8080 \
  -p 5060:5060/udp \
  -p 5060:5060/tcp \
  648540858/wvp-pro:latest

# 等 30-60 秒等应用起来
docker logs -f wvp-pro-m1 2>&1 | grep -E "Started WvpApplication|Tomcat started"
```

### 2. 打开管理界面

浏览器打开 http://localhost:8080

- 默认账号: `admin`
- 默认密码: `admin`

### 3. 记下 SIP 接入参数

在管理界面找到"国标接入" / "SIP 服务" 配置页,记下三个值:

| 字段 | 默认值 | 说明 |
|---|---|---|
| SIP ID (平台 ID) | `34020000002000000001` | 20 位 |
| SIP 域 | `3402000000` | 10 位 |
| SIP 密码 | `admin123` 或 `wvp_sip_password` | 版本不同 |

### 4. 准备 daemon 配置

创建 `test-cfg-wvp.json`:

```json
{
  "device_id": "34020000001320000001",
  "server_host": "127.0.0.1",
  "server_port": 5060,
  "server_id": "34020000002000000001",
  "server_domain": "3402000000",
  "password": "admin123",
  "transport": "udp",
  "expires_secs": 3600
}
```

**注意**:
- `password` 用上一步管理面看到的值
- `device_id` 是本设备 ID (任意 20 位,前 10 位跟平台域一致即可)
- `server_host` 是 Docker 映射到宿主机的 IP,本机就是 `127.0.0.1`

### 5. 跑一次注册

```bash
./uvp-daemon --mode once --config test-cfg-wvp.json --log-level debug
```

**期望输出**(stderr):

```
time=... level=INFO msg="uvp-daemon start" version=0.2.0 mode=once
time=... level=INFO msg="sending REGISTER" cseq=1 call_id=uvp-... request_uri=sip:34020000002000000001@3402000000
time=... level=DEBUG msg="Client transaction initialized" caller=TransactionLayer tx=z9hG4bK.xxx__REGISTER
time=... level=INFO msg="REGISTER 200 OK" registered_expires_secs=3600 platform_server=...
```

**exit code 0** 表示成功。

### 6. 验证平台侧看到设备

刷新 WVP 管理界面 → "设备列表" 或 "国标设备"

应该看到:
- 设备 ID: `34020000001320000001`
- 状态: **在线**
- 地址: `127.0.0.1:随机端口`

### 7. 清理

```bash
docker stop wvp-pro-m1
docker rm wvp-pro-m1
```

## 验收标准 (对齐 spec)

| # | 验收点 | 通过条件 |
|---|---|---|
| AC-1 | UDP 闭环 | daemon 打印 "REGISTER 200 OK" |
| AC-3 | Digest MD5 挑战应答 | daemon debug log 出现 "Sending REGISTER" 两次,第 2 次含 Authorization |
| AC-9 (partial) | Expires 解析 | 日志出现 `registered_expires_secs=3600` (或平台返回的实际值) |
| 平台侧 | 设备上线 | WVP 管理界面看到设备状态"在线" |

**不覆盖**(归 M2/M3/M4):
- ❌ TCP 传输 (M4)
- ❌ 心跳 (M3)
- ❌ 续约 (M3)
- ❌ 注销 (M3)
- ❌ LiveGBS 平台交叉验证 (M4)

## 常见问题排查

### daemon 15s 超时无响应

- 检查 WVP 是否真起来: `docker logs wvp-pro-m1 | tail -20`
- 检查端口映射: `docker port wvp-pro-m1` 应看到 `5060/udp -> 0.0.0.0:5060`
- 若 Docker 用 bridge 网络,`server_host` 可能要用 `172.17.0.1` 或宿主机 IP

### 收到 401 但客户端未重发

- 检查 password 是否跟 WVP 管理面配置一致
- daemon debug log 看有没有 "DoDigestAuth" 相关调用

### 收到 403 Forbidden

- 密码错 (spec AC-4)
- 或 device_id 不符合 WVP 白名单 (WVP 默认允许任意 device_id 注册,但企业版可能有限制)

### 400 Bad Request

- SIP 报文格式问题,用 `tcpdump -i any -A 'port 5060'` 抓包看具体报文
- 常见: Contact 头缺 / To 头 URI 格式不对 / CSeq 格式错

## 归档

验收通过后:
1. 截图 WVP 管理面显示设备在线
2. 保存 daemon stderr 完整 log
3. 归档到 `apps/daemon/test/verify/screenshots/YYYY-MM-DD-m1-wvp-pro-success.png`
4. Atlas log 追加一条 milestone-done 事件
