<script setup lang="ts">
// 「新建 Agent 任务」窗口壳（spec gui-agent-task-launch）：表单主体在共享组件
// `newtask/NewTaskForm.vue`（控制台内嵌版共用）。本层负责窗口 chrome、主题/语言、
// URL 预选与 goto 事件、启动成功后自动关窗（G10）。
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { applyTheme } from "../lib/theme";
import { applyLanguage } from "../i18n";
import { newTaskInit } from "../lib/ipc";
import type { ThemeMode } from "../lib/types";
import NewTaskForm from "./newtask/NewTaskForm.vue";

const { t } = useI18n();

const form = ref<InstanceType<typeof NewTaskForm> | null>(null);
const params = new URLSearchParams(window.location.search);
const initialProject = params.get("project");
const initialTodo = params.get("todo");

async function onLaunched(): Promise<void> {
  // Success: Terminal is open and attention has moved there → auto-close (G10).
  // Best-effort: a close failure must not surface as a launch error.
  try {
    await getCurrentWebviewWindow().close();
  } catch (err) {
    console.warn("new-task window close failed", err);
  }
}

let unlistenSettings: UnlistenFn | null = null;
let unlistenGoto: UnlistenFn | null = null;

onMounted(async () => {
  try {
    const init = await newTaskInit();
    applyTheme(init.theme);
    applyLanguage(init.lang);
  } catch {
    /* 读取失败：保持兜底外观 */
  }
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (e) => {
      if (typeof e.payload.theme === "string") applyTheme(e.payload.theme);
      if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
    }
  );
  // 窗口已开时带新预选再次打开（待办行入口）→ 整体重置到新预选。
  unlistenGoto = await listen<{ project?: string | null; todo?: string | null }>(
    "newtask-goto",
    async (e) => {
      await form.value?.applyPreselect(e.payload.project ?? null, e.payload.todo ?? null);
    }
  );
});

onBeforeUnmount(() => {
  unlistenSettings?.();
  unlistenGoto?.();
});
</script>

<template>
  <div class="newtask-win">
    <header class="nt-header" data-tauri-drag-region>
      <span class="nt-title" data-tauri-drag-region>{{ t("newTask.title") }}</span>
    </header>
    <NewTaskForm
      ref="form"
      :initial-project="initialProject"
      :initial-todo="initialTodo"
      @launched="onLaunched"
    />
  </div>
</template>

<style scoped>
.newtask-win {
  display: flex;
  flex-direction: column;
  height: 100%;
  color: var(--text-primary);
}
.nt-header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.macos .nt-header {
  padding-top: 30px;
}
.nt-title {
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
}
</style>
