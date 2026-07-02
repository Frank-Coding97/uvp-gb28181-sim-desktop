# crate: scenario

**状态:M3 完成** · 压测场景模型与设备批量生成。

> 变更:配置格式由 YAML 改为 **TOML**(与 Rust 生态更契合,示例见 examples/scenarios/linear.toml)。
> `LinearScenario` 从基础 20 位 ID 用 u128 递增生成 N 个 `DeviceConfig`,支持媒体档 A/B/C、
> 每设备多通道;`Scenario` trait 便于未来扩展其它生成策略。单测 2 个。

## 职责

把 YAML 压测场景解析成强类型结构,供 stress-engine 调度。纯数据 + 解析,无调度逻辑。

## 公开 API(已实现骨架)

```rust
pub struct Scenario { name, platform, devices, register, heartbeat, media, duration_seconds }
impl Scenario { pub fn from_yaml(text: &str) -> Result<Self>; }
```

## 数据类型(已实现)

- `Platform { host, port, domain }`
- `Devices { count, id_prefix, start_index, password }`(批量生成规则,FR-20)
- `RegisterPlan { enabled, rate_per_second }`(爬坡,FR-21)
- `HeartbeatPlan { enabled, interval_seconds }`
- `MediaMode { None, Light, Real }` + `MediaPlan { enabled, mode, active_ratio, bitrate_kbps, file }`(FR-23/24)

YAML 结构见 `10-functional/stress-testing.md#3` 与 `examples/scenarios/5000-register.yaml`,顶层包在 `scenario:` 键下。

## 错误
解析失败 → `common::Error::Config(...)`。

## 依赖
common、gb28181-simulator、serde、serde_yaml。

## 里程碑
- M0:结构 + from_yaml(已实现,含单测)。
- M3:校验(字段合法性、ID 前缀+序号能补齐 20 位)、`to_yaml`、生成 `Vec<DeviceConfig>`。

## 测试
已有:示例场景解析。待加:非法场景报错、边界(count=0、prefix 长度)、DeviceConfig 生成正确性。
