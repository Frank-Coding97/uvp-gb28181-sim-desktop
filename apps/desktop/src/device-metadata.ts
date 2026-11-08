/**
 * 设备安装位置与摄像头类型。
 *
 * 配置里只存稳定英文 id，中文仅作界面文案与协议报文值：日后改文案不会让已保存的
 * 配置失效。Rust 侧同名单在 `src-tauri/src/device_metadata.rs`，两份清单由
 * `device-metadata-contract.test.mjs` 逐项核对，禁止单边增删。
 */

export interface DeviceMetadataOption {
  value: string;
  label: string;
}

export const DEVICE_LOCATION_OPTIONS: readonly DeviceMetadataOption[] = [
  { value: "main_gate", label: "园区大门" },
  { value: "lobby", label: "大厅" },
  { value: "corridor", label: "走廊" },
  { value: "elevator", label: "电梯" },
  { value: "parking", label: "停车场" },
  { value: "server_room", label: "机房" },
  { value: "perimeter", label: "周界围墙" },
  { value: "main_road", label: "主干道" },
  { value: "other", label: "其他" },
];

export const CAMERA_TYPE_OPTIONS: readonly DeviceMetadataOption[] = [
  { value: "bullet", label: "固定枪机" },
  { value: "dome", label: "半球" },
  { value: "ptz_dome", label: "球机(PTZ)" },
  { value: "pan_tilt", label: "云台" },
  { value: "panoramic", label: "全景" },
  { value: "checkpoint", label: "卡口电警" },
  { value: "thermal", label: "热成像" },
  { value: "other", label: "其他" },
];

/** 空串表示「未设置」：不下发 Address / DeviceType，保持与旧版本一致的报文。 */
export const DEVICE_METADATA_UNSET = "";

function labelOf(options: readonly DeviceMetadataOption[], value: string): string | null {
  return options.find((option) => option.value === value)?.label ?? null;
}

export function deviceLocationLabel(value: string): string | null {
  return labelOf(DEVICE_LOCATION_OPTIONS, value);
}

export function cameraTypeLabel(value: string): string | null {
  return labelOf(CAMERA_TYPE_OPTIONS, value);
}
