export type CameraPreviewElement = Pick<HTMLVideoElement, "srcObject" | "play">;

export interface CameraMediaDevices {
  getUserMedia(constraints: MediaStreamConstraints): Promise<MediaStream>;
}

export interface CameraLeaseClient {
  acquire(): Promise<number>;
  release(token: number): Promise<void>;
}

export interface LeasedCameraPreview {
  stream: MediaStream;
  leaseToken: number;
}

export class CameraPreviewCancelledError extends Error {
  constructor() {
    super("camera preview request was cancelled");
    this.name = "CameraPreviewCancelledError";
  }
}

const CAMERA_CONSTRAINTS: MediaStreamConstraints = {
  video: {
    width: { ideal: 1280 },
    height: { ideal: 720 },
    facingMode: { ideal: "user" },
  },
  audio: false,
};

export async function startCameraPreview(
  video: CameraPreviewElement,
  mediaDevices: CameraMediaDevices,
  timeoutMs = 10_000,
): Promise<MediaStream> {
  let timedOut = false;
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const mediaRequest = mediaDevices.getUserMedia(CAMERA_CONSTRAINTS).then((stream) => {
    if (timedOut) stream.getTracks().forEach((track) => track.stop());
    return stream;
  });
  const timeout = new Promise<never>((_, reject) => {
    timeoutId = setTimeout(() => {
      timedOut = true;
      reject(new Error("camera request timed out"));
    }, timeoutMs);
  });
  let stream: MediaStream;
  try {
    stream = await Promise.race([mediaRequest, timeout]);
  } finally {
    if (timeoutId) clearTimeout(timeoutId);
  }
  video.srcObject = stream;
  try {
    await video.play();
    return stream;
  } catch (error) {
    stopCameraPreview(video, stream);
    throw error;
  }
}

export function stopCameraPreview(
  video: CameraPreviewElement | null,
  stream: MediaStream | null,
): void {
  stream?.getTracks().forEach((track) => track.stop());
  if (video) video.srcObject = null;
}

export async function startLeasedCameraPreview(
  video: CameraPreviewElement,
  mediaDevices: CameraMediaDevices,
  lease: CameraLeaseClient,
  timeoutMs = 10_000,
  shouldKeep = () => true,
): Promise<LeasedCameraPreview> {
  const leaseToken = await lease.acquire();
  let stream: MediaStream;
  try {
    stream = await startCameraPreview(video, mediaDevices, timeoutMs);
  } catch (error) {
    await lease.release(leaseToken);
    throw error;
  }
  const preview = { stream, leaseToken };
  if (!shouldKeep()) {
    await stopLeasedCameraPreview(video, preview, lease);
    throw new CameraPreviewCancelledError();
  }
  return preview;
}

export async function stopLeasedCameraPreview(
  video: CameraPreviewElement | null,
  preview: LeasedCameraPreview | null,
  lease: CameraLeaseClient,
): Promise<void> {
  stopCameraPreview(video, preview?.stream ?? null);
  if (preview) await lease.release(preview.leaseToken);
}
