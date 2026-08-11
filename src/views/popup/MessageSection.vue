<script setup lang="ts">
// 共享 Message 区（描述 + AI→人附件 + 复制/源码工具条），顶部常驻、不随题切换。
import { useI18n } from "vue-i18n";
import MarkdownContent from "../../components/MarkdownContent.vue";
import { usePopupContext } from "./context";

const { t } = useI18n();
const ctx = usePopupContext();
const {
  request,
  showDescription,
  messageText,
  viewSource,
  copiedMessage,
  copyMessage,
  attachments,
  selectedFile,
  thumbs,
  setAttRef,
  selectFile,
  openFile,
  onAttachmentDragStart,
  onAttachmentContextMenu,
  formatBytes,
} = ctx;
</script>

<template>
  <!-- 共享 Message 区（描述 + 附件），仅在有内容时展示，顶部常驻 -->
  <template v-if="showDescription">
    <MarkdownContent
      v-if="messageText && request?.isMarkdown && !viewSource"
      :source="messageText"
      data-find-seg="message"
    />
    <pre
      v-else-if="messageText"
      class="plain-body"
      data-find-seg="message"
    >{{ messageText }}</pre>

    <div v-if="attachments.length" class="attachments">
      <div class="att-caption" data-find-skip>
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
          <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
        </svg>
        <span>{{ t("popup.attachments", { n: attachments.length }) }}</span>
      </div>
      <div class="att-list">
        <div
          v-for="(file, i) in attachments"
          :key="file.path"
          :ref="(el) => setAttRef(el as Element | null, i)"
          class="attachment"
          :class="{ selected: selectedFile === i }"
          tabindex="0"
          draggable="true"
          :title="file.path"
          @click="selectFile(i)"
          @dblclick="openFile(file)"
          @dragstart="onAttachmentDragStart(file, $event)"
          @contextmenu="onAttachmentContextMenu(file, i, $event)"
        >
          <span class="att-icon" :class="{ 'is-image': file.isImage && thumbs[file.path] }" data-find-skip>
            <img v-if="file.isImage && thumbs[file.path]" :src="thumbs[file.path]" alt="" />
            <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
              <path d="M14 3v4a1 1 0 0 0 1 1h4" />
              <path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2z" />
            </svg>
          </span>
          <span class="att-meta">
            <span class="att-name" :data-find-seg="`att-${i}`">{{ file.name }}</span>
            <span class="att-size" data-find-skip>{{ formatBytes(file.size) }}</span>
          </span>
        </div>
      </div>
    </div>
  </template>

  <!-- message 下方右对齐工具条：复制 Message + Markdown/源码切换（切换作用于整篇）。
       仅在有共享 Message 时显示——直接提问（无 message）不显示复制/源码按钮。 -->
  <div v-if="messageText.trim()" class="msg-tools">
    <button
      class="mt-btn"
      :class="{ done: copiedMessage }"
      type="button"
      :title="copiedMessage ? t('common.copied') : t('popup.view.copyMessage')"
      :aria-label="t('popup.view.copyMessage')"
      @click="copyMessage"
    >
      <svg class="mt-ico mt-copy" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
      <svg class="mt-ico mt-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
    </button>
    <button
      class="mt-btn"
      :class="{ active: viewSource }"
      type="button"
      :title="viewSource ? t('popup.view.viewRendered') : t('popup.view.viewSource')"
      :aria-label="viewSource ? t('popup.view.viewRendered') : t('popup.view.viewSource')"
      @click="viewSource = !viewSource"
    >
      <svg class="mt-ico" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><polyline points="18 16 22 12 18 8" /><polyline points="6 8 2 12 6 16" /><line x1="14.5" y1="4" x2="9.5" y2="20" /></svg>
    </button>
  </div>
</template>
