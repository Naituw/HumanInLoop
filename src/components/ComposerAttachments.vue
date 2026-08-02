<script setup lang="ts">
import { useI18n } from "vue-i18n";

defineProps<{
  images: Array<{
    key?: string | number;
    data: string;
    filename?: string | null;
  }>;
  files: Array<{
    key?: string | number;
    path: string;
    name: string;
    available?: boolean;
  }>;
}>();

const emit = defineEmits<{
  removeImage: [index: number];
  removeFile: [index: number];
  imageContainerRef: [element: HTMLElement | null];
}>();
const { t } = useI18n();

function setImageContainerRef(element: unknown): void {
  emit("imageContainerRef", element instanceof HTMLElement ? element : null);
}
</script>

<template>
  <div v-if="images.length || files.length" class="composer-attachments">
    <div v-if="images.length" :ref="setImageContainerRef" class="thumbs">
      <div
        v-for="(image, index) in images"
        :key="image.key ?? index"
        class="thumb"
        :title="image.filename ?? undefined"
      >
        <img :src="image.data" alt="" />
        <button
          class="remove"
          type="button"
          :title="t('todosWin.removeAttachment')"
          :aria-label="t('todosWin.removeAttachment')"
          @click="emit('removeImage', index)"
        >
          ×
        </button>
      </div>
    </div>

    <div v-if="files.length" class="reply-files">
      <div
        v-for="(file, index) in files"
        :key="file.key ?? file.path"
        class="reply-file"
        :class="{ unavailable: file.available === false }"
        :title="file.available === false ? `${file.path} · ${t('interject.attachmentUnavailable')}` : file.path"
      >
        <span v-if="file.available === false" class="rf-warning" aria-hidden="true">
          <svg viewBox="0 0 16 16" fill="none" stroke="currentColor">
            <path d="M8 2.2 14 13H2L8 2.2Z" />
            <path d="M8 5.7v3.5M8 11.5h.01" />
          </svg>
        </span>
        <span v-else class="rf-icon" aria-hidden="true">📄</span>
        <span class="rf-name">{{ file.name }}</span>
        <button
          class="rf-remove"
          type="button"
          :title="t('todosWin.removeAttachment')"
          :aria-label="t('todosWin.removeAttachment')"
          @click="emit('removeFile', index)"
        >
          ×
        </button>
      </div>
    </div>
  </div>
</template>
