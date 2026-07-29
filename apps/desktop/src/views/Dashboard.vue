<script setup lang="ts">
// 首页总览：引擎健康、当前目标与核心工作流入口。
import { computed, onMounted, ref } from "vue";
import { useRouter } from "vue-router";
import { NButton, NIcon } from "naive-ui";
import GitNetworkOutline from "@vicons/ionicons5/es/GitNetworkOutline.js";
import HardwareChipOutline from "@vicons/ionicons5/es/HardwareChipOutline.js";
import LayersOutline from "@vicons/ionicons5/es/LayersOutline.js";
import TerminalOutline from "@vicons/ionicons5/es/TerminalOutline.js";
import { invoke } from "@tauri-apps/api/core";
import { useDevice } from "../device";
import { usePlatform } from "../platform";

const router = useRouter();
const engineInfo = ref("正在检查引擎");
const online = ref(false);
const { active } = usePlatform();
const { statusMeta, deviceLive } = useDevice();

const platformEndpoint = computed(() =>
  active.value ? `${active.value.server_host}:${active.value.server_port}` : "未配置",
);

onMounted(async () => {
  try {
    engineInfo.value = await invoke<string>("engine_version");
    online.value = true;
  } catch (error) {
    engineInfo.value = `引擎不可用：${String(error)}`;
  }
});

const features = [
  {
    key: "/device",
    icon: HardwareChipOutline,
    color: "#3884ff",
    step: "01",
    title: "单设备联调",
    desc: "验证注册、鉴权、目录、点播、PTZ 与主动上报，实时查看 SIP 原始报文。",
  },
  {
    key: "/channels",
    icon: GitNetworkOutline,
    color: "#00a6c8",
    step: "02",
    title: "多通道目录",
    desc: "构造 NVR、行政区划与业务分组目录，观察增量 Catalog NOTIFY。",
  },
  {
    key: "/scenario",
    icon: LayersOutline,
    color: "#715df2",
    step: "03",
    title: "压力测试",
    desc: "批量设备爬坡注册、心跳和媒体推流，实时统计成功率与失败归因。",
    highlight: true,
  },
  {
    key: "/system",
    icon: TerminalOutline,
    color: "#08a77e",
    step: "04",
    title: "系统日志",
    desc: "动态调整日志级别，按模块、等级与关键字检查协议和媒体运行细节。",
  },
];

const capabilities = [
  ["信令", "REGISTER / MESSAGE / SUBSCRIBE"],
  ["协议", "GB/T 28181-2016 / 2022"],
  ["媒体", "PS over RTP · UDP / TCP 媒体"],
  ["规模", "单机最多 10,000 虚拟设备"],
];
</script>

<template>
  <div class="page">
    <section class="hero">
      <div class="hero-copy">
        <div class="eyebrow">GB28181 DEVICE LAB</div>
        <h1>把一台电脑，变成完整的国标设备实验室</h1>
        <p>面向平台联调、协议验证与容量评估。无需真实摄像机，即可复现设备注册、控制、点播和并发压力。</p>
        <div class="hero-actions">
          <n-button type="primary" size="large" @click="router.push('/device')">开始单设备联调</n-button>
          <n-button secondary size="large" @click="router.push('/scenario')">创建压力场景</n-button>
        </div>
      </div>
      <div class="hero-visual" aria-hidden="true">
        <div class="radar-ring ring-1" />
        <div class="radar-ring ring-2" />
        <div class="radar-ring ring-3" />
        <div class="radar-core">SIP</div>
        <span class="node n1" />
        <span class="node n2" />
        <span class="node n3" />
        <span class="node n4" />
      </div>
    </section>

    <section class="status-grid">
      <div class="glass-card status-card">
        <span class="status-label">核心引擎</span>
        <div class="status-value"><i :class="{ online }" />{{ online ? "运行正常" : "离线" }}</div>
        <small>{{ engineInfo }}</small>
      </div>
      <div class="glass-card status-card">
        <span class="status-label">目标平台</span>
        <div class="status-value mono">{{ platformEndpoint }}</div>
        <small>{{ active?.name ?? "请在顶栏创建平台档案" }} · {{ active?.transport ?? "—" }}</small>
      </div>
      <div class="glass-card status-card">
        <span class="status-label">单设备状态</span>
        <div class="status-value" :style="{ color: statusMeta.color }">{{ statusMeta.text }}</div>
        <small>{{ deviceLive ? "后台设备实例正在运行" : "尚未启动设备实例" }}</small>
      </div>
    </section>

    <section class="section-head">
      <div>
        <h2>推荐工作流</h2>
        <p>先验证单设备协议，再扩展目录结构，最后进行可控爬坡压测。</p>
      </div>
      <span>4 个工作区</span>
    </section>

    <section class="feature-grid">
      <button
        v-for="feature in features"
        :key="feature.key"
        type="button"
        class="glass-card feature"
        :class="{ highlight: feature.highlight }"
        @click="router.push(feature.key)"
      >
        <div class="feature-top">
          <div class="feature-icon" :style="{ background: `${feature.color}14`, color: feature.color }">
            <n-icon :size="25"><component :is="feature.icon" /></n-icon>
          </div>
          <span>{{ feature.step }}</span>
        </div>
        <h3>{{ feature.title }}<em v-if="feature.highlight">核心</em></h3>
        <p>{{ feature.desc }}</p>
        <div class="feature-link">进入工作区 <span>→</span></div>
      </button>
    </section>

    <section class="glass-card capability-bar">
      <div v-for="item in capabilities" :key="item[0]">
        <span>{{ item[0] }}</span>
        <b>{{ item[1] }}</b>
      </div>
    </section>
  </div>
</template>

<style scoped>
.page { width: min(1200px, 100%); margin: 0 auto; padding-top: 18px; }
.hero { position: relative; display: grid; grid-template-columns: minmax(0, 1.35fr) minmax(280px, .65fr); min-height: 275px; overflow: hidden; border: 1px solid rgba(255,255,255,.68); border-radius: 24px; background: linear-gradient(125deg, rgba(255,255,255,.76), rgba(229,239,255,.54)); box-shadow: var(--shadow-card); }
.hero::before { content: ""; position: absolute; inset: 0; background: linear-gradient(100deg, transparent 55%, rgba(56,132,255,.07)); pointer-events: none; }
.hero-copy { position: relative; z-index: 2; padding: 38px 42px; }
.eyebrow { color: var(--accent); font-size: 10.5px; font-weight: 800; letter-spacing: 2px; }
h1 { max-width: 720px; margin: 9px 0 0; color: var(--text-primary); font-size: clamp(27px, 3vw, 40px); line-height: 1.18; letter-spacing: -1.1px; }
.hero-copy p { max-width: 680px; margin: 13px 0 0; color: var(--text-secondary); font-size: 13.5px; line-height: 1.75; }
.hero-actions { display: flex; gap: 10px; margin-top: 24px; }
.hero-visual { position: relative; display: grid; place-items: center; min-height: 275px; }
.radar-ring { position: absolute; border: 1px solid rgba(56,132,255,.18); border-radius: 50%; }
.ring-1 { width: 210px; height: 210px; }
.ring-2 { width: 145px; height: 145px; }
.ring-3 { width: 78px; height: 78px; background: rgba(56,132,255,.05); }
.radar-ring::before, .radar-ring::after { content: ""; position: absolute; background: rgba(56,132,255,.12); }
.radar-ring::before { left: 50%; top: 0; width: 1px; height: 100%; }
.radar-ring::after { top: 50%; left: 0; height: 1px; width: 100%; }
.radar-core { position: relative; z-index: 2; display: grid; place-items: center; width: 54px; height: 54px; border-radius: 16px; background: linear-gradient(145deg, #4f93ff, #266bdc); color: #fff; font: 800 13px/1 "SF Mono", Menlo, monospace; box-shadow: 0 12px 35px rgba(56,132,255,.35); }
.node { position: absolute; width: 10px; height: 10px; border: 3px solid rgba(255,255,255,.9); border-radius: 50%; background: var(--cyan); box-shadow: 0 3px 12px rgba(0,185,214,.35); }
.n1 { transform: translate(-86px, -58px); }.n2 { transform: translate(92px, -34px); }.n3 { transform: translate(-60px, 91px); }.n4 { transform: translate(78px, 72px); background: var(--violet); }
.status-grid { display: grid; grid-template-columns: repeat(3, 1fr); gap: 14px; margin-top: 16px; }
.status-card { padding: 17px 19px; }
.status-label { color: var(--text-tertiary); font-size: 11px; }
.status-value { display: flex; align-items: center; gap: 8px; min-height: 24px; margin-top: 5px; color: var(--text-primary); font-size: 16px; font-weight: 750; }
.status-value i { width: 8px; height: 8px; border-radius: 50%; background: var(--text-tertiary); }
.status-value i.online { background: var(--success); box-shadow: 0 0 0 4px rgba(8,179,136,.11); }
.status-value.mono { font-family: "SF Mono", Menlo, monospace; font-size: 14px; }
.status-card small { display: block; overflow: hidden; margin-top: 3px; color: var(--text-tertiary); font-size: 10.5px; text-overflow: ellipsis; white-space: nowrap; }
.section-head { display: flex; align-items: flex-end; justify-content: space-between; margin: 30px 2px 14px; }
.section-head h2 { margin: 0; color: var(--text-primary); font-size: 18px; }
.section-head p { margin: 4px 0 0; color: var(--text-tertiary); font-size: 11.5px; }
.section-head > span { color: var(--text-tertiary); font-size: 11px; }
.feature-grid { display: grid; grid-template-columns: repeat(4, 1fr); gap: 14px; }
.feature { display: flex; min-width: 0; padding: 18px; border: 1px solid var(--border-subtle); text-align: left; cursor: pointer; flex-direction: column; transition: transform var(--transition), box-shadow var(--transition), border-color var(--transition); }
.feature:hover { transform: translateY(-3px); border-color: var(--border-accent); box-shadow: var(--shadow-elevated); }
.feature:focus-visible { outline: 3px solid var(--accent-dim); outline-offset: 2px; }
.feature.highlight { background: linear-gradient(150deg, rgba(255,255,255,.72), rgba(122,108,255,.09)); border-color: rgba(122,108,255,.3); }
.feature-top { display: flex; align-items: center; justify-content: space-between; }
.feature-top > span { color: rgba(76,99,133,.35); font: 700 12px/1 "SF Mono", Menlo, monospace; }
.feature-icon { display: grid; place-items: center; width: 45px; height: 45px; border-radius: 13px; }
.feature h3 { display: flex; align-items: center; gap: 7px; margin: 16px 0 0; color: var(--text-primary); font-size: 14.5px; }
.feature h3 em { padding: 2px 7px; border-radius: 999px; background: var(--violet); color: #fff; font-size: 9.5px; font-style: normal; }
.feature p { flex: 1; min-height: 58px; margin: 7px 0 0; color: var(--text-tertiary); font-size: 11.5px; line-height: 1.62; }
.feature-link { margin-top: 13px; color: var(--text-secondary); font-size: 11.5px; font-weight: 650; }
.feature-link span { margin-left: 3px; color: var(--accent); }
.capability-bar { display: grid; grid-template-columns: repeat(4, 1fr); margin: 16px 0 24px; padding: 16px 20px; }
.capability-bar > div { min-width: 0; padding: 0 18px; border-left: 1px solid var(--border-default); }
.capability-bar > div:first-child { padding-left: 0; border-left: 0; }
.capability-bar span { display: block; color: var(--text-tertiary); font-size: 10.5px; }
.capability-bar b { display: block; overflow: hidden; margin-top: 4px; color: var(--text-secondary); font-size: 11.5px; text-overflow: ellipsis; white-space: nowrap; }
@media (max-width: 1100px) { .feature-grid { grid-template-columns: repeat(2, 1fr); } .capability-bar { grid-template-columns: repeat(2, 1fr); row-gap: 18px; } .capability-bar > div:nth-child(3) { padding-left: 0; border-left: 0; } }
@media (max-width: 780px) { .hero { grid-template-columns: 1fr; } .hero-visual { display: none; } .status-grid { grid-template-columns: 1fr; } }
@media (max-width: 560px) { .hero-copy { padding: 28px 24px; } .hero-actions { align-items: stretch; flex-direction: column; } .feature-grid, .capability-bar { grid-template-columns: 1fr; } .capability-bar > div { padding: 10px 0; border-top: 1px solid var(--border-default); border-left: 0; } }
</style>
