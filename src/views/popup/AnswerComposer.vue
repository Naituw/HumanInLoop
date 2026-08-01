<script setup lang="ts">
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import ComposerAttachments from "../../components/ComposerAttachments.vue";
import { usePopupContext } from "./context";

const props = defineProps<{
  qIndex: number;
  collapsible?: boolean;
}>();

const { t } = useI18n();
const {
  inputByQ,
  imagesByQ,
  replyFilesByQ,
  expandedQ,
  dockedComposerQ,
  setInputRef,
  setComposerAnchorRef,
  setComposerHomeRef,
  composerAnchorStyle,
  setThumbsRef,
  activateComposer,
  onTextareaFocus,
  onTextareaBlur,
  onComposerInput,
  onComposerMouseDown,
  onComposerCompositionStart,
  onComposerCompositionEnd,
  onUserCaretMaybeMoved,
  speechSupported,
  listening,
  speechReady,
  speechError,
  speechStatus,
  speechTargetQ,
  speechHotkeyLabel,
  speechErrorText,
  speechStatusText,
  toggleSpeech,
  focusQuestionAction,
  pickFiles,
  removeImage,
  removeReplyFile,
} = usePopupContext();

const isDocked = computed(() => dockedComposerQ.value === props.qIndex);
const isExpanded = computed(
  () => isDocked.value || !props.collapsible || expandedQ(props.qIndex)
);
const ownsSpeech = computed(
  () => listening.value && speechTargetQ.value === props.qIndex
);
const hasText = computed(
  () => (inputByQ.value[props.qIndex] ?? "").trim().length > 0
);

function handleSpeech() {
  focusQuestionAction(props.qIndex);
  toggleSpeech();
}

function handlePickFiles() {
  focusQuestionAction(props.qIndex);
  pickFiles(props.qIndex);
}
</script>

<template>
  <div
    :ref="(el) => setComposerAnchorRef(el as HTMLElement | null, qIndex)"
    class="composer-anchor"
    :class="{ 'is-docked': isDocked }"
    :style="composerAnchorStyle(qIndex)"
  >
    <Teleport defer to="#popup-composer-dock-target" :disabled="!isDocked">
      <div
        class="answer-composer"
        :class="{ 'is-docked': isDocked, 'is-collapsible': collapsible }"
      >
        <div
          :ref="(el) => setComposerHomeRef(el as HTMLElement | null, qIndex)"
          class="input-wrap"
          :class="{ 'has-text': hasText }"
        >
          <textarea
            :ref="(el) => setInputRef(el as HTMLTextAreaElement | null, qIndex)"
            v-model="inputByQ[qIndex]"
            class="textarea"
            :class="{ collapsed: !isExpanded }"
            :rows="collapsible ? 1 : undefined"
            :placeholder="t('popup.inputPlaceholder')"
            @input="onComposerInput(qIndex, $event)"
            @focus="onTextareaFocus(qIndex)"
            @blur="onTextareaBlur(qIndex)"
            @compositionstart="onComposerCompositionStart(qIndex)"
            @compositionend="onComposerCompositionEnd(qIndex)"
            @keyup="onUserCaretMaybeMoved"
            @keydown="activateComposer(qIndex)"
            @mousedown="onComposerMouseDown(qIndex)"
            @click="activateComposer(qIndex)"
          ></textarea>
          <div v-if="isExpanded" class="composer-actions">
            <button
              v-if="speechSupported"
              class="mic-btn"
              :class="{
                loading: ownsSpeech && !speechReady,
                recording: ownsSpeech && speechReady,
              }"
              type="button"
              :title="
                speechReady && listening
                  ? t('popup.speech.stop') +
                    (speechHotkeyLabel ? ' ' + speechHotkeyLabel : '')
                  : listening
                    ? t('popup.speech.preparing')
                    : t('popup.speech.start') +
                      (speechHotkeyLabel ? ' ' + speechHotkeyLabel : '')
              "
              :aria-label="listening ? t('popup.speech.stop') : t('popup.speech.start')"
              @mousedown.prevent
              @click="handleSpeech"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
                <rect x="9" y="2" width="6" height="12" rx="3" />
                <path d="M5 11a7 7 0 0 0 14 0" />
                <path d="M12 18v3" />
              </svg>
            </button>
            <button
              class="img-btn"
              type="button"
              :title="t('popup.addImage')"
              :aria-label="t('popup.addImage')"
              @mousedown.prevent
              @click="handlePickFiles"
            >
              <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
                <rect x="3" y="3" width="18" height="18" rx="2" />
                <circle cx="8.5" cy="8.5" r="1.6" />
                <path d="M21 15l-5-5L5 21" />
              </svg>
            </button>
          </div>
        </div>

        <p v-if="speechTargetQ === qIndex && speechError" class="speech-error">
          {{ speechErrorText(speechError) }}
        </p>
        <p
          v-else-if="ownsSpeech && speechStatus"
          class="speech-status"
        >
          {{ speechStatusText(speechStatus) }}
        </p>

        <ComposerAttachments
          :images="imagesByQ[qIndex] ?? []"
          :files="replyFilesByQ[qIndex] ?? []"
          @remove-image="removeImage(qIndex, $event)"
          @remove-file="removeReplyFile(qIndex, $event)"
          @image-container-ref="setThumbsRef($event, qIndex)"
        />
      </div>
    </Teleport>
  </div>
</template>
