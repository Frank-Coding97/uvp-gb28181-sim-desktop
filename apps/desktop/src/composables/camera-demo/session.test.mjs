import {
  CameraPreviewCancelledError,
  startCameraPreview,
  startLeasedCameraPreview,
  stopCameraPreview,
  stopLeasedCameraPreview,
} from "./session.ts";

function equal(actual, expected) {
  if (actual !== expected) throw new Error(`expected ${String(expected)}, got ${String(actual)}`);
}

function createTrack() {
  return {
    stopped: false,
    stop() {
      this.stopped = true;
    },
  };
}

function createStream() {
  const tracks = [createTrack(), createTrack()];
  return {
    tracks,
    getTracks() {
      return tracks;
    },
  };
}

const stream = createStream();
let requestedConstraints;
const mediaDevices = {
  async getUserMedia(constraints) {
    requestedConstraints = constraints;
    return stream;
  },
};
const video = {
  srcObject: null,
  played: false,
  async play() {
    this.played = true;
  },
};

const started = await startCameraPreview(video, mediaDevices);
equal(started, stream);
equal(video.srcObject, stream);
equal(video.played, true);
equal(requestedConstraints.audio, false);
equal(requestedConstraints.video.width.ideal, 1280);
equal(requestedConstraints.video.height.ideal, 720);

stopCameraPreview(video, stream);
equal(video.srcObject, null);
equal(stream.tracks.every((track) => track.stopped), true);

// DOM 上即使残留了另一个已结束 MediaStream，也必须清空播放器绑定。
const staleVideo = { srcObject: createStream(), async play() {} };
stopCameraPreview(staleVideo, null);
equal(staleVideo.srcObject, null);

// video.play() 失败时不能遗留仍占用摄像头的 MediaStream。
const failedStream = createStream();
const failedVideo = {
  srcObject: null,
  async play() {
    throw new Error("play failed");
  },
};
let rejected = false;
try {
  await startCameraPreview(failedVideo, {
    async getUserMedia() {
      return failedStream;
    },
  });
} catch {
  rejected = true;
}
equal(rejected, true);
equal(failedVideo.srcObject, null);
equal(failedStream.tracks.every((track) => track.stopped), true);

// WebView 权限链异常时 getUserMedia 可能一直 pending，必须超时退出。
let resolveLateStream;
const lateStream = createStream();
const timeoutVideo = { srcObject: null, async play() {} };
rejected = false;
try {
  await startCameraPreview(timeoutVideo, {
    getUserMedia() {
      return new Promise((resolve) => { resolveLateStream = resolve; });
    },
  }, 5);
} catch (error) {
  rejected = error instanceof Error && error.message === "camera request timed out";
}
equal(rejected, true);
resolveLateStream(lateStream);
await new Promise((resolve) => setTimeout(resolve, 0));
equal(lateStream.tracks.every((track) => track.stopped), true);

// 租约必须先于 getUserMedia 获取；任何启动失败都要释放自己的 token。
const leaseEvents = [];
const leaseClient = {
  async acquire() { leaseEvents.push("acquire"); return 77; },
  async release(token) { leaseEvents.push(`release:${token}`); },
};
rejected = false;
try {
  await startLeasedCameraPreview(
    { srcObject: null, async play() {} },
    { async getUserMedia() { leaseEvents.push("getUserMedia"); throw new Error("denied"); } },
    leaseClient,
  );
} catch {
  rejected = true;
}
equal(rejected, true);
equal(leaseEvents.join(","), "acquire,getUserMedia,release:77");

const leasedStream = createStream();
const leasedVideo = { srcObject: null, async play() {} };
const leased = await startLeasedCameraPreview(
  leasedVideo,
  { async getUserMedia() { return leasedStream; } },
  leaseClient,
);
await stopLeasedCameraPreview(leasedVideo, leased, leaseClient);
equal(leasedStream.tracks.every((track) => track.stopped), true);
equal(leaseEvents.at(-1), "release:77");

// 页面失活发生在 getUserMedia 返回之前时，迟到流必须立即停止且租约只释放一次。
let resolveCancelledStream;
let keepCancelledRequest = true;
const cancelledStream = createStream();
const cancelledVideo = { srcObject: null, async play() {} };
const releasesBeforeCancel = leaseEvents.filter((event) => event === "release:77").length;
const cancelledRequest = startLeasedCameraPreview(
  cancelledVideo,
  { getUserMedia() { return new Promise((resolve) => { resolveCancelledStream = resolve; }); } },
  leaseClient,
  1_000,
  () => keepCancelledRequest,
);
await Promise.resolve();
keepCancelledRequest = false;
resolveCancelledStream(cancelledStream);
rejected = false;
try {
  await cancelledRequest;
} catch (error) {
  rejected = error instanceof CameraPreviewCancelledError;
}
equal(rejected, true);
equal(cancelledVideo.srcObject, null);
equal(cancelledStream.tracks.every((track) => track.stopped), true);
equal(
  leaseEvents.filter((event) => event === "release:77").length,
  releasesBeforeCancel + 1,
);
