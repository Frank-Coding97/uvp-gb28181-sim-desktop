import { describe, expect, it } from "vitest";

import {
  DEFAULT_SCREEN_PROFILE,
  buildCameraUri,
  buildScreenUri,
  inferMediaMode,
  isAudioCodec,
  isLikelyPlayableFile,
  parseCameraSource,
  parseScreenNumber,
  parseScreenSource,
  validateVideoProfile,
} from "./liveSource";

const DEFAULTS = {
  width: DEFAULT_SCREEN_PROFILE.width,
  height: DEFAULT_SCREEN_PROFILE.height,
  bitrateKbps: DEFAULT_SCREEN_PROFILE.bitrateKbps,
  codec: "h264" as const,
  gop: DEFAULT_SCREEN_PROFILE.gop,
};

describe("摄像头源 URI", () => {
  it("解析合法 URI", () => {
    expect(parseCameraSource("live:camera:0?audio=none&audio_codec=g711a")).toEqual({
      videoIndex: 0,
      audio: "none",
      audioCodec: "g711a",
      ...DEFAULTS,
    });
    expect(parseCameraSource("live:camera:2?audio=1&audio_codec=aac")).toEqual({
      videoIndex: 2,
      audio: "1",
      audioCodec: "aac",
      ...DEFAULTS,
    });
  });

  it("解析摄像头编码档(与屏幕同一套参数)", () => {
    expect(
      parseCameraSource(
        "live:camera:1?audio=none&audio_codec=g711a&width=1920&height=1080&bitrate=4000&codec=h265&gop=100"
      )
    ).toEqual({
      videoIndex: 1,
      audio: "none",
      audioCodec: "g711a",
      width: 1920,
      height: 1080,
      bitrateKbps: 4000,
      codec: "h265",
      gop: 100,
    });
  });

  it("拒绝非法 URI", () => {
    // 非摄像头 URI
    expect(parseCameraSource("live:screen:1?audio=none")).toBeNull();
    expect(parseCameraSource("/tmp/a.h264")).toBeNull();
    // audio 既不是 none 也不是数字
    expect(parseCameraSource("live:camera:0?audio=default&audio_codec=g711a")).toBeNull();
    // 未知编码
    expect(parseCameraSource("live:camera:0?audio=none&audio_codec=mp3")).toBeNull();
    expect(parseCameraSource("live:camera:0?audio=none&codec=av1")).toBeNull();
    // 缺查询串
    expect(parseCameraSource("live:camera:0")).toBeNull();
  });

  it("audio_codec 缺省为 g711a", () => {
    expect(parseCameraSource("live:camera:1?audio=none")?.audioCodec).toBe("g711a");
  });

  it("构造与解析可往返", () => {
    const source = {
      videoIndex: 3,
      audio: "2",
      audioCodec: "opus" as const,
      width: 640,
      height: 360,
      bitrateKbps: 800,
      codec: "h264" as const,
      gop: 25,
    };
    const uri = buildCameraUri(source);
    expect(uri).toBe(
      "live:camera:3?audio=2&audio_codec=opus&width=640&height=360&bitrate=800&codec=h264&gop=25"
    );
    expect(parseCameraSource(uri)).toEqual(source);
  });
});

describe("屏幕源 URI", () => {
  const full =
    "live:screen:1?audio=system&audio_codec=g711u&width=1920&height=1080&bitrate=4000&codec=h265&gop=100";

  it("解析全参数 URI", () => {
    expect(parseScreenSource(full)).toEqual({
      target: { kind: "display", displayId: 1 },
      audio: "system",
      audioCodec: "g711u",
      width: 1920,
      height: 1080,
      bitrateKbps: 4000,
      codec: "h265",
      gop: 100,
    });
  });

  it("缺省字段回退默认档", () => {
    const parsed = parseScreenSource("live:screen:7?audio=none");
    expect(parsed).not.toBeNull();
    expect(parsed!.width).toBe(DEFAULT_SCREEN_PROFILE.width);
    expect(parsed!.height).toBe(DEFAULT_SCREEN_PROFILE.height);
    expect(parsed!.bitrateKbps).toBe(DEFAULT_SCREEN_PROFILE.bitrateKbps);
    expect(parsed!.codec).toBe("h264");
    expect(parsed!.gop).toBe(DEFAULT_SCREEN_PROFILE.gop);
  });

  it("接受 microphone:N 形式的音频源", () => {
    expect(parseScreenSource("live:screen:1?audio=microphone:3")?.audio).toBe("microphone:3");
  });

  it("拒绝非法音频源与编码", () => {
    expect(parseScreenSource("live:screen:1?audio=microphone")).toBeNull();
    expect(parseScreenSource("live:screen:1?audio=none&codec=av1")).toBeNull();
    // display_id 必须是正整数(0 表示"未选择",不是合法 URI)
    expect(parseScreenSource("live:screen:0?audio=none")).toBeNull();
  });

  it("构造与解析可往返", () => {
    const profile = {
      target: { kind: "display" as const, displayId: 2 },
      audio: "microphone:1" as const,
      audioCodec: "aac" as const,
      width: 1280,
      height: 720,
      bitrateKbps: 2500,
      codec: "h264" as const,
      gop: 50,
    };
    expect(parseScreenSource(buildScreenUri(profile))).toEqual(profile);
  });

  it("系统选择器的窗口/应用目标可往返", () => {
    const windowTarget = {
      target: { kind: "window" as const, windowId: 4291 },
      audio: "none" as const,
      audioCodec: "g711a" as const,
      width: 1280,
      height: 720,
      bitrateKbps: 2500,
      codec: "h264" as const,
      gop: 50,
    };
    const windowUri = buildScreenUri(windowTarget);
    expect(windowUri.startsWith("live:window:4291?")).toBe(true);
    expect(parseScreenSource(windowUri)).toEqual(windowTarget);

    const appTarget = {
      target: { kind: "application" as const, bundleId: "com.apple.Safari" },
      audio: "system" as const,
      audioCodec: "g711a" as const,
      width: 1920,
      height: 1080,
      bitrateKbps: 6000,
      codec: "h265" as const,
      gop: 50,
    };
    const appUri = buildScreenUri(appTarget);
    expect(appUri.startsWith("live:app:com.apple.Safari?")).toBe(true);
    expect(parseScreenSource(appUri)).toEqual(appTarget);
  });

  it("窗口/应用目标非法时判为非法 URI", () => {
    expect(parseScreenSource("live:window:0?audio=none")).toBeNull();
    expect(parseScreenSource("live:window:abc?audio=none")).toBeNull();
    expect(parseScreenSource("live:app:?audio=none")).toBeNull();
    expect(parseScreenSource("live:app:com.apple Safari?audio=none")).toBeNull();
    // 目标类型必须是 screen/window/app 三种之一
    expect(parseScreenSource("live:display:1?audio=none")).toBeNull();
  });

  it("窗口/应用目标也能被识别为屏幕模式", () => {
    expect(inferMediaMode("live:window:12?audio=none")).toBe("screen");
    expect(inferMediaMode("live:app:com.apple.Safari?audio=none")).toBe("screen");
  });
});

describe("parseScreenNumber", () => {
  it("非正数与非法值回退默认", () => {
    expect(parseScreenNumber(null, 720)).toBe(720);
    expect(parseScreenNumber("0", 720)).toBe(720);
    expect(parseScreenNumber("-1", 720)).toBe(720);
    expect(parseScreenNumber("abc", 720)).toBe(720);
    expect(parseScreenNumber("1.5", 720)).toBe(720);
    expect(parseScreenNumber("1080", 720)).toBe(1080);
  });
});

describe("视频编码参数校验", () => {
  const base = { width: 1280, height: 720, bitrateKbps: 2500, codec: "h264" };

  it("合法参数无错误", () => {
    expect(validateVideoProfile(base)).toBeNull();
    expect(validateVideoProfile({ ...base, gop: 50 })).toBeNull();
  });

  it("奇数宽高被拒(色度采样要求偶数)", () => {
    expect(validateVideoProfile({ ...base, width: 1281 })).toMatch("偶数");
    expect(validateVideoProfile({ ...base, height: 721 })).toMatch("偶数");
  });

  it("过小与过大分辨率被拒", () => {
    expect(validateVideoProfile({ ...base, width: 100, height: 100 })).toMatch("不能小于");
    expect(validateVideoProfile({ ...base, width: 4096, height: 2160 })).toMatch("3840×2160");
  });

  it("码率越界被拒", () => {
    expect(validateVideoProfile({ ...base, bitrateKbps: 64 })).toMatch("码率");
    expect(validateVideoProfile({ ...base, bitrateKbps: 30_000 })).toMatch("码率");
  });

  it("未知编码被拒", () => {
    expect(validateVideoProfile({ ...base, codec: "vp9" })).toMatch("H.264");
  });

  it("非整数分辨率被拒", () => {
    expect(validateVideoProfile({ ...base, width: 1280.5 })).toMatch("整数");
  });

  it("GOP 越界被拒", () => {
    expect(validateVideoProfile({ ...base, gop: 0 })).toMatch("GOP");
    expect(validateVideoProfile({ ...base, gop: 601 })).toMatch("GOP");
  });
});

describe("媒体模式推断", () => {
  it("按 URI 前缀区分", () => {
    expect(inferMediaMode("live:camera:0?audio=none")).toBe("camera");
    expect(inferMediaMode("live:screen:1?audio=none")).toBe("screen");
    expect(inferMediaMode("/tmp/a.mp4")).toBe("file");
    expect(inferMediaMode("")).toBe("file");
  });

  it("兼容 legacy 的 live:0 / live:ffmpeg:0", () => {
    expect(inferMediaMode("live:0")).toBe("screen");
    expect(inferMediaMode("live:ffmpeg:0")).toBe("screen");
  });
});

describe("其它", () => {
  it("isAudioCodec 只认白名单", () => {
    expect(isAudioCodec("g711a")).toBe(true);
    expect(isAudioCodec("opus")).toBe(true);
    expect(isAudioCodec("mp3")).toBe(false);
  });

  it("裸流不可在浏览器预览", () => {
    expect(isLikelyPlayableFile("/a/b.mp4")).toBe(true);
    expect(isLikelyPlayableFile("/a/b.MOV")).toBe(true);
    expect(isLikelyPlayableFile("/a/b.h264")).toBe(false);
    expect(isLikelyPlayableFile("/a/b.h265")).toBe(false);
  });
});
