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
| FR-9 | 上报报警(Alarm Notify);报警单管理:类型/等级/方式/经纬度/描述/时间偏移可配、固定单与随机发送、XML 预览、最近 50 条历史(含失败原因与发送路径)、本机复位 | P1 | M3+ ✅ |
| FR-10 | 响应录像查询(RecordInfo)、历史回放、倍速播放、录像下载 | P2 | M3+ ✅ |
| FR-11 | GB-2016 / GB-2022 版本切换 | P1 | M2 ✅ |
| FR-12 | 网络校时(解析平台 REGISTER 200 OK 的 Date 头对齐时钟) | P1 | M3+ ✅ |
| FR-13 | 设备控制:PTZ 云台方向/变倍、预置位设置·调用·删除、看守位、拉框 | P1 | M3+ ✅ |
| FR-14 | 设备配置查询(ConfigDownload)、预置位查询(PresetQuery) | P2 | M3+ ✅ |
| FR-15 | 订阅通知:目录/报警(对话内 NOTIFY)、移动位置周期上报 | P1 | M3+ ✅ |
| FR-16 | 回放控制:会话内 INFO/MANSRTSP 倍速、暂停/恢复 | P2 | M3+ ✅ |
| FR-17 | 扩展查询应答:报警状态(AlarmStatus)、看守位(HomePositionQuery)、存储卡状态(StorageCardStatusQuery)、巡航轨迹列表/详情(CruiseTrackListQuery/CruiseTrackQuery)、PTZ 精准状态(PTZPreciseStatusQuery,六字段 Pan/Tilt/Zoom + 视场角 + 最大可视距离,光学三量随变焦换算)、移动位置单次查询;ConfigDownload 增 VideoParamOpt | P1 | M6 |
| FR-18 | 扩展设备控制:精确云台(PTZPreciseCtrl)、巡航轨迹控制(PTZCmd 0x84-0x88)、辅助控制雨刷/红外/加热/除雾/制冷(PTZCmd 0x89/0x8A)、Focus 聚焦/光圈、目标跟踪(TargetTrack)、格式化 SD 卡(FormatSDCard) | P1 | M6 |
| FR-19 | 平台下发抓拍:SnapShotCmd(经 Alarm Notify 上报)+ SnapShotConfig(GB-2022,JPEG 经 HTTP PUT 上传 + 完成 NOTIFY,含 SSRF 上传白名单校验) | P2 | M6 |
| FR-30 | 在线升级 DeviceUpgrade:4 步进度(0/30/60/100%)DeviceUpgradeResult NOTIFY 闭环 | P2 | M6 |
| FR-31 | 主动通知:录像完成/异常/存储满 MediaStatusNotify(NotifyType 121/122/123) | P1 | M6 |
| FR-32 | 信令健壮性:Expires 到期 80% 主动续约、RTCP SR 周期反馈(5s)、Catalog 增量 NOTIFY(状态 diff) | P1 | M6 |
| FR-33 | 媒体编码扩展:H.265(PSM stream_type 0x24)、AAC 音频复合流 | P2 | M6 |
| FR-34 | 多视频通道 + 虚拟通道 CRUD + 模板(单设备/8ch NVR/3×2 跨区划/16ch 双业务分组);报警通道作独立 Catalog 节点(typeCode 134) | P1 | M6 |
| FR-35 | OSD 叠加:时间戳(左上)、通道名(右上)、自定义水印(斜向平铺)烧入推流画面(行业惯例) | P2 | M6 |
| FR-36 | 语音广播(平台→设备,G.711A 下行):Broadcast MESSAGE 应答 + 设备侧反向 INVITE(UAC)+ 音频接收播放 | P2 | M6 |
| FR-37 | GB28181-2022 合规补全:实时视音频回传通知(VideoUploadNotify A.2.5.8)、强制关键帧拼写兼容(IFrameCmd/IFameCmd)、RecordInfo 应答必选 Name(A.2.6.7)、SVAC 编解码/视频参数属性配置(A.2.3.2.3~5) | P1 | M7 |
| FR-46 | 设备校时可视化:平台基准时间/本机时间/偏移/来源凭证/SIP 回退值/最近失败原因一屏可见;NTP 服务器、端口、刷新间隔可配,NTP 生效期不受 SIP Date 覆盖,失败自动回退 | P1 | M7 |
| FR-47 | 本地抓拍与图库:预览页一键抓拍(取预览总线最新帧,不打断推流)、图库缩略图/尺寸/时间、放大查看、另存为、删除 | P2 | M7 |
| FR-48 | 定时录像计划:周时间表(周一至周日、每天最多 8 段、30 分钟网格 + 秒级编辑、相邻段自动合并)、设备运行期 10s 调度自动起停并标记「定时」来源、ConfigDownload/VideoRecordPlan 回读与实际行为对齐、计划默认关闭 | P1 | M7 |
| FR-49 | 订阅运行详情:四类订阅(位置/目录/报警/PTZ 精准位置)显示订阅者、Expires、剩余秒数倒计时、累计上报次数、最近应答码、最近错误;生命周期区分未订阅/已订阅/已过期/已退订/异常;设备下线清空详情 | P1 | M7 |
| FR-50 | 硬件版本可配:设置页可填硬件版本(≤64 字符、禁 XML 元字符),仅 GB-2022 的 DeviceInfo 应答输出,留空不输出空节点 | P2 | M7 |
| FR-51 | 录像中心增强:列表行首帧缩略图(外部 ffmpeg 按需抽帧 + 磁盘缓存,失败显示占位与原因)、时间范围筛选(全部/今天/近 7 天/近 30 天/自定义,按**时间窗重叠**命中以覆盖跨夜录像)、时长列 | P2 | M7 |
| FR-52 | 通知中心:顶栏铃铛未读角标 + 抽屉列表(最多 50 条),来源为设备状态翻转、平台命令、订阅建立/退订、语音广播、采集错误;周期性事件(位置上报/SIP 流水/指标)不进通知;打开即置读、可单条置读与清空 | P2 | M7 |

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
| FR-44 | 日志中心(检索、过滤、导出):导出当前页签可见内容,`Authorization`/`Proxy-Authorization`/`Password` 在 Rust 侧脱敏且不可恢复 | P1 | M4 ✅ |
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
