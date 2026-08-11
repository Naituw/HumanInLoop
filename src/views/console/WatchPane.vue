<script setup lang="ts">
// 焦点会话的 Watch 帧视图（spec gui-agent-console C2）：最后一段助手文字（Markdown）+
// 足迹时间线 + TODO 面板。帧由 daemon 按签名推送（`agent-detail` 事件，父级转发）。
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import MarkdownContent from "../../components/MarkdownContent.vue";
import type { AgentDetailFrame, DetailStep } from "../../lib/types";

const { t } = useI18n();

const props = defineProps<{
  /** 当前帧（尚未收到首帧时为 null → 加载占位）。 */
  frame: AgentDetailFrame | null;
}>();

const todosOpen = ref(true);

function stepLabel(s: DetailStep): string {
  if (s.kind === "other") return s.name || "?";
  return t(`console.step.${s.kind}`);
}

function clockTime(secs?: number | null): string {
  if (!secs) return "";
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

const todoSummary = computed(() => {
  const f = props.frame;
  if (!f || f.todos.length === 0) return "";
  const done = f.todos.filter((td) => td.state === "completed").length;
  const cur = f.todos.find((td) => td.state === "inProgress");
  const head = t("console.todoSummary", { done, total: f.todos.length });
  return cur ? head + t("console.todoCurrent", { text: cur.content }) : head;
});
</script>

<template>
  <div v-if="!frame" class="loading">
    <span class="spinner" />
  </div>
  <template v-else>
    <div class="act-heading-row">
      <span class="act-heading">{{ t("console.activityHeading", { time: clockTime(frame.at) }) }}</span>
      <slot name="heading-actions" />
    </div>
    <MarkdownContent v-if="frame.text" class="act-text" :source="frame.text" />
    <p v-else-if="!frame.steps.length" class="act-none">{{ t("console.noActivity") }}</p>

    <div v-if="frame.steps.length" class="steps">
      <div v-if="frame.stepsOmitted > 0" class="step omitted">
        {{ t("console.stepsOmitted", { n: frame.stepsOmitted }) }}
      </div>
      <div v-for="(st, i) in frame.steps" :key="i" class="step">
        <span class="step-dot" :class="st.state" />
        <span class="step-label">{{ stepLabel(st) }}</span>
        <span v-if="st.object" class="step-obj">{{ st.object }}</span>
      </div>
    </div>

    <div v-if="frame.todos.length" class="todos">
      <button class="todos-head" @click="todosOpen = !todosOpen">
        <svg class="chevron" :class="{ closed: !todosOpen }" viewBox="0 0 12 12">
          <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6"
            stroke-linecap="round" stroke-linejoin="round" />
        </svg>
        <span>📋 {{ todoSummary }}</span>
      </button>
      <ul v-show="todosOpen" class="todo-list">
        <li v-for="(td, i) in frame.todos" :key="i" class="todo" :class="td.state">
          <span class="todo-dot" :class="td.state" />
          <span class="todo-text">{{ td.content }}</span>
        </li>
      </ul>
    </div>
  </template>
</template>

<style scoped>
.loading {
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 40px 0;
}
.spinner {
  width: 18px;
  height: 18px;
  border-radius: 50%;
  border: 2px solid color-mix(in srgb, var(--text-primary) 18%, transparent);
  border-top-color: var(--text-secondary);
  animation: con-spin 0.7s linear infinite;
}
@keyframes con-spin {
  to {
    transform: rotate(360deg);
  }
}
.act-heading-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 8px;
}
.act-heading {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-tertiary);
}
.act-text {
  font-size: 13px;
  margin-bottom: 12px;
}
.act-none {
  font-size: 12px;
  color: var(--text-tertiary);
}
.steps {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 14px;
}
.step {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}
.step.omitted {
  color: var(--text-tertiary);
  font-size: 11.5px;
  padding-left: 16px;
}
.step-dot {
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
}
.step-dot.running {
  background: #30d158;
  animation: pulse-green 1.4s ease-in-out infinite;
}
.step-dot.done {
  background: var(--text-tertiary);
}
.step-dot.failed {
  background: #ff453a;
}
@keyframes pulse-green {
  0%,
  100% {
    box-shadow: 0 0 0 0 color-mix(in srgb, #30d158 30%, transparent);
  }
  50% {
    box-shadow: 0 0 0 4px color-mix(in srgb, #30d158 14%, transparent);
  }
}
.step-label {
  font-weight: 600;
}
.step-obj {
  min-width: 0;
  font-style: italic;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.todos {
  border: var(--hairline) solid var(--border);
  border-radius: 9px;
  overflow: hidden;
}
.todos-head {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  border: none;
  background: color-mix(in srgb, var(--text-primary) 4%, transparent);
  padding: 7px 10px;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
}
.chevron {
  flex: 0 0 auto;
  width: 11px;
  height: 11px;
  transform: rotate(90deg);
  transition: transform 0.15s ease;
}
.chevron.closed {
  transform: rotate(0deg);
}
.todo-list {
  list-style: none;
  margin: 0;
  padding: 6px 12px 8px;
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.todo {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}
.todo-dot {
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  border: 1.4px solid var(--text-tertiary);
  background: transparent;
}
.todo-dot.inProgress {
  background: #30d158;
  border-color: #30d158;
}
.todo-dot.completed {
  background: var(--text-tertiary);
  border-color: var(--text-tertiary);
}
.todo.inProgress .todo-text {
  font-weight: 600;
}
.todo.completed .todo-text {
  color: var(--text-tertiary);
  text-decoration: line-through;
}
</style>
