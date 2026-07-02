//! 视频源抽象(可插拔)。
//!
//! M0 占位。规划三类实现,对应压测三档媒体强度:
//! - `NoneSource`:空媒体(不推流,只维持信令)—— 压测 A 档
//! - `LightSource`:轻量伪 PS 包 —— 压测 B 档
//! - `FileSource`:真实 H.264/H.265 文件循环 —— 压测 C 档 / 单设备联调
//!
//! M2 实现 `FileSource`,M3 实现 `None`/`Light`。
