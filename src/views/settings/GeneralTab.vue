<script setup lang="ts">
// 「通用」tab：外观 / 弹窗行为 / 多问题纵排 / 菜单栏 / 历史 / 语音 / 关于与自更新 / 实验开关。
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { openTestPopup } from "../../lib/ipc";
import { formatShortcut } from "../../lib/shortcut";
import { primaryModifierLabel, primaryShortcutLabel } from "../../lib/platform";
import type { UiLanguage } from "../../lib/types";
import { useSettingsContext } from "./context";

const { t } = useI18n();
const ctx = useSettingsContext();
const {
  isMac,
  persist,
  changeTheme,
  changeLanguage,
  changeSubmitKey,
  glassSupported,
  effectiveWindowEffect,
  changeWindowEffect,
  changeAnimation,
  soundSupport,
  changePopupSound,
  previewSound,
  changeMenuBarIcon,
  changeHistoryLimit,
  changeTodoHistoryLimit,
  overLimit,
  cleanHistoryNow,
  SPEECH_LANGUAGES,
  changeSpeechLanguage,
  recordingShortcut,
  shortcutPreview,
  shortcutError,
  startRecordShortcut,
  clearShortcut,
  toggleExperimental,
  appVersion,
  updateInfo,
  updateChecking,
  updateApplying,
  updatePreparing,
  updateDone,
  manualCommandCopied,
  updateError,
  updateProgress,
  notesHtml,
  currentNotesOpen,
  currentNotesHtml,
  currentNotesLoading,
  currentNotesError,
  toggleCurrentNotes,
  checkUpdate,
  applyUpdate,
  copyManualCommand,
  prepareManualUpdate,
  openReleases,
  onNotesClick,
  restartSettingsNow,
} = ctx;
// 父组件仅在 config 加载后渲染本 tab，这里可安全断言非空。
const config = computed(() => ctx.config.value!);
</script>

<template>
  <div class="card">
    <p class="card-title">{{ t("settings.appearance.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.appearance.theme") }}</span>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          :class="{ active: config.general.theme === 'system' }"
          @click="changeTheme('system')"
        >
          {{ t("settings.appearance.themeSystem") }}
        </button>
        <button
          :class="{ active: config.general.theme === 'light' }"
          @click="changeTheme('light')"
        >
          {{ t("settings.appearance.themeLight") }}
        </button>
        <button
          :class="{ active: config.general.theme === 'dark' }"
          @click="changeTheme('dark')"
        >
          {{ t("settings.appearance.themeDark") }}
        </button>
      </div>
    </div>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.appearance.language") }}</span>
      <span class="spacer"></span>
      <select
        class="select"
        :value="config.general.language"
        @change="changeLanguage(($event.target as HTMLSelectElement).value as UiLanguage)"
      >
        <option value="auto">
          {{ t("settings.appearance.languageSystem") }}
        </option>
        <option value="en">English</option>
        <option value="zh">简体中文</option>
      </select>
    </div>
  </div>

  <div class="card">
    <p class="card-title">{{ t("settings.popupBehavior.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.popupBehavior.alwaysOnTop") }}</span>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.general.alwaysOnTop"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.popupBehavior.submitKey") }}</span>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          type="button"
          :class="{ active: (config.general.popupSubmitKey ?? 'cmdEnter') === 'cmdEnter' }"
          @click="changeSubmitKey('cmdEnter')"
        >
          {{ t("settings.popupBehavior.submitKeyCmdEnter", { shortcut: primaryShortcutLabel("enter") }) }}
        </button>
        <button
          type="button"
          :class="{ active: config.general.popupSubmitKey === 'enter' }"
          @click="changeSubmitKey('enter')"
        >
          {{ t("settings.popupBehavior.submitKeyEnter") }}
        </button>
      </div>
    </div>
    <p class="card-desc">
      {{
        (config.general.popupSubmitKey ?? "cmdEnter") === "enter"
          ? t("settings.popupBehavior.submitKeyEnterHint")
          : t("settings.popupBehavior.submitKeyCmdEnterHint", { shortcut: primaryShortcutLabel("enter") })
      }}
    </p>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.popupBehavior.prewarm") }}</span>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.general.popupPrewarm"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p class="card-desc">{{ t("settings.popupBehavior.prewarmHint") }}</p>
    <template v-if="isMac">
      <hr class="divider" />
      <div class="row">
        <span class="label">{{
          t("settings.popupBehavior.windowEffect")
        }}</span>
        <span class="spacer"></span>
        <div class="segmented">
          <button
            :class="{ active: effectiveWindowEffect === 'solid' }"
            @click="changeWindowEffect('solid')"
          >
            {{ t("settings.popupBehavior.effectSolid") }}
          </button>
          <button
            :class="{ active: effectiveWindowEffect === 'blur' }"
            @click="changeWindowEffect('blur')"
          >
            {{ t("settings.popupBehavior.effectBlur") }}
          </button>
          <button
            v-if="glassSupported"
            :class="{ active: effectiveWindowEffect === 'glass' }"
            @click="changeWindowEffect('glass')"
          >
            {{ t("settings.popupBehavior.effectGlass") }}
          </button>
        </div>
      </div>
    </template>
    <template v-if="isMac">
      <hr class="divider" />
      <div class="row">
        <span class="label">{{
          t("settings.popupBehavior.appearAnimation")
        }}</span>
        <span class="spacer"></span>
        <div class="segmented">
          <button
            :class="{ active: config.general.appearAnimation === 'none' }"
            @click="changeAnimation('none')"
          >
            None
          </button>
          <button
            :class="{
              active: config.general.appearAnimation === 'document',
            }"
            @click="changeAnimation('document')"
          >
            Document
          </button>
          <button
            :class="{ active: config.general.appearAnimation === 'alert' }"
            @click="changeAnimation('alert')"
          >
            Alert
          </button>
        </div>
      </div>
    </template>
    <template v-if="soundSupport.kind !== 'none'">
      <hr class="divider" />
      <div class="row">
        <span class="label">{{ t("settings.popupBehavior.sound") }}</span>
        <span class="spacer"></span>
        <select
          class="select"
          :value="config.general.popupSound"
          @change="changePopupSound(($event.target as HTMLSelectElement).value)"
        >
          <option value="">{{ t("settings.popupBehavior.soundOff") }}</option>
          <template v-if="soundSupport.kind === 'named'">
            <option v-for="n in soundSupport.names" :key="n" :value="n">{{ n }}</option>
          </template>
          <option v-else value="default">{{ t("settings.popupBehavior.soundOn") }}</option>
        </select>
        <button
          class="btn"
          type="button"
          style="margin-left: 6px"
          :disabled="!config.general.popupSound"
          @click="previewSound"
        >
          {{ t("settings.popupBehavior.soundPreview") }}
        </button>
      </div>
    </template>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.popupBehavior.testPopup") }}</span>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="openTestPopup">
        {{ t("common.test") }}
      </button>
    </div>
  </div>

  <!-- 多问题纵向同时显示（从实验区迁来；跨平台，含 Windows） -->
  <div class="card">
    <div class="row">
      <p class="card-title">
        {{ t("settings.experimental.verticalQuestionsTitle") }}
      </p>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.experimental.verticalQuestions"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p class="card-desc">
      {{ t("settings.experimental.verticalQuestionsDesc") }}
    </p>
  </div>

  <!-- Menu bar / system tray icon. -->
  <div class="card">
    <p class="card-title">{{ t("settings.menuBar.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.menuBar.icon") }}</span>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          :class="{ active: config.general.menuBarIcon === 'off' }"
          @click="changeMenuBarIcon('off')"
        >
          {{ t("settings.menuBar.off") }}
        </button>
        <button
          :class="{ active: config.general.menuBarIcon === 'active' }"
          @click="changeMenuBarIcon('active')"
        >
          {{ t("settings.menuBar.active") }}
        </button>
        <button
          :class="{ active: config.general.menuBarIcon === 'always' }"
          @click="changeMenuBarIcon('always')"
        >
          {{ t("settings.menuBar.always") }}
        </button>
      </div>
    </div>
    <p class="card-desc">{{ t("settings.menuBar.hint") }}</p>
  </div>

  <!-- 回复历史 -->
  <div class="card">
    <p class="card-title">{{ t("settings.history.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.history.limit") }}</span>
      <span class="spacer"></span>
      <input
        class="input num"
        type="number"
        min="0"
        step="1"
        :value="config.general.historyLimit"
        @change="changeHistoryLimit(Number(($event.target as HTMLInputElement).value))"
      />
    </div>
    <p class="card-desc">{{ t("settings.history.limitHint") }}</p>
    <template v-if="overLimit">
      <hr class="divider" />
      <div class="row">
        <span class="result err">{{ t("settings.history.overLimit") }}</span>
        <span class="spacer"></span>
        <button class="btn" type="button" @click="cleanHistoryNow">
          {{ t("settings.history.cleanNow") }}
        </button>
      </div>
    </template>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.history.todoLimit") }}</span>
      <span class="spacer"></span>
      <input
        class="input num"
        type="number"
        min="0"
        step="1"
        :value="config.general.todoHistoryLimit"
        @change="changeTodoHistoryLimit(Number(($event.target as HTMLInputElement).value))"
      />
    </div>
    <p class="card-desc">{{ t("settings.history.todoLimitHint") }}</p>
  </div>

  <!-- 语音输入（仅 macOS） -->
  <div v-if="isMac" class="card">
    <p class="card-title">{{ t("settings.speech.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.speech.language") }}</span>
      <span class="spacer"></span>
      <select
        class="select"
        :value="config.general.speechLanguage"
        @change="changeSpeechLanguage(($event.target as HTMLSelectElement).value)"
      >
        <option
          v-for="lang in SPEECH_LANGUAGES"
          :key="lang.value"
          :value="lang.value"
        >
          {{
            lang.value === "auto"
              ? t("settings.speech.languageSystem")
              : lang.label
          }}
        </option>
      </select>
    </div>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.speech.shortcut") }}</span>
      <span class="spacer"></span>
      <button
        class="btn shortcut-rec"
        :class="{ recording: recordingShortcut }"
        type="button"
        @click="startRecordShortcut"
      >
        {{
          recordingShortcut
            ? shortcutPreview || t("settings.speech.recording")
            : config.general.speechShortcut
            ? formatShortcut(config.general.speechShortcut)
            : t("shortcut.none")
        }}
      </button>
      <button
        class="btn"
        type="button"
        style="margin-left: 6px"
        :disabled="!config.general.speechShortcut && !recordingShortcut"
        @click="clearShortcut"
      >
        {{ t("settings.speech.clear") }}
      </button>
    </div>
    <p v-if="shortcutError" class="result err">
      {{ t("shortcut.conflict." + shortcutError.key, shortcutError.params || {}) }}
    </p>
    <p
      v-else-if="recordingShortcut"
      class="card-desc"
      style="margin-top: 6px"
    >
      {{ t("settings.speech.recordHint", { modifier: primaryModifierLabel() }) }}
    </p>
  </div>

  <!-- 关于 / 版本自更新 -->
  <div class="card">
    <p class="card-title">{{ t("settings.about.title") }}</p>
    <div class="row">
      <span class="label">{{ t("settings.about.currentVersion") }}</span>
      <span class="spacer"></span>
      <span class="value">{{ appVersion || "—" }}</span>
    </div>
    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.about.latestVersion") }}</span>
      <span class="spacer"></span>
      <span class="value" v-if="updateInfo && !updateChecking">
        {{ updateInfo.latestVersion }}
        <template v-if="!updateInfo.available">
          · {{ t("settings.about.upToDate") }}</template
        >
      </span>
      <span class="value" v-else-if="updateChecking">{{
        t("settings.about.checking")
      }}</span>
      <span class="value" v-else>—</span>
      <button
        class="btn"
        type="button"
        style="margin-left: 8px"
        :disabled="updateChecking"
        @click="checkUpdate(true)"
      >
        {{ t("settings.about.check") }}
      </button>
    </div>

    <hr class="divider" />
    <div class="row">
      <span class="label">{{ t("settings.about.currentNotesTitle") }}</span>
      <span class="spacer"></span>
      <a class="link" href="#" @click.prevent="toggleCurrentNotes">{{
        currentNotesOpen
          ? t("settings.about.hideCurrentNotes")
          : t("settings.about.viewCurrentNotes")
      }}</a>
    </div>
    <template v-if="currentNotesOpen">
      <p v-if="currentNotesLoading" class="card-desc">
        {{ t("settings.about.notesLoading") }}
      </p>
      <p v-else-if="currentNotesError" class="result err">
        {{ currentNotesError }}
      </p>
      <div
        v-else-if="currentNotesHtml"
        class="release-notes markdown"
        v-html="currentNotesHtml"
        @click="onNotesClick"
      ></div>
      <p v-else class="card-desc">{{ t("settings.about.noNotes") }}</p>
    </template>

    <template v-if="updateInfo && updateInfo.available">
      <hr class="divider" />
      <div id="manual-update" class="row">
        <span class="label">{{
          t("settings.about.updateAvailable", {
            version: updateInfo.latestVersion,
          })
        }}</span>
        <span class="spacer"></span>
        <button
          v-if="updateInfo.applyMode === 'automatic' && !updateDone"
          class="btn btn-primary"
          type="button"
          :disabled="updateApplying"
          @click="applyUpdate"
        >
          {{
            updateApplying
              ? updateProgress > 0
                ? `${t("settings.about.updating")} ${updateProgress}%`
                : t("settings.about.updating")
              : t("settings.about.update")
          }}
        </button>
        <button
          v-else-if="updateInfo.applyMode === 'automatic'"
          class="btn btn-primary"
          type="button"
          @click="restartSettingsNow"
        >
          {{ t("settings.about.restartSettings") }}
        </button>
        <button
          v-else
          class="btn btn-primary"
          type="button"
          :disabled="updatePreparing"
          @click="prepareManualUpdate"
        >
          {{
            updatePreparing
              ? t("settings.about.preparingManual")
              : t("settings.about.prepareManual")
          }}
        </button>
      </div>
      <p v-if="updateInfo.applyMode === 'automatic'" class="card-desc">
        {{
          updateDone
            ? t("settings.about.updatedRestartHint")
            : t("settings.about.applyAfterAnswer")
        }}
      </p>
      <template v-else>
        <p class="card-desc">{{ t("settings.about.manualUnavailable") }}</p>
        <div v-if="updateInfo.applyMode === 'manualNpm'" class="row">
          <code class="value">{{ updateInfo.manualCommand }}</code>
          <span class="spacer"></span>
          <button class="btn" type="button" @click="copyManualCommand">
            {{
              manualCommandCopied
                ? t("settings.about.commandCopied")
                : t("settings.about.copyCommand")
            }}
          </button>
        </div>
        <p v-else class="card-desc">
          <a class="link" href="#" @click.prevent="openReleases">{{
            t("settings.about.openReleasePage")
          }}</a>
        </p>
        <p class="card-desc">{{ t("settings.about.prepareManualHint") }}</p>
      </template>

      <template v-if="notesHtml">
        <hr class="divider" />
        <p class="label">{{ t("settings.about.releaseNotes") }}</p>
        <div
          class="release-notes markdown"
          v-html="notesHtml"
          @click="onNotesClick"
        ></div>
      </template>
      <div class="row" style="margin-top: 8px">
        <span class="spacer"></span>
        <a class="link" href="#" @click.prevent="openReleases">{{
          t("settings.about.viewAllReleases")
        }}</a>
      </div>
    </template>

    <p v-if="updateError" class="result err" style="margin-top: 8px">
      {{ updateError }}
    </p>
  </div>

  <!-- Hidden switch for experimental features. -->
  <div class="card experimental-toggle">
    <div class="row">
      <div class="col">
        <span class="label">{{ t("settings.experimental.enableLabel") }}</span>
        <p class="card-desc">{{ t("settings.experimental.enableHint") }}</p>
      </div>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.experimental.enabled"
          @change="toggleExperimental"
        />
        <span class="track"></span>
      </label>
    </div>
  </div>
</template>
