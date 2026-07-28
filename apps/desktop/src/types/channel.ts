// M5 目录查询三件套前后端共享类型定义.
// 与 Go 侧 pkg/ipc/handlers.go 的 ChannelDTO / ApplyConfigReq 严格一致 (camelCase).

/** 通道类型 (S-B4: 类型码 video=132 / alarm=134). */
export type ChannelType = "video" | "alarm";

/** 编码模式 (D7: 严格三模式声明+实际一致). */
export type EncodingMode = "GB18030" | "GB2312" | "UTF-8";

/** 通道 DTO. */
export interface Channel {
  /** 20 位国标编码. */
  id: string;
  name: string;
  type: ChannelType;
  manufacturer: string;
  model: string;
  owner: string;
  /** 行政区划码 (默认 340200). */
  civilCode: string;
  /** 安装地址 (可空). */
  address: string;
  /** ON / OFF. */
  status: "ON" | "OFF";
  /** 父设备 ID (由 ConfigStore 强制填, 前端传 "" 会被覆盖). */
  parentId: string;
}

/** apply_config 请求 (D1 唯一保存路径). */
export interface ApplyConfigReq {
  encoding: EncodingMode;
  channels: Channel[];
}

/** apply_config 响应. */
export interface ApplyConfigResp {
  encoding: EncodingMode;
  channels: Channel[];
}

/** healthz 响应 (D6 hydrate 门禁探针). */
export interface HealthzResp {
  ready: boolean;
  version: string;
}

/** get_manscdp_encoding 响应. */
export interface EncodingResp {
  encoding: EncodingMode;
}

/** hydrate 三态结果 (D5). */
export type HydrateResult =
  | { kind: "fresh" }
  | { kind: "restored" }
  | { kind: "corrupted"; err: string };

/** localStorage key. */
export const STORE_CHANNELS = "uvp_channels";
export const STORE_ENCODING = "uvp_manscdp_encoding";
