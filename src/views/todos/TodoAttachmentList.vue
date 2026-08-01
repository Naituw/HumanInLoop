<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { startDrag } from "@crabnebula/tauri-plugin-drag";
import {
  fileIconDataUrl,
  openPath,
  previewAttachments,
  showAttachmentMenu,
  todoAttachmentThumbnail,
} from "../../lib/ipc";
import type { TodoAttachmentView } from "../../lib/types";

const props = withDefaults(
  defineProps<{
    project: string;
    todoId: string;
    attachments: TodoAttachmentView[];
    removable?: boolean;
    busy?: boolean;
    confirmRemoveId?: string | null;
    previewSelectedId?: string | null;
  }>(),
  {
    removable: false,
    busy: false,
    confirmRemoveId: null,
    previewSelectedId: null,
  }
);

const emit = defineEmits<{
  remove: [id: string];
  cancelRemove: [];
  previewStarted: [attachmentIds: string[]];
}>();
const { t } = useI18n();
const expanded = ref(false);
const thumbs = ref<Record<string, string>>({});
const dragIcons = ref<Record<string, string>>({});
const selected = ref<number | null>(null);
const root = ref<HTMLElement | null>(null);
const assetsVisible = ref(false);
let visibilityObserver: IntersectionObserver | null = null;

const summaryImages = computed(() =>
  props.attachments.filter((attachment) => attachment.isImage).slice(0, 3)
);
const hiddenPreviewCount = computed(() =>
  Math.max(0, props.attachments.length - summaryImages.value.length)
);

function formatBytes(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

async function loadThumbnail(attachment: TodoAttachmentView): Promise<void> {
  if (!attachment.isImage || thumbs.value[attachment.id]) return;
  try {
    const data = await todoAttachmentThumbnail(
      props.project,
      props.todoId,
      attachment.id
    );
    if (data) thumbs.value[attachment.id] = data;
  } catch {
    // Missing and stale thumbnails use the neutral document placeholder.
  }
}

async function loadSummaryAssets(): Promise<void> {
  await Promise.all(summaryImages.value.map(loadThumbnail));
}

async function loadExpandedAssets(): Promise<void> {
  await Promise.all(
    props.attachments.map(async (attachment) => {
      await loadThumbnail(attachment);
      if (!attachment.available || dragIcons.value[attachment.id]) return;
      try {
        dragIcons.value[attachment.id] = await fileIconDataUrl(attachment.path);
      } catch {
        // Native dragging can still proceed without a custom icon.
      }
    })
  );
}

function toggleExpanded(): void {
  emit("cancelRemove");
  expanded.value = !expanded.value;
  if (expanded.value && assetsVisible.value) void loadExpandedAssets();
}

function openAttachment(attachment: TodoAttachmentView): void {
  if (!attachment.available) return;
  void openPath(attachment.path);
}

function preview(index: number): void {
  const available = props.attachments.filter((attachment) => attachment.available);
  const attachment = props.attachments[index];
  const availableIndex = available.findIndex((item) => item.id === attachment.id);
  if (availableIndex < 0) return;
  emit(
    "previewStarted",
    available.map((item) => item.id)
  );
  void previewAttachments(
    available.map((item) => item.path),
    availableIndex
  );
}

function onKeydown(index: number, event: KeyboardEvent): void {
  if (event.key === "Enter") {
    event.preventDefault();
    openAttachment(props.attachments[index]);
  } else if (event.key === " ") {
    event.preventDefault();
    preview(index);
  } else if (event.key === "ArrowRight" || event.key === "ArrowDown") {
    event.preventDefault();
    selected.value = Math.min(props.attachments.length - 1, index + 1);
  } else if (event.key === "ArrowLeft" || event.key === "ArrowUp") {
    event.preventDefault();
    selected.value = Math.max(0, index - 1);
  }
}

function onContextMenu(
  attachment: TodoAttachmentView,
  index: number,
  event: MouseEvent
): void {
  event.preventDefault();
  emit("cancelRemove");
  selected.value = index;
  if (attachment.available) void showAttachmentMenu(attachment.path);
}

function onDragStart(attachment: TodoAttachmentView, event: DragEvent): void {
  event.preventDefault();
  if (!attachment.available) return;
  const icon = dragIcons.value[attachment.id] || thumbs.value[attachment.id] || "";
  void startDrag({ item: [attachment.path], icon }, () => {});
}

watch(
  () => [
    props.project,
    props.todoId,
    props.attachments.map((item) => item.id).join(","),
  ],
  () => {
    if (!props.attachments.length) expanded.value = false;
    if (!assetsVisible.value) return;
    void loadSummaryAssets();
    if (expanded.value) void loadExpandedAssets();
  }
);

watch(
  () => props.previewSelectedId,
  (attachmentId) => {
    if (!attachmentId) return;
    const index = props.attachments.findIndex((attachment) => attachment.id === attachmentId);
    if (index < 0) return;
    selected.value = index;
    void nextTick(() => {
      const row = root.value?.querySelectorAll<HTMLElement>(".todo-attachment")[index];
      row?.focus({ preventScroll: true });
      row?.scrollIntoView?.({ block: "nearest" });
    });
  }
);

onMounted(() => {
  if (!("IntersectionObserver" in window) || !root.value) {
    assetsVisible.value = true;
    void loadSummaryAssets();
    return;
  }
  visibilityObserver = new IntersectionObserver((entries) => {
    if (!entries.some((entry) => entry.isIntersecting)) return;
    assetsVisible.value = true;
    visibilityObserver?.disconnect();
    visibilityObserver = null;
    void loadSummaryAssets();
  });
  visibilityObserver.observe(root.value);
});

onBeforeUnmount(() => visibilityObserver?.disconnect());
</script>

<template>
  <div v-if="attachments.length" ref="root" class="todo-attachments">
    <button
      type="button"
      class="todo-attachments-summary"
      :class="{ open: expanded }"
      :aria-expanded="expanded"
      @click.stop="toggleExpanded"
    >
      <svg class="todo-attachments-caret" viewBox="0 0 12 12" aria-hidden="true">
        <path
          d="M4 2.5 8 6l-4 3.5"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
        />
      </svg>
      <svg
        class="todo-attachments-paperclip"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="1.7"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
      </svg>
      <span class="todo-attachments-label">
        {{ t("todosWin.attachmentsCount", { n: attachments.length }) }}
      </span>
      <span v-if="summaryImages.length" class="todo-attachments-previews" aria-hidden="true">
        <span
          v-for="attachment in summaryImages"
          :key="attachment.id"
          class="todo-attachments-preview"
        >
          <img v-if="thumbs[attachment.id]" :src="thumbs[attachment.id]" alt="" />
          <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <rect x="3.5" y="4" width="17" height="16" rx="3" stroke-width="1.5" />
            <circle cx="9" cy="9" r="1.5" stroke-width="1.5" />
            <path d="m5.5 17 4.2-4 3.1 2.7 2.5-2.2 3.2 3.5" stroke-width="1.5" />
          </svg>
        </span>
        <span v-if="hiddenPreviewCount" class="todo-attachments-more">
          +{{ hiddenPreviewCount }}
        </span>
      </span>
    </button>

    <div v-if="expanded" class="todo-attachments-list">
      <div
        v-for="(attachment, index) in attachments"
        :key="attachment.id"
        class="todo-attachment"
        :class="{
          selected: selected === index,
          missing: !attachment.available,
          confirming: confirmRemoveId === attachment.id,
        }"
        tabindex="0"
        draggable="true"
        :title="attachment.sourcePath"
        @click.stop="selected = index; emit('cancelRemove')"
        @dblclick.stop="openAttachment(attachment)"
        @keydown="onKeydown(index, $event)"
        @contextmenu="onContextMenu(attachment, index, $event)"
        @dragstart="onDragStart(attachment, $event)"
      >
        <img
          v-if="thumbs[attachment.id]"
          class="todo-attachment-thumb"
          :src="thumbs[attachment.id]"
          alt=""
        />
        <img
          v-else-if="dragIcons[attachment.id]"
          class="todo-attachment-thumb todo-attachment-file-icon"
          :src="dragIcons[attachment.id]"
          alt=""
        />
        <span v-else class="todo-attachment-icon" aria-hidden="true">
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M6 2.5h8l4 4V21.5H6z" />
            <path d="M14 2.5v4h4" />
          </svg>
        </span>
        <span class="todo-attachment-copy">
          <span class="todo-attachment-name">{{ attachment.name }}</span>
          <span class="todo-attachment-meta">
            {{ formatBytes(attachment.size) }}
            <template v-if="!attachment.available">
              · {{ t("todosWin.attachmentMissing") }}
            </template>
          </span>
        </span>
        <button
          v-if="removable && confirmRemoveId === attachment.id"
          type="button"
          class="todo-attachment-remove todo-attachment-remove-confirm"
          :disabled="busy"
          :title="t('todosWin.deleteConfirm')"
          @click.stop="emit('remove', attachment.id)"
        >
          {{ t("todosWin.deleteConfirm") }}
        </button>
        <button
          v-else-if="removable"
          type="button"
          class="todo-attachment-remove"
          :disabled="busy"
          :title="t('todosWin.removeAttachment')"
          :aria-label="t('todosWin.removeAttachment')"
          @click.stop="emit('remove', attachment.id)"
        >
          <svg viewBox="0 0 12 12" aria-hidden="true">
            <path
              d="M3 3 9 9M9 3 3 9"
              fill="none"
              stroke="currentColor"
              stroke-width="1.4"
              stroke-linecap="round"
            />
          </svg>
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.todo-attachments {
  min-width: 0;
  margin-top: 3px;
}
.todo-attachments-summary {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  max-width: 100%;
  min-height: 27px;
  padding: 2px 7px 2px 5px;
  border: var(--hairline) solid color-mix(in srgb, var(--border) 82%, transparent);
  border-radius: 7px;
  background: color-mix(in srgb, var(--bg-secondary) 66%, transparent);
  color: var(--text-secondary);
  font: inherit;
  font-size: 11px;
  cursor: pointer;
  box-shadow: 0 1px 0 color-mix(in srgb, #fff 7%, transparent);
}
.todo-attachments-summary:hover,
.todo-attachments-summary:focus-visible,
.todo-attachments-summary.open {
  border-color: color-mix(in srgb, #0a84ff 40%, var(--border));
  background: color-mix(in srgb, #0a84ff 8%, var(--bg-secondary));
  color: var(--text-primary);
  outline: none;
}
.todo-attachments-caret {
  width: 10px;
  height: 10px;
  transition: transform 0.15s ease;
}
.todo-attachments-summary.open .todo-attachments-caret {
  transform: rotate(90deg);
}
.todo-attachments-paperclip {
  width: 13px;
  height: 13px;
}
.todo-attachments-label {
  white-space: nowrap;
}
.todo-attachments-previews {
  display: inline-flex;
  align-items: center;
  gap: 3px;
  margin-left: 2px;
  padding-left: 2px;
}
.todo-attachments-preview {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 22px;
  height: 22px;
  margin-left: 0;
  overflow: hidden;
  border: 1px solid var(--bg-elevated);
  border-radius: 5px;
  background: var(--control-bg);
  color: var(--text-secondary);
  box-shadow: 0 1px 3px rgba(0, 0, 0, 0.16);
}
.todo-attachments-preview img,
.todo-attachments-preview svg {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.todo-attachments-preview svg {
  width: 16px;
  height: 16px;
}
.todo-attachments-more {
  margin-left: 1px;
  color: var(--text-secondary);
  font-size: 10px;
  font-weight: 600;
}
.todo-attachments-list {
  display: flex;
  flex-direction: column;
  gap: 3px;
  max-width: 440px;
  margin-top: 5px;
  padding: 4px;
  border: var(--hairline) solid var(--border);
  border-radius: 8px;
  background: color-mix(in srgb, var(--bg-secondary) 64%, transparent);
}
.todo-attachment {
  display: grid;
  grid-template-columns: 30px minmax(0, 1fr) 24px;
  align-items: center;
  gap: 7px;
  min-height: 36px;
  padding: 3px 4px 3px 5px;
  border-radius: 6px;
  cursor: default;
  outline: none;
}
.todo-attachment.confirming {
  grid-template-columns: 30px minmax(0, 1fr) auto;
}
.todo-attachment:hover,
.todo-attachment.selected,
.todo-attachment:focus-visible {
  background: var(--control-hover-bg);
}
.todo-attachment:focus-visible {
  box-shadow: inset 0 0 0 1px color-mix(in srgb, #0a84ff 55%, transparent);
}
.todo-attachment.missing {
  opacity: 0.55;
}
.todo-attachment-thumb {
  width: 30px;
  height: 30px;
  border-radius: 5px;
  object-fit: cover;
}
.todo-attachment-file-icon {
  object-fit: contain;
}
.todo-attachment-icon {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 30px;
  height: 30px;
  color: var(--text-secondary);
}
.todo-attachment-icon svg {
  width: 20px;
  height: 20px;
}
.todo-attachment-copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 1px;
}
.todo-attachment-name {
  min-width: 0;
  overflow: hidden;
  color: var(--text-primary);
  font-size: 12px;
  line-height: 1.25;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.todo-attachment-meta {
  color: var(--text-secondary);
  font-size: 10px;
  line-height: 1.2;
  white-space: nowrap;
}
.todo-attachment-remove {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  border: 0;
  border-radius: 6px;
  color: var(--text-secondary);
  background: transparent;
  cursor: pointer;
  opacity: 0.55;
}
.todo-attachment:hover .todo-attachment-remove,
.todo-attachment-remove:focus-visible {
  opacity: 0.82;
}
.todo-attachment-remove:hover:not(:disabled) {
  opacity: 1;
  color: #ff453a;
  background: color-mix(in srgb, #ff453a 13%, transparent);
}
.todo-attachment-remove:disabled {
  cursor: default;
  opacity: 0.35;
}
.todo-attachment-remove svg {
  width: 11px;
  height: 11px;
}
.todo-attachment-remove-confirm {
  width: auto;
  padding: 0 7px;
  color: #ff453a;
  background: color-mix(in srgb, #ff453a 12%, transparent);
  font-size: 10px;
  white-space: nowrap;
  opacity: 1;
}
.todo-attachment-remove-confirm:hover:not(:disabled) {
  background: color-mix(in srgb, #ff453a 20%, transparent);
}
</style>
