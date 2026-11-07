import { ref } from "vue";

/**
 * 首页本地预览是否由用户开启。
 *
 * 模块级状态:切换页面后保持用户的选择;不持久化,应用重启一律恢复为关闭
 * (media-pipeline-v2 §9.3)。关闭预览不影响采集与平台推流。
 */
export const previewEnabled = ref(false);
