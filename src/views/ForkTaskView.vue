<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { applyLanguage } from "../i18n";
import { forkTaskInit, forkTaskLaunch } from "../lib/ipc";
import { applyTheme } from "../lib/theme";
import { primaryModifierPressed, primaryShortcutLabel } from "../lib/platform";
import type { ForkTaskSource, PopupSubmitKey, ThemeMode } from "../lib/types";
import LaunchPermission from "./newtask/LaunchPermission.vue";

const { t } = useI18n();
const params = new URLSearchParams(window.location.search);
const session = ref(params.get("session") ?? "");
const source = ref<ForkTaskSource | null>(null);
const permissionPrompt = ref("ask");
const permissionChoice = ref("");
const popupSubmitKey = ref<PopupSubmitKey>("cmdEnter");
const task = ref("");
const loading = ref(true);
const launching = ref(false);
const error = ref("");

const effectivePermission = computed<"agent-default" | "yolo" | null>(() => {
  if (source.value?.kind === "pi") return "agent-default";
  const value =
    permissionPrompt.value === "ask" ? permissionChoice.value : permissionPrompt.value;
  return value === "agent-default" || value === "yolo" ? value : null;
});
const taskChars = computed(() => [...task.value.trim()].length);
const tooLong = computed(() => taskChars.value > 3000);
const canLaunch = computed(
  () =>
    !loading.value &&
    !launching.value &&
    !!source.value &&
    !!effectivePermission.value &&
    taskChars.value > 0 &&
    !tooLong.value
);
const submitKeyLabel = computed(() =>
  popupSubmitKey.value === "enter" ? "↵" : primaryShortcutLabel("enter")
);
const shortSession = (id?: string | null) => id?.slice(0, 8) ?? "";
let loadGeneration = 0;

async function loadSource(nextSession: string): Promise<void> {
  if (launching.value) {
    error.value = t("forkTask.busyRetarget");
    return;
  }
  const generation = ++loadGeneration;
  loading.value = true;
  error.value = "";
  task.value = "";
  permissionChoice.value = "";
  try {
    const init = await forkTaskInit(nextSession);
    if (generation !== loadGeneration) return;
    session.value = nextSession;
    source.value = init.source;
    permissionPrompt.value = init.permissionPrompt;
    popupSubmitKey.value = init.popupSubmitKey;
    applyTheme(init.theme);
    applyLanguage(init.lang);
  } catch (err) {
    if (generation !== loadGeneration) return;
    source.value = null;
    error.value = t("forkTask.loadFailed", { e: String(err) });
  } finally {
    if (generation === loadGeneration) loading.value = false;
  }
}

async function launch(): Promise<void> {
  if (!canLaunch.value || !source.value || !effectivePermission.value) return;
  launching.value = true;
  error.value = "";
  try {
    await forkTaskLaunch({
      session: source.value.sessionId,
      permission: effectivePermission.value,
      task: task.value.trim(),
    });
    await getCurrentWebviewWindow().close();
  } catch (err) {
    error.value = t("forkTask.launchFailed", { e: String(err) });
  } finally {
    launching.value = false;
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== "Enter" || event.isComposing) return;
  const modified = primaryModifierPressed(event) && !event.shiftKey && !event.altKey;
  const bare = !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey;
  if (popupSubmitKey.value === "enter" ? !bare : !modified) return;
  event.preventDefault();
  void launch();
}

let unlistenGoto: UnlistenFn | null = null;
let unlistenSettings: UnlistenFn | null = null;
onMounted(async () => {
  unlistenGoto = await listen<{ session?: string }>("forktask-goto", (event) => {
    const next = event.payload.session?.trim();
    if (next) void loadSource(next);
  });
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (event) => {
      if (typeof event.payload.theme === "string") applyTheme(event.payload.theme);
      if (typeof event.payload.language === "string") applyLanguage(event.payload.language);
    }
  );
  await loadSource(session.value);
});
onBeforeUnmount(() => {
  unlistenGoto?.();
  unlistenSettings?.();
});
</script>

<template>
  <div class="forktask-win">
    <header class="ft-header" data-tauri-drag-region>
      <span class="ft-title" data-tauri-drag-region>{{ t("forkTask.title") }}</span>
    </header>
    <div class="ft-form">
      <div class="ft-body">
        <div v-if="loading" class="ft-empty"><span class="ft-spinner" /></div>
        <template v-else-if="source">
          <section class="nt-section">
            <span class="nt-label">{{ t("forkTask.source") }}</span>
            <div class="ft-source">
              <div class="ft-source-title">
                <span class="ft-badge">{{ source.kind }}</span>
                <strong>#{{ source.seq }} {{ source.title || t("agents.untitled") }}</strong>
              </div>
              <span>{{ source.cwd }}</span>
              <span>
                {{ t("agents.state." + source.state) }}
                <template v-if="source.forkedFromSessionId">
                  · {{ t("forkTask.forkedFrom", { id: shortSession(source.forkedFromSessionId) }) }}
                </template>
              </span>
              <span class="ft-source-note">{{ t("forkTask.sourceContinues") }}</span>
            </div>
          </section>

          <LaunchPermission
            v-if="source.kind !== 'pi'"
            v-model="permissionChoice"
            :permission-prompt="permissionPrompt"
          />
          <p v-else class="ft-source-note">{{ t("newTask.piPermissionHint") }}</p>

          <section class="nt-section">
            <label class="nt-label" for="ft-task">{{ t("forkTask.instruction") }}</label>
            <textarea
              id="ft-task"
              v-model="task"
              class="ft-input"
              rows="6"
              :placeholder="t('forkTask.placeholder')"
              @keydown="onKeydown"
            />
            <p v-if="tooLong" class="ft-error">
              {{ t("newTask.tooLong", { n: taskChars }) }}
            </p>
          </section>
        </template>
      </div>
      <footer class="ft-footer">
        <p v-if="error" class="ft-error">{{ error }}</p>
        <div class="ft-footer-row">
          <span class="ft-note">{{ t("forkTask.launchNote") }}</span>
          <button class="ft-launch" :disabled="!canLaunch" @click="launch">
            {{ launching ? t("forkTask.launching") : t("forkTask.launch") }}
            <kbd v-if="!launching">{{ submitKeyLabel }}</kbd>
          </button>
        </div>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.forktask-win, .ft-form { display: flex; flex-direction: column; height: 100%; min-height: 0; color: var(--text-primary); }
.ft-header { display: flex; align-items: center; padding: 10px 14px; border-bottom: var(--hairline) solid var(--border); }
.macos .ft-header { padding-top: 30px; }
.ft-title { font-size: 14px; font-weight: 600; }
.ft-body { flex: 1 1 auto; min-height: 0; overflow-y: auto; padding: 14px; display: flex; flex-direction: column; gap: 16px; }
.ft-empty { display: grid; place-items: center; height: 100%; }
.ft-spinner { width: 20px; height: 20px; border: 2px solid color-mix(in srgb, var(--text-primary) 18%, transparent); border-top-color: var(--text-secondary); border-radius: 50%; animation: ft-spin 0.7s linear infinite; }
@keyframes ft-spin { to { transform: rotate(360deg); } }
.ft-source { display: flex; flex-direction: column; gap: 4px; padding: 10px; border: var(--hairline) solid var(--border); border-radius: 8px; background: var(--bg-elevated); color: var(--text-secondary); font-size: 11px; overflow-wrap: anywhere; }
.ft-source-title { display: block; color: var(--text-primary); font-size: 12.5px; }
.ft-source-title strong { overflow-wrap: anywhere; }
.ft-badge { display: inline-block; margin-right: 7px; padding: 1px 6px; border-radius: 5px; background: color-mix(in srgb, #0a84ff 12%, transparent); color: #0a84ff; font-size: 10px; line-height: 1.5; text-transform: uppercase; vertical-align: 1px; white-space: nowrap; }
.ft-source-note { color: #0a84ff; }
.ft-input { width: 100%; min-height: 112px; resize: vertical; box-sizing: border-box; border: var(--hairline) solid var(--control-border); border-radius: 7px; padding: 8px 9px; background: var(--control-bg); color: var(--text-primary); font: inherit; font-size: 12px; line-height: 1.45; box-shadow: var(--clickable-shadow); }
.ft-input:focus { outline: none; box-shadow: var(--focus-ring), var(--clickable-shadow); }
.ft-footer { padding: 10px 14px 12px; border-top: var(--hairline) solid var(--border); }
.ft-footer-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; }
.ft-note, .ft-error { margin: 0; font-size: 11px; color: var(--text-secondary); }
.ft-error { color: #ff453a; white-space: pre-wrap; overflow-wrap: anywhere; }
.ft-launch { border: 0; border-radius: 7px; padding: 6px 12px; background: #0a84ff; color: #fff; font-size: 12px; font-weight: 600; cursor: pointer; }
.ft-launch:disabled { opacity: 0.45; cursor: default; }
.ft-launch kbd { margin-left: 6px; border: 0; background: transparent; color: inherit; }
</style>
