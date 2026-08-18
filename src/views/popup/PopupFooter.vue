<script setup lang="ts">
// 页脚区（多根组件）：R6 一次性 IM 引导条 + 三种底部按钮排布（多题导航 / 单题发送 / 确认提交）。
import { useI18n } from "vue-i18n";
import { primaryShortcutLabel } from "../../lib/platform";
import { usePopupContext } from "./context";

const { t } = useI18n();
const cancelShortcut = primaryShortcutLabel("w");
const previousShortcut = primaryShortcutLabel("[");
const {
  isConfirm,
  isMulti,
  verticalMode,
  imTipVisible,
  imTipConfigure,
  imTipDismiss,
  submitting,
  requestCancel,
  canGoPrev,
  goPrev,
  goNext,
  actionQuestionIndex,
  total,
  onLastQuestion,
  submitShowsCmdEnter,
  submitKeyLabel,
  submitPrimary,
  nextPrimary,
  lastSeen,
  allViewed,
  canSubmit,
  submit,
  confirmRequest,
  confirmCanSubmit,
  submitConfirm,
  requestConfirmClose,
} = usePopupContext();
</script>

<template>
  <!-- 首次运行引导（R6）：未配置 IM 渠道时的一次性提示条 -->
  <div v-if="imTipVisible" class="im-tip">
    <span class="im-tip-text">{{ t("popup.imTip.text") }}</span>
    <button class="im-tip-action" type="button" @click="imTipConfigure">
      {{ t("popup.imTip.action") }}
    </button>
    <button
      class="im-tip-close"
      type="button"
      :title="t('popup.imTip.dismiss')"
      @click="imTipDismiss"
    >
      <svg viewBox="0 0 12 12" width="10" height="10" aria-hidden="true">
        <line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
        <line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
      </svg>
    </button>
  </div>

  <!-- 多问题底部：取消(左) + 上一个/下一个/提交(右) -->
  <div v-if="!isConfirm && isMulti" class="footer" data-tauri-drag-region>
    <button class="btn" type="button" :disabled="submitting" @click="requestCancel">
      {{ t("common.cancel") }} <kbd class="sc">{{ cancelShortcut }}</kbd>
    </button>
    <span class="spacer"></span>
    <button
      class="btn"
      type="button"
      :disabled="submitting || !canGoPrev"
      @mousedown.prevent
      @click="goPrev"
    >
      {{ t("popup.prev") }} <kbd v-if="canGoPrev" class="sc">{{ previousShortcut }}</kbd>
    </button>
    <button
      class="btn"
      :class="{ 'btn-primary': nextPrimary }"
      type="button"
      :disabled="submitting || actionQuestionIndex === total - 1"
      @mousedown.prevent
      @click="goNext"
    >
      {{ t("popup.next") }}
      <kbd v-if="!onLastQuestion && !submitShowsCmdEnter" class="sc">{{ submitKeyLabel }}</kbd>
    </button>
    <button
      v-if="verticalMode ? lastSeen : allViewed"
      class="btn"
      :class="{ 'btn-primary': submitPrimary }"
      type="button"
      :disabled="submitting || !canSubmit"
      @click="submit"
    >
      {{ t("common.submit") }}
      <kbd v-if="submitShowsCmdEnter" class="sc">{{ submitKeyLabel }}</kbd>
    </button>
  </div>

  <!-- 单问题底部：取消(左) + 发送(右) -->
  <div v-else-if="!isConfirm" class="footer" data-tauri-drag-region>
    <button class="btn" type="button" :disabled="submitting" @click="requestCancel">
      {{ t("common.cancel") }} <kbd class="sc">{{ cancelShortcut }}</kbd>
    </button>
    <span class="spacer"></span>
    <button
      class="btn btn-primary"
      type="button"
      :disabled="submitting || !canSubmit"
      @click="submit"
    >
      {{ t("popup.send") }} <kbd class="sc">{{ submitKeyLabel }}</kbd>
    </button>
  </div>

  <div v-else class="footer" data-tauri-drag-region>
    <button class="btn" type="button" :disabled="submitting" @click="requestConfirmClose">
      {{ t("common.cancel") }} <kbd class="sc">{{ cancelShortcut }}</kbd>
    </button>
    <span class="spacer"></span>
    <button
      class="btn btn-primary"
      type="button"
      :disabled="!confirmCanSubmit"
      @click="submitConfirm"
    >
      {{ confirmRequest?.presentation.submitLabel ?? t("common.submit") }}
      <kbd class="sc">{{ submitKeyLabel }}</kbd>
    </button>
  </div>
</template>
