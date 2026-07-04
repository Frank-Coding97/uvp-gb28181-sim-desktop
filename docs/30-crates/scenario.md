# crate: scenario

**状态:M3 完成** · 压测场景模型与设备批量生成。

> 变更:配置格式由 YAML 改为 **TOML**(与 Rust 生态更契合,示例见 examples/scenarios/linear.toml)。
> `LinearScenario` 从基础 20 位 ID 用 u128 递增生成 N 个 `DeviceConfig`,支持媒体档 A/B/C、
> 每设备多通道;`Scenario` trait 便于未来扩展其它生成策略。单测 2 个。

## 职责

把 TOML 压测场景解析成强类型结构,供 stress-engine 调度。纯数据 + 解析 + 设备批量生成,无调度逻辑。

## 公开 API(已实现)

`Scenario` 是一个 **trait**(便于未来扩展其它生成策略);当前唯一实现是结构体 `LinearScenario`(连续 ID 递增)。

```rust
pub trait Scenario { /* 批量生成 DeviceConfig 等 */ }

pub struct LinearScenario {
    pub base_device_id: String,       // 起始 20 位 ID(u128 递增)
    pub password: String,
    pub server_host: String,
    pub server_port: u16,
    pub server_domain: String,
    pub transport: Transport,         // UDP / TCP
    pub heartbeat_interval_secs: u64,
    pub channels_per_device: usize,
    pub device_info: DeviceInfoTemplate,
    pub media_profile: MediaProfile,  // A / B / C
    pub video_source: Option<String>,
    pub video_fps: u32,
    pub bitrate_kbps: u32,
    pub active_ratio: f32,
    pub ramp_per_second: u32,         // 爬坡:每秒拉起台数(0=一次全拉起)
    pub gb_version: GbVersion,        // 2016 / 2022
}

impl LinearScenario {
    pub fn from_toml(path) -> Result<Self>;
    pub fn from_toml_str(text: &str) -> Result<Self>;
}
```

## 数据类型(已实现)

- `LinearScenario`(见上)。
- `DeviceInfoTemplate { device_name, manufacturer, model, firmware }`。
- `MediaProfile { A, B, C }`:A=空媒体、B=轻量伪流、C=真实文件(FR-23/24)。

TOML 结构见 `10-functional/stress-testing.md#3` 与 `examples/scenarios/linear.toml`,字段平铺在顶层(无 `scenario:` 包裹)。

## 错误
解析失败 → `common::Error::Config(...)`。

## 依赖
common、gb28181-simulator、serde、`toml = "0.8"`。

## 里程碑
- M0:结构骨架。
- M3(完成):`from_toml`/`from_toml_str` 解析、`LinearScenario` 从基础 20 位 ID 用 u128 递增生成 N 个 `DeviceConfig`、媒体档 A/B/C、每设备多通道。

## 测试
已有:示例场景解析、A/C 档生成(单测 2 个)。待加:非法场景报错、边界(count=0)、DeviceConfig 生成正确性。
