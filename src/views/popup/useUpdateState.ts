// 弹窗「版本自更新」域：导航栏入口按钮 + 浮层（日志/一键更新）+ 待生效横条的状态。
import { ref, type ComputedRef } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  openSettings,
  popupUpdateState,
  updateApply,
  updateGetNotes,
} from "../../lib/ipc";
import { renderMarkdown, markdownReady } from "../../lib/markdown";
import type { PushedUpdateState, UpdateApplyMode } from "../../lib/types";

export function useUpdateState(deps: {
  codeCopyLabels: ComputedRef<{ copyLabel: string; copiedLabel: string }>;
}) {
  const { t } = useI18n();
  const { codeCopyLabels } = deps;

  const updateAvailable = ref(false);
  const updatePending = ref(false);
  const updateLatest = ref("");
  // Fail closed until the backend snapshot arrives. An old/missing snapshot must never reveal the
  // automatic apply action in a newly bundled frontend.
  const updateApplyMode = ref<UpdateApplyMode>("manualDirect");
  const updatePopoverOpen = ref(false);
  const updating = ref(false);
  const updateStarted = ref(false);
  const updateError = ref("");
  const updateNotesHtml = ref("");

  async function toggleUpdatePopover() {
    updatePopoverOpen.value = !updatePopoverOpen.value;
    if (updatePopoverOpen.value && !updateNotesHtml.value) {
      try {
        const notes = await updateGetNotes(false);
        await markdownReady; // one-shot render below (not reactive) — wait for the real renderer
        updateNotesHtml.value = notes.trim()
          ? renderMarkdown(notes, codeCopyLabels.value)
          : "";
      } catch {
        updateNotesHtml.value = "";
      }
    }
  }

  async function applyUpdateFromPopup() {
    if (updateApplyMode.value !== "automatic") {
      try {
        await openSettings("general#manual-update");
        updatePopoverOpen.value = false;
      } catch (e) {
        updateError.value = `${t("popup.update.failed")}: ${String(e)}`;
      }
      return;
    }
    if (updating.value || updateStarted.value) return;
    updating.value = true;
    updateError.value = "";
    try {
      await updateApply();
      updateStarted.value = true;
    } catch (e) {
      const s = String(e);
      updateError.value = /rate-limited|\b403\b|\b429\b/i.test(s)
        ? t("popup.update.rateLimited")
        : `${t("popup.update.failed")}: ${s}`;
    } finally {
      updating.value = false;
    }
  }

  let unlistenUpdate: UnlistenFn | null = null;

  // 首帧后初始化：先拉初值（规避事件早于监听），再监听 daemon 经 GUI Helper 转发的实时变更。
  async function initUpdateState() {
    try {
      const u = await popupUpdateState();
      updateAvailable.value = u.available;
      updatePending.value = u.pending;
      updateLatest.value = u.latestVersion;
      updateApplyMode.value = u.applyMode;
    } catch {
      /* 单进程回退 / 无 daemon：忽略 */
    }
    unlistenUpdate = await listen<PushedUpdateState>("update-state", (e) => {
      updateAvailable.value = e.payload.available;
      updatePending.value = e.payload.pending;
      updateLatest.value = e.payload.latestVersion;
      updateApplyMode.value = e.payload.applyMode;
    });
  }

  function disposeUpdateState() {
    unlistenUpdate?.();
  }

  return {
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
    initUpdateState,
    disposeUpdateState,
  };
}
