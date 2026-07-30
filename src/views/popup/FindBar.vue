<script setup lang="ts">
// Floating in-page find bar in the navbar action corner (spec docs/specs/popup-find.md).
import { nextTick } from "vue";
import { useI18n } from "vue-i18n";
import { usePopupContext } from "./context";

const { t } = useI18n();
const {
  findActive,
  findQuery,
  findCaseSensitive,
  findCountLabel,
  findNoMatch,
  findInputEl,
  closeFind,
  goFind,
  onFindQueryInput,
  toggleFindCase,
} = usePopupContext();

/** Focus after enter animation so the slide-in draws attention (no focus ring). */
function onFindEnter(el: Element): void {
  void nextTick(() => {
    const input =
      (el.querySelector(".popup-find-input") as HTMLInputElement | null) ??
      null;
    input?.focus({ preventScroll: true });
    if (findQuery) input?.select();
  });
}
</script>

<template>
  <Transition name="popup-find-slide" @after-enter="onFindEnter">
    <div
      v-if="findActive"
      class="popup-find-bar"
      role="search"
      :aria-label="t('popup.find.label')"
      @mousedown.stop
      @click.stop
    >
      <svg
        class="popup-find-icon"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        stroke-width="2"
        stroke-linecap="round"
        stroke-linejoin="round"
        aria-hidden="true"
      >
        <circle cx="11" cy="11" r="7" />
        <path d="m21 21-4.3-4.3" />
      </svg>
      <input
        :ref="(el) => (findInputEl = el as HTMLInputElement | null)"
        class="popup-find-input"
        :class="{ empty: findNoMatch }"
        type="search"
        enterkeyhint="search"
        autocomplete="off"
        autocorrect="off"
        spellcheck="false"
        :placeholder="t('popup.find.placeholder')"
        :aria-keyshortcuts="t('popup.find.ariaShortcut')"
        :value="findQuery"
        @input="onFindQueryInput(($event.target as HTMLInputElement).value)"
      />
      <span
        class="popup-find-count"
        :class="{ empty: findNoMatch }"
        aria-live="polite"
      >{{ findCountLabel }}</span>
      <button
        type="button"
        class="popup-find-btn"
        :title="t('popup.find.prev')"
        :aria-label="t('popup.find.prev')"
        @click="goFind(-1)"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="m18 15-6-6-6 6" />
        </svg>
      </button>
      <button
        type="button"
        class="popup-find-btn"
        :title="t('popup.find.next')"
        :aria-label="t('popup.find.next')"
        @click="goFind(1)"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>
      <button
        type="button"
        class="popup-find-btn popup-find-case"
        :class="{ active: findCaseSensitive }"
        :title="t('popup.find.caseSensitive')"
        :aria-label="t('popup.find.caseSensitive')"
        :aria-pressed="findCaseSensitive"
        @click="toggleFindCase"
      >
        Aa
      </button>
      <button
        type="button"
        class="popup-find-btn"
        :title="t('popup.find.close')"
        :aria-label="t('popup.find.close')"
        @click="closeFind"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M18 6 6 18" />
          <path d="m6 6 12 12" />
        </svg>
      </button>
    </div>
  </Transition>
</template>
