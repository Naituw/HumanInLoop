// 设置搜索（R9）：静态索引 + 搜索态交互（放大镜进入、↑↓ 选择、回车跳转、Esc 退出、
// Cmd/Ctrl+F 快捷键）。
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch, type Ref } from "vue";
import { useI18n } from "vue-i18n";
import type { AppConfig } from "../../lib/types";
import { isMac, supportsAgentTasks } from "../../lib/platform";
import type { Tab } from "./context";

// 静态索引：每条 = 一个设置项（tab + 展示/锚定标题 + 参与匹配的额外文案）。标题文本
// 与模板渲染文本同源（都经 t()），跳转后按文本在 DOM 里定位，无须给每行加 id。
interface SearchEntry {
  tab: Tab;
  /** 结果展示文本，也是跳转后 DOM 定位的锚文本（须与渲染文本一致）。 */
  title: string;
  /** 额外参与匹配的文案（描述/子项等），不展示。 */
  extra: string[];
}

export function useSettingsSearch(deps: {
  config: Ref<AppConfig | null>;
  activeTab: Ref<Tab>;
}) {
  const { t } = useI18n();
  const { activeTab } = deps;

  // 搜索态：点放大镜进入（隐藏 tab、显示输入框并聚焦），Esc/✕/选中结果退出。
  const searchActive = ref(false);
  const searchQuery = ref("");
  const searchSelected = ref(0);
  const searchInputEl = ref<HTMLInputElement | null>(null);

  function openSearch() {
    searchActive.value = true;
    searchQuery.value = "";
    searchSelected.value = 0;
    void nextTick(() => searchInputEl.value?.focus());
  }

  // Cmd+F（macOS）/ Ctrl+F 直接进入搜索（已在搜索态则重新聚焦输入框）。
  function onGlobalSearchHotkey(e: KeyboardEvent) {
    if (!(isMac ? e.metaKey : e.ctrlKey) || e.key.toLowerCase() !== "f") return;
    if (e.altKey || e.shiftKey) return;
    e.preventDefault();
    if (searchActive.value) searchInputEl.value?.focus();
    else openSearch();
  }
  onMounted(() => window.addEventListener("keydown", onGlobalSearchHotkey));
  onBeforeUnmount(() => window.removeEventListener("keydown", onGlobalSearchHotkey));

  function closeSearch() {
    searchActive.value = false;
    searchQuery.value = "";
  }

  /** 键盘导航：↑↓ 移动选中（拦截默认的光标跳行为）、回车打开选中项、Esc 清空/退出。 */
  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      if (searchQuery.value) searchQuery.value = "";
      else closeSearch();
      return;
    }
    const n = searchResults.value.length;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (n > 0) searchSelected.value = (searchSelected.value + 1) % n;
      return;
    }
    if (e.key === "ArrowUp") {
      e.preventDefault();
      if (n > 0) searchSelected.value = (searchSelected.value - 1 + n) % n;
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const r = searchResults.value[searchSelected.value];
      if (r) void gotoSearchResult(r);
    }
  }

  watch(searchQuery, () => {
    searchSelected.value = 0;
  });
  // 键盘上下移动选中时保持可见（结果列表可滚动）。
  watch(searchSelected, async () => {
    await nextTick();
    document
      .querySelector(".tab-search-item.selected")
      ?.scrollIntoView({ block: "nearest" });
  });

  const searchIndex = computed<SearchEntry[]>(() => {
    const e = (tab: Tab, titleKey: string, extraKeys: string[] = []): SearchEntry => ({
      tab,
      title: t(titleKey),
      extra: extraKeys.map((k) => t(k)),
    });
    const lit = (tab: Tab, title: string, extra: string[] = []): SearchEntry => ({
      tab,
      title,
      extra,
    });
    const list: SearchEntry[] = [
      // 通用
      e("general", "settings.appearance.title"),
      e("general", "settings.appearance.theme", [
        "settings.appearance.themeSystem",
        "settings.appearance.themeLight",
        "settings.appearance.themeDark",
      ]),
      e("general", "settings.appearance.language"),
      e("general", "settings.popupBehavior.title"),
      e("general", "settings.popupBehavior.alwaysOnTop"),
      e("general", "settings.popupBehavior.submitKey"),
      e("general", "settings.popupBehavior.prewarm", [
        "settings.popupBehavior.prewarmHint",
      ]),
      e("general", "settings.popupBehavior.testPopup"),
      e("general", "settings.history.title"),
      e("general", "settings.history.limit", ["settings.history.limitHint"]),
      e("general", "settings.about.title", [
        "settings.about.currentVersion",
        "settings.about.latestVersion",
      ]),
      // Agents 集成
      e("integration", "settings.integration.promptTitle"),
      e("integration", "settings.integration.stopTitle"),
      lit("integration", "Claude Code", ["Agent"]),
      lit("integration", "Codex", ["Agent"]),
      lit("integration", "Cursor", ["Agent"]),
      lit("integration", "Grok", ["Agent"]),
      // 通信渠道
      e("channel", "settings.channels.popupTitle", [
        "settings.channels.rememberSize",
        "settings.channels.defaultWidth",
        "settings.channels.defaultHeight",
      ]),
      e("channel", "settings.channels.feishuTitle", [
        "settings.channels.appId",
        "settings.channels.appSecret",
      ]),
      e("channel", "settings.channels.telegramTitle", [
        "settings.channels.botToken",
        "settings.channels.chatId",
      ]),
      e("channel", "settings.channels.dingtalkTitle", [
        "settings.channels.clientId",
        "settings.channels.clientSecret",
        "settings.channels.userId",
      ]),
      e("channel", "settings.channels.slackTitle", [
        "settings.channels.slackBotToken",
        "settings.channels.slackAppToken",
        "settings.channels.slackUserId",
      ]),
    ];
    list.push(e("general", "settings.popupBehavior.sound"));
    if (isMac) {
      list.push(
        e("general", "settings.popupBehavior.appearAnimation"),
        e("general", "settings.speech.title", [
          "settings.speech.language",
          "settings.speech.shortcut",
        ]),
      );
    }
    if (isMac) {
      list.push(e("general", "settings.popupBehavior.windowEffect"));
    }
    list.push(
      e("general", "settings.menuBar.title", [
        "settings.menuBar.icon",
        "settings.menuBar.hint",
      ]),
      e("advanced", "settings.experimental.lifecycleTitle", [
        "settings.experimental.lifecycleDesc",
      ]),
      e("advanced", "settings.experimental.daemonLifecycleTitle", [
        "settings.experimental.daemonLifecycleLabel",
        "settings.experimental.daemonLifecycleActivity",
        "settings.experimental.daemonLifecycleKeepalive",
      ]),
      e("advanced", "settings.channels.autoActivationTitle", [
        "settings.channels.autoActivationDesc",
      ]),
      e("advanced", "settings.channels.autoEndWatchTitle", [
        "settings.channels.autoEndWatchDesc",
      ]),
      // Permission details are loaded on demand; the static title/description remain searchable.
      e("advanced", "settings.permissionRules.title", [
        "settings.permissionRules.desc",
      ]),
    );
    if (supportsAgentTasks) {
      list.push(
        e("advanced", "settings.agentTasks.title", [
          "settings.agentTasks.description",
          "settings.agentTasks.permission",
          "settings.agentTasks.readiness",
          "settings.agentTasks.workspaces",
        ]),
      );
    }
    return list;
  });

  const searchResults = computed<SearchEntry[]>(() => {
    const q = searchQuery.value.trim().toLowerCase();
    if (!q) return [];
    return searchIndex.value
      .filter(
        (s) =>
          s.title.toLowerCase().includes(q) ||
          s.extra.some((x) => x.toLowerCase().includes(q)),
      )
      .slice(0, 12);
  });

  /** 跳到搜索结果：退出搜索态 → 切 tab → 按锚文本定位行/卡片 → 滚动居中 + 短暂高亮。 */
  async function gotoSearchResult(entry: SearchEntry) {
    closeSearch();
    activeTab.value = entry.tab;
    await nextTick();
    const nodes = Array.from(
      document.querySelectorAll(".settings-body .card-title, .settings-body .label"),
    );
    const anchor = nodes.find((n) => (n.textContent ?? "").trim().startsWith(entry.title));
    if (!anchor) return;
    // 行级条目高亮所在行，卡片标题条目高亮整卡。
    const el = (anchor.classList.contains("label")
      ? (anchor.closest(".row") ?? anchor.closest(".card") ?? anchor)
      : (anchor.closest(".card") ?? anchor)) as HTMLElement;
    el.scrollIntoView({ behavior: "smooth", block: "center" });
    el.classList.add("search-hit-highlight");
    window.setTimeout(() => el.classList.remove("search-hit-highlight"), 2200);
  }

  return {
    searchActive,
    searchQuery,
    searchSelected,
    searchInputEl,
    openSearch,
    closeSearch,
    onSearchKeydown,
    searchResults,
    gotoSearchResult,
  };
}
