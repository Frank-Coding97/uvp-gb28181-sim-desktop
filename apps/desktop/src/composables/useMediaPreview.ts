// 本地媒体预览(模块级单例):文件走 asset 协议,摄像头/屏幕走 WebView 的
// getUserMedia / getDisplayMedia。只是"给人看"的本地回显,真正推给平台的是后端 PS/RTP 流。
import { computed, nextTick, ref, watch } from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { isLikelyPlayableFile } from "../lib/liveSource";
import { formatError } from "../lib/ipcError";
import { useDevice } from "../device";
import { useLiveSources } from "./useLiveSources";

export type PreviewTone = "idle" | "live" | "warn";
export type VideoFit = "contain" | "cover";

const { form } = useDevice();
const { mediaMode, selectedMicrophone, selectedScreenAudio, selectedShareTarget } =
  useLiveSources();

const videoPreviewEl = ref<HTMLVideoElement | null>(null);
const previewUrl = ref("");
const previewState = ref<{ tone: PreviewTone; text: string }>({
  tone: "idle",
  text: "未选择媒体源",
});
const videoFitMode = ref<VideoFit>("contain");
let previewStream: MediaStream | null = null;

const previewTitle = computed(() => {
  if (mediaMode.value === "file") return "本地文件预览";
  if (mediaMode.value === "camera") return "摄像头实时预览";
  if (mediaMode.value === "screen") return "屏幕实时预览";
  return "媒体预览";
});

const previewMeta = computed(() => {
  if (mediaMode.value === "none") return "当前仅做 SIP 信令联调";
  if (mediaMode.value === "file") {
    return form.value.video_source
      ? "预览源文件；平台点播时由引擎转 PS/RTP 推送"
      : "选择 MP4/MOV 等容器文件可直接预览";
  }
  if (mediaMode.value === "camera") {
    return "预览使用 WebView 本地媒体流；实际推流使用后端 FFmpeg 采集";
  }
  if (selectedShareTarget.value && selectedShareTarget.value.kind !== "display") {
    return "采集目标为窗口/应用：本地预览无法反映它（WebView 只能整屏共享），平台收到的是该目标";
  }
  return "预览使用屏幕共享授权；实际推流使用后端 ScreenCaptureKit";
});

function toggleVideoFitMode() {
  videoFitMode.value = videoFitMode.value === "contain" ? "cover" : "contain";
}

function stopPreviewStream() {
  if (previewStream) {
    previewStream.getTracks().forEach((track) => track.stop());
    previewStream = null;
  }
  if (videoPreviewEl.value) videoPreviewEl.value.srcObject = null;
}

async function refreshVideoPreview(auto = false) {
  await nextTick();
  stopPreviewStream();
  previewUrl.value = "";
  const video = videoPreviewEl.value;
  if (!video) return;
  if (mediaMode.value === "none") {
    previewState.value = { tone: "idle", text: "空媒体模式：不会发送视频画面" };
    return;
  }
  if (mediaMode.value === "file") {
    const path = form.value.video_source.trim();
    if (!path) {
      previewState.value = { tone: "idle", text: "请选择一个视频文件" };
      return;
    }
    if (!isLikelyPlayableFile(path)) {
      previewState.value = { tone: "warn", text: "裸 H.264/H.265 可推流，但浏览器无法直接预览" };
      return;
    }
    // assetProtocol.scope 已收窄(不再常开文档/桌面/下载目录),
    // 用户主动选中的文件需要在运行时逐个放行。
    try {
      await invoke("allow_media_preview", { path });
    } catch (e) {
      previewState.value = { tone: "warn", text: formatError(e, "预览未授权") };
      return;
    }
    previewUrl.value = convertFileSrc(path);
    previewState.value = { tone: "live", text: "正在预览本地视频文件" };
    return;
  }
  if (!navigator.mediaDevices) {
    previewState.value = { tone: "warn", text: "当前 WebView 不支持本地媒体预览" };
    return;
  }
  try {
    previewState.value = { tone: "idle", text: "正在请求本地预览权限…" };
    if (mediaMode.value === "camera") {
      previewStream = await navigator.mediaDevices.getUserMedia({
        video: true,
        audio: selectedMicrophone.value !== "none",
      });
    } else if (
      selectedShareTarget.value &&
      selectedShareTarget.value.kind !== "display"
    ) {
      // 窗口/应用目标只有后端 ScreenCaptureKit 能采:WebView 的 getDisplayMedia 只能
      // 整屏共享,拿它当预览会让用户以为"选的目标没生效"——这里如实说明并跳过预览。
      previewState.value = {
        tone: "warn",
        text: "该目标由系统选择器指定(窗口/应用),本地预览不可用;请到平台侧确认画面",
      };
      return;
    } else {
      previewStream = await navigator.mediaDevices.getDisplayMedia({
        video: true,
        audio: selectedScreenAudio.value !== "none",
      });
    }
    video.srcObject = previewStream;
    await video.play().catch(() => undefined);
    previewState.value = { tone: "live", text: auto ? "实时预览已刷新" : "实时预览中" };
  } catch (e) {
    previewState.value = { tone: "warn", text: formatError(e, "预览未开启") };
  }
}

// 单例监听:模式或源变化时自动跟随。放在模块作用域是有意的——预览状态跟随
// 应用生命周期,不随某个组件的挂载/卸载来回创建。
watch(
  () => [mediaMode.value, form.value.video_source] as const,
  () => {
    if (mediaMode.value === "file") {
      refreshVideoPreview(true);
    } else if (mediaMode.value === "none") {
      stopPreviewStream();
      previewState.value = { tone: "idle", text: "空媒体模式：不会发送视频画面" };
      previewUrl.value = "";
    }
  }
);

export function useMediaPreview() {
  return {
    videoPreviewEl,
    previewUrl,
    previewState,
    videoFitMode,
    previewTitle,
    previewMeta,
    toggleVideoFitMode,
    stopPreviewStream,
    refreshVideoPreview,
  };
}
