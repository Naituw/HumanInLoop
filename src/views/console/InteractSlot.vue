<script setup lang="ts">
// 交互区插槽（spec gui-agent-console R1/C3/C9）：按优先级切换——插话输入（工作中且非 Grok）/
// Grok 提示 / 空闲提示（+ 新建任务快捷入口）/ 已结束提示。未来「提问卡」为最高优先级内容。
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { AgentRecord, ImageAttachment } from "../../lib/types";
import ComposerAttachments from "../../components/ComposerAttachments.vue";
import { useInterjectAttachments } from "../interject/useInterjectAttachments";
import { primaryModifierPressed, primaryShortcutLabel } from "../../lib/platform";

const { t } = useI18n();
const submitShortcut = primaryShortcutLabel("enter");

const props = defineProps<{
  record: AgentRecord;
  /** 裸 Enter 发送（popupSubmitKey=enter）；否则 ⌘/Ctrl+Enter。 */
  submitBareEnter: boolean;
  /** 待送达全文（interject_peek；空=无待送达）。 */
  pendingText: string;
  pendingCount: number;
  pendingAttachmentCount: number;
  newTaskSupported: boolean;
}>();

const emit = defineEmits<{
  send: [text: string, filePaths: string[], pastedImages: ImageAttachment[]];
  revoke: [];
  newTask: [projectPath: string];
}>();

const draft = ref("");
const attachments = useInterjectAttachments();
let unlistenDrop: UnlistenFn | null = null;

// 切换会话时清空草稿（简单直接；控制台一次只对一个会话说话）。
watch(
  () => props.record.sessionId,
  () => {
    draft.value = "";
    attachments.reset();
  }
);

function canSend(): boolean {
  return props.record.state === "working" && props.record.kind !== "grok";
}

function send(): void {
  const text = draft.value.trim();
  if ((!text && !attachments.hasAttachments.value) || !canSend()) return;
  emit("send", text, attachments.filePaths.value, attachments.pastedImages.value);
  draft.value = "";
  attachments.reset();
}

function onKeydown(e: KeyboardEvent): void {
  if (e.key !== "Enter") return;
  const withModifier = primaryModifierPressed(e);
  if (props.submitBareEnter) {
    // enter 模式：裸 Enter 发送，任意修饰键换行。
    if (!withModifier && !e.shiftKey && !e.altKey) {
      e.preventDefault();
      send();
    }
  } else if (withModifier) {
    e.preventDefault();
    send();
  }
}

function isComposerDrop(x: number, y: number): boolean {
  const dpr = window.devicePixelRatio || 1;
  for (const [cx, cy] of [
    [x, y],
    [x / dpr, y / dpr],
  ]) {
    if (document.elementFromPoint(cx, cy)?.closest("[data-interject-drop]")) return true;
  }
  return false;
}

onMounted(async () => {
  unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
    if (
      event.payload.type === "drop" &&
      canSend() &&
      isComposerDrop(event.payload.position.x, event.payload.position.y)
    ) {
      attachments.appendPaths(event.payload.paths);
    }
  });
});

onBeforeUnmount(() => unlistenDrop?.());
</script>

<template>
  <div class="interact">
    <template v-if="canSend()">
      <div v-if="pendingCount > 0" class="ij-pending">
        <span class="pending-badge">{{ t("console.pendingCount", { n: pendingCount }) }}</span>
        <span v-if="pendingAttachmentCount" class="pending-attachments">
          {{ t("console.pendingAttachments", { n: pendingAttachmentCount }) }}
        </span>
        <span class="pending-text" :title="pendingText">{{ pendingText }}</span>
        <button class="pending-revoke" @click="emit('revoke')">{{ t("console.revoke") }}</button>
      </div>
      <div class="composer-stack" data-interject-drop @paste="attachments.onPaste">
        <div class="answer-composer message-composer">
          <div class="input-wrap">
            <textarea
              v-model="draft"
              class="textarea"
              rows="2"
              :placeholder="t('console.composerPlaceholder')"
              @keydown="onKeydown"
            />
            <button
              class="img-btn"
              type="button"
              :title="t('interject.addAttachment')"
              :aria-label="t('interject.addAttachment')"
              :disabled="attachments.busy.value"
              @mousedown.prevent
              @click="attachments.chooseFiles"
            >
              <svg
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
                stroke-linejoin="round"
                aria-hidden="true"
              >
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <circle cx="8.5" cy="8.5" r="1.6" />
                <path d="M21 15l-5-5L5 21" />
              </svg>
            </button>
          </div>
          <ComposerAttachments
            :images="attachments.composerImages.value"
            :files="attachments.composerFiles.value"
            @remove-image="attachments.removeComposerImage"
            @remove-file="attachments.removeComposerFile"
          />
          <div class="composer-actions">
            <button
              class="btn primary send"
              :disabled="(!draft.trim() && !attachments.hasAttachments.value) || attachments.busy.value"
              @click="send"
            >
              {{ t("console.send") }} <span class="kbd">{{ submitBareEnter ? "↵" : submitShortcut }}</span>
            </button>
          </div>
        </div>
        <p v-if="attachments.error.value" class="attachment-error" role="alert">
          {{ attachments.error.value }}
        </p>
      </div>
    </template>
    <p v-else-if="record.state === 'working' && record.kind === 'grok'" class="interact-hint">
      {{ t("console.grokHint") }}
    </p>
    <p v-else-if="record.state === 'idle'" class="interact-hint">
      {{ t("console.idleHint") }}
      <template v-if="newTaskSupported && record.cwd">
        ·
        <a href="#" @click.prevent="emit('newTask', record.cwd!)">{{ t("console.newTask") }}</a>
      </template>
    </p>
    <p v-else class="interact-hint">{{ t("console.endedHint") }}</p>
  </div>
</template>

<style scoped>
.interact {
  flex: 0 0 auto;
  padding: 10px 16px 12px;
  border-top: var(--hairline) solid var(--border);
}
.ij-pending {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 9px;
  margin-bottom: 8px;
  border-radius: 7px;
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  font-size: 12px;
}
.pending-badge {
  flex: 0 0 auto;
  padding: 1px 8px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 600;
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--accent);
  white-space: nowrap;
}
.pending-text {
  flex: 1 1 auto;
  min-width: 0;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pending-attachments {
  flex: 0 0 auto;
  color: var(--text-tertiary);
  font-size: 11px;
  white-space: nowrap;
}
.pending-revoke {
  flex: 0 0 auto;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 11px;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 5px;
  cursor: pointer;
}
.pending-revoke:hover {
  background: color-mix(in srgb, var(--text-primary) 10%, transparent);
  color: var(--text-primary);
}
.composer-stack {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.composer-stack :deep(.reply-files) {
  margin-top: 0;
}
.message-composer {
  gap: 12px;
}
.message-composer .input-wrap {
  flex: 1 1 auto;
}
.message-composer .textarea {
  min-height: 64px;
  max-height: 120px;
}
.composer-actions {
  display: flex;
  justify-content: flex-end;
}
.attachment-error {
  margin: 0;
  color: var(--danger, #d70015);
  font-size: 11px;
}
.btn {
  appearance: none;
  border: var(--hairline) solid var(--control-border);
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  color: var(--text-primary);
  font-size: 12.5px;
  font-weight: 600;
  padding: 5px 14px;
  border-radius: 7px;
  cursor: pointer;
}
.btn:disabled {
  opacity: 0.45;
  cursor: default;
}
.btn.primary {
  border-color: transparent;
  background: var(--accent);
  color: #fff;
}
.btn.primary:hover:not(:disabled) {
  background: #0071e3;
}
.btn.send {
  flex: 0 0 auto;
}
.kbd {
  font-size: 10.5px;
  opacity: 0.75;
  margin-left: 2px;
}
.interact-hint {
  margin: 2px 0;
  font-size: 12px;
  color: var(--text-tertiary);
}
.interact-hint a {
  color: var(--accent);
  text-decoration: none;
}
</style>
