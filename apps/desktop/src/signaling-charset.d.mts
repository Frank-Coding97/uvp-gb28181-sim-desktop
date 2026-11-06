/** 信令字符集(GB/T 28181 §6.10)，与 Rust `common::SignalingEncoding` 同语义。 */
export type SignalingEncodingValue = "Gb2312" | "Gb18030" | "Utf8";

export interface CharsetOption {
  value: SignalingEncodingValue;
  label: string;
  disabled: boolean;
  hint: string;
}

export declare const ENCODING_VALUES: SignalingEncodingValue[];
export declare function defaultFor(version: string): SignalingEncodingValue;
export declare function isAvailableFor(encoding: SignalingEncodingValue, version: string): boolean;
export declare function effective(stored: SignalingEncodingValue, version: string): SignalingEncodingValue;
export declare function parseEncoding(raw: unknown): SignalingEncodingValue;
export declare function xmlName(encoding: SignalingEncodingValue): string;
export declare function encodingLabel(encoding: SignalingEncodingValue): string;
export declare function optionsFor(version: string): CharsetOption[];
export declare function effectiveSummary(stored: SignalingEncodingValue, version: string): string;
export declare function normalizeForVersion(
  stored: SignalingEncodingValue,
  version: string,
): SignalingEncodingValue;
