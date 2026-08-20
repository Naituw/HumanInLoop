// 「关于 / 版本自更新」域：版本号、检查更新、应用更新、更新日志渲染。
import { nextTick, onBeforeUnmount, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  getAppVersion,
  openPath,
  restartSettings,
  updateApply,
  updateCheck,
  updateGetNotes,
  updateGetVersionNotes,
  updatePrepare,
} from "../../lib/ipc";
import {
  renderMarkdown,
  markdownReady,
  handleCodeCopyClick,
} from "../../lib/markdown";
import type { UpdateInfo } from "../../lib/types";

export function useAboutUpdates() {
  const { t } = useI18n();

  const appVersion = ref("");
  const updateInfo = ref<UpdateInfo | null>(null);
  const updateChecking = ref(false);
  const updateApplying = ref(false);
  const updatePreparing = ref(false);
  const updateDone = ref(false);
  const manualCommandCopied = ref(false);
  const updateError = ref("");
  const updateProgress = ref(0);
  const notesHtml = ref("");
  const releasesUrl = "https://github.com/Naituw/AskHuman/releases";

  // 当前版本更新日志（折叠，懒加载；与「发现新版」的日志独立）。
  const currentNotesOpen = ref(false);
  const currentNotesHtml = ref("");
  const currentNotesLoading = ref(false);
  const currentNotesError = ref("");
  const currentNotesLoaded = ref(false);

  // 把后端错误转可读文案：限流（403/429，后端带 rate-limited 标记）→ 友好提示并引导手动下载 /
  // 设 token；其余沿用「<前缀>: <原始错误>」。
  function updateErrText(e: unknown, prefixKey: string): string {
    const s = String(e);
    if (/rate-limited|\b403\b|\b429\b/i.test(s)) {
      return t("settings.about.rateLimited");
    }
    return `${t(`settings.about.${prefixKey}`)}: ${s}`;
  }

  async function toggleCurrentNotes() {
    currentNotesOpen.value = !currentNotesOpen.value;
    if (!currentNotesOpen.value || currentNotesLoaded.value || !appVersion.value) {
      return;
    }
    currentNotesLoading.value = true;
    currentNotesError.value = "";
    try {
      const notes = await updateGetVersionNotes(appVersion.value);
      await markdownReady; // one-shot render below (not reactive) — wait for the real renderer
      currentNotesHtml.value = notes.trim()
        ? renderMarkdown(notes, {
            copyLabel: t("common.copyCode"),
            copiedLabel: t("common.copied"),
          })
        : "";
      currentNotesLoaded.value = true;
    } catch (e) {
      currentNotesError.value = updateErrText(e, "notesFailed");
    } finally {
      currentNotesLoading.value = false;
    }
  }

  async function checkUpdate(manual: boolean) {
    if (updateChecking.value) return;
    updateChecking.value = true;
    updateError.value = "";
    try {
      const info = await updateCheck(manual);
      updateInfo.value = info;
      manualCommandCopied.value = false;
      if (
        info.available &&
        new URLSearchParams(window.location.search)
          .get("tab")
          ?.endsWith("#manual-update")
      ) {
        await nextTick();
        document
          .getElementById("manual-update")
          ?.scrollIntoView({ behavior: "smooth", block: "center" });
      }
      notesHtml.value = "";
      if (info.available) {
        try {
          const notes = await updateGetNotes(true);
          await markdownReady; // one-shot render below (not reactive) — wait for the real renderer
          notesHtml.value = notes.trim()
            ? renderMarkdown(notes, {
                copyLabel: t("common.copyCode"),
                copiedLabel: t("common.copied"),
              })
            : "";
        } catch {
          notesHtml.value = "";
        }
      }
    } catch (e) {
      updateError.value = updateErrText(e, "checkFailed");
    } finally {
      updateChecking.value = false;
    }
  }

  async function applyUpdate() {
    if (updateApplying.value) return;
    updateApplying.value = true;
    updateError.value = "";
    updateProgress.value = 0;
    try {
      await updateApply();
      updateDone.value = true;
    } catch (e) {
      updateError.value = updateErrText(e, "updateFailed");
    } finally {
      updateApplying.value = false;
    }
  }

  async function copyManualCommand(): Promise<boolean> {
    const command = updateInfo.value?.manualCommand.trim();
    if (!command) return false;
    try {
      await navigator.clipboard.writeText(command);
      manualCommandCopied.value = true;
      return true;
    } catch (e) {
      updateError.value = `${t("settings.about.copyCommandFailed")}: ${String(e)}`;
      return false;
    }
  }

  async function prepareManualUpdate() {
    const info = updateInfo.value;
    if (!info || info.applyMode === "automatic" || updatePreparing.value) return;
    if (!window.confirm(t("settings.about.prepareConfirm"))) return;

    updatePreparing.value = true;
    updateError.value = "";
    try {
      // Preserve the next step before the helper closes the GUI Host. npm users get the exact
      // command on their clipboard; Direct users get the release page in their browser.
      if (info.applyMode === "manualNpm") {
        if (!(await copyManualCommand())) {
          updatePreparing.value = false;
          return;
        }
      } else {
        await openPath(info.sourceUrl || releasesUrl);
      }
      await updatePrepare();
    } catch (e) {
      updateError.value = updateErrText(e, "prepareFailed");
      updatePreparing.value = false;
    }
  }

  function openReleases() {
    void openPath(releasesUrl);
  }

  // 渲染后的更新日志里的链接：用系统默认浏览器打开，避免在设置 webview 内跳转。
  function onNotesClick(e: MouseEvent) {
    if (handleCodeCopyClick(e)) return;
    const anchor = (e.target as HTMLElement | null)?.closest?.("a") as
      | HTMLAnchorElement
      | null;
    if (!anchor) return;
    const href = anchor.href;
    if (!/^(https?:|mailto:)/i.test(href)) return;
    e.preventDefault();
    e.stopPropagation();
    void openPath(href);
  }

  async function restartSettingsNow() {
    try {
      await restartSettings();
    } catch {
      /* ignore */
    }
  }

  // 关于区初始化：取本地版本，并静默检查一次（best-effort，失败不打扰）。
  async function initAbout() {
    try {
      appVersion.value = await getAppVersion();
    } catch {
      appVersion.value = "";
    }
    void checkUpdate(false);
  }

  let aboutReady = false;
  let aboutInflight: Promise<void> | null = null;

  async function ensureAbout() {
    if (aboutReady) return;
    if (aboutInflight) return aboutInflight;
    const run = initAbout().finally(() => {
      if (aboutInflight === run) aboutInflight = null;
      aboutReady = true;
    });
    aboutInflight = run;
    return run;
  }

  let unlistenProgress: UnlistenFn | null = null;
  listen<{ percentage: number }>("update_download_progress", (e) => {
    updateProgress.value = Math.round(e.payload.percentage);
  }).then((un) => {
    unlistenProgress = un;
  });
  onBeforeUnmount(() => unlistenProgress?.());

  return {
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
    initAbout,
    ensureAbout,
  };
}
