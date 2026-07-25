<script setup lang="ts">
// 「实验」tab：当前没有在试验中的功能（「从 IM 创建 Agent 任务」已转正到「高级」tab）。
// Tab 本身仍由通用页底部的隐蔽开关 `experimental.enabled` 控制显示，留作下一个实验功能的落点。
import { useI18n } from "vue-i18n";

const { t } = useI18n();
</script>

<template>
  <div class="empty-state">
    <p class="empty-title">{{ t("settings.experimental.emptyTitle") }}</p>
    <p class="empty-desc">{{ t("settings.experimental.emptyDesc") }}</p>
  </div>
</template>
