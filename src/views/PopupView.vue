<script setup lang="ts">
// 弹窗编排层：状态/逻辑在 ./popup/*（usePopupCore 组装、createPopupContext provide，
// 各区块子组件 inject）。此处仅负责根布局：导航栏 / 内容区（确认面板 或 Message+问题区）/
// 页脚 / 根级弹层。样式统一在 ./popup/popup.css（弹窗为独立窗口，天然隔离）。
import { useI18n } from "vue-i18n";
import { createPopupContext } from "./popup/context";
import PopupNavbar from "./popup/PopupNavbar.vue";
import ConfirmPane from "./popup/ConfirmPane.vue";
import MessageSection from "./popup/MessageSection.vue";
import QuestionCards from "./popup/QuestionCards.vue";
import SequentialPane from "./popup/SequentialPane.vue";
import TodoSection from "./popup/TodoSection.vue";
import ComposerDock from "./popup/ComposerDock.vue";
import PopupFooter from "./popup/PopupFooter.vue";
import PopupOverlays from "./popup/PopupOverlays.vue";
import "./popup/popup.css";

const { t } = useI18n();

const {
  request,
  confirmRequest,
  isConfirm,
  loadError,
  cmdHeld,
  flashing,
  verticalMode,
  contentRef,
  fileRef,
  onScroll,
  onContentWheel,
  refreshFind,
  onDrop,
  onBackgroundClick,
  onFileChange,
} = createPopupContext();
</script>

<template>
  <div v-if="!request && !confirmRequest" class="popup popup-status">
    <p v-if="loadError" class="status-error">
      {{ t("popup.loadError", { msg: loadError }) }}
    </p>
    <p v-else class="status-loading">{{ t("popup.loading") }}</p>
  </div>

  <div
    v-else
    class="popup"
    :class="{ 'cmd-held': cmdHeld }"
    @dragover.prevent
    @drop.prevent="onDrop"
    @click="onBackgroundClick"
  >
    <div v-if="flashing" class="flash-overlay" aria-hidden="true"></div>
    <PopupNavbar />
    <div
      :ref="(el) => (contentRef = el as HTMLElement | null)"
      class="content"
      @scroll="onScroll"
      @wheel.passive="onContentWheel"
      @markdown-content-updated="refreshFind"
    >
      <ConfirmPane v-if="isConfirm" />
      <template v-else>
        <MessageSection />
        <QuestionCards v-if="verticalMode" />
        <SequentialPane v-else />
        <!-- 待办下拉区：跟在最后一个问题后面（sequential 多题时仅最后一题面板显示） -->
        <TodoSection />
      </template>
    </div>

    <ComposerDock v-if="!isConfirm" />

    <input
      :ref="(el) => (fileRef = el as HTMLInputElement | null)"
      type="file"
      accept="image/*"
      multiple
      hidden
      @change="onFileChange"
    />

    <PopupFooter />
    <PopupOverlays />
  </div>
</template>
