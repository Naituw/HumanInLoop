<script setup lang="ts">
// 顶部导航栏：来源头部（agent/workspace 胶囊 + 相对时间）+ 右侧动作（自更新入口/置顶/待办/
// 历史/设置）。多根组件：navbar 之外还含自更新浮层背板与「更新待生效」横条（保持 .popup 的
// flex 子级顺序）。
import { useI18n } from "vue-i18n";
import { usePopupContext } from "./context";
import FindBar from "./FindBar.vue";

const { t } = useI18n();
const {
  scrolled,
  agentInline,
  headerPrefix,
  headerSuffix,
  agentLabel,
  agentFocusable,
  onFocusAgentTerminal,
  projectName,
  projectPath,
  onOpenWorkspace,
  popupTimeRel,
  popupTimeAbs,
  updateAvailable,
  updatePending,
  updateLatest,
  updateApplyMode,
  updatePopoverOpen,
  updating,
  updateStarted,
  updateError,
  updateNotesHtml,
  toggleUpdatePopover,
  applyUpdateFromPopup,
  onContentClick,
  pinned,
  togglePin,
  agentConsoleAvailable,
  openAgentConsoleWindow,
  openTodosWindow,
  openHistoryWindow,
  openSettingsWindow,
  findActive,
} = usePopupContext();
</script>

<template>
  <header class="navbar" :class="{ scrolled }" data-tauri-drag-region>
    <span class="brand" :class="{ inline: agentInline }">
      <span class="brand-dot"></span>
      <span class="brand-title">{{ headerPrefix }}</span>
      <component
        :is="agentFocusable ? 'button' : 'span'"
        v-if="agentLabel"
        class="brand-chip brand-agent"
        :class="{ clickable: agentFocusable }"
        :type="agentFocusable ? 'button' : undefined"
        :title="agentFocusable ? t('agents.focusTerminal') : undefined"
        @click="onFocusAgentTerminal"
      >
        <span class="chip-text">{{ agentLabel }}</span>
        <svg
          v-if="agentFocusable"
          class="chip-arrow"
          viewBox="0 0 10 10"
          aria-hidden="true"
        >
          <path
            d="M3 7 L7 3 M4 3 H7 V6"
            fill="none"
            stroke="currentColor"
            stroke-width="1.2"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </component>
      <button
        v-if="projectName"
        type="button"
        class="brand-chip brand-workspace clickable"
        :title="projectPath"
        @click="onOpenWorkspace"
      >
        <span class="chip-text">{{ projectName }}</span>
        <svg class="chip-arrow" viewBox="0 0 10 10" aria-hidden="true">
          <path
            d="M3 7 L7 3 M4 3 H7 V6"
            fill="none"
            stroke="currentColor"
            stroke-width="1.2"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
      </button>
      <span v-if="headerSuffix" class="brand-title brand-suffix">{{
        headerSuffix
      }}</span>
      <span
        v-if="popupTimeRel"
        class="brand-time"
        :title="popupTimeAbs"
        >· {{ popupTimeRel }}</span
      >
    </span>
    <span class="nav-actions" :class="{ 'find-open': findActive }">
      <!-- Action buttons fade out while find bar occupies this corner. -->
      <span class="nav-actions-btns" :aria-hidden="findActive ? 'true' : undefined">
        <div v-if="updateAvailable" class="update-wrap">
          <button
            class="nav-btn update-btn"
            type="button"
            :title="t('popup.nav.update')"
            :tabindex="findActive ? -1 : undefined"
            @click.stop="toggleUpdatePopover"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
              <path d="M12 3v12" />
              <path d="M7 10l5 5 5-5" />
              <path d="M5 21h14" />
            </svg>
            <span class="update-dot"></span>
          </button>
          <div v-if="updatePopoverOpen" class="update-popover" @click.stop>
            <p class="up-title">
              {{ t("popup.update.title", { version: updateLatest }) }}
            </p>
            <div
              v-if="updateNotesHtml"
              class="up-notes markdown-body"
              v-html="updateNotesHtml"
              @click="onContentClick"
            ></div>
            <p v-else class="up-notes muted">{{ t("popup.update.noNotes") }}</p>
            <p class="up-hint">
              {{
                updateApplyMode !== "automatic"
                  ? t("popup.update.manualHint")
                  : updateStarted
                  ? t("popup.update.startedHint")
                  : t("popup.update.applyHint")
              }}
            </p>
            <p v-if="updateError" class="up-error">{{ updateError }}</p>
            <div class="up-actions">
              <button
                class="btn btn-primary"
                type="button"
                :disabled="updating || updateStarted"
                @click="applyUpdateFromPopup"
              >
                {{
                  updateApplyMode !== "automatic"
                    ? t("popup.update.manualButton")
                    : updating
                    ? t("popup.update.updating")
                    : t("popup.update.button")
                }}
              </button>
            </div>
          </div>
        </div>
        <button
          class="nav-btn"
          :class="{ active: pinned }"
          type="button"
          :title="t('popup.nav.pin')"
          :tabindex="findActive ? -1 : undefined"
          @click="togglePin"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 17v5" />
            <path d="M9 10.8a2 2 0 0 1-1.1 1.8l-1.8.9A2 2 0 0 0 5 15.2V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.8a2 2 0 0 0-1.1-1.8l-1.8-.9A2 2 0 0 1 15 10.8V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z" />
          </svg>
        </button>
        <button
          v-if="agentConsoleAvailable"
          class="nav-btn"
          type="button"
          :title="t('popup.nav.agentConsole')"
          :aria-label="t('popup.nav.agentConsole')"
          :tabindex="findActive ? -1 : undefined"
          @click="openAgentConsoleWindow"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="3" />
            <path d="M7 13h2l2-4 3 7 2-4h2" />
          </svg>
        </button>
        <button
          class="nav-btn"
          type="button"
          :title="t('popup.nav.todos')"
          :tabindex="findActive ? -1 : undefined"
          @click="openTodosWindow"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <rect x="3" y="4" width="18" height="16" rx="3" />
            <path d="M7 10l2 2 3-3" />
            <path d="M14.5 10.5H18M7 16h11" />
          </svg>
        </button>
        <button
          class="nav-btn"
          type="button"
          :title="t('popup.nav.history')"
          :tabindex="findActive ? -1 : undefined"
          @click="openHistoryWindow"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M3 3v5h5" />
            <path d="M3.05 13a9 9 0 1 0 2.5-6.36L3 8" />
            <path d="M12 7v5l3 2" />
          </svg>
        </button>
        <button
          class="nav-btn"
          type="button"
          :title="t('popup.nav.settings')"
          :tabindex="findActive ? -1 : undefined"
          @click="openSettingsWindow"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.6 1.6 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.6 1.6 0 0 0-1.8-.3 1.6 1.6 0 0 0-1 1.5V21a2 2 0 0 1-4 0v-.1a1.6 1.6 0 0 0-1-1.5 1.6 1.6 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.6 1.6 0 0 0 .3-1.8 1.6 1.6 0 0 0-1.5-1H3a2 2 0 0 1 0-4h.1a1.6 1.6 0 0 0 1.5-1 1.6 1.6 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.6 1.6 0 0 0 1.8.3H9a1.6 1.6 0 0 0 1-1.5V3a2 2 0 0 1 4 0v.1a1.6 1.6 0 0 0 1 1.5 1.6 1.6 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.6 1.6 0 0 0-.3 1.8V9a1.6 1.6 0 0 0 1.5 1H21a2 2 0 0 1 0 4h-.1a1.6 1.6 0 0 0-1.5 1z" />
          </svg>
        </button>
      </span>
      <FindBar />
    </span>
  </header>
  <div
    v-if="updatePopoverOpen"
    class="update-backdrop"
    @click="updatePopoverOpen = false"
  ></div>
  <div v-if="updatePending" class="update-pending-banner">
    {{ t("popup.update.pendingBanner") }}
  </div>
</template>
