# 需求清单

**状态:已评审** · 需求是功能规格与验收的依据,新增/变更需更新本表并同步相关 `10-functional/` 文档。

编号:`FR-x` 功能需求 · `NFR-x` 非功能需求 · 优先级 P0(必须)/P1(重要)/P2(可选)。

---

## 功能需求(FR)

### 设备模拟

| 编号 | 需求 | 优先级 | 里程碑 |
|---|---|---|---|
| FR-1 | 单设备向上级平台 REGISTER 注册,支持 Digest MD5 鉴权与注销 | P0 | M1 |
| FR-2 | 心跳保活(Keepalive),超时自动重注册(指数退避) | P0 | M1 |
| FR-3 | 响应平台 OPTIONS 探活 | P1 | M1 |
| FR-4 | 支持 SIP over UDP 与 TCP | P0 | M1 |
| FR-5 | 响应目录查询(Catalog),支持 GB-2022 字段 | P0 | M2 |
| FR-6 | 响应设备信息(DeviceInfo)/ 设备状态(DeviceStatus)查询 | P0 | M2 |
| FR-7 | 响应实时点播:INVITE 建流 / ACK / BYE 停流 / CANCEL 取消 | P0 | M2 ✅ |
| FR-8 | 推送 H.264/H.265 码流,PS 封装,RTP over UDP/TCP;音视频复合流(G.711A);MP4/FLV/MKV 容器源 | P0 | M2 ✅ |
| FR-9 | 上报报警(Alarm Notify) | P1 | M3+ ✅ |
| FR-10 | 响应录像查询(RecordInfo)、历史回放、倍速播放、录像下载 | P2 | M3+ ✅ |
| FR-11 | GB-2016 / GB-2022 版本切换 | P1 | M2 ✅ |
| FR-12 | 网络校时(解析平台 REGISTER 200 OK 的 Date 头对齐时钟) | P1 | M3+ ✅ |
| FR-13 | 设备控制:PTZ 云台方向/变倍、预置位设置·调用·删除、看守位、拉框 | P1 | M3+ ✅ |
| FR-14 | 设备配置查询(ConfigDownload)、预置位查询(PresetQuery) | P2 | M3+ ✅ |
| FR-15 | 订阅通知:目录/报警(对话内 NOTIFY)、移动位置周期上报 | P1 | M3+ ✅ |
| FR-16 | 回放控制:会话内 INFO/MANSRTSP 倍速、暂停/恢复 | P2 | M3+ ✅ |
| FR-17 | 扩展查询应答:报警状态(AlarmStatus)、看守位(HomePositionQuery)、存储卡状态(StorageCardStatusQuery)、巡航轨迹列表/详情(CruiseTrackListQuery/CruiseTrackQuery)、PTZ 精准状态(PTZPreciseStatusQuery)、移动位置单次查询;ConfigDownload 增 VideoParamOpt | P1 | M6 |
| FR-18 | 扩展设备控制:精确云台(PTZPreciseCtrl)、巡航轨迹控制(PTZCmd 0x84-0x88)、辅助控制雨刷/红外/加热/除雾/制冷(PTZCmd 0x89/0x8A)、Focus 聚焦/光圈、目标跟踪(TargetTrack)、格式化 SD 卡(FormatSDCard) | P1 | M6 |
| FR-19 | 平台下发抓拍:SnapShotCmd(经 Alarm Notify 上报)+ SnapShotConfig(GB-2022,JPEG 经 HTTP PUT 上传 + 完成 NOTIFY,含 SSRF 上传白名单校验) | P2 | M6 |
| FR-30 | 在线升级 DeviceUpgrade:4 步进度(0/30/60/100%)DeviceUpgradeResult NOTIFY 闭环 | P2 | M6 |
| FR-31 | 主动通知:录像完成/异常/存储满 MediaStatusNotify(NotifyType 121/122/123) | P1 | M6 |
| FR-32 | 信令健壮性:Expires 到期 80% 主动续约、RTCP SR 周期反馈(5s)、Catalog 增量 NOTIFY(状态 diff) | P1 | M6 |
| FR-33 | 媒体编码扩展:H.265(PSM stream_type 0x24)、AAC 音频复合流 | P2 | M6 |
| FR-34 | 多视频通道 + 虚拟通道 CRUD + 模板(单设备/8ch NVR/3×2 跨区划/16ch 双业务分组);报警通道作独立 Catalog 节点(typeCode 134) | P1 | M6 |
| FR-35 | OSD 叠加:时间戳(左上)、通道名(右上)、自定义水印(斜向平铺)烧入推流画面(行业惯例) | P2 | M6 |
| FR-36 | 语音广播(平台→设备,G.711A 下行):Broadcast MESSAGE 应答 + 设备侧反向 INVITE(UAC)+ 音频接收播放 | P2 | M6 |

### 压力测试(核心)

| 编号 | 需求 | 优先级 | 里程碑 |
|---|---|---|---|
| FR-20 | 批量生成 N 台虚拟设备(ID 按序号递增) | P0 | M3 |
| FR-21 | 按爬坡速率(每秒 X 台)分批注册,避免瞬时风暴 | P0 | M3 |
| FR-22 | 万级设备并发维持注册 + 心跳 | P0 | M3 |
| FR-23 | 媒体三档强度:空媒体 / 轻量 RTP / 真实码流 | P0 | M3 |
| FR-24 | 按 active_ratio 抽样部分设备真推流 | P0 | M3 |
| FR-25 | 场景以 YAML 定义,可保存/加载/复跑 | P0 | M3 |
| FR-26 | 实时采集指标:注册/心跳/INVITE 成功率、时延、活跃推流、码率 | P0 | M3 |
| FR-27 | 失败原因归类统计(超时/认证失败/拒绝/网络错误)Top-N | P1 | M3 |
| FR-28 | 压测结束导出报告(含指标曲线与失败明细) | P1 | M4 |

### 桌面应用 / 可视化

| 编号 | 需求 | 优先级 | 里程碑 |
|---|---|---|---|
| FR-40 | 平台连接配置向导(host/port/domain/密码) | P0 | M4 |
| FR-41 | 压测场景可视化编排(表单生成 YAML) | P0 | M4 |
| FR-42 | 运行监控大盘(实时曲线 + 关键指标卡) | P0 | M4 |
| FR-43 | SIP 信令实时查看(可开关,压测大规模时默认关) | P1 | M4 |
| FR-44 | 日志中心(检索、过滤、导出) | P1 | M4 |
| FR-45 | 引擎连通性自检 | P1 | M0 ✅ |

---

## 非功能需求(NFR)

| 编号 | 需求 | 指标 / 说明 |
|---|---|---|
| NFR-1 | 跨平台 | Windows 10+ / macOS 12+ / 主流 Linux 发行版 |
| NFR-2 | 安装简单 | 单安装包,无需 JVM/Python/Docker;安装包体积尽量小 |
| NFR-3 | 并发规模 | 单机空媒体档 ≥ 5000 台设备稳定注册+心跳(目标万级) |
| NFR-4 | 资源可控 | 长时间(≥30min)压测无内存泄漏,CPU/内存随规模线性 |
| NFR-5 | 稳定性 | UI 崩溃不影响引擎;单设备异常不拖垮整体 |
| NFR-6 | 启动性能 | 冷启动到可操作 < 3s |
| NFR-7 | 可维护 | 代码整洁、中文注释;协议/领域逻辑必配单测 |
| NFR-8 | 可扩展 | 引擎可脱离 UI 以 CLI 运行;预留分布式压测架构 |
| NFR-9 | 安全边界 | 明文协议,仅限受控内网/测试环境;不对未授权平台施压 |

---

## 追踪

每条需求应能追踪到:功能规格(`10-functional/`)→ crate 规格(`30-crates/`)→ 代码 PR。里程碑定义见 `90-process/roadmap.md`。
