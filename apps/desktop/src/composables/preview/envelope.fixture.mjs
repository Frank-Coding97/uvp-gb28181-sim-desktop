// 测试共用:构造 UVPJ v2 预览帧(与 Rust preview_channel::encode_envelope 一致)。
export const HEADER_LEN = 36;
const KINDS = { jpeg: 0, h264: 1, h265: 2 };

export function envelopeV2({
  kind = "jpeg",
  key = true,
  codec = "",
  generation = 7,
  sequence = 11,
  capturedAtMs = Date.now(),
  payload,
}) {
  const codecBytes = [...codec].map((char) => char.charCodeAt(0));
  const bytes = new Uint8Array(HEADER_LEN + codecBytes.length + payload.length);
  bytes.set([0x55, 0x56, 0x50, 0x4a], 0);
  const view = new DataView(bytes.buffer);
  view.setUint16(4, 2, true);
  view.setUint16(6, HEADER_LEN, true);
  view.setUint8(8, KINDS[kind]);
  view.setUint8(9, key ? 1 : 0);
  view.setUint16(10, codecBytes.length, true);
  view.setBigUint64(12, BigInt(generation), true);
  view.setBigUint64(20, BigInt(sequence), true);
  view.setBigUint64(28, BigInt(capturedAtMs), true);
  bytes.set(codecBytes, HEADER_LEN);
  bytes.set(payload, HEADER_LEN + codecBytes.length);
  return bytes.buffer;
}

export function jpegEnvelope(generation, sequence, capturedAtMs = Date.now()) {
  return envelopeV2({
    generation,
    sequence,
    capturedAtMs,
    payload: [0xff, 0xd8, 0xff, sequence & 0xff, 0xff, 0xd9],
  });
}
