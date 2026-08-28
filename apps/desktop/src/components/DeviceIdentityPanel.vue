<script setup lang="ts">
// 设备身份与国标参数卡片:设备自身的国标参数(平台连接参数在顶栏全局平台档案里),
// 卡片下半部分是媒体源配置(MediaSourcePanel)。
import { useDevice } from "../device";
import MediaSourcePanel from "./MediaSourcePanel.vue";

const { form } = useDevice();
</script>

<template>
  <div class="glass-card console-panel identity-panel">
    <div class="panel-head-tight">
      <div class="panel-title">设备身份与国标参数</div>
      <span class="param-badge">GB/T 28181</span>
    </div>

    <!-- 国标基础与扩展参数:自适应多列网格 -->
    <div class="identity-grid">
      <div class="fg">
        <label>国标版本</label>
        <div class="seg" role="group" aria-label="国标版本">
          <button type="button" :class="{ on: form.gb_version === '2022' }" @click="form.gb_version = '2022'">
            GB/T 2022
          </button>
          <button type="button" :class="{ on: form.gb_version === '2016' }" @click="form.gb_version = '2016'">
            GB/T 2016
          </button>
        </div>
      </div>
      <div class="fg">
        <label for="device-id-input">设备国标编号</label>
        <input id="device-id-input" v-model="form.device_id" class="inp" placeholder="20 位国标 ID" />
      </div>
      <div class="fg">
        <label for="channel-name-input">主通道名称</label>
        <input id="channel-name-input" v-model="form.channel_name" class="inp" placeholder="通道名称" />
      </div>
      <div class="fg">
        <label for="catalog-template-select">目录组织模板</label>
        <select id="catalog-template-select" v-model="form.catalog_template" class="inp">
          <option value="">单通道(默认)</option>
          <option value="nvr-8ch">8 通道 NVR</option>
          <option value="civil-3x2">跨区划(3 区 × 2 通道)</option>
          <option value="large-16ch">16 通道大型监控</option>
        </select>
      </div>
      <div class="fg">
        <label for="manufacturer-input">设备厂商 (Manufacturer)</label>
        <input id="manufacturer-input" v-model="form.manufacturer" class="inp" placeholder="厂商名称" />
      </div>
      <div class="fg">
        <label for="model-input">设备型号 (Model)</label>
        <input id="model-input" v-model="form.model" class="inp" placeholder="产品型号" />
      </div>
      <div class="fg">
        <label for="firmware-input">固件版本 (Firmware)</label>
        <input id="firmware-input" v-model="form.firmware" class="inp" placeholder="固件版本号" />
      </div>
      <div class="fg">
        <label for="expires-input">注册有效期 (Expires · 秒)</label>
        <input
          id="expires-input"
          v-model.number="form.expires"
          type="number"
          min="60"
          max="86400"
          step="60"
          class="inp"
          placeholder="3600"
        />
      </div>
      <div class="fg">
        <label for="heartbeat-input">心跳周期 (Interval · 秒)</label>
        <input
          id="heartbeat-input"
          v-model.number="form.heartbeat_interval"
          type="number"
          min="5"
          max="300"
          step="5"
          class="inp"
          placeholder="60"
        />
      </div>
      <div class="fg">
        <label for="heartbeat-timeout-input">心跳超时次数 (Max Timeout)</label>
        <input
          id="heartbeat-timeout-input"
          v-model.number="form.max_heartbeat_timeout"
          type="number"
          min="1"
          max="10"
          step="1"
          class="inp"
          placeholder="3"
        />
      </div>
    </div>

    <MediaSourcePanel />
  </div>
</template>

<style scoped>
.identity-panel { box-sizing: border-box; display: flex; flex-direction: column; }
.identity-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 6px 10px; }
.identity-grid .fg { margin-bottom: 0; }

@media (max-width: 760px) {
  .identity-grid { grid-template-columns: 1fr; }
}
</style>
