# GB28181 功能点覆盖矩阵

**状态:核心已实现并真机验证** · 本项目实现 GB/T 28181-2022 **下级设备(IPC)侧**协议。协议流程参考开源项目 uvp-gb28181-sim(Android 版),桌面端用 Rust 独立重写。

图例:✅ 已实现并对真实 WVP 验证 · 🟢 已实现(单测,未逐一真机) · 🚧 部分 · ⬜ 规划 · 🚫 明确不做

> 真机验证平台:WVP-Pro `192.168.10.222:8160`(SIP 域 `3502000000`),设备 `35020000001310000001`。

---

## 注册与信令

| 功能 | 需求 | 状态 |
|---|---|---|
| REGISTER / Digest MD5 / 注销 | FR-1 | ✅ WVP 显示设备在线 |
| 心跳保活 + 超时重注册(指数退避) | FR-2 | ✅ |
| OPTIONS 探活响应 | FR-3 | ✅ |
| SUBSCRIBE 订阅应答(回 200) | FR-3 | ✅ 注册后 SUBSCRIBE 正常应答 |
| SIP over UDP | FR-4 | ✅ |
| SIP over TCP | FR-4 | 🟢 传输层已支持 |

## 设备查询(平台 → 设备)

| 功能 | 需求 | 状态 |
|---|---|---|
| 目录查询(Catalog,独立 MESSAGE 应答) | FR-5 | ✅ WVP 目录同步出通道 |
| 设备信息(DeviceInfo) | FR-6 | ✅ |
| 设备状态(DeviceStatus) | FR-6 | ✅ |
| 录像列表(RecordInfo) | FR-10 | ✅ WVP 显示录像段 |
| GB-2016 / GB-2022 版本切换 | FR-11 | ✅ 两版 WVP 验证(2022 输出 SecurityLevelCode/IPAddress/Port,2016 省略) |

## 实时音视频 / 回放

| 功能 | 需求 | 状态 |
|---|---|---|
| INVITE 点播 / ACK / BYE 停流 | FR-7 | ✅ WVP 点播成功出流 |
| SDP 协商(PS/90000,y=ssrc,TCP/UDP proto) | FR-7 | ✅ |
| RTP over UDP 推流 | FR-8 | ✅ |
| RTP over TCP(RFC 4571) | FR-8 | ✅ WVP TCP-PASSIVE 收流 |
| PS 封装 H.264(Pack/System/PSM/PES) | FR-8 | ✅ WVP 转 FLV 出画面 |
| H.264 文件循环推流(C 档) | FR-8 | ✅ |
| 历史回放流推送 | FR-10 | 🟢 复用点播 INVITE 推流路径 |
| 倍速 / 下载 | FR-10 | ⬜ |
| 强制关键帧(IFrameCmd) | FR-8 | ⬜ |

## 主动上报(设备 → 平台)

| 功能 | 需求 | 状态 |
|---|---|---|
| 报警上报(Alarm Notify) | FR-9 | ✅ WVP 接受(code 200) |
| 录像完成/异常通知 | FR-10 | ⬜ |

## 压力测试(差异化能力)

| 功能 | 需求 | 状态 |
|---|---|---|
| 批量生成 N 设备(ID 递增) | FR-20 | 🟢 |
| 爬坡速率控制 | FR-21 | 🟢 |
| 万级并发注册 + 心跳 | FR-22 | 🟢 单机架构支持 |
| 媒体三档 A/B/C | FR-23 | 🟢 |
| active_ratio 推流采样 | FR-24 | 🟢 |
| 场景 TOML 保存/加载/复跑 | FR-25 | 🟢 |
| 实时指标 + 失败归因 | FR-26/27 | 🟢 |
| 压测报告导出 | FR-28 | 🟢 |

## 明确不做

| 功能 | 说明 |
|---|---|
| 双向语音对讲 | 🚫 排期外 |
| GB35114 国密扩展 | 🚫 后续评估 |
| 上级平台(SU)侧 | 🚫 本项目只做下级设备 |

---

## 关键 ID 编码

GB28181 设备/通道 ID 为 20 位:`中心编码(8) + 行业(2) + 类型(3) + 序号(7)`。批量生成虚拟设备按序号递增。详见 `40-protocol/manscdp.md#id-编码` 与 `30-crates/gb28181-protocol.md`。

## 联调平台与运行

单设备联调运行方式见 `10-functional/device-simulation.md#51`;真实 WVP 联调参数见 `40-protocol/gb28181-联调.md`。真实 H.264 测试码流用 `assets/media/gen-test-h264.sh` 生成(需 ffmpeg)。
