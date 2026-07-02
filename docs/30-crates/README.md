# crate 规格总览

**状态:草案** · 每个 Rust crate 一份规格,是对应开发线的**实现契约**。统一模板:职责 / 公开 API / 数据类型 / 错误 / 依赖 / 里程碑 / 测试。

## 依赖方向(禁止环依赖)

```
common → sip-core / gb28181-protocol / media-rtp
       → gb28181-simulator → scenario → stress-engine → apps/desktop/src-tauri
```

## 清单与 worktree 归属

| crate | 规格 | worktree / 线 | 主里程碑 |
|---|---|---|---|
| common | [common.md](common.md) | 共享(develop) | M0 ✅ |
| sip-core | [sip-core.md](sip-core.md) | `feat/protocol` | M1 |
| gb28181-protocol | [gb28181-protocol.md](gb28181-protocol.md) | `feat/protocol` | M1/M2 |
| gb28181-simulator | [gb28181-simulator.md](gb28181-simulator.md) | `feat/protocol` | M1/M2 |
| media-rtp | [media-rtp.md](media-rtp.md) | `feat/media` | M2 |
| scenario | [scenario.md](scenario.md) | `feat/protocol`→引擎 | M3 |
| stress-engine | [stress-engine.md](stress-engine.md) | 集成 | M3 |

详细分层见 [../20-architecture/overview.md](../20-architecture/overview.md)。
