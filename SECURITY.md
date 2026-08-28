# 安全说明

本工具会连接上级平台、持有平台凭据、并按平台指令向指定地址推送媒体流,因此以下几点
是设计上的硬约束,改动相关代码前请先读这一节。

## 凭据存储

- 平台 SIP 密码**只存 OS 钥匙串**:macOS Keychain / Windows Credential Manager /
  Linux libsecret(实现见 `apps/desktop/src-tauri/src/secrets.rs`,服务名
  `uvp-gb28181-desktop`)。
- 前端(`apps/desktop/src/platform.ts`)**不持有明文**:localStorage 里只有平台地址、
  域 ID、传输方式,以及"该档案是否已配置密码"这个布尔。
- `start_device` / `start_stress` 只接收 `profile_id`,由 Rust 侧现取现用;
  密码不经 IPC 回传 webview,也不写进压测场景 TOML。
- 钥匙串不可用时(无桌面会话的 Linux、被策略禁用)退化为**仅本进程内存**:
  重启后需重新输入,但绝不落磁盘明文。
- 旧版本若在 localStorage 留有明文密码,首次启动会自动搬进钥匙串并抹掉明文
  (`migrateLegacyPasswords`)。
- 导出的压测报告里场景是**结构化脱敏**后再序列化的(`serialize_redacted_scenario`),
  不是对 TOML 文本做正则替换,因此 quoted key、多行字符串等写法都绕不过去。

### 已知的历史泄露

提交 `e310d45bc`(“安全:移除仓库中的平台凭据”)只修改了工作区,**没有重写 git 历史**。
在此之前 `apps/desktop/src/platform.ts` 里硬编码过一个平台 SIP 密码与内网地址,
现在仍可通过 `git show e310d45bc~1:apps/desktop/src/platform.ts` 取出。

如果这个仓库曾经对外可见:

1. **先轮换那个 SIP 密码** —— 历史重写救不回已经泄露的凭据,轮换是唯一有效补救。
2. 需要清理历史时用 `git filter-repo --replace-text`,然后强推并通知所有克隆者
   (破坏性操作,需要明确授权)。
3. CI 已接入 gitleaks(`.gitleaks.toml`),防止同类问题再次进入仓库。

## SIP 入站来源

设备默认只接受来自**配置的上级平台 IP** 的报文
(`SipTransport::trust_only_host`,在 `start_device` 与压测编排器里都会调用)。

不设限的后果很具体:任何主机只要知道设备 AOR 就能注入 PTZ / DeviceConfig / INVITE,
而 INVITE 的 `c=` / `m=` 决定推流目标 —— 那就是一个**可被利用的媒体流量放大器**。
按 IP(而不是 IP:端口)匹配是有意的:部分平台从与 SIP 监听端口不同的源端口发出站请求。

平台地址填域名时无法启用白名单,会打一条 warning;联调环境建议直接填 IP。

## 抓拍上传(SSRF)

`http_put_jpeg` 会**先把目标解析成具体 IP 再判定**,并直接连到已校验的地址,
消除"校验用域名、连接再解析一次"的 DNS rebinding 窗口。域名的**全部**解析结果都必须
安全,只要有一条指向内网就整体拒绝。

拒绝的范围:环回、链路本地(含 `169.254.169.254` 云元数据)、私网、组播、广播、
未指定、文档段、运营商级 NAT(`100.64.0.0/10`)、IPv6 唯一本地与链路本地。

## Webview 权限面

- `capabilities/default.json` 只授予 `core:default` 与 `dialog:allow-open`,
  没有引入 fs / shell / http 插件。
- CSP:`default-src 'self'; script-src 'self'`。`style-src 'unsafe-inline'` 是
  Naive UI 的 CSS-in-JS 所需。前端不使用 `v-html`。
- `assetProtocol.scope` 只放行 `$VIDEO`、`$TEMP/uvp-gb28181`、`$APPDATA/media`。
  用户从其它路径选中的媒体文件通过 `allow_media_preview` 命令**在运行时逐个放行**,
  而不是把文档 / 桌面 / 下载目录常开给 webview。

## 资源上界(拒绝服务面)

- SIP 入站队列有界(每设备 128),满了记 `dropped_queue_full` 而不是无界堆积。
- 单条报文 body 上限 16 MiB,头部上限 1 MiB;TCP 连接缓冲上限 4 MiB。
- 服务端事务表有容量上限,防止伪造 branch 的洪泛打爆内存。
- Annex B 采集缓冲上限 8 MiB,超限丢弃并重新同步(并计数)。

## 报告一份问题

请提交 issue,或直接联系维护者。涉及可利用漏洞时请勿在公开 issue 里附带 PoC 与真实凭据。
