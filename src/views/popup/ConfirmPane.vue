<script setup lang="ts">
// Agent 权限确认面板（confirm 交互）：标题 + 理由 + 工具详情 + 单选动作 + 可选备注输入。
import { computed, nextTick, ref } from "vue";
import { useI18n } from "vue-i18n";
import { primaryShortcutLabel } from "../../lib/platform";
import { usePopupContext } from "./context";
import PermissionDiffPane from "./PermissionDiffPane.vue";

const { t } = useI18n();
const optionShortcut = (index: number) => primaryShortcutLabel(String(index + 1));
const {
  confirmRequest,
  confirmChoiceIndex,
  confirmComment,
  confirmInput,
  showConfirmInput,
  confirmDetailHtml,
  confirmToolName,
  confirmRows,
  confirmVariantLevel,
  confirmVariantLevels,
  confirmVariantRecommendedLevel,
  selectConfirmVariantLevel,
  permissionEdit,
  permissionDiff,
  permissionDiffLoading,
  selectConfirmChoice,
  onContentClick,
} = usePopupContext();

const hoveredVariantLevel = ref<number | null>(null);
const highlightedVariantLevel = computed(
  () => hoveredVariantLevel.value ?? confirmVariantLevel.value,
);

function leaveVariantTrack(event: FocusEvent): void {
  const track = event.currentTarget as HTMLElement | null;
  if (!track?.contains(event.relatedTarget as Node | null)) {
    hoveredVariantLevel.value = null;
  }
}

async function moveVariantFocus(
  event: KeyboardEvent,
  level: number,
  offset: -1 | 1,
): Promise<void> {
  const currentIndex = confirmVariantLevels.value.findIndex((entry) => entry.level === level);
  const nextIndex = Math.max(
    0,
    Math.min(confirmVariantLevels.value.length - 1, currentIndex + offset),
  );
  const nextLevel = confirmVariantLevels.value[nextIndex]?.level;
  if (nextLevel === undefined) return;
  selectConfirmVariantLevel(nextLevel);
  hoveredVariantLevel.value = nextLevel;
  await nextTick();
  const track = (event.currentTarget as HTMLElement | null)?.parentElement;
  track?.querySelectorAll<HTMLButtonElement>(".confirm-variant-segment")[nextIndex]?.focus();
}
</script>

<template>
  <section v-if="confirmRequest" class="confirm-request">
    <h1 class="confirm-request-title" data-find-seg="confirm-title">{{ confirmRequest.title }}</h1>
    <p v-if="confirmRequest.detail.summary" class="confirm-reason" data-find-seg="confirm-summary">
      <strong data-find-skip>{{ t("popup.permissionReason") }}</strong>
      {{ confirmRequest.detail.summary }}
    </p>
    <section class="confirm-tool">
      <header class="confirm-tool-header" data-find-seg="confirm-tool">{{ confirmToolName }}</header>
      <PermissionDiffPane
        v-if="permissionEdit && permissionDiff"
        :model="permissionDiff"
        :loading="permissionDiffLoading"
        :workspace="permissionEdit.workspace"
      />
      <details
        v-if="permissionEdit && confirmRequest.detail.bodyMd"
        class="confirm-raw-details"
      >
        <summary>{{ t("popup.permissionDiff.originalParams") }}</summary>
        <div
          class="markdown-body confirm-detail"
          data-find-seg="confirm-body"
          v-html="confirmDetailHtml"
          @click="onContentClick"
        ></div>
      </details>
      <div
        v-else-if="confirmRequest.detail.bodyMd"
        class="markdown-body confirm-detail"
        data-find-seg="confirm-body"
        v-html="confirmDetailHtml"
        @click="onContentClick"
      ></div>
    </section>
    <div class="confirm-options" role="radiogroup" :aria-label="confirmRequest.title">
      <!-- 前缀档位选择器（D51）：所有档位 group 共享；切档实时更新下方选项文案。 -->
      <div v-if="confirmVariantLevels.length" class="confirm-variant-bar" data-find-skip>
        <div class="confirm-variant-heading">
          <span class="confirm-variant-title">{{ t("popup.prefixLevel") }}</span>
          <button
            v-if="confirmVariantLevel !== confirmVariantRecommendedLevel"
            type="button"
            class="confirm-variant-reset"
            @click="selectConfirmVariantLevel(confirmVariantRecommendedLevel)"
          >
            {{ t("popup.resetPrefixLevel") }}
          </button>
        </div>
        <div
          class="confirm-variant-track"
          role="radiogroup"
          :aria-label="t('popup.prefixLevel')"
          @mouseleave="hoveredVariantLevel = null"
          @focusout="leaveVariantTrack"
        >
          <button
            v-for="entry in confirmVariantLevels"
            :key="entry.level"
            type="button"
            class="confirm-variant-segment"
            :class="{
              'in-prefix': entry.level <= highlightedVariantLevel,
              boundary: entry.level === highlightedVariantLevel,
              committed: entry.level === confirmVariantLevel,
            }"
            role="radio"
            :aria-checked="confirmVariantLevel === entry.level"
            :aria-label="entry.prefixLabel"
            :title="entry.label"
            :tabindex="confirmVariantLevel === entry.level ? 0 : -1"
            @mouseenter="hoveredVariantLevel = entry.level"
            @focus="hoveredVariantLevel = entry.level"
            @click="selectConfirmVariantLevel(entry.level)"
            @keydown.left.prevent="moveVariantFocus($event, entry.level, -1)"
            @keydown.right.prevent="moveVariantFocus($event, entry.level, 1)"
          >
            <code>{{ entry.label }}</code>
          </button>
        </div>
      </div>
      <div
        v-for="(row, rowPosition) in confirmRows"
        :key="row.group ?? row.choice.id"
        class="option single confirm-option"
        :class="[
          `role-${row.choice.role}`,
          { selected: confirmChoiceIndex === row.index },
        ]"
        role="radio"
        tabindex="0"
        :aria-checked="confirmChoiceIndex === row.index"
        @click="selectConfirmChoice(row.index)"
        @keydown.enter.prevent="selectConfirmChoice(row.index)"
        @keydown.space.prevent="selectConfirmChoice(row.index)"
      >
        <span class="check radio" aria-hidden="true" data-find-skip></span>
        <span class="label confirm-option-label" :data-find-seg="`confirm-choice-${rowPosition}`">
          <span>{{ row.choice.label }}</span>
          <small v-if="row.choice.description">{{ row.choice.description }}</small>
        </span>
        <kbd v-if="rowPosition < 9" class="opt-sc" data-find-skip>{{ optionShortcut(rowPosition) }}</kbd>
      </div>
    </div>
    <label v-if="showConfirmInput && confirmInput" class="confirm-input-block">
      <span>{{ confirmInput.label }}</span>
      <textarea
        v-model="confirmComment"
        class="textarea"
        :maxlength="confirmInput.maxChars"
        :placeholder="confirmInput.placeholder"
        rows="4"
      ></textarea>
      <small>{{ confirmComment.length }} / {{ confirmInput.maxChars }}</small>
    </label>
  </section>
</template>
