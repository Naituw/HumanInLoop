<script setup lang="ts">
// 旧版（顺序模式）：单题 / 实验开关关时——一次显示一个问题，上一步/下一步左右滑动切换。
import { useI18n } from "vue-i18n";
import MarkdownContent from "../../components/MarkdownContent.vue";
import { usePopupContext } from "./context";
import AnswerComposer from "./AnswerComposer.vue";
import { optionDisplayText } from "./optionDisplay";

const { t } = useI18n();
const {
  request,
  showQuestionHeader,
  showDescription,
  questionHeaderLabel,
  qHeaderRef,
  transitionName,
  onQuestionEntered,
  current,
  currentQuestion,
  viewSource,
  chosen,
  single,
  selectOnly,
  optionHotkey,
  toggle,
} = usePopupContext();
</script>

<template>
  <!-- 问题头部：间距 + 分割线 + 问号图标 + 「Question i/n」 -->
  <div
    v-if="showQuestionHeader"
    :ref="(el) => (qHeaderRef = el as HTMLElement | null)"
    class="q-header"
    :class="{ 'with-divider': showDescription }"
  >
    <svg class="q-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
      <circle cx="12" cy="12" r="9" />
      <path d="M9.2 9.3a2.8 2.8 0 0 1 5.4 1c0 1.9-2.8 2.5-2.8 2.5" />
      <path d="M12 17.2h.01" />
    </svg>
    <span class="q-label">{{ questionHeaderLabel }}</span>
  </div>

  <!-- 当前问题区（上一个/下一个左右滑动） -->
  <Transition :name="transitionName" mode="out-in" @after-enter="onQuestionEntered">
    <div class="question-pane" :key="current">
      <MarkdownContent
        v-if="request?.isMarkdown && !viewSource && currentQuestion?.message"
        :source="currentQuestion.message"
        :data-find-seg="`q-${current}-msg`"
      />
      <pre
        v-else-if="currentQuestion?.message"
        class="plain-body"
        :data-find-seg="`q-${current}-msg`"
      >{{ currentQuestion?.message }}</pre>

      <div v-if="currentQuestion && currentQuestion.predefinedOptions.length" class="options">
        <div
          v-for="(opt, i) in currentQuestion.predefinedOptions"
          :key="i"
          class="option"
          :class="{ selected: chosen.includes(opt.text), single }"
          @click="toggle(current, opt.text)"
        >
          <span class="check" :class="{ radio: single }" data-find-skip>{{ single ? "" : (chosen.includes(opt.text) ? "✓" : "") }}</span>
          <span class="label">
            <span v-if="request?.whatsNext && opt.todoId" class="todo-option-badge" data-find-skip>TODO</span>
            <span v-if="opt.recommended" class="rec-badge" data-find-skip><span class="rec-badge-pill"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 9V5a3 3 0 0 0-3-3l-4 9v11h11.28a2 2 0 0 0 2-1.7l1.38-9a2 2 0 0 0-2-2.3z"></path><path d="M7 22H4a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h3"></path></svg>{{ t("popup.recommended") }}</span></span>
            <span :data-find-seg="`q-${current}-opt-${i}`">{{ optionDisplayText(opt, Boolean(request?.whatsNext), t("popup.todos.optionPrefix")) }}</span>
            <span
              v-if="request?.whatsNext && opt.todoId && opt.todoAttachments?.length"
              class="todo-attachment-badge"
              data-find-skip
            >{{ t("common.attachmentBadge", { n: opt.todoAttachments.length }) }}</span>
          </span>
          <kbd v-if="optionHotkey(i)" class="opt-sc" data-find-skip>{{ optionHotkey(i) }}</kbd>
        </div>
      </div>

      <AnswerComposer v-if="!selectOnly" :q-index="current" />
    </div>
  </Transition>
</template>
