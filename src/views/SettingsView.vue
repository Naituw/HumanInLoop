<script setup lang="ts">
// 设置页编排层：tabbar（含 R9 搜索态）+ 各 tab 子组件 + 根级弹层。
// 共享状态与各域逻辑在 ./settings/*（createSettingsContext provide，子组件 inject）。
import { onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { applyLanguage } from "../i18n";
import { getSettings } from "../lib/ipc";
import { applyTheme } from "../lib/theme";
import type { ThemeMode, UiLanguage } from "../lib/types";
import {
  createSettingsContext,
  isMac,
  supportsAgentTasks,
  TABS,
  type Tab,
} from "./settings/context";
import { tabLoads, type SettingsLoad } from "./settings/tabData";
import GeneralTab from "./settings/GeneralTab.vue";
import AdvancedTab from "./settings/AdvancedTab.vue";
import ExperimentalTab from "./settings/ExperimentalTab.vue";
import IntegrationTab from "./settings/IntegrationTab.vue";
import ChannelsTab from "./settings/ChannelsTab.vue";
import SettingsOverlays from "./settings/SettingsOverlays.vue";
import "./settings/settings.css";

const { t } = useI18n();

const ctx = createSettingsContext();
const {
  config,
  activeTab,
  secretsPresent,
  updateSummary,
  searchActive,
  searchQuery,
  searchSelected,
  searchInputEl,
  openSearch,
  closeSearch,
  onSearchKeydown,
  searchResults,
  gotoSearchResult,
} = ctx;

// tab 按钮**仅 macOS** 兼作窗口拖拽区（按住 tab 拖窗）：用屏幕坐标区分「点击切换」与「拖动移窗」。
// 原生拖窗时窗口跟随光标，clientX/Y 几乎不变，故必须用 screenX/Y。
// Windows/Linux 不给按钮标 data-tauri-drag-region（tabDrag=null 即不渲染属性）：这两个平台的
// start_dragging 走原生模态拖动（Win32 WM_NCLBUTTONDOWN / GTK begin_move_drag），会吞掉后续
// mouseup/click，@click 永不触发 → tab 点了没反应。摘掉属性后按钮是 Tauri drag.js 语义里的
// 「可点击元素」，自动阻断拖拽（外层 tabbar 空白区仍可拖），纯点击即可切换。
const tabDrag = isMac ? "" : null;
const tabDown = ref<{ x: number; y: number } | null>(null);
function onTabDown(e: MouseEvent) {
  tabDown.value = { x: e.screenX, y: e.screenY };
}
function onTabClick(tab: Tab, e: MouseEvent) {
  const d = tabDown.value;
  tabDown.value = null;
  if (d && Math.hypot(e.screenX - d.x, e.screenY - d.y) > 4) return;
  activeTab.value = tab;
  void ensureTabData(tab);
}

function runLoad(load: SettingsLoad): Promise<void> {
  switch (load.type) {
    case "integration-status":
      return ctx.ensureIntegration();
    case "pi-version":
      return ctx.ensurePiVersion();
    case "general":
      return ctx.ensureGeneral();
    case "about":
      return ctx.ensureAbout();
    case "agent-tasks":
      return ctx.ensureAgentTaskSettings(load.force);
  }
}

function ensureTabData(tab: Tab): Promise<unknown> {
  return Promise.all(tabLoads(tab, supportsAgentTasks).map(runLoad));
}

// 其它窗口改了语言时，本窗口也同步切换。
let unlistenSettings: UnlistenFn | null = null;
// 已开窗时的 tab 定位请求（settings-goto-tab）。
let unlistenGotoTab: UnlistenFn | null = null;
onBeforeUnmount(() => {
  unlistenSettings?.();
  unlistenGotoTab?.();
});

onMounted(async () => {
  const payload = await getSettings();
  config.value = payload.config;
  secretsPresent.value = payload.secretsPresent;
  applyTheme(payload.config.general.theme);
  applyLanguage(payload.config.general.language);
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: UiLanguage }>(
    "settings-updated",
    (e) => {
      if (e.payload.language) applyLanguage(e.payload.language);
      // 其它入口（弹窗导航栏/CLI）改主题时本窗口同步：切换外观 + 更新单选高亮。
      if (typeof e.payload.theme === "string") {
        applyTheme(e.payload.theme);
        if (config.value) config.value.general.theme = e.payload.theme;
      }
    }
  );
  // 窗口已开时的定位请求（托盘「渠道异常」行等）：新开窗走 URL ?tab=，已开窗走本事件。
  // payload 支持 `tab#elementId` 锚点后缀（跨窗口定位，spec gui-agent-task-launch G5）。
  unlistenGotoTab = await listen<string>("settings-goto-tab", (e) => {
    gotoTabTarget(e.payload);
  });
  // Local integration status is cheap after CLI probes were removed; start it in the
  // background so the tab-bar update badge can appear without blocking the window.
  void ctx.ensureIntegration();
  watch(activeTab, (tab) => {
    void ensureTabData(tab);
  });
  void ensureTabData(activeTab.value);
  // Initial URLs can include an element anchor (for example integration#lifecycle-claude).
  // Wait only for that tab's data before scrolling.
  const rawTab = new URLSearchParams(window.location.search).get("tab");
  if (rawTab?.includes("#")) {
    await ensureTabData(activeTab.value);
    gotoTabTarget(rawTab);
  }
});

/** 解析 `tab[#elementId]` 并切 tab + 可选滚动定位（settings-goto-tab / 初始 URL 共用）。 */
function gotoTabTarget(raw: string) {
  const [tab, target] = raw.split("#");
  if (!TABS.includes(tab as Tab)) return;
  activeTab.value = tab as Tab;
  void (async () => {
    await ensureTabData(tab as Tab);
    if (target) await ctx.gotoSettingsTarget(target);
  })();
}
</script>

<template>
  <div v-if="config" class="settings">
    <nav class="tabbar" data-tauri-drag-region>
      <template v-if="!searchActive">
      <button
        :data-tauri-drag-region="tabDrag"
        :class="{ active: activeTab === 'general' }"
        @mousedown="onTabDown"
        @click="onTabClick('general', $event)"
      >
        {{ t("settings.tabs.general") }}
      </button>
      <button
        :data-tauri-drag-region="tabDrag"
        :class="{ active: activeTab === 'integration' }"
        @mousedown="onTabDown"
        @click="onTabClick('integration', $event)"
      >
        {{ t("settings.tabs.integration")
        }}<span
          v-if="updateSummary.total > 0"
          class="tab-update-dot"
          :title="t('settings.integration.updatesAvailable')"
        ></span>
      </button>
      <button
        :data-tauri-drag-region="tabDrag"
        :class="{ active: activeTab === 'channel' }"
        @mousedown="onTabDown"
        @click="onTabClick('channel', $event)"
      >
        {{ t("settings.tabs.channel") }}
      </button>
      <button
        :data-tauri-drag-region="tabDrag"
        :class="{ active: activeTab === 'advanced' }"
        @mousedown="onTabDown"
        @click="onTabClick('advanced', $event)"
      >
        {{ t("settings.tabs.advanced") }}
      </button>
      <button
        v-if="config.experimental.enabled"
        :data-tauri-drag-region="tabDrag"
        :class="{ active: activeTab === 'experimental' }"
        @mousedown="onTabDown"
        @click="onTabClick('experimental', $event)"
      >
        {{ t("settings.tabs.experimental") }}
      </button>
      <!-- 设置搜索入口（R9）：右上角放大镜，点击进入搜索态（隐藏 tab、显示输入框） -->
      <button
        class="tab-search-toggle"
        type="button"
        :title="t('settings.search.placeholder')"
        @click="openSearch"
      >
        <!-- 用 circle/line 图元 + stroke 保证线宽均匀（手绘 path 会出现粗细不均） -->
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <circle
            cx="7"
            cy="7"
            r="4.25"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
          />
          <line
            x1="10.3"
            y1="10.3"
            x2="13.4"
            y2="13.4"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
          />
        </svg>
      </button>
      </template>
      <!-- 搜索态：输入框占据 tabbar，↑↓ 选择、回车跳转、Esc 退出 -->
      <div v-else class="tab-search" :class="{ mac: isMac }">
        <input
          :ref="(el) => (searchInputEl = el as HTMLInputElement | null)"
          class="tab-search-input"
          type="text"
          :placeholder="t('settings.search.placeholder')"
          v-model="searchQuery"
          @keydown="onSearchKeydown"
        />
        <button
          class="tab-search-close"
          type="button"
          :title="t('settings.search.close')"
          @click="closeSearch"
        >
          ✕
        </button>
        <div v-if="searchQuery.trim()" class="tab-search-results">
          <button
            v-for="(r, i) in searchResults"
            :key="`${r.tab}:${r.title}`"
            type="button"
            class="tab-search-item"
            :class="{ selected: i === searchSelected }"
            @mouseenter="searchSelected = i"
            @click="gotoSearchResult(r)"
          >
            <span class="tab-search-tab">{{ t(`settings.tabs.${r.tab}`) }}</span>
            <span class="tab-search-title">{{ r.title }}</span>
          </button>
          <p v-if="searchResults.length === 0" class="tab-search-empty">
            {{ t("settings.search.empty") }}
          </p>
        </div>
      </div>
    </nav>

    <div class="settings-body">
      <GeneralTab v-if="activeTab === 'general'" />
      <AdvancedTab v-else-if="activeTab === 'advanced'" />
      <ExperimentalTab v-else-if="activeTab === 'experimental'" />
      <IntegrationTab v-else-if="activeTab === 'integration'" />
      <ChannelsTab v-else />
    </div>

    <SettingsOverlays />
  </div>
</template>
