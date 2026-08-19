<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { applyTheme } from "../lib/theme";
import { primaryModifierPressed, primaryShortcutLabel } from "../lib/platform";
import { applyLanguage } from "../i18n";
import { interjectCancel, interjectInit, interjectSubmit } from "../lib/ipc";
import type { AgentKind, ThemeMode } from "../lib/types";
import ComposerAttachments from "../components/ComposerAttachments.vue";
import { useInterjectAttachments } from "./interject/useInterjectAttachments";

const { t } = useI18n();
const cancelShortcut = primaryShortcutLabel("w");
const submitShortcut = primaryShortcutLabel("enter");

// 目标 agent 信息由 Rust 侧经窗口 URL 注入：?view=interject&session=...&kind=...&project=...
const params = new URLSearchParams(window.location.search);
const session = params.get("session") ?? "";
const kind = params.get("kind") ?? "";
const project = params.get("project") ?? "";

const text = ref("");
// 打开时已有待送达条数（>0 时提示「提交将整体覆盖」）。
const pendingEntries = ref(0);
const loaded = ref(false);
const sending = ref(false);
const textarea = ref<HTMLTextAreaElement | null>(null);
const attachments = useInterjectAttachments();

function kindLabel(k: string): string {
  const known: AgentKind[] = ["claude", "codex", "cursor", "grok", "pi"];
  return known.includes(k as AgentKind) ? t(`agents.kind.${k}`) : k;
}

// 可提交：有内容，或「清空已有待送达」（预填被删空也算一次有效提交 = 撤回）。
const canSend = computed(
  () =>
    !sending.value &&
    !attachments.busy.value &&
    (text.value.trim().length > 0 || attachments.hasAttachments.value || pendingEntries.value > 0),
);

async function send(): Promise<void> {
  if (!canSend.value) return;
  sending.value = true;
  try {
    await interjectSubmit(
      session,
      text.value.trim(),
      attachments.filePaths.value,
      attachments.pastedImages.value,
    );
    // 后端提交后即关窗；此处无需善后。
  } catch (err) {
    console.warn("interject submit failed", err);
    sending.value = false;
  }
}

async function cancel(): Promise<void> {
  try {
    await interjectCancel(session);
  } catch (err) {
    console.warn("interject cancel failed", err);
  }
}

function onKeydown(e: KeyboardEvent): void {
  if (primaryModifierPressed(e) && e.key === "Enter") {
    e.preventDefault();
    void send();
  } else if (
    e.key === "Escape" ||
    (primaryModifierPressed(e) && e.key.toLowerCase() === "w")
  ) {
    e.preventDefault();
    void cancel();
  }
}

let unlistenSettings: UnlistenFn | null = null;
let unlistenDragDrop: UnlistenFn | null = null;

onMounted(async () => {
  try {
    const init = await interjectInit(session);
    applyTheme(init.theme);
    applyLanguage(init.lang);
    text.value = init.text;
    pendingEntries.value = init.entries;
    attachments.reset(init.attachments);
  } catch {
    /* daemon 不可达：保持空预填，提交时后端兜底重试 */
  }
  // 设置变更实时生效（主题/语言与设置窗口同宿主进程广播）。
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (e) => {
      if (typeof e.payload.theme === "string") applyTheme(e.payload.theme);
      if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
    }
  );
  unlistenDragDrop = await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "drop") attachments.appendPaths(event.payload.paths);
  });
  loaded.value = true;
  // 聚焦输入框、光标移到末尾（预填内容之后继续输入）。
  requestAnimationFrame(() => {
    const el = textarea.value;
    if (el) {
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    }
  });
});

onBeforeUnmount(() => {
  unlistenSettings?.();
  unlistenDragDrop?.();
});
</script>

<template>
  <div class="interject" @keydown="onKeydown" @paste="attachments.onPaste">
    <header class="ij-header" data-tauri-drag-region>
      <span class="ij-title" data-tauri-drag-region>{{ t("interject.title") }}</span>
      <span v-if="kind" class="kind-badge">{{ kindLabel(kind) }}</span>
      <span v-if="project" class="ij-project" :title="project">{{ project }}</span>
    </header>

    <div class="ij-body">
      <p class="ij-hint">{{ t("interject.hint") }}</p>
      <div class="answer-composer ij-composer">
        <div class="input-wrap">
          <textarea
            ref="textarea"
            v-model="text"
            class="textarea"
            :placeholder="t('interject.placeholder')"
            :disabled="!loaded || sending"
            spellcheck="false"
          />
          <button
            class="img-btn"
            type="button"
            :title="t('interject.addAttachment')"
            :aria-label="t('interject.addAttachment')"
            :disabled="sending || attachments.busy.value"
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
        <p v-if="attachments.error.value" class="ij-error" role="alert">
          {{ attachments.error.value }}
        </p>
      </div>
    </div>

    <footer class="footer ij-footer" data-tauri-drag-region>
      <button type="button" class="btn" :disabled="sending" @click="cancel">
        {{ t("common.cancel") }} <kbd class="sc">{{ cancelShortcut }}</kbd>
      </button>
      <span v-if="pendingEntries > 0" class="ij-pending">
        {{ t("interject.overwriteNote", { n: pendingEntries }) }}
      </span>
      <span class="spacer" />
      <button type="button" class="btn btn-primary" :disabled="!canSend" @click="send">
        {{ t("popup.send") }} <kbd class="sc">{{ submitShortcut }}</kbd>
      </button>
    </footer>
  </div>
</template>

<style scoped>
.interject {
  display: flex;
  flex-direction: column;
  height: 100%;
  color: var(--text-primary);
}
.ij-header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.macos .ij-header {
  padding-top: 30px;
}
.ij-title {
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
}
.kind-badge {
  flex: 0 0 auto;
  padding: 1px 7px;
  border-radius: 5px;
  font-size: 10px;
  font-weight: 600;
  background: color-mix(in srgb, var(--text-primary) 9%, transparent);
  color: var(--text-secondary);
  white-space: nowrap;
}
.ij-project {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 12px;
  color: var(--text-secondary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  text-align: right;
}
.ij-body {
  flex: 1 1 auto;
  min-height: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px 14px 5px;
  overflow-y: auto;
}
.ij-body :deep(.reply-files) {
  margin-top: 0;
}
.ij-composer {
  flex: 1 1 auto;
}
.ij-composer .input-wrap {
  flex: 1 1 auto;
  min-height: 96px;
}
.ij-composer .textarea {
  flex: 1 1 auto;
  min-height: 96px;
  max-height: none;
}
.ij-error {
  margin: 0;
  color: var(--danger, #d70015);
  font-size: 11px;
}
.ij-hint {
  flex: 0 0 auto;
  margin: 0;
  font-size: 11px;
  color: var(--text-secondary);
}
.ij-footer {
  min-width: 0;
  border-top: none;
}
.ij-pending {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 11px;
  color: #c77700;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
