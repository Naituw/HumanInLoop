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
        :title="file.path"
      >
        <span class="rf-icon" aria-hidden="true">📄</span>
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
