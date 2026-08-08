<script setup lang="ts">
import { computed, defineAsyncComponent } from "vue";
// 弹窗是关键路径：保持静态导入，入口 chunk 直接含 PopupView，不引入额外动态 import 往返。
import PopupView from "./views/PopupView.vue";
// 设置 / 历史 / Agents 非关键路径：异步加载，Vite 自动分块，使弹窗入口 chunk 不再
// 携带这三个视图及其依赖（减少解析/执行，落在 page boot 与 frontend boot 段）。
const SettingsView = defineAsyncComponent(() => import("./views/SettingsView.vue"));
const HistoryView = defineAsyncComponent(() => import("./views/HistoryView.vue"));
const AgentsView = defineAsyncComponent(() => import("./views/AgentsView.vue"));
const InterjectView = defineAsyncComponent(() => import("./views/InterjectView.vue"));
const TodosView = defineAsyncComponent(() => import("./views/TodosView.vue"));
const NewTaskView = defineAsyncComponent(() => import("./views/NewTaskView.vue"));
const ForkTaskView = defineAsyncComponent(() => import("./views/ForkTaskView.vue"));

// 视图模式由 Rust 侧通过窗口 URL 的查询参数注入：
// ?view=popup | settings | history | agents | interject | todos | newtask | forktask
const view = computed(() => {
  const params = new URLSearchParams(window.location.search);
  return params.get("view") ?? "popup";
});
</script>

<template>
  <SettingsView v-if="view === 'settings'" />
  <HistoryView v-else-if="view === 'history'" />
  <AgentsView v-else-if="view === 'agents'" />
  <InterjectView v-else-if="view === 'interject'" />
  <TodosView v-else-if="view === 'todos'" />
  <NewTaskView v-else-if="view === 'newtask'" />
  <ForkTaskView v-else-if="view === 'forktask'" />
  <PopupView v-else />
</template>
