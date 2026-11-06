<script setup lang="ts">
// 顶栏通知铃铛 + 抽屉。只做展示与交互，事件订阅在 notification-center.ts。
import { computed, onMounted, onUnmounted, ref } from "vue";
import { NBadge, NButton, NDrawer, NDrawerContent, NEmpty, NIcon, NTag, NTooltip } from "naive-ui";
import NotificationsOutline from "@vicons/ionicons5/es/NotificationsOutline.js";
import TrashOutline from "@vicons/ionicons5/es/TrashOutline.js";
import { formatRelative } from "../notifications.mjs";
import {
  clearNotifications,
  markNotificationRead,
  markNotificationsRead,
  notificationItems,
  notificationUnread,
} from "../notification-center";

const open = ref(false);
const now = ref(Date.now());
let ticker: number | null = null;

const levelText = { info: "信息", success: "成功", warning: "注意", error: "错误" } as const;
const levelTagType = { info: "info", success: "success", warning: "warning", error: "error" } as const;

const hasUnread = computed(() => notificationUnread.value > 0);

function toggle() {
  open.value = !open.value;
  // 打开即视为已读：铃铛的红点是"有新东西"，进了抽屉就算看过了。
  if (open.value && hasUnread.value) markNotificationsRead();
}

onMounted(() => {
  // 相对时间每分钟刷一次，不必每秒重渲染整列表。
  ticker = window.setInterval(() => { now.value = Date.now(); }, 60_000);
});
onUnmounted(() => { if (ticker) clearInterval(ticker); });
</script>

<template>
  <n-tooltip trigger="hover">
    <template #trigger>
      <n-badge :value="notificationUnread" :max="99" :show="hasUnread" dot>
        <n-button quaternary circle size="small" aria-label="通知中心" @click="toggle">
          <template #icon><n-icon :component="NotificationsOutline" /></template>
        </n-button>
      </n-badge>
    </template>
    {{ hasUnread ? `${notificationUnread} 条未读通知` : "通知中心" }}
  </n-tooltip>

  <n-drawer v-model:show="open" :width="380" placement="right">
    <n-drawer-content title="通知中心" closable>
      <div v-if="notificationItems.length" class="notice-list">
        <article
          v-for="item in notificationItems"
          :key="item.id"
          class="notice-row"
          :class="[`level-${item.level}`, { unread: !item.read }]"
          @click="markNotificationRead(item.id)"
        >
          <div class="notice-head">
            <n-tag size="small" :bordered="false" :type="levelTagType[item.level]">{{ levelText[item.level] }}</n-tag>
            <strong>{{ item.title }}</strong>
            <small>{{ formatRelative(item.ts, now) }}</small>
          </div>
          <p v-if="item.detail">{{ item.detail }}</p>
        </article>
      </div>
      <n-empty v-else description="暂无通知。平台操作、订阅变化与设备状态会出现在这里" class="notice-empty" />

      <template #footer>
        <div class="notice-foot">
          <span>最多保留 50 条，最新在最前</span>
          <n-button text size="small" :disabled="!notificationItems.length" @click="clearNotifications">
            <template #icon><n-icon :component="TrashOutline" /></template>
            清空
          </n-button>
        </div>
      </template>
    </n-drawer-content>
  </n-drawer>
</template>

<style scoped>
.notice-list { display: grid; gap: 6px; }
.notice-row { padding: 9px 10px; border: 1px solid var(--border-subtle); border-left-width: 3px; border-radius: 9px; background: rgba(255,255,255,.5); cursor: pointer; }
.notice-row.unread { background: rgba(56,132,255,.06); }
.notice-row.level-info { border-left-color: #3884ff; }
.notice-row.level-success { border-left-color: #18a058; }
.notice-row.level-warning { border-left-color: #d07818; }
.notice-row.level-error { border-left-color: #d03050; }
.notice-head { display: flex; align-items: center; gap: 6px; }
.notice-head strong { flex: 1; min-width: 0; overflow: hidden; font-size: 12px; text-overflow: ellipsis; white-space: nowrap; }
.notice-head small { flex: none; color: var(--text-tertiary); font-size: 10px; }
.notice-row p { margin: 5px 0 0; color: var(--text-tertiary); font-size: 11px; overflow-wrap: anywhere; }
.notice-empty { margin-top: 40px; }
.notice-foot { display: flex; align-items: center; justify-content: space-between; gap: 10px; color: var(--text-tertiary); font-size: 10px; }
</style>
