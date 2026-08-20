// 「通用」tab 相关状态与动作：外观 / 弹窗行为 / 菜单栏 / 历史 / 语音 / 窗口材质。
// （daemonLifecycle 虽展示在「高级」tab，但同属 general 配置段，也放这里。）
import { computed, onBeforeUnmount, ref } from "vue";
import { applyLanguage } from "../../i18n";
import {
  applyWindowEffect,
  historyCount,
  playPopupSound,
  popupSoundSupport,
  setTheme,
  trimHistory,
} from "../../lib/ipc";
import { isGlassSupported } from "tauri-plugin-liquid-glass-api";
import { isMac } from "../../lib/platform";
import { applyTheme, applyWindowMaterial } from "../../lib/theme";
import {
  eventToSpec,
  formatModifierPreview,
  isModifierOnly,
  shortcutConflict,
  specToString,
  type ConflictReason,
} from "../../lib/shortcut";
import type {
  DaemonLifecycleMode,
  MenuBarIconMode,
  PopupAnimation,
  PopupSoundSupport,
  PopupSubmitKey,
  ThemeMode,
  UiLanguage,
  WindowEffect,
} from "../../lib/types";
import type { SettingsCore } from "./context";

export function useGeneralSettings(core: SettingsCore) {
  const { config, activeTab, persist } = core;

  async function changeTheme(theme: ThemeMode) {
    if (!config.value) return;
    config.value.general.theme = theme;
    applyTheme(theme);
    await setTheme(theme);
    await persist();
  }

  // 切换界面语言：本窗口立即生效；persist 广播 settings-updated 令其它窗口同步。
  async function changeLanguage(lang: UiLanguage) {
    if (!config.value) return;
    config.value.general.language = lang;
    applyLanguage(lang);
    await persist();
  }

  async function changeAnimation(anim: PopupAnimation) {
    if (!config.value) return;
    config.value.general.appearAnimation = anim;
    await persist();
  }

  async function changeSubmitKey(mode: PopupSubmitKey) {
    if (!config.value) return;
    config.value.general.popupSubmitKey = mode;
    await persist();
  }

  // 菜单栏图标三态（off/active/always）。仅持久化；宿主进程监听 config 变化后自行
  // 建/移图标、装/卸登录项（见 src-tauri app::gui_host）。
  async function changeMenuBarIcon(mode: MenuBarIconMode) {
    if (!config.value) return;
    config.value.general.menuBarIcon = mode;
    await persist();
  }

  // 守护进程生命周期二态（activity/keepalive）。仅持久化；daemon 与宿主监听 config 变化后自行
  // 换挡（保活→立即拉起 + 装开机自启登录项 + 不空闲退出；见 src-tauri daemon / app::gui_host）。
  // 「从 IM 创建 Agent 任务」开启期间锁定为保活（控件已禁用，这里再兜底一次）。
  async function changeDaemonLifecycle(mode: DaemonLifecycleMode) {
    if (!config.value || config.value.agentTasks.enabled) return;
    config.value.general.daemonLifecycle = mode;
    await persist();
  }

  // Popup sound support: named choices on macOS, toggle on Linux, hidden otherwise.
  const soundSupport = ref<PopupSoundSupport>({ kind: "none", names: [] });

  async function changePopupSound(value: string) {
    if (!config.value) return;
    config.value.general.popupSound = value;
    await persist();
    // Preview immediately after selecting a non-empty sound.
    if (value) playPopupSound(value).catch(() => {});
  }

  function previewSound() {
    const name = config.value?.general.popupSound;
    if (name) playPopupSound(name).catch(() => {});
  }

  // 当前历史总条数（用于「超额」提示与「立即清理」）。
  const historyTotal = ref(0);
  const overLimit = computed(() => {
    const limit = config.value?.general.historyLimit ?? 0;
    return historyTotal.value > limit;
  });

  // 改保留条数：仅持久化；裁剪发生在下次 AskHuman 或点击「立即清理」。
  async function changeHistoryLimit(raw: number) {
    if (!config.value) return;
    const v = Number.isFinite(raw) ? Math.max(0, Math.floor(raw)) : 0;
    config.value.general.historyLimit = v;
    await persist();
  }

  // 待办执行历史保留条数（每项目；裁剪发生在下次执行出队记录时）。
  async function changeTodoHistoryLimit(raw: number) {
    if (!config.value) return;
    const v = Number.isFinite(raw) ? Math.max(0, Math.floor(raw)) : 0;
    config.value.general.todoHistoryLimit = v;
    await persist();
  }

  async function cleanHistoryNow() {
    const limit = config.value?.general.historyLimit ?? 0;
    historyTotal.value = await trimHistory(limit);
  }

  // 语音识别语言下拉项：第一项「跟随系统」(auto) + 常用语言（BCP-47）。
  const SPEECH_LANGUAGES: { value: string; label: string }[] = [
    // auto 的显示文案在模板里走 i18n（settings.speech.languageSystem）。
    { value: "auto", label: "" },
    { value: "zh-CN", label: "简体中文" },
    { value: "zh-TW", label: "繁体中文" },
    { value: "en-US", label: "English (US)" },
    { value: "ja-JP", label: "日本語" },
    { value: "ko-KR", label: "한국어" },
  ];

  async function changeSpeechLanguage(lang: string) {
    if (!config.value) return;
    config.value.general.speechLanguage = lang;
    await persist();
  }

  // 语音快捷键录制：点一下进入录制，直接按组合键录入；Esc 取消。
  const recordingShortcut = ref(false);
  const shortcutPreview = ref("");
  const shortcutError = ref<ConflictReason | null>(null);
  let shortcutHandler: ((e: KeyboardEvent) => void) | null = null;

  function stopRecordShortcut() {
    recordingShortcut.value = false;
    shortcutPreview.value = "";
    if (shortcutHandler) {
      window.removeEventListener("keydown", shortcutHandler, true);
      shortcutHandler = null;
    }
  }

  function startRecordShortcut() {
    if (recordingShortcut.value) return;
    recordingShortcut.value = true;
    shortcutError.value = null;
    shortcutPreview.value = "";
    shortcutHandler = (e: KeyboardEvent) => {
      // Capture before the webview handles platform shortcuts such as close-window.
      e.preventDefault();
      e.stopPropagation();
      if (e.key === "Escape") {
        stopRecordShortcut();
        return;
      }
      if (isModifierOnly(e)) {
        shortcutPreview.value = formatModifierPreview(e);
        return;
      }
      const spec = eventToSpec(e);
      if (!spec) return;
      const reason = shortcutConflict(spec);
      if (reason) {
        shortcutError.value = reason;
        return;
      }
      if (config.value) {
        config.value.general.speechShortcut = specToString(spec);
        persist();
      }
      stopRecordShortcut();
    };
    window.addEventListener("keydown", shortcutHandler, true);
  }

  function clearShortcut() {
    if (!config.value) return;
    config.value.general.speechShortcut = "";
    shortcutError.value = null;
    persist();
    stopRecordShortcut();
  }

  onBeforeUnmount(stopRecordShortcut);

  // Liquid Glass is an optional third material on macOS 26+.
  const glassSupported = ref(false);
  const effectiveWindowEffect = computed<WindowEffect>(() => {
    const requested = config.value?.general.windowEffect ?? "blur";
    return requested === "glass" && !glassSupported.value ? "blur" : requested;
  });

  // Apply locally at once; the backend emits the resolved effect to every open window.
  async function changeWindowEffect(effect: WindowEffect) {
    if (!config.value) return;
    config.value.general.windowEffect = effect;
    applyWindowMaterial(effect);
    await persist();
    try {
      await applyWindowEffect(effect);
    } catch (e) {
      console.error("切换窗口材质失败", e);
    }
  }

  // 打开「实验」开关时显露实验 Tab；关闭时若停留在实验 Tab 则退回通用。
  async function toggleExperimental() {
    if (!config.value) return;
    if (!config.value.experimental.enabled && activeTab.value === "experimental") {
      activeTab.value = "general";
    }
    await persist();
  }

  // 通用域初始化：历史条数、弹窗声音支持、Liquid Glass 支持探测。
  async function initGeneral() {
    historyTotal.value = await historyCount();
    try {
      soundSupport.value = await popupSoundSupport();
    } catch {
      soundSupport.value = { kind: "none", names: [] };
    }
    if (isMac) {
      try {
        glassSupported.value = await isGlassSupported();
      } catch {
        glassSupported.value = false;
      }
    }
  }

  let generalReady = false;
  let generalInflight: Promise<void> | null = null;

  async function ensureGeneral() {
    if (generalReady) return;
    if (generalInflight) return generalInflight;
    const run = initGeneral().finally(() => {
      if (generalInflight === run) generalInflight = null;
      generalReady = true;
    });
    generalInflight = run;
    return run;
  }

  return {
    changeTheme,
    changeLanguage,
    changeAnimation,
    changeSubmitKey,
    changeMenuBarIcon,
    changeDaemonLifecycle,
    soundSupport,
    changePopupSound,
    previewSound,
    historyTotal,
    overLimit,
    changeHistoryLimit,
    changeTodoHistoryLimit,
    cleanHistoryNow,
    SPEECH_LANGUAGES,
    changeSpeechLanguage,
    recordingShortcut,
    shortcutPreview,
    shortcutError,
    startRecordShortcut,
    clearShortcut,
    glassSupported,
    effectiveWindowEffect,
    changeWindowEffect,
    toggleExperimental,
    initGeneral,
    ensureGeneral,
  };
}
