<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { applyTheme } from "../lib/theme";
import { applyLanguage } from "../i18n";
import {
  clearHistory,
  getHistory,
  getHistoryProjects,
  historyInit,
} from "../lib/ipc";
import type { HistoryEntry, ProjectInfo, ThemeMode } from "../lib/types";
import { agentKindOf, workspaceNameOf } from "../lib/history";
import HistoryDetail from "../components/HistoryDetail.vue";

const { t, locale } = useI18n();

const ALL = "__all__";

const currentProject = ref("");
const currentProjectName = ref("");
const projects = ref<ProjectInfo[]>([]);
const selected = ref<string>(ALL); // ALL or a project key ("" = unknown project)
const entries = ref<HistoryEntry[]>([]);
const activeId = ref<string | null>(null);
const loading = ref(false);

// Keyword search: whitespace-split, AND-matched, case-insensitive.
const query = ref("");

// Clear confirmation: null | "current" | "all".
const confirmKind = ref<null | "current" | "all">(null);
const menuOpen = ref(false);

// Lowercased keywords (whitespace-split, empties dropped).
const keywords = computed(() =>
  query.value.trim().toLowerCase().split(/\s+/).filter(Boolean)
);

// Localized agent family label (falls back to the raw id for unknown families).
function agentLabelOf(e: HistoryEntry): string {
  const k = agentKindOf(e);
  if (!k) return "";
  const label = t(`agents.kind.${k}`);
  return label === `agents.kind.${k}` ? k : label;
}

// Build the searchable haystack for one entry: shared message + each question
// prompt + selected options + typed replies + attachment / reply file names +
// workspace (name + full path) + agent (family id + localized label) + caller
// source name + channel (id + localized name).
function haystackOf(e: HistoryEntry): string {
  const parts: string[] = [];
  if (e.message.text) parts.push(e.message.text);
  for (const f of e.message.files) parts.push(f.name);
  for (const q of e.questions) if (q.message) parts.push(q.message);
  for (const a of e.answers) {
    for (const s of a.selectedOptions) parts.push(s);
    if (a.userInput) parts.push(a.userInput);
    for (const img of a.images) parts.push(fileName(img));
    for (const f of a.files) parts.push(fileName(f));
  }
  if (e.project) {
    parts.push(e.project);
    parts.push(workspaceNameOf(e));
  }
  const kind = agentKindOf(e);
  if (kind) {
    parts.push(kind);
    parts.push(agentLabelOf(e));
  }
  if (e.source) parts.push(e.source);
  parts.push(e.channel);
  parts.push(channelName(e.channel));
  return parts.join("\n").toLowerCase();
}

// Entries matching every keyword (applied on top of the project filter).
const filteredEntries = computed(() => {
  const kws = keywords.value;
  if (!kws.length) return entries.value;
  return entries.value.filter((e) => {
    const hay = haystackOf(e);
    return kws.every((k) => hay.includes(k));
  });
});

const activeEntry = computed(
  () => filteredEntries.value.find((e) => e.id === activeId.value) ?? null
);

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function escapeHtml(s: string): string {
  return s.replace(
    /[&<>"]/g,
    (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!
  );
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Render text with matched keywords wrapped in <mark>. The text is
// HTML-escaped first; keywords are escaped the same way so matching stays in
// sync (and special chars can never break out of the markup).
function highlightText(text: string): string {
  const esc = escapeHtml(text);
  const kws = keywords.value;
  if (!kws.length) return esc;
  const pattern = kws
    .map((k) => escapeRegExp(escapeHtml(k)))
    .sort((a, b) => b.length - a.length)
    .join("|");
  try {
    return esc.replace(new RegExp(`(${pattern})`, "gi"), "<mark>$1</mark>");
  } catch {
    return esc;
  }
}

function highlightedSummary(e: HistoryEntry): string {
  return highlightText(summaryOf(e) || t("history.noReply"));
}

// Keep the selection valid as the filtered set changes (typing / reload).
watch(filteredEntries, (list) => {
  if (!list.some((e) => e.id === activeId.value)) {
    activeId.value = list.length ? list[0].id : null;
  }
});

interface Opt {
  token: string;
  label: string;
}

const projectOptions = computed<Opt[]>(() => {
  const opts: Opt[] = [{ token: ALL, label: t("history.allProjects") }];
  let hasCurrent = false;
  for (const p of projects.value) {
    if (p.key === currentProject.value) hasCurrent = true;
    const name = p.key ? p.name : t("history.unknownProject");
    opts.push({ token: p.key, label: `${name} (${p.count})` });
  }
  // Always offer the current project even if it has no history yet.
  if (!hasCurrent && currentProject.value) {
    opts.push({ token: currentProject.value, label: `${currentProjectName.value} (0)` });
  }
  return opts;
});

function channelName(id: string): string {
  const key = `history.channel.${id}`;
  const name = t(key);
  return name === key ? t("history.channel.unknown") : name;
}

function summaryOf(e: HistoryEntry): string {
  const msg = e.message.text.trim();
  if (msg) return firstLine(msg);
  const q = e.questions.find((x) => x.message.trim());
  return q ? firstLine(q.message) : "";
}

function firstLine(s: string): string {
  const line = s.split("\n").find((l) => l.trim()) ?? "";
  return line.replace(/^#+\s*/, "").trim();
}

function relativeTime(ms: number): string {
  const now = Date.now();
  const diff = Math.max(0, now - ms);
  const min = Math.floor(diff / 60000);
  if (min < 1) return t("history.time.justNow");
  if (min < 60) return t("history.time.minutesAgo", { n: min });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t("history.time.hoursAgo", { n: hr });
  const d = new Date(ms);
  const yd = new Date(now - 86400000);
  if (
    d.getFullYear() === yd.getFullYear() &&
    d.getMonth() === yd.getMonth() &&
    d.getDate() === yd.getDate()
  ) {
    return t("history.time.yesterday");
  }
  try {
    return new Intl.DateTimeFormat(locale.value, { dateStyle: "short" }).format(d);
  } catch {
    return d.toLocaleDateString();
  }
}

async function reload() {
  loading.value = true;
  try {
    const list =
      selected.value === ALL
        ? await getHistory(null, true)
        : await getHistory(selected.value, false);
    entries.value = list;
    // Preserve the entry the user is currently viewing; only fall back to the
    // first one when the previous selection no longer exists (or none was set).
    if (!list.some((e) => e.id === activeId.value)) {
      activeId.value = list.length ? list[0].id : null;
    }
  } finally {
    loading.value = false;
  }
}

async function onSelectProject(token: string) {
  selected.value = token;
  activeId.value = null;
  await reload();
}

function askClear(kind: "current" | "all") {
  menuOpen.value = false;
  confirmKind.value = kind;
}

async function doClear() {
  const kind = confirmKind.value;
  confirmKind.value = null;
  if (!kind) return;
  if (kind === "all") {
    await clearHistory(true, null);
  } else {
    const proj = selected.value === ALL ? currentProject.value : selected.value;
    await clearHistory(false, proj);
  }
  projects.value = await getHistoryProjects();
  await reload();
}

let unlistenUpdated: UnlistenFn | null = null;
let unlistenSettings: UnlistenFn | null = null;

onMounted(async () => {
  const init = await historyInit();
  applyTheme(init.theme);
  // 精确语言来自 history_init（main.ts 只做 auto 兜底，不再读配置）。
  applyLanguage(init.lang);
  projects.value = await getHistoryProjects();

  const params = new URLSearchParams(window.location.search);
  // When opened via the unified GUI host, the caller's project is carried in the URL
  // (the host process's own project is meaningless). Prefer it over historyInit()'s.
  const urlProject = params.get("project");
  if (urlProject !== null) {
    currentProject.value = urlProject;
    currentProjectName.value = params.get("projectName") ?? urlProject;
  } else {
    currentProject.value = init.project;
    currentProjectName.value = init.projectName;
  }

  // Default to the current project; `--history --all` opens with everything.
  selected.value = params.get("all") === "1" ? ALL : currentProject.value;
  await reload();

  // Live update: the backend watches history.jsonl and emits this when a new
  // reply (from any process) is recorded. reload() keeps the current selection.
  unlistenUpdated = await listen("history-updated", async () => {
    projects.value = await getHistoryProjects();
    await reload();
  });

  // 设置变更实时生效（主题/语言与设置窗口同宿主进程广播）。
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (e) => {
      if (typeof e.payload.theme === "string") applyTheme(e.payload.theme);
      if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
    }
  );
});

onBeforeUnmount(() => {
  unlistenUpdated?.();
  unlistenSettings?.();
});
</script>

<template>
  <div class="history">
    <header class="hist-header" data-tauri-drag-region>
      <span class="hist-title" data-tauri-drag-region>{{ t("history.title") }}</span>
      <div class="hist-tools">
        <select
          class="project-select"
          :value="selected"
          @change="onSelectProject(($event.target as HTMLSelectElement).value)"
        >
          <option v-for="o in projectOptions" :key="o.token" :value="o.token">
            {{ o.label }}
          </option>
        </select>
        <div class="clear-wrap">
          <button class="clear-btn" type="button" @click="menuOpen = !menuOpen">
            {{ t("history.clear") }}
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6" /></svg>
          </button>
          <div v-if="menuOpen" class="clear-menu">
            <button
              type="button"
              :disabled="selected === ALL && !currentProject"
              @click="askClear('current')"
            >
              {{ t("history.clearCurrent") }}
            </button>
            <button type="button" @click="askClear('all')">
              {{ t("history.clearAll") }}
            </button>
          </div>
        </div>
      </div>
    </header>

    <div class="hist-search">
      <svg class="search-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7" /><path d="m21 21-4.3-4.3" /></svg>
      <input
        v-model="query"
        type="search"
        class="search-input"
        :placeholder="t('history.searchPlaceholder')"
      />
      <button
        v-if="query"
        type="button"
        class="search-clear"
        :title="t('history.searchClear')"
        @click="query = ''"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18M6 6l12 12" /></svg>
      </button>
    </div>

    <div class="hist-body">
      <!-- Left list -->
      <ul v-if="filteredEntries.length" class="entry-list">
        <li
          v-for="e in filteredEntries"
          :key="e.id"
          class="entry"
          :class="{ active: e.id === activeId }"
          @click="activeId = e.id"
        >
          <div class="entry-top">
            <span class="badge" :class="e.action">{{ channelName(e.channel) }}</span>
            <span v-if="agentLabelOf(e)" class="agent-badge">{{ agentLabelOf(e) }}</span>
            <span class="entry-time">{{ relativeTime(e.timestampMs) }}</span>
          </div>
          <div class="entry-summary" v-html="highlightedSummary(e)"></div>
          <div
            v-if="workspaceNameOf(e)"
            class="entry-workspace"
            :title="e.project"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" /></svg>
            <span class="ws-name" v-html="highlightText(workspaceNameOf(e))"></span>
          </div>
        </li>
      </ul>
      <div v-else-if="keywords.length" class="empty">
        <p class="empty-title">{{ t("history.searchEmpty") }}</p>
        <p class="empty-hint">{{ t("history.searchEmptyHint") }}</p>
      </div>
      <div v-else class="empty">
        <p class="empty-title">{{ t("history.empty") }}</p>
        <p class="empty-hint">{{ t("history.emptyHint") }}</p>
      </div>

      <!-- Right detail -->
      <div class="detail-pane">
        <HistoryDetail v-if="activeEntry" :key="activeEntry.id" :entry="activeEntry" />
        <div v-else class="select-hint">{{ t("history.selectHint") }}</div>
      </div>
    </div>

    <!-- Clear confirmation -->
    <div v-if="confirmKind" class="overlay" @click.self="confirmKind = null">
      <div class="dialog">
        <h3>{{ confirmKind === "all" ? t("history.confirmClearAllTitle") : t("history.confirmClearCurrentTitle") }}</h3>
        <p>{{ confirmKind === "all" ? t("history.confirmClearAllDesc") : t("history.confirmClearCurrentDesc") }}</p>
        <div class="dialog-actions">
          <button class="btn-ghost" type="button" @click="confirmKind = null">{{ t("history.confirmCancel") }}</button>
          <button class="btn-danger" type="button" @click="doClear">{{ t("history.confirmOk") }}</button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.history {
  display: flex;
  flex-direction: column;
  height: 100%;
  color: var(--text-primary);
}
/* Header */
.hist-header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.macos .hist-header {
  padding-top: 30px;
}
.hist-title {
  font-size: 14px;
  font-weight: 600;
  flex: 1 1 auto;
}
.hist-tools {
  display: flex;
  align-items: center;
  gap: 8px;
}
.project-select {
  height: 30px;
  max-width: 240px;
  padding: 0 28px 0 10px;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  box-shadow: var(--clickable-shadow);
}
.clear-wrap {
  position: relative;
}
.clear-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 30px;
  padding: 0 10px;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  cursor: pointer;
  box-shadow: var(--clickable-shadow);
}
.clear-btn svg {
  width: 13px;
  height: 13px;
}
.clear-menu {
  position: absolute;
  right: 0;
  top: calc(100% + 4px);
  z-index: 10;
  min-width: 170px;
  display: flex;
  flex-direction: column;
  padding: 4px;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--surface-overlay);
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18);
}
.clear-menu button {
  text-align: left;
  padding: 8px 10px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-primary);
  font-size: 13px;
  cursor: pointer;
}
.clear-menu button:hover:not(:disabled) {
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
}
.clear-menu button:disabled {
  opacity: 0.4;
  cursor: default;
}
/* Search bar */
.hist-search {
  flex: 0 0 auto;
  position: relative;
  display: flex;
  align-items: center;
  padding: 8px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.search-icon {
  position: absolute;
  left: 24px;
  width: 14px;
  height: 14px;
  color: var(--text-secondary);
  pointer-events: none;
}
.search-input {
  flex: 1 1 auto;
  height: 30px;
  padding: 0 30px 0 32px;
  border: var(--hairline) solid var(--control-border);
  border-radius: var(--radius-sm, 8px);
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 13px;
  outline: none;
  box-shadow: var(--clickable-shadow);
}
.search-input:focus,
.search-input:focus-visible {
  box-shadow: var(--focus-ring), var(--clickable-shadow);
}
.search-input::-webkit-search-cancel-button {
  -webkit-appearance: none;
  appearance: none;
}
.search-clear {
  position: absolute;
  right: 20px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: color-mix(in srgb, var(--text-primary) 12%, transparent);
  color: var(--text-secondary);
  cursor: pointer;
}
.search-clear:hover {
  background: color-mix(in srgb, var(--text-primary) 20%, transparent);
}
.search-clear svg {
  width: 12px;
  height: 12px;
}
/* Body split */
.hist-body {
  flex: 1 1 auto;
  display: flex;
  min-height: 0;
}
.entry-list {
  flex: 0 0 264px;
  margin: 0;
  padding: 6px;
  list-style: none;
  overflow-y: auto;
  border-right: var(--hairline) solid var(--border);
}
.entry {
  padding: 9px 10px;
  border-radius: var(--radius-sm, 8px);
  cursor: pointer;
}
.entry:hover {
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
}
.entry.active {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
}
.entry-top {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 4px;
}
.badge {
  display: inline-flex;
  align-items: center;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 600;
  background: color-mix(in srgb, var(--accent) 16%, transparent);
  color: var(--accent);
}
.badge.cancel {
  background: color-mix(in srgb, #ff453a 16%, transparent);
  color: #ff453a;
}
.agent-badge {
  display: inline-flex;
  align-items: center;
  padding: 1px 7px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 600;
  max-width: 96px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  background: color-mix(in srgb, var(--text-primary) 9%, transparent);
  color: var(--text-secondary);
}
.entry-time {
  margin-left: auto;
  font-size: 11px;
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}
.entry-workspace {
  display: flex;
  align-items: center;
  gap: 4px;
  margin-top: 3px;
  font-size: 11px;
  color: var(--text-secondary);
  min-width: 0;
}
.entry-workspace svg {
  flex: 0 0 auto;
  width: 11px;
  height: 11px;
}
.entry-workspace .ws-name {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.entry-workspace :deep(mark) {
  padding: 0 1px;
  border-radius: 3px;
  background: color-mix(in srgb, var(--accent) 32%, transparent);
  color: inherit;
}
.entry-summary {
  font-size: 13px;
  color: var(--text-primary);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.entry-summary :deep(mark) {
  padding: 0 1px;
  border-radius: 3px;
  background: color-mix(in srgb, var(--accent) 32%, transparent);
  color: inherit;
}
/* Empty list */
.empty {
  flex: 0 0 264px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  padding: 24px;
  border-right: var(--hairline) solid var(--border);
  text-align: center;
}
.empty-title {
  font-size: 14px;
  font-weight: 600;
  margin: 0;
}
.empty-hint {
  font-size: 12px;
  color: var(--text-secondary);
  margin: 0;
}
/* Detail pane */
.detail-pane {
  flex: 1 1 auto;
  min-width: 0;
  overflow-y: auto;
}
.select-hint {
  display: flex;
  align-items: center;
  justify-content: center;
  height: 100%;
  color: var(--text-secondary);
  font-size: 13px;
}
/* Confirm dialog */
.overlay {
  position: fixed;
  inset: 0;
  z-index: 50;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.32);
}
.dialog {
  width: 320px;
  padding: 20px;
  border-radius: var(--radius, 12px);
  border: var(--hairline) solid var(--border);
  background: var(--surface-overlay);
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3);
}
.dialog h3 {
  margin: 0 0 8px;
  font-size: 15px;
}
.dialog p {
  margin: 0 0 18px;
  font-size: 13px;
  color: var(--text-secondary);
}
.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.btn-ghost,
.btn-danger {
  height: 32px;
  padding: 0 16px;
  border-radius: var(--radius-sm, 8px);
  font-size: 13px;
  cursor: pointer;
}
.btn-ghost {
  border: var(--hairline) solid var(--border);
  background: transparent;
  color: var(--text-primary);
}
.btn-danger {
  border: none;
  background: #ff453a;
  color: #fff;
}
</style>
