# M3 长稳验收: WVP-Pro 30 分钟挂机 + 心跳恢复

> 目的: 用真实 WVP-Pro 验证 M3 三件套 (心跳保活 + 到期续约 + 主动注销) 在生产条件下能否长稳。
>
> **不进 CI**。集成测试用短周期在 mock 上跑, 真机长稳老板每次改 M3 关键路径后手工过一遍。

## 前置

- WVP-Pro Docker 已按 M1 [`wvp-pro.md`](./wvp-pro.md) 跑起来 (端口 8160/udp 已开)
- `apps/daemon` 已编译 `go build -o uvp-daemon ./cmd/uvp-daemon`
- Simulator.vue 前端能通过 `npm run tauri dev` 跑起来

## Case A: 30 分钟持续挂机不掉线 (spec AC-6, AC-9)

### 步骤

1. 用 Simulator UI 或 daemon 手工模式起 session:
   ```bash
   # 手工路径(不走前端 UI):
   cd apps/daemon
   ./uvp-daemon --mode=once --config=./test-cfg-wvp.json --log-level=debug 2>&1 | tee /tmp/m3-longrun-a.log
   ```
   > 注:`once` 模式 M1 阶段一注册成功就退,M3 长稳需要走 stdio 模式或 Simulator UI 长驻。手工 stdio 场景先起 UI。

2. WVP 管理面 (http://localhost:8080) 打开 "国标设备管理"
   - 起始时刻: 记下时间 `T0`
   - 该设备行"状态"应变绿 (在线)

3. 每 5 分钟对账一次 (共 6 次, 30 分钟):
   | 时间 | WVP 显示 | UI 心跳文案 | daemon stderr 心跳 log |
   |---|---|---|---|
   | T0+5m | 在线 ✅ | 心跳正常 (N) | 每分钟 1 条 "heartbeat send OK" |
   | T0+10m | 在线 ✅ | ... | ... |
   | T0+15m | 在线 ✅ | ... | ... |
   | T0+20m | 在线 ✅ | ... | ... |
   | T0+25m | 在线 ✅ | ... | ... |
   | T0+30m | 在线 ✅ | ... | ... |

4. `T0+30m` 后停 UI 或 Ctrl+C daemon,确认过程中:
   - **UI 心跳计数** 应等于 30 (60s 一次)
   - **SIP trace 面板** 应有对应 30 个 outbound MESSAGE / inbound 200 OK

### 通过标准

- [ ] 30 分钟内 WVP 管理面从未变"离线"
- [ ] daemon 每 60±5 秒发 1 条 Keepalive MESSAGE
- [ ] Simulator UI 心跳状态一直显示"心跳正常"

### 失败排查

- WVP 显示离线但 daemon 无 fail log → 检查 WVP 平台侧配置 (可能是 WVP 侧短心跳阈值)
- daemon fail log 但 WVP 显示在线 → 检查 daemon `heartbeat send timeout` 是否 5s (可能网络慢, 调 `heartbeatSendTimeout` 常量)
- UI 心跳计数不动 → 检查 Tauri `daemon.rs` `forward_event("heartbeat_result", ...)` 是否触发

---

## Case B: 到期续约无感 (spec AC-9)

### 步骤

1. 手工把 `test-cfg-wvp.json` 里 `expires_secs` 改成 60 (从默认 3600 缩短, 便于快速验)
2. 起 session (走 UI 或 stdio)
3. 观察 SIP trace 面板:
   - `T0`: REGISTER / 401 / AUTH / 200 (4 条)
   - `T0+48s` (60 * 0.8 = 48s): 又出现 REGISTER / 401 / AUTH / 200 (4 条,续约)
   - `T0+96s`: 再一轮续约
4. UI 顶部"注册状态"始终显示"已注册"不闪烁

### 通过标准

- [ ] 48s 附近触发第 1 次续约,SIP trace 出现 4 条报文
- [ ] UI 状态无闪烁 (不出现"注册中" → "已注册"来回切)
- [ ] daemon stderr: `INFO renewal success new_expires_secs=60`

---

## Case C: 网络抖动恢复 (spec AC-7 R3)

### 步骤

1. 起 session,等 UI 显示"心跳正常"
2. 阻断上行:
   ```bash
   # macOS pfctl: 阻掉出向 udp:8160
   echo "block drop out proto udp to any port 8160" | sudo pfctl -f -
   sudo pfctl -e
   ```
3. 观察 90 秒:
   - UI 心跳应变 "心跳异常 (1/3)" → "心跳异常 (2/3)"
   - 90s 一般 1-2 次失败 (60s 一次心跳),不到 3 次
4. 恢复:
   ```bash
   sudo pfctl -d  # 关掉 firewall
   ```
5. 下一次心跳应成功,UI 恢复"心跳正常"

### 通过标准

- [ ] 阻断 90s 期间 UI 显示"心跳异常 N/3" (N < 3)
- [ ] 恢复后**不需要重新注册**,UI 状态一直是"已注册"
- [ ] daemon stderr 有 `heartbeat fail consecutive_fails=1` 后接 `heartbeat_result{ok:true}` 事件

---

## Case D: 长时间断网降级 (spec AC-8)

### 步骤

1. 起 session,等"心跳正常"
2. 阻断 3 分钟以上 (同 Case C 的 pfctl):
   ```bash
   echo "block drop out proto udp to any port 8160" | sudo pfctl -f -
   sudo pfctl -e
   sleep 200  # 3 分 20 秒
   ```
3. 观察 UI:
   - `T0+60s`: "心跳异常 (1/3)"
   - `T0+120s`: "心跳异常 (2/3)"
   - `T0+180s`: **"心跳超限 (3/3)"** + 注册状态转"注册失败"
4. 恢复网络后 UI 保持 Failed (不自动重连,老板需手工点"注册"重来)

### 通过标准

- [ ] 3 次连续失败后 UI 状态变 "Failed"
- [ ] `device_state:Failed` 事件 payload.reason 含"心跳连续失败"
- [ ] daemon stderr: `WARN session marked failed reason=心跳连续失败`

---

## Case E: 主动注销 (spec AC-10, AC-11)

### 步骤

1. 起 session,等"已注册"
2. UI 点"注销"按钮
3. 观察:
   - SIP trace 应出现 REGISTER (Expires: 0) → 200 OK
   - UI 状态转"未连接"
   - WVP 管理面: 该设备应在 6 秒内变"离线"或消失
4. daemon stderr 应有 `sending shutdown REGISTER (Expires=0)` + `heartbeat loop exit` + `renewal loop exit`

### 通过标准

- [ ] UI 点注销后 SIP trace 出现 Expires:0 REGISTER
- [ ] 平台 200 OK 后 UI 变"未连接"
- [ ] daemon 侧 heartbeat + renewal 两个 goroutine 都退 (log)
- [ ] 平台无响应场景 (可临时阻断上行):UI 6s 内仍转"未连接"

---

## 结果记录模板

老板每跑一轮抄进 [[../../../../Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/handoffs/YYYY-MM-DD-HHMM-m3-longrun.md]]:

```markdown
# M3 长稳验收 YYYY-MM-DD HH:MM

- Case A 30 分钟挂机: PASS / FAIL (备注)
- Case B 到期续约: PASS / FAIL (备注)
- Case C 90s 抖动恢复: PASS / FAIL (备注)
- Case D 3 分钟断网降级: PASS / FAIL (备注)
- Case E 主动注销: PASS / FAIL (备注)

## 观察到的问题
- ...

## 是否可进 M4
- 5 case 全绿 → 是
- 有任何一条红 → 记根因回到 M3 iter
```
