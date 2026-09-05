import test from "node:test";
import assert from "node:assert/strict";

const storage = new Map();
globalThis.localStorage = {
  getItem: (key) => storage.has(key) ? storage.get(key) : null,
  setItem: (key, value) => storage.set(key, String(value)),
  removeItem: (key) => storage.delete(key),
  clear: () => storage.clear(),
};

let invokeHandler = async () => { throw new Error("未配置测试 invoke handler"); };
globalThis.window = {
  __TAURI_INTERNALS__: {
    invoke: (command, args) => invokeHandler(command, args),
  },
};

const { loadDesktopConfig, saveMediaConfig, usePlatform } = await import("./platform.ts");

function baseConfig() {
  return {
    schema_version: 2,
    config_revision: 0,
    media: {
      width: 1280,
      height: 720,
      video_fps: 25,
      bitrate_kbps: 2000,
      keyframe_interval_seconds: 1,
      video_codec: "h264",
      audio_codec: "g711_a",
      audio_sample_rate_hz: 16000,
    },
    active_profile_id: "local",
    profiles: [{
      id: "local",
      name: "本地",
      server_host: "127.0.0.1",
      server_port: 5060,
      server_id: "34020000002000000001",
      server_domain: "3402000000",
      password: "secret",
      transport: "UDP",
      gb_version: "V2022",
      signaling_encoding: "Gb18030",
    }],
    device: {
      device_id: "34020000001310000001",
      device_name: "桌面模拟器",
      manufacturer: "UVP",
      model: "Desktop",
      firmware: "0.1.2",
      channel_name: "Camera-1",
      register_expires_secs: 86400,
      heartbeat_interval_secs: 60,
      heartbeat_fail_threshold: 3,
    },
    network: { bind_mode: "auto", bind_address: "", sip_trace: true },
    catalog_tree: [],
  };
}

function resetHarness() {
  storage.clear();
  invokeHandler = async () => { throw new Error("未配置测试 invoke handler"); };
}

test("旧 localStorage FPS 迁移到 media，只有保存成功才删除旧 key", async () => {
  resetHarness();
  storage.set("uvp_device_form", JSON.stringify({ video_fps: 17 }));
  const saved = [];
  invokeHandler = async (command, args) => {
    if (command === "get_desktop_config") return baseConfig();
    if (command === "save_desktop_config") {
      saved.push(args.config);
      return { ...args.config, config_revision: 1 };
    }
    throw new Error(`unexpected command ${command}`);
  };

  const migrated = await loadDesktopConfig();
  assert.equal(saved[0].media.video_fps, 17);
  assert.equal(migrated.media.video_fps, 17);
  assert.equal(storage.has("uvp_device_form"), false);
});

test("旧 localStorage 迁移保存失败时保留 key", async () => {
  resetHarness();
  storage.set("uvp_device_form", JSON.stringify({ video_fps: 17 }));
  invokeHandler = async (command) => {
    if (command === "get_desktop_config") return baseConfig();
    if (command === "save_desktop_config") throw new Error("磁盘写入失败");
    throw new Error(`unexpected command ${command}`);
  };

  await assert.rejects(loadDesktopConfig, /磁盘写入失败/);
  assert.equal(storage.has("uvp_device_form"), true);
});

test("媒体保存发生 revision 冲突时重新读取快照并保留可重试版本", async () => {
  resetHarness();
  const calls = [];
  const latest = { ...baseConfig(), config_revision: 4, media: { ...baseConfig().media, bitrate_kbps: 4000 } };
  invokeHandler = async (command, args) => {
    calls.push({ command, args });
    if (command === "get_desktop_config") return calls.length === 1 ? baseConfig() : latest;
    if (command === "save_media_config") throw new Error("配置版本冲突，请重新读取后保存");
    throw new Error(`unexpected command ${command}`);
  };

  await loadDesktopConfig();
  const draft = { ...baseConfig().media, bitrate_kbps: 6000 };
  await assert.rejects(saveMediaConfig(draft), /配置版本冲突/);
  assert.deepEqual(calls.map((call) => call.command), ["get_desktop_config", "save_media_config", "get_desktop_config"]);
  assert.equal(calls[1].args.expectedRevision, 0);
  assert.equal(usePlatform().config.value.config_revision, 4);
  assert.equal(usePlatform().config.value.media.bitrate_kbps, 4000);
});

test("媒体草稿使用编辑开始时的版本，即使全局快照已经刷新", async () => {
  resetHarness();
  let seenRevision;
  invokeHandler = async (command, args) => {
    if (command === "get_desktop_config") return { ...baseConfig(), config_revision: 8 };
    if (command === "save_media_config") {
      seenRevision = args.expectedRevision;
      return { ...baseConfig(), config_revision: 9 };
    }
    throw new Error(`unexpected command ${command}`);
  };
  await loadDesktopConfig();
  await saveMediaConfig({ ...baseConfig().media }, 3);
  assert.equal(seenRevision, 3);
});
