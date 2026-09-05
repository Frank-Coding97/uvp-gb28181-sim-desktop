import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_MEDIA_PROFILE,
  MEDIA_PRESETS,
  applyMediaPreset,
  buildLiveSourceUri,
  effectiveAudioSampleRate,
  liveSourceAudio,
  matchedPreset,
  mediaProfileSummary,
  normalizeMediaProfile,
  syncMediaDraft,
  validateMediaProfile,
} from "./media-profile.ts";

test("四档画质预设只改变四个视频字段", () => {
  const base = { ...DEFAULT_MEDIA_PROFILE, video_codec: "h265", audio_codec: "aac", audio_sample_rate_hz: 8000 };
  const expected = [
    [640, 480, 15, 600, 1],
    [1280, 720, 20, 1200, 1],
    [1280, 720, 25, 2000, 1],
    [1920, 1080, 25, 4000, 1],
  ];
  for (const [index, preset] of MEDIA_PRESETS.entries()) {
    const next = applyMediaPreset(base, preset.id);
    assert.deepEqual(
      [next.width, next.height, next.video_fps, next.bitrate_kbps, next.keyframe_interval_seconds],
      expected[index],
    );
    assert.equal(next.video_codec, "h265");
    assert.equal(next.audio_codec, "aac");
    assert.equal(next.audio_sample_rate_hz, 8000);
  }
});

test("G.711 显示有效 8 kHz，AAC 保留 8/16 kHz", () => {
  assert.equal(effectiveAudioSampleRate({ ...DEFAULT_MEDIA_PROFILE, audio_codec: "g711_a", audio_sample_rate_hz: 16000 }), 8000);
  assert.equal(effectiveAudioSampleRate({ ...DEFAULT_MEDIA_PROFILE, audio_codec: "g711_u", audio_sample_rate_hz: 8000 }), 8000);
  assert.equal(effectiveAudioSampleRate({ ...DEFAULT_MEDIA_PROFILE, audio_codec: "aac", audio_sample_rate_hz: 8000 }), 8000);
  assert.equal(effectiveAudioSampleRate({ ...DEFAULT_MEDIA_PROFILE, audio_codec: "aac", audio_sample_rate_hz: 16000 }), 16000);
});

test("参数摘要使用用户可读的编码名称并只展示一次有效采样率", () => {
  const g711Summary = mediaProfileSummary({ ...DEFAULT_MEDIA_PROFILE, audio_codec: "g711_a" });
  assert.match(g711Summary, /H\.264 \/ G\.711A · 8 kHz$/);
  assert.doesNotMatch(g711Summary, /H264|G711_A/);

  const aacSummary = mediaProfileSummary({ ...DEFAULT_MEDIA_PROFILE, video_codec: "h265", audio_codec: "aac", audio_sample_rate_hz: 16000 });
  assert.match(aacSummary, /H\.265 \/ AAC · 16 kHz$/);
  assert.equal(aacSummary.match(/16 kHz/g)?.length, 1);
});

test("旧的有效 FPS 保留为自定义，不被默认预设覆盖", () => {
  const profile = normalizeMediaProfile({ video_fps: 17 });
  assert.equal(profile.video_fps, 17);
  assert.equal(matchedPreset(profile), null);
  assert.equal(validateMediaProfile(profile), null);
});

test("刷新服务端快照时保留未保存媒体草稿，显式同步时才覆盖", () => {
  const draft = { ...DEFAULT_MEDIA_PROFILE, video_fps: 17 };
  const persisted = { ...DEFAULT_MEDIA_PROFILE, video_fps: 30 };
  assert.equal(syncMediaDraft(draft, persisted).video_fps, 17);
  assert.equal(syncMediaDraft(draft, persisted, true).video_fps, 30);
});

test("首页实时源 URI 保留来源和音频源，不再携带固定编码参数", () => {
  assert.equal(buildLiveSourceUri("camera", 2), "live:camera:2?audio=none");
  assert.equal(buildLiveSourceUri("screen", 1, "microphone"), "live:screen:1?audio=microphone");
  assert.doesNotMatch(buildLiveSourceUri("screen", 1), /width|height|bitrate|codec|fps|gop/);
  assert.equal(liveSourceAudio("live:camera:2?audio=3"), "3");
  assert.equal(liveSourceAudio("/tmp/video.mp4"), "none");
});
