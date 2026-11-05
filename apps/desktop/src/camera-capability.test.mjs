import test from "node:test";
import assert from "node:assert/strict";
import {
  DEFAULT_MEDIA_PROFILE,
  cameraFpsOptions,
  cameraMaxFps,
  cameraResolutionOptions,
  cameraSupportsResolution,
  clampToCamera,
  liveSourceCameraIndex,
} from "./media-profile.ts";

// 用户实测的那台摄像头（`vcodec=mjpeg ... max s=1280x720 fps=30`）。
const WEB_CAM_720P = [
  { width: 352, height: 288, min_fps: 30, max_fps: 30, format: "mjpeg" },
  { width: 1280, height: 720, min_fps: 30, max_fps: 30, format: "mjpeg" },
  { width: 640, height: 360, min_fps: 30, max_fps: 30, format: "yuyv422" },
  { width: 640, height: 480, min_fps: 30, max_fps: 30, format: "yuyv422" },
  { width: 320, height: 240, min_fps: 30, max_fps: 30, format: "yuyv422" },
];

test("能力未知时保留通用档位，不把设置页锁死", () => {
  const options = cameraResolutionOptions(null);
  assert.equal(options.length, 3);
  assert.deepEqual(options.map((item) => item.width), [640, 1280, 1920]);
  assert.ok(options.every((item) => item.max_fps === 0));
});

test("1080p 摄像头不支持的档位不出现在选项里", () => {
  const options = cameraResolutionOptions(WEB_CAM_720P);
  assert.deepEqual(options.map((item) => [item.width, item.height, item.max_fps]), [
    [640, 480, 30],
    [1280, 720, 30],
  ]);
  assert.equal(cameraSupportsResolution(WEB_CAM_720P, 1920, 1080), false);
  assert.equal(cameraSupportsResolution(WEB_CAM_720P, 1280, 720), true);
});

test("帧率选项按该分辨率的实际上限裁剪", () => {
  assert.deepEqual(cameraFpsOptions(WEB_CAM_720P, 1280, 720), [15, 20, 25, 30]);
  const lowFps = [{ width: 1280, height: 720, min_fps: 10, max_fps: 10, format: "yuyv422" }];
  assert.deepEqual(cameraFpsOptions(lowFps, 1280, 720), [10]);
  assert.equal(cameraMaxFps(lowFps, 1280, 720), 10);
  assert.equal(cameraMaxFps(WEB_CAM_720P, 1920, 1080), null);
});

test("多个模式取同一分辨率的最高帧率", () => {
  const modes = [
    { width: 1280, height: 720, min_fps: 5, max_fps: 10, format: "yuyv422" },
    { width: 1280, height: 720, min_fps: 5, max_fps: 30, format: "mjpeg" },
  ];
  assert.equal(cameraMaxFps(modes, 1280, 720), 30);
  assert.deepEqual(cameraFpsOptions(modes, 1280, 720), [15, 20, 25, 30]);
});

test("载入超能力配置时收敛到最高可用档并给出说明", () => {
  const clamped = clampToCamera({ ...DEFAULT_MEDIA_PROFILE, width: 1920, height: 1080, video_fps: 25 }, WEB_CAM_720P);
  assert.ok(clamped);
  assert.equal(clamped.profile.width, 1280);
  assert.equal(clamped.profile.height, 720);
  assert.equal(clamped.profile.video_fps, 25);
  assert.match(clamped.notice, /不支持 1920×1080/);
});

test("分辨率可用但帧率超出上限时只降帧率", () => {
  const modes = [{ width: 1280, height: 720, min_fps: 10, max_fps: 10, format: "yuyv422" }];
  const clamped = clampToCamera({ ...DEFAULT_MEDIA_PROFILE, width: 1280, height: 720, video_fps: 30 }, modes);
  assert.ok(clamped);
  assert.equal(clamped.profile.width, 1280);
  assert.equal(clamped.profile.video_fps, 10);
  assert.match(clamped.notice, /最高 10 FPS/);
});

test("配置本来就在能力范围内时不做任何改动", () => {
  assert.equal(clampToCamera({ ...DEFAULT_MEDIA_PROFILE, width: 1280, height: 720, video_fps: 25 }, WEB_CAM_720P), null);
  assert.equal(clampToCamera({ ...DEFAULT_MEDIA_PROFILE, width: 1920, height: 1080 }, null), null);
});

test("只从摄像头媒体源里取索引", () => {
  assert.equal(liveSourceCameraIndex("live:camera:0?audio=none"), 0);
  assert.equal(liveSourceCameraIndex("live:camera:12"), 12);
  assert.equal(liveSourceCameraIndex("live:screen:0?audio=none"), null);
  assert.equal(liveSourceCameraIndex(""), null);
  assert.equal(liveSourceCameraIndex("/videos/demo.mp4"), null);
});
