<script setup lang="ts">
// 待办下拉区（spec todo-whats-next D7，第 11 轮改版）：跟在最后一个问题后面，仅在
// 该提问项目有待办时显示。收起时只显示数量；展开后是与预选答案同构的可选行
// （选中＝提交时其文本加前缀并入最后一题回答、后端按 id 出队）+ 行内删除按钮。
// whats-next 弹窗不渲染本区（待办已是问题选项本体）；严格选择下仅可删除。
import { onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { usePopupContext } from "./context";

const { t } = useI18n();
const {
  todos,
  todosOpen,
  todoChosenIds,
  todoChipsEnabled,
  todoSectionVisible,
  toggleTodo,
  removeTodo,
  submitting,
} = usePopupContext();

// 删除二次确认（第 13 轮定案，防误删）：首次点 ✕ 变「确认删除」文字，再点才删。
// 点删除按钮之外的任意区域取消确认态（按钮 @click.stop 不冒泡，能冒泡到 document
// 的点击都算「别处」）。
const confirmDeleteId = ref<string | null>(null);

function onDocClick() {
  confirmDeleteId.value = null;
}

onMounted(() => document.addEventListener("click", onDocClick));
onBeforeUnmount(() => document.removeEventListener("click", onDocClick));

function onDelete(id: string) {
  if (confirmDeleteId.value !== id) {
    confirmDeleteId.value = id;
    return;
  }
  confirmDeleteId.value = null;
  void removeTodo(id);
}

function onToggleOpen() {
  todosOpen.value = !todosOpen.value;
  confirmDeleteId.value = null;
}
</script>

<template>
  <div v-if="todoSectionVisible" class="todo-section">
    <button
      class="todo-toggle"
      type="button"
      :aria-expanded="todosOpen"
      @click="onToggleOpen"
    >
      <svg
        class="todo-caret"
        :class="{ open: todosOpen }"
        viewBox="0 0 12 12"
        width="10"
        height="10"
        aria-hidden="true"
      >
        <path
          d="M4 2.5 8 6l-4 3.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <span class="todo-title">{{ t("popup.todos.title") }}</span>
      <span class="todo-count">{{ todos.length }}</span>
      <span v-if="!todosOpen && todoChosenIds.length" class="todo-picked">
        {{ t("popup.todos.picked", { n: todoChosenIds.length }) }}
      </span>
    </button>

    <div v-if="todosOpen" class="options todo-options">
      <div
        v-for="td in todos"
        :key="td.id"
        class="option todo-option"
        :class="{
          selected: todoChosenIds.includes(td.id),
          static: !todoChipsEnabled,
        }"
        :title="todoChipsEnabled ? t('popup.todos.chipHint') : undefined"
        @click="!submitting && toggleTodo(td.id)"
      >
        <span v-if="todoChipsEnabled" class="check">{{
          todoChosenIds.includes(td.id) ? "✓" : ""
        }}</span>
        <span class="label">
          {{ td.text }}
          <span v-if="td.attachments?.length" class="todo-file-count">
            {{ t("common.attachmentBadge", { n: td.attachments.length }) }}
          </span>
        </span>
        <button
          v-if="confirmDeleteId === td.id"
          class="todo-del-confirm"
          type="button"
          :disabled="submitting"
          @click.stop="onDelete(td.id)"
        >
          {{ t("popup.todos.deleteConfirm") }}
        </button>
        <button
          v-else
          class="todo-del"
          type="button"
          :disabled="submitting"
          :title="t('popup.todos.delete')"
          @click.stop="onDelete(td.id)"
        >
          <svg viewBox="0 0 12 12" width="9" height="9" aria-hidden="true">
            <line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
            <line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.todo-file-count {
  margin-left: 6px;
  color: var(--text-secondary);
  font-size: inherit;
  white-space: nowrap;
}
</style>
