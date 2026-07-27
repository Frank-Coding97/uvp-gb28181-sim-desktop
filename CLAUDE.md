# UVP GB28181 Desktop 项目指引

> **面向 AI 编程助手**的操作手册。编码规范、v2 重构边界、开发命令、需避开的地雷。

---

## 项目定位

把普通电脑模拟成 GB/T 28181-2022 国标设备（IPC/NVR），注册到上级平台（WVP/LiveGBS 等）进行联调与压力测试。

**技术栈**（v2 定型 2026-07-27）：
- **前端**: Vue 3 + Naive UI + Vite + TypeScript
- **桌面壳**: Tauri 2 (Rust,只做窗口 + IPC 转发,不含业务)
- **业务后端**: **Go daemon** (通过 Tauri sidecar 打包)
- **SIP 库**: `github.com/emiago/sipgo v1.4.0` (官方版,遇问题现遇现改)
- **通信**: Tauri (Rust) ↔ Go daemon 走 localhost IPC (具体协议 plan 阶段定)

**架构拓扑**：
```
Vue UI
  ↓ Tauri IPC (invoke/emit)
Rust Tauri 壳 (apps/desktop/src-tauri/)
  ↓ localhost IPC
Go daemon (apps/daemon/)
  ├─ SIP 栈 (sipgo)
  ├─ GB28181 方言层 (GB18030/MANSCDP/SSRC)
  ├─ 设备状态机 (RegistrationSession/DeviceRuntime)
  ├─ 媒体推流 (H.264 → PS → RTP)
  └─ 压测引擎 (万级虚拟设备)
```

**仓库结构**（v2）：
- `apps/desktop/` — Tauri 桌面壳
  - `src-tauri/` — Rust 转发层（几百行,不含业务）
  - `src/` — Vue 3 前端
- `apps/daemon/` — Go 业务后端（v2 主要工作在此,尚未创建）
- `.claude/CLAUDE.md` — 本文件

---

## v1 vs v2 现状

### v1（develop 分支）

可运行基线，包含：
- 单设备模拟（注册/心跳/目录查询/实时点播/历史回放/PTZ/报警/位置/配置/预置位/录像下载）
- 压测引擎（万级虚拟设备并发注册+心跳+点播）
- 多通道目录 CRUD（前端 Channels.vue + 后端 load_catalog_template / upsert_channel / remove_channel）
- Dashboard 监控（压测指标/报告导出）

### v2（v2 分支，**重构中**）

**当前状态**（2026-07-27）：
- 前端：Simulator.vue 单页保留（UI 老板满意，不动）+ App.vue 壳
- Tauri 后端：清空为骨架（`AppState {} + run()` 入口，零 tauri command）
- **crates/*（7 个 Rust crate + 12k+ 行）全部删除，docs/ 全部删除，examples/ 删除**
- v2 后端从零开始用 **Go** 重写

**v2 重构路线图**（Codex 联合评审 2026-07-27 · 语言选型 2026-07-27）：

**Round 1 — 语言选型**（已定）：
- Rust 只保留 Tauri 壳，业务后端切 **Go**
- SIP 库定 `emiago/sipgo v1.4.0`（姊妹项目 UVP-GB28181 已 GB28181 生产验证）

**Round 2 — 按国标协议功能点推进**（进行中）：
1. **注册闭环** ← 当前 spec 阶段 · `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/specs/gb28181-register.md`
2. 目录查询与订阅（Catalog Query + Subscribe NOTIFY）
3. 设备信息/状态查询（DeviceInfo / DeviceStatus）
4. 实时点播 INVITE + 媒体推流（H.264 → PS → RTP）
5. PTZ 控制 + 预置位 + 看守位
6. 报警上报（Alarm Notify）
7. 历史回放 + 倍速播放 + 录像下载
8. 压测能力（万级并发，daemon 内建）

**v2 硬约束**（AI 必须遵守）：
- ❌ **禁止**回 develop 分支修改 v1 遗留代码（都已 nuke，别翻回来）
- ❌ **禁止**从 v1 代码 `git cherry-pick` 或拷贝到 v2（避免 v1 隐雷跟进）
- ❌ **禁止**在 Rust 侧写 SIP/GB28181 业务逻辑（业务全部在 Go daemon）
- ✅ 可以参考 v1 实现思路（`git show develop:<path>`），但要重写不拷贝
- ✅ 可以参考姊妹项目 `/Users/menglulu/code/uvp/UVP-GB28181` 的 Go/sipgo 用法（**只读不写**）

---

## 开发命令

### 前端（apps/desktop/）

```bash
# 开发模式(热重载)
npm run tauri dev

# 类型检查
npx vue-tsc --noEmit

# 构建(类型检查 + Vite 打包)
npm run build
```

### Tauri Rust 壳（apps/desktop/src-tauri/）

```bash
cd apps/desktop/src-tauri
cargo check      # 检查编译
cargo fmt        # 格式化(提交前必跑)
cargo clippy --all-targets -- -D warnings  # Lint(0 警告)
cargo test       # 单元测试
```

### Go daemon（apps/daemon/，尚未创建）

```bash
cd apps/daemon
go mod tidy      # 依赖整理
go build ./...   # 全量编译
go test ./...    # 单元测试
go vet ./...     # 静态检查
gofmt -w .       # 格式化(提交前必跑)
golangci-lint run  # Lint(需装 golangci-lint)
```

### 全量验证（提交前三件套）

**前端**: `npx vue-tsc --noEmit && npm run build`
**Rust 壳**: `cargo fmt && cargo clippy -- -D warnings && cargo test`
**Go daemon**: `gofmt -w . && go vet ./... && go test ./...`

---

## 编码规范

### 通用原则

#### 1. 单一职责（SRP）

**函数**：一个函数只做一件事，典型 <50 行，超过拆分。

```rust
// ❌ 差：一个函数干三件事
fn handle_invite(req: SipRequest) -> Result<()> {
    validate_sdp(&req)?;
    start_rtp_stream()?;
    send_200_ok()?;
    Ok(())
}

// ✅ 好：拆成三个独立函数
fn handle_invite(req: SipRequest) -> Result<()> {
    let sdp = validate_and_parse_sdp(&req)?;
    let stream_id = start_rtp_stream(&sdp)?;
    send_invite_ok(&req, stream_id)
}
```

**模块/文件**：200-400 行典型，800 行封顶。超过拆文件（按领域/协议/生命周期）。

#### 2. 设计模式优先场景

| 模式 | 何时用 | 本项目典型 |
|---|---|---|
| **Builder** | 参数 >5 个 / 有可选参数 / 需逐步构造 | `DeviceConfig::builder()` |
| **Strategy** | 同一接口多种实现可运行时切换 | `Transport` trait (UDP/TCP) |
| **Observer** | 一对多事件通知 | `DeviceObserver` (设备状态变更推给 UI) |
| **State** | 对象行为随内部状态变化 | SIP 事务状态机(Trying→Proceeding→Completed) |
| **Adapter** | 对接外部库/协议到内部接口 | `CameraStream` 封装 ffmpeg |
| **Repository** | 数据访问统一抽象 | (v2 若加持久化考虑引入) |

**反模式警告**：
- ❌ God Object：一个结构体字段 >15 个 → 按职责拆
- ❌ 过早抽象：只有 1-2 个实现就搞 trait → 等到 3 个再抽象
- ❌ 贫血模型：数据结构 + 一堆游离函数 → 方法归到结构体

#### 3. 命名约定

**Rust**：
- `snake_case`：函数/变量/模块 (`send_message` / `device_id` / `sip_core`)
- `PascalCase`：类型/trait (`DeviceSimulator` / `Transport`)
- `SCREAMING_SNAKE_CASE`：常量 (`DEFAULT_PORT` / `MAX_RETRY`)
- 缩写全大写或全小写：`SipRequest`(好) / `SIPRequest`(差) / `RtpPacket`(好) / `RTPPacket`(差)

**Vue**：
- 组件文件：`PascalCase.vue` (`Simulator.vue` / `Dashboard.vue`)
- 组合式函数：`use` 前缀 (`useDeviceStatus` / `useSipTrace`)
- 事件处理：`on` 前缀 (`onClick` / `onDeviceStateChange`)
- ref 变量：描述性名词 (`deviceLive` / `registered` / `sipConfig`)

**通用**：
- 布尔变量：`is_` / `has_` / `can_` / `should_` 前缀 (`is_registered` / `has_video` / `can_retry`)
- 集合：复数或 `_list` / `_map` (`channels` / `device_map`)
- 临时/中间：`tmp_` 前缀避免（直接用描述性名字：`parsed_sdp` 比 `tmp` 好）

---

### Rust 编码规范（仅 Tauri 壳）

> **注意**：v2 里 Rust 只写 Tauri 壳（转发 IPC 到 Go daemon），几百行代码。业务逻辑一律不允许写在 Rust 侧。

#### 1. 错误处理

**强制**：
- 公开 tauri command 返回 `Result<T, String>`（前端友好的错误消息）
- 内部函数用 `anyhow::Result<T>`（原型阶段够用，正式定型后可换 `thiserror`）
- 可恢复路径**禁止** `unwrap()` / `expect()` / `panic!`（测试/启动初始化/示例代码除外）

```rust
// ❌ 差：unwrap 会 panic
let port = config.get("port").unwrap().parse::<u16>().unwrap();

// ✅ 好：显式传播错误
let port_str = config.get("port")
    .ok_or_else(|| Error::ConfigMissing("port".into()))?;
let port = port_str.parse::<u16>()
    .map_err(|e| Error::ConfigInvalid(format!("port 非法: {e}")))?;

// ✅ 更好：用 ? 简化
let port = config.get("port")
    .ok_or_else(|| Error::ConfigMissing("port".into()))?
    .parse::<u16>()?;
```

**日志分级**：
- `error!`：不可恢复错误（连接断开/注册失败/关键资源耗尽）
- `warn!`：可恢复但值得关注（重传/超时/降级）
- `info!`：关键状态变更（注册成功/点播开始/设备上线）
- `debug!`：详细执行流程（SIP 报文内容/RTP 包统计）
- `trace!`：极详细调试（每帧时间戳/每个字节）

#### 2. 生命周期与所有权

**优先**：
- 能 `&T` 不 `&mut T`，能 `&mut T` 不 `T`（最小权限原则）
- 能 `&str` 不 `String`，能 `&[T]` 不 `Vec<T>`（参数用借用）
- 返回值能 `Cow<'a, str>` 考虑用（避免不必要克隆）

**Arc/Rc 使用**：
- 单线程共享用 `Rc`，多线程用 `Arc`
- 需要内部可变用 `Arc<Mutex<T>>` 或 `Arc<RwLock<T>>`（读多写少优先 RwLock）
- 避免 `Arc<Mutex<Arc<Mutex<T>>>>` 嵌套（设计有问题，重新分层）

**tokio::sync vs std::sync**：
- 异步上下文（`async fn` 里）统一用 `tokio::sync::{Mutex, RwLock}`
- 锁内不 `.await` 用 `std::sync::{Mutex, RwLock}` 或 `parking_lot`

**Rust 壳的实际使用**：壳只做 IPC 转发（收前端 invoke → 转 Go daemon → 拿结果回前端），主要用 `tokio::process::Command`（起 daemon）/ `tokio::net::UnixStream` 或 `TcpStream`（与 daemon 通信）/ `tokio::sync::mpsc`（内部消息）。异步 Rust 高阶特性不会用到。

#### 3. 异步 Rust

**spawn 规范**：
- 必须命名或注释用途：`tokio::spawn(async move { /* SIP 心跳 keepalive */ })`
- 后台任务用 `JoinHandle` 持有或走 `tokio::select!` 管理取消
- 避免孤儿任务（spawn 完就忘，无法优雅停止）

**`.await` 位置**：
- 持锁时不 `.await`：
  ```rust
  // ❌ 差：锁跨 await 点
  let mut guard = state.lock().await;
  let result = some_async_call().await;  // 锁一直持有
  guard.value = result;
  
  // ✅ 好：锁前完成异步
  let result = some_async_call().await;
  let mut guard = state.lock().await;
  guard.value = result;
  drop(guard);  // 显式释放（可选但清晰）
  ```

**Channel 选择**：
- 一次性通知用 `oneshot`
- 多生产/多消费用 `mpsc`（有界 `mpsc::channel(N)` 优先，避免无界内存膨胀）
- 广播用 `broadcast`（注意 `RecvError::Lagged` 处理，见下文地雷）

#### 4. 类型设计

**newtype 模式**：业务 ID/单位包一层避免混用

```rust
// ✅ 好：类型安全
pub struct DeviceId(String);
pub struct ChannelId(String);

fn query_channel(device: DeviceId, channel: ChannelId) { }
// 编译期防止 query_channel(channel_id, device_id) 传反

// ❌ 差：全是 String 容易传错
fn query_channel(device: String, channel: String) { }
```

**Enum 优于布尔**：状态 >2 种或将来可能扩展用枚举

```rust
// ❌ 差：布尔组合爆炸
struct Connection {
    is_connected: bool,
    is_registered: bool,
    is_retry: bool,
}

// ✅ 好：状态枚举
enum ConnectionState {
    Disconnected,
    Connected,
    Registered,
    Retrying { attempt: u32 },
}
```

#### 5. 并发与性能

**原则**：
- 热路径避免锁（用 `Arc<AtomicU64>` 或无锁数据结构）
- 批处理优于逐个（100 次 1KB write 不如 1 次 100KB write）
- 预分配容器：`Vec::with_capacity(n)` / `String::with_capacity(n)`

**示例**：
```rust
// ❌ 差：每条 trace 都抢锁
for msg in messages {
    state.lock().await.traces.push(msg);  // N 次锁
}

// ✅ 好：批量加锁
let batch: Vec<_> = messages.collect();
state.lock().await.traces.extend(batch);  // 1 次锁
```

---

### Go 编码规范（业务后端）

> **核心原则**：v2 业务全在 Go daemon 里，风格对齐姊妹项目 `/Users/menglulu/code/uvp/UVP-GB28181` (Gin 后端)，特别是 SIP 相关代码。

#### 1. 命名

- **包名**：短小、全小写、无下划线（`sip` / `manscdp` / `device` / `registration`）
- **导出符号**：`PascalCase` (`RegistrationSession` / `DigestAuth` / `NewDevice`)
- **未导出符号**：`camelCase` (`parseCatalog` / `sendKeepalive`)
- **常量**：`PascalCase` (`DefaultRegisterExpires`) — 不用 `SCREAMING_SNAKE_CASE`（Go 风格）
- **接口**：动词或 `-er` 结尾 (`Transport` / `Codec` / `Registrar`)，单方法接口常用 `-er` (`Reader` / `Closer`)
- **错误变量**：`Err` 前缀 (`ErrRegisterTimeout` / `ErrPlatformRejected`)
- **文件名**：`snake_case.go` (`registration_session.go` / `digest_auth.go`)

#### 2. 错误处理

**强制**：
- **每一个 error 显式检查**（不用 `_ = fn()` 吞错），除非该函数明确无副作用
- 错误在**边界层**包装上下文（用 `fmt.Errorf("register to %s failed: %w", host, err)`），内部层直接传
- 用 `errors.Is` / `errors.As` 判定错误类型，不用 `err.Error() == "..."`
- 定义领域错误用 `var ErrXxx = errors.New(...)` 或自定义类型

```go
// ✅ 好：包装上下文 + 传递
func (s *RegistrationSession) register(ctx context.Context) error {
    req, err := s.buildRegisterRequest()
    if err != nil {
        return fmt.Errorf("build register request: %w", err)
    }
    resp, err := s.transport.Send(ctx, req)
    if err != nil {
        return fmt.Errorf("send register to %s: %w", s.serverAddr, err)
    }
    return s.handleResponse(resp)
}

// ❌ 差：吞错 / 字符串比较
func (s *RegistrationSession) register(ctx context.Context) error {
    req, _ := s.buildRegisterRequest()  // 吞错
    resp, err := s.transport.Send(ctx, req)
    if err != nil && err.Error() == "timeout" {  // 字符串比较易碎
        return err
    }
    return nil  // 万一 send 失败但错误信息不是 "timeout" 就吞了
}
```

#### 3. 并发

**强制**：
- **每个 goroutine 必须有明确的退出机制**（context 取消 / channel close / 显式 done 信号）
- **禁止孤儿 goroutine**（起了忘了怎么停）
- 用 `context.Context` 树形取消（父 ctx cancel → 所有子任务收到 Done）
- Channel 用途要显式：数据传递用 buffered chan，事件通知用 `chan struct{}`，取消用 context

```go
// ✅ 好：ctx 树形取消
func (d *Device) Start(ctx context.Context) error {
    heartbeatCtx, cancelHB := context.WithCancel(ctx)
    renewalCtx, cancelRN := context.WithCancel(ctx)
    d.stopFns = []context.CancelFunc{cancelHB, cancelRN}

    go d.heartbeatLoop(heartbeatCtx)
    go d.renewalLoop(renewalCtx)
    return nil
}

func (d *Device) Stop() {
    for _, cancel := range d.stopFns {
        cancel()
    }
}

// ❌ 差：起了 goroutine 没法停
func (d *Device) Start() {
    go func() { for { d.heartbeat() } }()  // 泄漏
}
```

#### 4. 定时器（重要）

**强制**：
- **万级设备场景不为每个设备起独立 `time.Ticker`**（10k ticker = 10k goroutine + 定时器堆）
- 用**集中 timer wheel** 或分片调度器（多个设备共享少量调度 goroutine）
- 单设备场景（当前 spec 目标）可以每设备用 `time.Ticker`，但要在 stop 时 `.Stop()`

```go
// ✅ 单设备 OK
ticker := time.NewTicker(60 * time.Second)
defer ticker.Stop()
for {
    select {
    case <-ctx.Done():
        return
    case <-ticker.C:
        d.sendKeepalive()
    }
}

// ❌ 万级设备场景：Ticker 太多
// 10000 台设备 = 10000 goroutine + 10000 Ticker → 内存膨胀
// 改为集中调度器：一个 goroutine 管所有到期事件
```

#### 5. 内存与 GC 敏感区

**热路径避免高频分配**（每设备心跳 60s 一次 × 10k = 每分钟 10k SIP 消息构造）：

- 用 `sync.Pool` 缓存 `bytes.Buffer` / SIP 消息对象
- 字符串拼接用 `strings.Builder`（避免 `+` 连接产生大量临时字符串）
- 复用 `[]byte` slice（预分配 capacity）
- XML 编码用流式（`xml.NewEncoder(w)`）而非 `xml.Marshal`（后者会先分配完整 buffer）

```go
// ✅ 好：Buffer pool
var bufPool = sync.Pool{
    New: func() any { return &bytes.Buffer{} },
}

func encodeCatalog(nodes []*Node) []byte {
    buf := bufPool.Get().(*bytes.Buffer)
    defer func() { buf.Reset(); bufPool.Put(buf) }()
    xml.NewEncoder(buf).Encode(nodes)
    return append([]byte(nil), buf.Bytes()...)  // copy 一份返出
}
```

#### 6. 类型设计

**优先小接口**（Go 惯用）：
```go
// ✅ 好：小接口
type Transport interface {
    Send(ctx context.Context, msg *sip.Message) error
}

// ❌ 差：大接口（10 个方法）
// 一次用不到几个方法，测试 mock 也累
```

**接受接口，返回具体类型**：
```go
// ✅ 好
func NewDevice(t Transport) *Device { ... }

// ❌ 差
func NewDevice(t *UDPTransport) *Device { ... }
```

**struct 字段用指针 vs 值**：
- 大 struct（>64 字节）传指针避免拷贝
- 小 struct（<= 64 字节）传值更快（栈上）
- 需要 mutation 用指针

#### 7. 日志

对齐 Rust 侧的分级习惯（用 `log/slog` 或 `zap`）：
- `Error`：不可恢复错误
- `Warn`：可恢复但值得关注
- `Info`：关键状态变更（注册成功/点播开始/设备上线）
- `Debug`：详细执行流程（SIP 报文内容）

**结构化日志**：
```go
// ✅ 好：结构化字段
slog.Info("register success",
    "device_id", d.ID,
    "server", d.serverAddr,
    "cseq", cseq)

// ❌ 差：字符串拼接
slog.Info(fmt.Sprintf("device %s registered to %s", d.ID, d.serverAddr))
```

---

### Vue 3 编码规范

#### 1. 组合式 API 优先

**强制** `<script setup lang="ts">`，禁止 Options API（除非对接遗留组件）。

#### 2. 响应式使用

**ref vs reactive**：
- 基本类型/单值：`ref` (`const count = ref(0)`)
- 对象且需整体替换：`ref` (`const user = ref<User | null>(null)`)
- 对象且只改属性：`reactive` (`const state = reactive({ count: 0 })`)

**computed 纯函数**：
```rust
// ❌ 差：computed 有副作用
const statusText = computed(() => {
    console.log('计算中...');  // 副作用
    api.logAccess();           // 副作用
    return registered.value ? '在线' : '离线';
});

// ✅ 好：纯计算
const statusText = computed(() => 
    registered.value ? '在线' : '离线'
);
```

#### 3. Props / Emits 类型化

```typescript
// ✅ 好：运行时+类型双重校验
interface Props {
    deviceId: string;
    channels?: ChannelNode[];
}
const props = defineProps<Props>();

interface Emits {
    (e: 'update:modelValue', value: string): void;
    (e: 'close'): void;
}
const emit = defineEmits<Emits>();
```

#### 4. 生命周期

**onMounted / onUnmounted 对称**：
```typescript
// ✅ 好：订阅与清理对称
onMounted(() => {
    const unlisten = listen('device_state', handler);
    onUnmounted(() => unlisten());  // 嵌套写法自动绑定
});

// ❌ 差：忘记清理
onMounted(() => {
    listen('device_state', handler);  // 泄漏
});
```

#### 5. Tauri IPC

**invoke 类型安全**：
```typescript
// ✅ 好：带泛型
const status = await invoke<{ running: boolean }>('get_device_status');

// ❌ 差：any 类型
const status = await invoke('get_device_status');
```

**批量调用并行**：
```typescript
// ✅ 好：Promise.all
const [status, cameras] = await Promise.all([
    invoke<DeviceStatus>('get_device_status'),
    invoke<CameraDevice[]>('list_cameras'),
]);

// ❌ 差：串行
const status = await invoke('get_device_status');
const cameras = await invoke('list_cameras');  // 等上一个完成才发
```

---

## 需避开的地雷

> v1 遗留地雷 = 历史证据（说明为什么切 Go）+ v2 若照抄 v1 思路会撞上的同一堵墙。
> Go daemon 新地雷 = v2 特有需要注意的坑。

### v1 历史地雷（Go 侧要主动规避）

#### V1-1: SIP 事务层不完整

**v1 症状**：Rust 自研 sip-core 只用 `Call-ID + CSeq` 匹配响应，缺 `Via branch + Method` 事务键。重传/并发/CANCEL-ACK 场景下响应误匹配。

**v2 处理**：sipgo 内建完整 RFC 3261 17.1.3 事务查找。**不要自己在 Go 侧再造 SIP 事务层**。

#### V1-2: broadcast::Lagged 花屏

**v1 症状**：Rust 用 `tokio::broadcast` 分发 H.264 帧，消费慢的订阅者收到 `RecvError::Lagged` 后继续解码会花屏（缺 SPS/PPS）。

**v2 处理**：Go 侧媒体分发时，慢消费者要**追到最新 IDR 并补发 SPS/PPS**，不消费完整队列。

#### V1-3: 逐帧 JSON IPC（Vue ↔ Rust）

**v1 症状**：`Simulator.vue` 从 Rust 收 H.264 帧走 JSON `number[]`，每帧序列化 + GC。

**v2 处理**：Vue ↔ Rust ↔ Go 三层间媒体流走**二进制通道**：Go daemon localhost WebSocket 推 Annex-B，前端 WebCodecs 直接解码，中间 Rust 壳只做端口路由。

#### V1-4: SSRC 前导零

**v1 症状**：部分平台（LiveGBS）要求 SSRC 固定 10 位数字，前导零不能丢（`0012345678` ≠ `12345678`）。

**v2 处理**：Go 侧构造 SSRC 用 `fmt.Sprintf("%010d", ssrc)`。

#### V1-5: SIP Request-URI 用 server_id 不用 server_domain

**v1 症状**：REGISTER Request-URI 的 user 部分要用 20 位平台 SIP ID（server_id），不是 10 位域（server_domain）。v1 修过这个 bug（`e2f189a`）。

**v2 处理**：spec 已明确（Q5），Go 侧构造 REGISTER 时用 server_id。

#### V1-6: TCP setup 协商属性

**v1 症状**：某些平台走 SIP over TCP 时要求 setup 属性协商。

**v2 处理**：sipgo 内建 TCP 传输，若遇到平台方言可加 write observer 拦截（参考 UVP-GB28181 的 UVP_PATCH.md 模式）。

### Go daemon 新地雷（v2 特有）

#### G1: `sipgo` 无 GB28181 生产先例（设备方向 UAC）

**症状**：sipgo 主要生产场景是 SIP proxy / PBX / registrar（UAS 方向），姊妹项目 UVP-GB28181 是**平台方向 UAS**。v2 是**设备方向 UAC**，sipgo 的 UAC API 使用姿势需实测验证。

**规避**：注册闭环的第一个里程碑就是"跑通向 WVP-Pro 的最小 REGISTER"，验证通过再做其他功能。

#### G2: goroutine 泄漏

**症状**：Go 起 goroutine 太便宜，容易忘记停止。心跳 goroutine + 续约 goroutine + 传输层各自独立起就会残留。

**规避**：所有 goroutine 必须绑定 `context.Context`，`select { case <-ctx.Done() }` 退出。启动一个 goroutine 前先想清楚**谁负责停它**。

#### G3: GC 热路径分配

**症状**：万级设备场景每分钟 10k+ SIP 消息构造，若每次都新建 `bytes.Buffer` / string / XML DOM，GC 压力大。

**规避**：热路径用 `sync.Pool` 缓存 buffer，字符串用 `strings.Builder`，XML 用流式 `xml.NewEncoder`。**先做正确再做优化**：单设备阶段不用担心，压测阶段用 `pprof` 定位。

#### G4: 定时器惊群

**症状**：万级设备每台起 `time.NewTicker` = 万级定时器堆 + 万级 goroutine。

**规避**：**万级压测阶段用集中 timer wheel**（一个调度 goroutine 管所有到期事件）。单设备阶段用 ticker 无所谓。

#### G5: sipgo 抽象泄漏

**症状**：sipgo 内建报文构造，某些字段可能强制封装（比如 Contact 的 tag 参数、Via 的 rport），跟 GB28181 平台方言冲突。

**规避**：先用 sipgo 默认行为，遇到问题走两条路：
1. **短期**: fork sipgo 加 write observer 拦截字段（参考 UVP-GB28181 UVP_PATCH.md）
2. **长期**: 给 sipgo 提 upstream issue

### 通用地雷

#### T1: 中文路径 Write 工具 bug

**症状**：`Write` 工具对中文路径 + 大文件（>200 行）高频触发 `InputValidationError`。

**规避**：
- 新建文档路径全用英文/拼音 kebab-case（`register-spec.md` 不 `注册-spec.md`）
- 中文标题挂 frontmatter `title:` / `aliases:`
- 超 200 行改用 Bash heredoc 分段写或 Edit 增量

---

## 四件套归档（Spec-Driven + TDD）

按全局 CLAUDE.md 第 4 节"开发工作流"，本项目四件套归档到 **Atlas**（`~/Documents/Atlas/`）而非项目目录：

| 阶段 | 归档路径 | 备注 |
|---|---|---|
| spec | `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/specs/<feature>.md` | 需求规格+验收标准 |
| plan | `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/plans/<feature>.md` | 技术方案+架构选型 |
| tasks | `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/tasks/<feature>.md` | 任务拆解+测试清单 |
| handoff | `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/handoffs/<时间>-<阶段>.md` | 会话切片+续接点 |

**测试代码**留在项目仓库（`crates/*/tests/` / `apps/desktop/src-tauri/src/*.rs` 的 `#[test]` / `apps/desktop/src/__tests__/`），不归档到 Atlas。

**AI 禁止**在项目目录（`/Users/menglulu/code/uvp/uvp-gb28181-sim-desktop/`）下新建 `spec.md` / `plan.md` / `tasks.md` 等长期性文档 —— 统一走 Atlas 路径。

---

## 文档去哪找

v2 极限清场时删掉了 v1 的 `docs/` 目录（对齐"从零重构"原则）。文档现走 Atlas。

| 问题 | 去哪看 |
|---|---|
| 项目当前状态 / v2 路线 / 编码规范 / 地雷 | 本文件（`.claude/CLAUDE.md`）|
| v1 需求/架构参考（历史） | `git show develop:docs/**` |
| GB28181 协议细节 | GB/T 28181-2022 国标原文 + [[UVP-GB28181]] 姊妹项目 CLAUDE.md |
| sipgo 用法 / GB28181 平台方言实践 | `/Users/menglulu/code/uvp/UVP-GB28181/server/` 只读参考 |
| 各功能 spec/plan/tasks | Atlas: `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/` |
| 各功能会话切片 | Atlas: `~/Documents/Atlas/wiki/projects/uvp-gb28181-sim-desktop/handoffs/` |

---

## 提交前检查清单

- [ ] **Vue 前端**: `npx vue-tsc --noEmit && npm run build`
- [ ] **Rust 壳**: `cargo fmt && cargo clippy -- -D warnings && cargo test`
- [ ] **Go daemon**: `gofmt -w . && go vet ./... && go test ./...`
- [ ] `git diff` 确认无意外改动（配置文件/lock 文件/临时文件）
- [ ] commit message 符合 `<type>(<scope>): <中文描述>` 格式
- [ ] v2 分支：未从 develop 拷回 v1 代码 / 未在 Rust 侧写业务
