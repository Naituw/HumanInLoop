<script setup lang="ts">
// 纵向模式（实验开关 + 多题）：所有问题纵向平铺成卡片，scroll-spy 定位当前题。
import { useI18n } from "vue-i18n";
import MarkdownContent from "../../components/MarkdownContent.vue";
import { usePopupContext } from "./context";
import AnswerComposer from "./AnswerComposer.vue";
import { optionDisplayText } from "./optionDisplay";

const { t } = useI18n();
const {
  request,
  questions,
  total,
  viewSource,
  chosenByQ,
  single,
  selectOnly,
  cardOptionHotkey,
  toggle,
  setActive,
  setCardRef,
  setSentinelRef,
  dropTargetQ,
} = usePopupContext();
</script>

<template>
  <div
    v-for="(q, qi) in questions"
    :key="qi"
    :ref="(el) => setCardRef(el as HTMLElement | null, qi)"
    class="q-card"
    :class="{ 'drop-target': dropTargetQ === qi }"
    :data-q-index="qi"
    @mousedown="setActive(qi, false)"
  >
    <!-- 问题头部：问号图标 + 「Question i/n」。每题上方加分割线（与 Message/上一题区隔）。 -->
    <div
      class="q-header with-divider"
    >
      <svg class="q-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round">
        <circle cx="12" cy="12" r="9" />
        <path d="M9.2 9.3a2.8 2.8 0 0 1 5.4 1c0 1.9-2.8 2.5-2.8 2.5" />
        <path d="M12 17.2h.01" />
      </svg>
      <span class="q-label">{{
        t("popup.question.indexed", { i: qi + 1, n: total })
      }}</span>
    </div>

    <MarkdownContent
      v-if="request?.isMarkdown && !viewSource && q.message"
      :source="q.message"
      :data-find-seg="`q-${qi}-msg`"
    />
    <pre
      v-else-if="q.message"
      class="plain-body"
      :data-find-seg="`q-${qi}-msg`"
    >{{ q.message }}</pre>

    <div v-if="q.predefinedOptions.length" class="options">
      <div
        v-for="(opt, i) in q.predefinedOptions"
        :key="i"
        class="option"
        :class="{ selected: (chosenByQ[qi] ?? []).includes(opt.text), single }"
        @click="toggle(qi, opt.text)"
      >
        <span class="check" :class="{ radio: single }" data-find-skip>{{ single ? "" : ((chosenByQ[qi] ?? []).includes(opt.text) ? "✓" : "") }}</span>
        <span class="label">
          <span v-if="request?.whatsNext && opt.todoId" class="todo-option-badge" data-find-skip>TODO</span>
          <span v-if="opt.recommended" class="rec-badge" data-find-skip><span class="rec-badge-pill"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 9V5a3 3 0 0 0-3-3l-4 9v11h11.28a2 2 0 0 0 2-1.7l1.38-9a2 2 0 0 0-2-2.3z"></path><path d="M7 22H4a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h3"></path></svg>{{ t("popup.recommended") }}</span></span>
          <span :data-find-seg="`q-${qi}-opt-${i}`">{{ optionDisplayText(opt, Boolean(request?.whatsNext), t("popup.todos.optionPrefix")) }}</span>
          <span
            v-if="request?.whatsNext && opt.todoId && opt.todoAttachments?.length"
            class="todo-attachment-badge"
            data-find-skip
          >{{ t("common.attachmentBadge", { n: opt.todoAttachments.length }) }}</span>
        </span>
        <kbd v-if="cardOptionHotkey(qi, i)" class="opt-sc" data-find-skip>{{ cardOptionHotkey(qi, i) }}</kbd>
      </div>
    </div>

    <AnswerComposer v-if="!selectOnly" :q-index="qi" collapsible />

    <!-- 底部哨兵：进视口即「已看到」该题（兼容超长题） -->
    <div
      :ref="(el) => setSentinelRef(el as HTMLElement | null, qi)"
      class="q-sentinel"
      :data-q-sentinel="qi"
      aria-hidden="true"
    ></div>
  </div>
</template>
