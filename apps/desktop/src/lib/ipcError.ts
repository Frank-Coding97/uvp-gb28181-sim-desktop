// IPC 错误的统一呈现。
//
// # 为什么需要
//
// 之前 8 处直接 `message.error(String(e))` —— 把 Rust 侧的错误原文原样弹给用户,
// 里面可能是 `网络错误: Address already in use (os error 48)` 这种东西。
// 用户看到的是"发生了一件事",但不知道**该做什么**。
//
// 商业级的错误呈现要回答三个问题:出了什么事(标题)、为什么(细节)、下一步做什么(建议)。
// 这个模块只做最后一件最缺的事:把已知的错误特征映射成一句可执行的建议,
// 并保证原文仍然可见(排障时需要它),但不再是唯一的信息。

/** 一条已归类的错误呈现。 */
export interface PresentedError {
  /** 一句话说清出了什么事(用户语言,不含 os error 之类)。 */
  title: string;
  /** 建议动作。没有匹配到已知特征时为空串。 */
  hint: string;
  /** 后端原文,始终保留 —— 排障要它,而且它是唯一不会被我们的映射弄错的信息。 */
  detail: string;
}

/**
 * 已知错误特征 → 可执行建议。
 *
 * 匹配按顺序,先命中者胜。特征刻意用后端错误里稳定出现的**中文短语或 errno 文本**,
 * 而不是完整文案 —— 后者一改措辞映射就失效。
 */
const RULES: Array<{ match: RegExp; title: string; hint: string }> = [
  {
    match: /Address already in use|已在使用|EADDRINUSE|os error 48|os error 98/i,
    title: "本地端口被占用",
    hint: "可能已有另一个实例在运行,或该端口被其它程序占用。关闭重复实例后重试。",
  },
  {
    match: /Permission denied|权限|os error 13|os error 1/i,
    title: "本地权限不足",
    hint: "绑定端口或访问文件被系统拒绝。低于 1024 的端口需要管理员权限;macOS 上摄像头/屏幕录制需要在「系统设置 → 隐私与安全性」里授权。",
  },
  {
    match: /尚未配置密码|未配置 SIP 密码/,
    title: "该平台档案还没有密码",
    hint: "在顶栏平台设置里填写 SIP 密码并保存(密码存入系统钥匙串,不落配置文件)。",
  },
  {
    match: /设备未启动|没有正在运行的设备/,
    title: "设备未启动",
    hint: "先到「单设备联调」页点「注册」启动设备,再执行此操作。",
  },
  {
    match: /已有压测正在运行|没有正在运行的压测/,
    title: "压测状态不匹配",
    hint: "等待上一次压测完全退出后重试;停止过程可能仍在收敛设备任务。",
  },
  {
    match: /必须是有效的 IP|不支持域名/,
    title: "平台地址格式不对",
    hint: "当前只支持 IP:端口 形式。若平台用域名,请先解析成 IP 填入。",
  },
  {
    match: /未找到 ffmpeg|需要 ffmpeg/i,
    title: "缺少 FFmpeg",
    hint: "容器格式转封装与实时采集依赖 FFmpeg。macOS 可用 brew install ffmpeg;或直接改用 .h264/.h265 裸流文件。",
  },
  {
    match: /尚未经你选择授权|放行媒体预览失败/,
    title: "预览未授权",
    hint: "点「选择文件」重新选取该视频 —— 预览授权只能由你在系统文件对话框里的真实选择产生。",
  },
  {
    match: /连接超时|事务超时|timed? ?out/i,
    title: "与平台通信超时",
    hint: "检查平台 IP/端口是否正确、本机到平台的网络是否可达、以及防火墙是否放行了 SIP 端口。",
  },
  {
    match: /平台拒绝|401|403/,
    title: "平台拒绝了请求",
    hint: "多为凭据或权限问题:核对 SIP 用户名/密码、平台域 ID,以及该设备编号是否已在平台注册。",
  },
  {
    match: /磁盘|No space left|os error 28/i,
    title: "磁盘空间不足",
    hint: "清理磁盘后重试。日志与报告位于应用数据目录,可在系统页查看路径。",
  },
];

/**
 * 把任意抛出物归类成可呈现的错误。
 *
 * 匹配不到已知特征时 `hint` 为空 —— 宁可不给建议,也不要给一句放之四海而皆准
 * 但毫无用处的话("请检查配置")。
 */
export function presentError(error: unknown, fallbackTitle = "操作失败"): PresentedError {
  const detail = normalize(error);
  const rule = RULES.find((r) => r.match.test(detail));
  return {
    title: rule?.title ?? fallbackTitle,
    hint: rule?.hint ?? "",
    detail,
  };
}

/** 拼成一条适合放进 message/toast 的单行文案。 */
export function formatError(error: unknown, fallbackTitle = "操作失败"): string {
  const { title, hint, detail } = presentError(error, fallbackTitle);
  const parts = [title];
  if (hint) parts.push(hint);
  // 原文放最后并加括号:它是排障依据,但不该抢在建议前面。
  if (detail && detail !== title) parts.push(`(${detail})`);
  return parts.join(" ");
}

/** 把 unknown 抛出物转成可读字符串。 */
function normalize(error: unknown): string {
  if (typeof error === "string") return error.trim();
  if (error instanceof Error) return error.message.trim();
  if (error && typeof error === "object") {
    // Tauri 的 invoke 在 Err(String) 时抛出的就是字符串;其它情况尽量序列化。
    try {
      return JSON.stringify(error);
    } catch {
      return String(error);
    }
  }
  return String(error);
}
