import { annexBNals, annexBToAvcc, buildH264Config, h264ParameterSets } from "./codec.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

const sample = new Uint8Array([
  0, 0, 0, 1, 0x67, 0x64, 0, 0x1f,
  0, 0, 1, 0x68, 0xee,
  0, 0, 0, 1, 0x65, 1, 2,
]);

equal(annexBNals(sample).length, 3);
equal(h264ParameterSets(sample)?.sps[0], 0x67);
const config = buildH264Config(sample);
if (!config) throw new Error("expected H264 config");
equal(config.codec, "avc1.64001f");
equal(config.description[0], 1);
const avcc = annexBToAvcc(sample);
equal(new DataView(avcc.buffer).getUint32(0), 4);
equal(avcc[4], 0x67);
