<script setup lang="ts">
// 控制台内嵌「新建任务」面板（spec gui-agent-console C4）：项目锁定为「＋」所在项目，
// 表单主体复用共享 NewTaskForm；启动成功 emit launched（父级切回详情并自动选中新会话）。
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import NewTaskForm from "../newtask/NewTaskForm.vue";

const { t } = useI18n();

const props = defineProps<{
  /** 目标项目路径（锁定）。 */
  project: string;
}>();

const emit = defineEmits<{
  launched: [];
  close: [];
}>();

const projectName = computed(() => {
  const parts = props.project.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || props.project;
});
</script>

<template>
  <div class="ntp">
    <div class="ntp-head">
      <span class="ntp-title">{{ t("newTask.title") }}</span>
      <span class="ntp-proj" :title="project">{{ projectName }}</span>
      <span class="spacer" />
      <button class="icon-btn" :title="t('agents.confirmCancel')" @click="emit('close')">
        <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
      </button>
    </div>
    <NewTaskForm :key="project" :lock-project="project" @launched="emit('launched')" />
  </div>
</template>

<style scoped>
.ntp {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
}
.ntp-head {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 16px 10px;
  border-bottom: var(--hairline) solid var(--border);
}
.ntp-title {
  font-size: 14px;
  font-weight: 600;
}
.ntp-proj {
  font-size: 12px;
  color: var(--text-secondary);
  padding: 1px 8px;
  border-radius: 5px;
  background: color-mix(in srgb, var(--text-primary) 7%, transparent);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.spacer {
  flex: 1 1 auto;
}
.icon-btn {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
}
.icon-btn:hover {
  background: color-mix(in srgb, var(--text-primary) 10%, transparent);
  color: var(--text-primary);
}
.icon-btn svg {
  width: 13px;
  height: 13px;
}
</style>
