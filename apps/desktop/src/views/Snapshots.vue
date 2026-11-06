<script setup lang="ts">
import { computed, onActivated, ref } from "vue";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { NButton, NEmpty, NModal, NPopconfirm, useMessage } from "naive-ui";
import {
  deleteSnapshot,
  exportSnapshot,
  listSnapshots,
  readSnapshot,
  type SnapshotMeta,
} from "../platform";

const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
const message = useMessage();
const snapshots = ref<SnapshotMeta[]>([]);
const urls = ref<Record<string, string>>({});
const loading = ref(false);
const busyId = ref("");
const active = ref<SnapshotMeta | null>(null);
const PAGE_SIZE = 24;
const limit = ref(PAGE_SIZE);

const totalBytes = computed(() => snapshots.value.reduce((sum, item) => sum + item.byte_len, 0));
// NModal 的 show 只接受 boolean，这里与「当前查看的抓拍」双向绑定，关闭即清空选择。
const showViewer = computed({
  get: () => active.value !== null,
  set: (value: boolean) => { if (!value) active.value = null; },
});
const canLoadMore = computed(() => snapshots.value.length >= limit.value && limit.value < 200);

function fmtTime(ms: number) {
  if (!ms) return "—";
  const date = new Date(ms);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

function fmtSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MB`;
}

/**
 * 逐张拉取缩略图。
 *
 * 图片以 data URL 返回，所以这里控制并发（4 路）并把已取到的缓存下来：
 * 一次性铺满整个图库会把几百兆 base64 塞进内存。
 */
async function loadThumbnails(items: SnapshotMeta[]) {
  const pending = items.filter((item) => !urls.value[item.id]);
  let cursor = 0;
  const worker = async () => {
    while (cursor < pending.length) {
      const item = pending[cursor++];
      try {
        const url = await readSnapshot(item.id);
        urls.value = { ...urls.value, [item.id]: url };
      } catch {
        // 单张读取失败不影响整页；卡片会显示占位。
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(4, pending.length) }, worker));
}

async function load(reset = false) {
  if (!isTauri) return;
  if (reset) limit.value = PAGE_SIZE;
  loading.value = true;
  try {
    const items = await listSnapshots(limit.value);
    snapshots.value = items;
    await loadThumbnails(items);
  } catch (error) {
    message.error(`读取抓拍失败：${String(error)}`);
  } finally {
    loading.value = false;
  }
}

async function loadMore() {
  limit.value = Math.min(limit.value + PAGE_SIZE, 200);
  await load();
}

async function remove(item: SnapshotMeta) {
  busyId.value = item.id;
  try {
    await deleteSnapshot(item.id);
    snapshots.value = snapshots.value.filter((entry) => entry.id !== item.id);
    if (active.value?.id === item.id) active.value = null;
    message.success("已删除");
  } catch (error) {
    message.error(`删除失败：${String(error)}`);
  } finally {
    busyId.value = "";
  }
}

/** 另存为：抓拍文件本体复制到用户选定位置，不重新编码。 */
async function saveAs(item: SnapshotMeta) {
  busyId.value = item.id;
  try {
    const target = await saveDialog({
      defaultPath: item.file_name,
      filters: [{ name: "JPEG 图片", extensions: ["jpg", "jpeg"] }],
    });
    if (typeof target !== "string") return;
    await exportSnapshot(item.id, target);
    message.success(`已另存到 ${target}`);
  } catch (error) {
    message.error(`另存失败：${String(error)}`);
  } finally {
    busyId.value = "";
  }
}

async function open(item: SnapshotMeta) {
  active.value = item;
  if (!urls.value[item.id]) {
    try {
      const url = await readSnapshot(item.id);
      urls.value = { ...urls.value, [item.id]: url };
    } catch (error) {
      message.error(`读取图片失败：${String(error)}`);
    }
  }
}

onActivated(() => { void load(); });
</script>

<template>
  <div class="page">
    <div class="page-header">
      <div>
        <div class="page-title">抓拍图库</div>
        <div class="page-sub">抓拍取自推流预览的最新一帧，与平台看到的画面同源，不会打断推流</div>
      </div>
      <div class="header-actions">
        <span class="stat">{{ snapshots.length }} 张 · {{ fmtSize(totalBytes) }}</span>
        <n-button size="small" :loading="loading" @click="load(true)">刷新</n-button>
      </div>
    </div>

    <section class="glass-card panel">
      <div v-if="!snapshots.length && !loading" class="empty-wrap">
        <n-empty description="还没有抓拍">
          <template #extra>
            <span class="empty-hint">在「推流预览」页播放画面后点击「抓拍」，图片会出现在这里</span>
          </template>
        </n-empty>
      </div>

      <div v-else class="grid">
        <article v-for="item in snapshots" :key="item.id" class="shot">
          <button class="thumb" :disabled="busyId === item.id" @click="open(item)">
            <img v-if="urls[item.id]" :src="urls[item.id]" :alt="`抓拍 ${item.file_name}`" loading="lazy" />
            <div v-else class="thumb-placeholder">读取中…</div>
          </button>
          <div class="meta">
            <b>{{ fmtTime(item.captured_at_ms) }}</b>
            <span>
              {{ item.width && item.height ? `${item.width}×${item.height}` : "尺寸未知" }} · {{ fmtSize(item.byte_len) }}
            </span>
          </div>
          <div class="row-actions">
            <n-button size="tiny" secondary :disabled="busyId === item.id" @click="saveAs(item)">另存为</n-button>
            <n-popconfirm positive-text="删除" negative-text="取消" @positive-click="remove(item)">
              <template #trigger>
                <n-button size="tiny" quaternary type="error" :disabled="busyId === item.id">删除</n-button>
              </template>
              删除这张抓拍？删除后不可恢复。
            </n-popconfirm>
          </div>
        </article>
      </div>

      <div v-if="canLoadMore" class="load-more">
        <n-button size="small" :loading="loading" @click="loadMore">加载更多</n-button>
      </div>
    </section>

    <n-modal v-model:show="showViewer" preset="card" class="viewer" :title="active ? fmtTime(active.captured_at_ms) : ''">
      <img v-if="active && urls[active.id]" :src="urls[active.id]" :alt="active.file_name" class="viewer-img" />
      <div v-else class="viewer-placeholder">图片读取中…</div>
      <template #footer>
        <div class="viewer-foot">
          <span>{{ active ? `${active.file_name} · ${active.width ?? "?"}×${active.height ?? "?"}` : "" }}</span>
          <n-button v-if="active" size="small" @click="saveAs(active)">另存为</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
.page { width: min(1480px, 100%); margin: 0 auto; }
.page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin: 18px 2px 20px; }
.page-title { color: var(--text-primary); font-size: 24px; font-weight: 750; }
.page-sub { margin-top: 6px; color: var(--text-tertiary); font-size: 13px; }
.header-actions { display: flex; align-items: center; gap: 12px; }
.stat { color: var(--text-tertiary); font-size: 12px; }
.panel { padding: 18px; }
.empty-wrap { display: grid; place-items: center; padding: 56px 0; }
.empty-hint { color: var(--text-tertiary); font-size: 12px; }
.grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(240px, 1fr)); gap: 16px; }
.shot { display: grid; gap: 8px; padding: 10px; border: 1px solid var(--border-default); border-radius: 12px; background: rgba(255,255,255,.5); }
.thumb { display: block; width: 100%; aspect-ratio: 16 / 9; padding: 0; overflow: hidden; border: 0; border-radius: 8px; background: #071424; cursor: zoom-in; }
.thumb img { width: 100%; height: 100%; object-fit: contain; }
.thumb-placeholder { display: grid; place-items: center; width: 100%; height: 100%; color: rgba(220,232,250,.5); font-size: 12px; }
.meta { display: grid; gap: 3px; }
.meta b { color: var(--text-secondary); font-size: 12px; font-weight: 650; }
.meta span { color: var(--text-tertiary); font-size: 11px; }
.row-actions { display: flex; gap: 8px; }
.load-more { display: flex; justify-content: center; padding-top: 16px; }
.viewer { width: min(1080px, 92vw); }
.viewer-img { width: 100%; max-height: 68vh; object-fit: contain; border-radius: 10px; background: #071424; }
.viewer-placeholder { padding: 40px; color: var(--text-tertiary); text-align: center; }
.viewer-foot { display: flex; align-items: center; justify-content: space-between; gap: 12px; color: var(--text-tertiary); font-size: 12px; }
@media (max-width: 900px) { .grid { grid-template-columns: repeat(auto-fill, minmax(180px, 1fr)); } }
</style>
