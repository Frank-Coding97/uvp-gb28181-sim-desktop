# 测试策略

**状态:已评审** · 关联 NFR-7 · "协议/领域逻辑必配单测"。

## 分层

| 层 | 范围 | 工具 | 何时跑 |
|---|---|---|---|
| 单元测试 | crate 内纯逻辑:协议解析、状态机转移、编码、指标聚合 | `cargo test` | 每次提交 |
| 集成测试 | 跨 crate:注册流程(mock 平台)、点播链路、loopback RTP | `cargo test`(tests/) | 每次提交 |
| 端到端 | 对真实 WVP 联调:单设备上线、点播出图 | 手动 / 脚本 | 里程碑验收 |
| 压测自测 | 引擎自身规模/稳定性(本机 loopback 或测试平台) | CLI + 指标 | M3+ |

## 重点单测(必须)

- **sip-core**:消息解析/序列化往返;Digest 响应对拍已知向量;事务超时重传时序(模拟时钟)。
- **gb28181-protocol**:真实平台样例 XML 往返;ID 编解码与批量生成序号。
- **gb28181-simulator**:状态机转移(mock 传输注入 401/200/超时);心跳超时→重注册退避。
- **media-rtp**:RTP 头/序号递增;PS 封装字节结构;FileSource 循环边界。
- **scenario**:合法/非法场景解析;DeviceConfig 生成。
- **stress-engine**:成功率计算;爬坡速率(模拟时钟);指标并发聚合。

## Mock 与确定性

- 平台交互用 mock transport,注入固定响应,保证测试确定、不依赖网络。
- 时间相关(重传、心跳、爬坡)用可注入时钟,避免 `sleep` 真等待。

## CI(规划)

M0 后接入:`cargo fmt --check` + `cargo clippy -D warnings` + `cargo test` + 前端 `npm run build`。全绿方可合入 develop。

## 验收测试映射

里程碑验收标准见 `roadmap.md`;用例场景见 `00-product/use-cases.md`。E2E 按 UC-1(单设备)/ UC-2(压测)执行。
