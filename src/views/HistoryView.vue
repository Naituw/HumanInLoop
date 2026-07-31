<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { applyTheme } from "../lib/theme";
import { applyLanguage } from "../i18n";
import {
  clearAllHistory,
  deleteHistoryEntries,
  getHistory,
  getHistoryProjects,
  historyInit,
  resolveHistorySessionTitles,
} from "../lib/ipc";
import type {
  HistoryEntry,
  HistoryOpenRequest,
  HistorySessionGroup,
  HistorySessionRef,
  ProjectInfo,
  ThemeMode,
} from "../lib/types";
import {
  ALL_HISTORY_SESSIONS,
  agentKindOf,
  groupHistorySessions,
  historySessionOf,
  historySessionToken,
  matchesHistorySession,
  shortSessionId,
  workspaceNameOf,
} from "../lib/history";
import HistoryDetail from "../components/HistoryDetail.vue";

const { t, locale } = useI18n();
const ALL = "__all__";

const currentProject = ref("");
const currentProjectName = ref("");
const projects = ref<ProjectInfo[]>([]);
const selected = ref<string>(ALL);
const selectedSession = ref(ALL_HISTORY_SESSIONS);
const entries = ref<HistoryEntry[]>([]);
const activeId = ref<string | null>(null);
const loading = ref(false);
const query = ref("");
const sessionTitles = ref<Record<string, string>>({});
const sessionTitleRequestedAt = new Map<string, number>();
const SESSION_TITLE_RETRY_MS = 30_000;

const confirmKind = ref<null | "scope" | "all">(null);
const pendingDeleteIds = ref<string[]>([]);
const pendingDeleteContext = ref<null | {
  project: string;
  session: string;
  keywords: string;
}>(null);
const deleteBusy = ref(false);
const deleteError = ref("");
const deleteNotice = ref("");
const menuOpen = ref(false);
const scopeMenuOpen = ref(false);
const scopeMenuProject = ref(ALL);
const pendingDeleteScopeKind = ref<null | "search" | "session" | "project">(null);
let queuedOpenRequest: HistoryOpenRequest | null = null;
let noticeTimer: ReturnType<typeof setTimeout> | null = null;

const keywords = computed(() =>
  query.value.trim().toLowerCase().split(/\s+/).filter(Boolean)
);

function agentLabel(kind: string): string {
  if (!kind) return "";
  const label = t(`agents.kind.${kind}`);
  return label === `agents.kind.${kind}` ? kind : label;
}

function agentLabelOf(e: HistoryEntry): string {
  return agentLabel(agentKindOf(e));
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function projectName(path: string): string {
  if (!path) return t("history.unknownProject");
  return projects.value.find((project) => project.key === path)?.name || fileName(path);
}

function channelName(id: string): string {
  const key = `history.channel.${id}`;
  const name = t(key);
  return name === key ? t("history.channel.unknown") : name;
}

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
  if (e.project) parts.push(e.project, workspaceNameOf(e));
  const kind = agentKindOf(e);
  if (kind) parts.push(kind, agentLabel(kind));
  const session = historySessionOf(e);
  const token = historySessionToken(session);
  if (session.type === "agent") parts.push(session.sessionId, session.agentKind);
  if (session.type === "mcp") parts.push(session.instanceId, session.project, "mcp");
  if (sessionTitles.value[token]) parts.push(sessionTitles.value[token]);
  if (e.source) parts.push(e.source);
  parts.push(e.channel, channelName(e.channel));
  return parts.join("\n").toLowerCase();
}

const projectEntries = computed(() =>
  selected.value === ALL
    ? entries.value
    : entries.value.filter((entry) => entry.project === selected.value)
);
const sessionGroups = computed(() => groupHistorySessions(projectEntries.value));

interface Opt {
  token: string;
  label: string;
  compactLabel: string;
  title?: string;
}

const projectOptions = computed<Opt[]>(() => {
  const opts: Opt[] = [
    {
      token: ALL,
      label: t("history.allProjects"),
      compactLabel: t("history.allProjects"),
    },
  ];
  let hasCurrent = false;
  for (const project of projects.value) {
    if (project.key === currentProject.value) hasCurrent = true;
    const name = project.key ? project.name : t("history.unknownProject");
    opts.push({
      token: project.key,
      label: `${name} (${project.count})`,
      compactLabel: name,
      title: project.key,
    });
  }
  if (!hasCurrent && currentProject.value) {
    opts.push({
      token: currentProject.value,
      label: `${currentProjectName.value || projectName(currentProject.value)} (0)`,
      compactLabel: currentProjectName.value || projectName(currentProject.value),
      title: currentProject.value,
    });
  }
  return opts;
});

function sessionOption(group: HistorySessionGroup): Opt {
  const count = group.count;
  const ref = group.ref;
  if (ref.type === "agent") {
    const title = sessionTitles.value[group.token];
    const id = shortSessionId(ref.sessionId);
    const compactLabel = title
      ? t("history.sessionAgentTitleCompact", {
          agent: agentLabel(ref.agentKind),
          title,
          id,
        })
      : t("history.sessionAgentCompact", { agent: agentLabel(ref.agentKind), id });
    return {
      token: group.token,
      label: title
        ? t("history.sessionAgentTitle", {
            agent: agentLabel(ref.agentKind),
            title,
            id,
            count,
          })
        : t("history.sessionAgent", { agent: agentLabel(ref.agentKind), id, count }),
      compactLabel,
      title: `${agentLabel(ref.agentKind)} · ${title ? `${title} · ` : ""}${ref.sessionId}`,
    };
  }
  if (ref.type === "mcp") {
    return {
      token: group.token,
      label: t("history.sessionMcp", { id: shortSessionId(ref.instanceId), count }),
      compactLabel: t("history.sessionMcpCompact", {
        id: shortSessionId(ref.instanceId),
      }),
      title: `${t("history.sessionMcpFull")} · ${ref.instanceId} · ${ref.project}`,
    };
  }
  return {
    token: group.token,
    label: t("history.sessionUnbound", { count }),
    compactLabel: t("history.sessionUnboundCompact"),
  };
}

const sessionOptions = computed<Opt[]>(() => [
  {
    token: ALL_HISTORY_SESSIONS,
    label: t("history.allSessions"),
    compactLabel: t("history.allSessions"),
  },
  ...sessionGroups.value.map(sessionOption),
]);

function entriesForProject(projectToken: string): HistoryEntry[] {
  return projectToken === ALL
    ? entries.value
    : entries.value.filter((entry) => entry.project === projectToken);
}

const scopeMenuSessionGroups = computed(() =>
  groupHistorySessions(entriesForProject(scopeMenuProject.value))
);
const scopeMenuSessionOptions = computed<Opt[]>(() => [
  {
    token: ALL_HISTORY_SESSIONS,
    label: t("history.allSessions"),
    compactLabel: t("history.allSessions"),
  },
  ...scopeMenuSessionGroups.value.map(sessionOption),
]);
const scopeMenuSpecificSessionOptions = computed(() =>
  scopeMenuSessionOptions.value.slice(1)
);
const scopeMenuAllSessionsLabel = computed(() =>
  scopeMenuProject.value === ALL
    ? t("history.allProjectsAllSessions")
    : t("history.thisProjectAllSessions")
);

const selectedProjectOption = computed(
  () =>
    projectOptions.value.find((option) => option.token === selected.value) ??
    projectOptions.value[0]
);
const selectedSessionOption = computed(
  () =>
    sessionOptions.value.find((option) => option.token === selectedSession.value) ??
    sessionOptions.value[0]
);
const scopeButtonLabel = computed(
  () => `${selectedProjectOption.value.compactLabel} · ${selectedSessionOption.value.compactLabel}`
);
const scopeButtonTooltip = computed(() =>
  [
    selectedProjectOption.value.title || selectedProjectOption.value.compactLabel,
    selectedSessionOption.value.title || selectedSessionOption.value.compactLabel,
  ].join(" · ")
);

const sessionEntries = computed(() =>
  projectEntries.value.filter((entry) =>
    matchesHistorySession(entry, selectedSession.value)
  )
);

const filteredEntries = computed(() => {
  const kws = keywords.value;
  if (!kws.length) return sessionEntries.value;
  return sessionEntries.value.filter((entry) => {
    const haystack = haystackOf(entry);
    return kws.every((keyword) => haystack.includes(keyword));
  });
});

const cleanupScopeKind = computed<"search" | "session" | "project" | "all">(
  () => {
    if (keywords.value.length) return "search";
    if (selectedSession.value !== ALL_HISTORY_SESSIONS) return "session";
    if (selected.value !== ALL) return "project";
    return "all";
  }
);
const cleanupScopeEntries = computed(() =>
  keywords.value.length ? filteredEntries.value : sessionEntries.value
);
const showScopeCleanup = computed(() => cleanupScopeKind.value !== "all");
const scopeCleanupLabel = computed(() => {
  const n = cleanupScopeEntries.value.length;
  switch (cleanupScopeKind.value) {
    case "search":
      return t("history.deleteSearchResults", { n });
    case "session":
      return t("history.clearSelectedSession", { n });
    case "project":
      return t("history.clearSelectedProject", { n });
    case "all":
      return t("history.clearAllCount", { n });
  }
});
const scopeConfirmTitle = computed(() => {
  switch (pendingDeleteScopeKind.value) {
    case "search":
      return t("history.confirmDeleteSearchTitle");
    case "session":
      return t("history.confirmClearSelectedSessionTitle");
    case "project":
      return t("history.confirmClearSelectedProjectTitle");
    default:
      return "";
  }
});
const scopeConfirmDesc = computed(() => {
  const n = pendingDeleteIds.value.length;
  switch (pendingDeleteScopeKind.value) {
    case "search":
      return t("history.confirmDeleteSearchDesc", { n });
    case "session":
      return t("history.confirmClearSelectedSessionDesc", { n });
    case "project":
      return t("history.confirmClearSelectedProjectDesc", { n });
    default:
      return "";
  }
});

const activeEntry = computed(
  () => filteredEntries.value.find((entry) => entry.id === activeId.value) ?? null
);
const activeSessionTitle = computed(() => {
  const entry = activeEntry.value;
  if (!entry) return "";
  return sessionTitles.value[historySessionToken(historySessionOf(entry))] ?? "";
});

function escapeHtml(value: string): string {
  return value.replace(
    /[&<>"]/g,
    (char) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[char]!
  );
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function highlightText(value: string): string {
  const escaped = escapeHtml(value);
  const kws = keywords.value;
  if (!kws.length) return escaped;
  const pattern = kws
    .map((keyword) => escapeRegExp(escapeHtml(keyword)))
    .sort((a, b) => b.length - a.length)
    .join("|");
  try {
    return escaped.replace(new RegExp(`(${pattern})`, "gi"), "<mark>$1</mark>");
  } catch {
    return escaped;
  }
}

function highlightedSummary(entry: HistoryEntry): string {
  return highlightText(summaryOf(entry) || t("history.noReply"));
}

function summaryOf(entry: HistoryEntry): string {
  const message = entry.message.text.trim();
  if (message) return firstLine(message);
  const question = entry.questions.find((item) => item.message.trim());
  return question ? firstLine(question.message) : "";
}

function firstLine(value: string): string {
  const line = value.split("\n").find((item) => item.trim()) ?? "";
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
  const date = new Date(ms);
  const yesterday = new Date(now - 86400000);
  if (
    date.getFullYear() === yesterday.getFullYear() &&
    date.getMonth() === yesterday.getMonth() &&
    date.getDate() === yesterday.getDate()
  ) {
    return t("history.time.yesterday");
  }
  try {
    return new Intl.DateTimeFormat(locale.value, { dateStyle: "short" }).format(date);
  } catch {
    return date.toLocaleDateString();
  }
}

watch(filteredEntries, (list) => {
  if (!list.some((entry) => entry.id === activeId.value)) {
    activeId.value = list.length ? list[0].id : null;
  }
});

watch(sessionGroups, (groups) => {
  if (
    selectedSession.value !== ALL_HISTORY_SESSIONS &&
    !groups.some((group) => group.token === selectedSession.value)
  ) {
    selectedSession.value = ALL_HISTORY_SESSIONS;
  }
});

watch(projectOptions, (options) => {
  if (!options.some((option) => option.token === selected.value)) {
    selected.value = ALL;
    selectedSession.value = ALL_HISTORY_SESSIONS;
  }
});

async function refreshSessionTitles(list: HistoryEntry[]) {
  const now = Date.now();
  const requests = groupHistorySessions(list)
    .filter(
      (group) =>
        group.ref.type === "agent" &&
        !sessionTitles.value[group.token] &&
        now - (sessionTitleRequestedAt.get(group.token) ?? 0) >= SESSION_TITLE_RETRY_MS
    )
    .map((group) => {
      const ref = group.ref as Extract<HistorySessionRef, { type: "agent" }>;
      return {
        token: group.token,
        agentKind: ref.agentKind,
        sessionId: ref.sessionId,
      };
    });
  if (!requests.length) return;
  for (const request of requests) sessionTitleRequestedAt.set(request.token, now);
  try {
    const results = await resolveHistorySessionTitles(requests);
    const next = { ...sessionTitles.value };
    for (const result of results) next[result.token] = result.title;
    sessionTitles.value = next;
  } catch {
    // Titles are a best-effort enhancement; identity and filtering remain available.
  }
}

let loadSeq = 0;
let openInFlight = 0;
let reloadAfterOpen = false;
function acceptEntries(list: HistoryEntry[]) {
  entries.value = list;
  if (!list.some((entry) => entry.id === activeId.value)) {
    activeId.value = filteredEntries.value[0]?.id ?? null;
  }
  void refreshSessionTitles(list);
}

async function reload() {
  const seq = ++loadSeq;
  loading.value = true;
  try {
    const list = await getHistory(null, true);
    if (seq !== loadSeq) return;
    acceptEntries(list);
  } finally {
    if (seq === loadSeq) loading.value = false;
  }
}

function toggleScopeMenu() {
  menuOpen.value = false;
  scopeMenuProject.value = selected.value;
  scopeMenuOpen.value = !scopeMenuOpen.value;
}

function selectScope(projectToken: string, sessionToken: string) {
  selected.value = projectToken;
  selectedSession.value = sessionToken;
  scopeMenuOpen.value = false;
  activeId.value = filteredEntries.value[0]?.id ?? null;
}

function toggleCleanupMenu() {
  scopeMenuOpen.value = false;
  menuOpen.value = !menuOpen.value;
}

let openSeq = 0;
async function applyOpenRequest(request: HistoryOpenRequest) {
  const seq = ++openSeq;
  openInFlight += 1;
  const requestProject = request.project ?? currentProject.value;
  if (request.project !== undefined && request.project !== null) {
    currentProject.value = request.project;
    currentProjectName.value = projectName(request.project);
  }
  selectedSession.value = ALL_HISTORY_SESSIONS;
  activeId.value = null;
  const load = ++loadSeq;
  loading.value = true;
  try {
    const allEntries = await getHistory(null, true);
    if (seq !== openSeq || load !== loadSeq) return;
    acceptEntries(allEntries);
    if (request.target) {
      const targetToken = historySessionToken(request.target);
      if (allEntries.some((entry) => matchesHistorySession(entry, targetToken))) {
        selected.value = ALL;
        selectedSession.value = targetToken;
        activeId.value = filteredEntries.value[0]?.id ?? null;
        return;
      }
    }

    selected.value = request.all ? ALL : requestProject;
    selectedSession.value = ALL_HISTORY_SESSIONS;
    activeId.value = filteredEntries.value[0]?.id ?? null;
  } finally {
    if (seq === openSeq && load === loadSeq) loading.value = false;
    openInFlight -= 1;
    if (openInFlight === 0 && reloadAfterOpen) {
      reloadAfterOpen = false;
      void refreshAfterHistoryUpdate();
    }
  }
}

async function refreshAfterHistoryUpdate() {
  const nextProjects = await getHistoryProjects();
  if (openInFlight > 0) {
    reloadAfterOpen = true;
    return;
  }
  projects.value = nextProjects;
  await reload();
}

async function handleOpenRequest(request: HistoryOpenRequest) {
  menuOpen.value = false;
  scopeMenuOpen.value = false;
  if (deleteBusy.value) {
    queuedOpenRequest = request;
    return;
  }
  confirmKind.value = null;
  pendingDeleteIds.value = [];
  pendingDeleteContext.value = null;
  pendingDeleteScopeKind.value = null;
  deleteError.value = "";
  query.value = "";
  await applyOpenRequest(request);
}

function askClear(kind: "scope" | "all") {
  menuOpen.value = false;
  scopeMenuOpen.value = false;
  deleteError.value = "";
  if (kind === "scope") {
    pendingDeleteIds.value = cleanupScopeEntries.value.map((entry) => entry.id);
    if (!pendingDeleteIds.value.length) return;
    const scopeKind = cleanupScopeKind.value;
    if (scopeKind === "all") {
      pendingDeleteIds.value = [];
      return;
    }
    pendingDeleteScopeKind.value = scopeKind;
    pendingDeleteContext.value = {
      project: selectedProjectOption.value.compactLabel,
      session: selectedSessionOption.value.compactLabel,
      keywords: query.value.trim() || t("history.noKeywords"),
    };
  } else {
    pendingDeleteIds.value = [];
    pendingDeleteContext.value = null;
    pendingDeleteScopeKind.value = null;
  }
  confirmKind.value = kind;
}

function closeConfirm() {
  if (deleteBusy.value) return;
  confirmKind.value = null;
  pendingDeleteIds.value = [];
  pendingDeleteContext.value = null;
  pendingDeleteScopeKind.value = null;
  deleteError.value = "";
}

function showDeleteNotice(count: number) {
  deleteNotice.value = t("history.deletedCount", { n: count });
  if (noticeTimer) clearTimeout(noticeTimer);
  noticeTimer = setTimeout(() => (deleteNotice.value = ""), 2500);
}

function onWindowPointerDown(event: PointerEvent) {
  const target = event.target;
  if (
    menuOpen.value &&
    (!(target instanceof Element) || !target.closest(".clear-wrap"))
  ) {
    menuOpen.value = false;
  }
  if (
    scopeMenuOpen.value &&
    (!(target instanceof Element) || !target.closest(".scope-wrap"))
  ) {
    scopeMenuOpen.value = false;
  }
}

function onWindowBlur() {
  menuOpen.value = false;
  scopeMenuOpen.value = false;
}

async function doClear() {
  const kind = confirmKind.value;
  if (!kind || deleteBusy.value) return;
  const ids = [...pendingDeleteIds.value];
  deleteBusy.value = true;
  deleteError.value = "";
  try {
    const count =
      kind === "all" ? await clearAllHistory() : await deleteHistoryEntries(ids);
    confirmKind.value = null;
    pendingDeleteIds.value = [];
    pendingDeleteContext.value = null;
    pendingDeleteScopeKind.value = null;
    showDeleteNotice(count);
    projects.value = await getHistoryProjects();
    await reload();
  } catch (error) {
    deleteError.value = t("history.deleteFailed", { error: String(error) });
  } finally {
    deleteBusy.value = false;
    const queued = queuedOpenRequest;
    queuedOpenRequest = null;
    if (queued) await handleOpenRequest(queued);
  }
}

let unlistenUpdated: UnlistenFn | null = null;
let unlistenSettings: UnlistenFn | null = null;
let unlistenOpenTarget: UnlistenFn | null = null;

onMounted(async () => {
  window.addEventListener("pointerdown", onWindowPointerDown);
  window.addEventListener("blur", onWindowBlur);
  const init = await historyInit();
  applyTheme(init.theme);
  applyLanguage(init.lang);
  projects.value = await getHistoryProjects();

  const params = new URLSearchParams(window.location.search);
  const urlProject = params.get("project");
  currentProject.value = urlProject ?? init.project;
  currentProjectName.value =
    params.get("projectName") ??
    (urlProject !== null ? projectName(urlProject) : init.projectName);

  unlistenUpdated = await listen("history-updated", () => {
    void refreshAfterHistoryUpdate();
  });
  unlistenOpenTarget = await listen<HistoryOpenRequest>(
    "history-open-target",
    (event) => void handleOpenRequest(event.payload)
  );
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (event) => {
      if (typeof event.payload.theme === "string") applyTheme(event.payload.theme);
      if (typeof event.payload.language === "string") applyLanguage(event.payload.language);
    }
  );

  let target: HistoryOpenRequest["target"] = null;
  const encodedTarget = params.get("historyTarget");
  if (encodedTarget) {
    try {
      target = JSON.parse(encodedTarget) as HistoryOpenRequest["target"];
    } catch {
      target = null;
    }
  }
  await handleOpenRequest({
    all: params.get("all") === "1",
    project: currentProject.value,
    target,
  });
});

onBeforeUnmount(() => {
  window.removeEventListener("pointerdown", onWindowPointerDown);
  window.removeEventListener("blur", onWindowBlur);
  unlistenUpdated?.();
  unlistenSettings?.();
  unlistenOpenTarget?.();
  if (noticeTimer) clearTimeout(noticeTimer);
});
</script>

<template>
  <div class="history">
    <header class="hist-header" data-tauri-drag-region>
      <span class="hist-title" data-tauri-drag-region>{{ t("history.title") }}</span>
      <div class="hist-tools">
        <span v-if="deleteNotice" class="hist-notice" role="status">{{ deleteNotice }}</span>
        <div class="scope-wrap">
          <button
            class="scope-btn"
            :class="{ 'session-scoped': selectedSession !== ALL_HISTORY_SESSIONS }"
            type="button"
            :title="scopeButtonTooltip"
            :aria-label="t('history.scopeFilter')"
            :aria-expanded="scopeMenuOpen"
            aria-haspopup="menu"
            @click="toggleScopeMenu"
          >
            <svg class="scope-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 5h18M6 12h12M10 19h4" /></svg>
            <span class="scope-btn-label">{{ scopeButtonLabel }}</span>
            <svg class="scope-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6" /></svg>
          </button>
          <div v-if="scopeMenuOpen" class="scope-menu" role="menu">
            <div class="scope-project-menu" :aria-label="t('history.projectFilter')">
              <button
                v-for="o in projectOptions"
                :key="o.token"
                type="button"
                role="menuitem"
                :class="{
                  active: scopeMenuProject === o.token,
                  selected: selected === o.token,
                }"
                :title="o.title"
                @pointerenter="scopeMenuProject = o.token"
                @focus="scopeMenuProject = o.token"
                @click="scopeMenuProject = o.token"
              >
                <span class="scope-project-label">{{ o.label }}</span>
                <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6" /></svg>
              </button>
            </div>
            <div class="scope-session-menu" :aria-label="t('history.sessionFilter')">
              <button
                type="button"
                role="menuitemradio"
                :aria-checked="
                  selected === scopeMenuProject &&
                  selectedSession === ALL_HISTORY_SESSIONS
                "
                :class="{
                  selected:
                    selected === scopeMenuProject &&
                    selectedSession === ALL_HISTORY_SESSIONS,
                }"
                @click="selectScope(scopeMenuProject, ALL_HISTORY_SESSIONS)"
              >
                <span class="scope-check">✓</span>
                <span>{{ scopeMenuAllSessionsLabel }}</span>
              </button>
              <div
                v-if="scopeMenuSpecificSessionOptions.length"
                class="scope-divider"
                role="separator"
              ></div>
              <button
                v-for="o in scopeMenuSpecificSessionOptions"
                :key="o.token"
                type="button"
                role="menuitemradio"
                :aria-checked="
                  selected === scopeMenuProject && selectedSession === o.token
                "
                :class="{
                  selected:
                    selected === scopeMenuProject && selectedSession === o.token,
                }"
                :title="o.title"
                @click="selectScope(scopeMenuProject, o.token)"
              >
                <span class="scope-check">✓</span>
                <span class="scope-session-label">{{ o.label }}</span>
              </button>
            </div>
          </div>
        </div>
        <div class="clear-wrap">
          <button class="clear-btn" type="button" @click="toggleCleanupMenu">
            {{ t("history.cleanup") }}
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6" /></svg>
          </button>
          <div v-if="menuOpen" class="clear-menu">
            <button
              v-if="showScopeCleanup"
              type="button"
              :disabled="cleanupScopeEntries.length === 0"
              @click="askClear('scope')"
            >
              {{ scopeCleanupLabel }}
            </button>
            <button type="button" @click="askClear('all')">
              {{ t("history.clearAllCount", { n: entries.length }) }}
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
      <div
        v-else-if="keywords.length || selectedSession !== ALL_HISTORY_SESSIONS"
        class="empty"
      >
        <p class="empty-title">{{ t("history.searchEmpty") }}</p>
        <p class="empty-hint">{{ t("history.searchEmptyHint") }}</p>
      </div>
      <div v-else class="empty">
        <p class="empty-title">{{ t("history.empty") }}</p>
        <p class="empty-hint">{{ t("history.emptyHint") }}</p>
      </div>

      <!-- Right detail -->
      <div class="detail-pane">
        <HistoryDetail
          v-if="activeEntry"
          :key="activeEntry.id"
          :entry="activeEntry"
          :session-title="activeSessionTitle"
        />
        <div v-else class="select-hint">{{ t("history.selectHint") }}</div>
      </div>
    </div>

    <!-- Destructive confirmation -->
    <div v-if="confirmKind" class="overlay" @click.self="closeConfirm">
      <div class="dialog">
        <h3>
          {{
            confirmKind === "all"
              ? t("history.confirmClearAllTitle")
              : scopeConfirmTitle
          }}
        </h3>
        <p>
          {{
            confirmKind === "all"
              ? t("history.confirmClearAllDesc")
              : scopeConfirmDesc
          }}
        </p>
        <dl
          v-if="confirmKind === 'scope' && pendingDeleteContext"
          class="delete-context"
        >
          <div>
            <dt>{{ t("history.deleteScopeProject") }}</dt>
            <dd>{{ pendingDeleteContext.project }}</dd>
          </div>
          <div>
            <dt>{{ t("history.deleteScopeSession") }}</dt>
            <dd>{{ pendingDeleteContext.session }}</dd>
          </div>
          <div>
            <dt>{{ t("history.deleteScopeKeywords") }}</dt>
            <dd>{{ pendingDeleteContext.keywords }}</dd>
          </div>
        </dl>
        <p v-if="deleteError" class="dialog-error" role="alert">{{ deleteError }}</p>
        <div class="dialog-actions">
          <button
            class="btn-ghost"
            type="button"
            :disabled="deleteBusy"
            @click="closeConfirm"
          >
            {{ t("history.confirmCancel") }}
          </button>
          <button
            class="btn-danger"
            type="button"
            :disabled="deleteBusy"
            @click="doClear"
          >
            {{
              deleteBusy
                ? t("history.deleting")
                : confirmKind === "all"
                  ? t("history.confirmOk")
                  : pendingDeleteScopeKind === "search"
                    ? t("history.deleteOk")
                    : t("history.confirmOk")
            }}
          </button>
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
  position: relative;
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
  min-width: 64px;
}
.hist-tools {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}
.hist-notice {
  position: absolute;
  z-index: 20;
  right: 14px;
  top: calc(100% + 6px);
  padding: 6px 9px;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--surface-overlay);
  color: var(--text-primary);
  font-size: 12px;
  white-space: nowrap;
  box-shadow: 0 8px 24px rgba(0, 0, 0, 0.16);
}
.scope-wrap {
  position: relative;
  min-width: 0;
}
.scope-btn {
  display: flex;
  align-items: center;
  gap: 6px;
  width: clamp(240px, 40vw, 420px);
  height: 30px;
  padding: 0 8px;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  cursor: pointer;
  box-shadow: var(--clickable-shadow);
}
.scope-btn.session-scoped {
  border-color: color-mix(in srgb, var(--accent) 66%, var(--border));
  background: color-mix(in srgb, var(--accent) 13%, var(--control-bg));
  font-weight: 600;
}
.scope-icon,
.scope-chevron {
  flex: 0 0 auto;
  width: 13px;
  height: 13px;
}
.scope-icon {
  color: var(--text-secondary);
}
.scope-chevron {
  margin-left: auto;
}
.scope-btn-label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.scope-menu {
  position: absolute;
  z-index: 30;
  top: calc(100% + 4px);
  right: 0;
  display: grid;
  grid-template-columns: minmax(170px, 0.8fr) minmax(260px, 1.35fr);
  width: min(560px, calc(100vw - 28px));
  max-height: min(420px, calc(100vh - 100px));
  overflow: hidden;
  border: var(--hairline) solid var(--border);
  border-radius: var(--radius-sm, 8px);
  background: var(--surface-overlay);
  box-shadow: 0 10px 30px rgba(0, 0, 0, 0.2);
}
.scope-project-menu,
.scope-session-menu {
  min-width: 0;
  overflow-y: auto;
  padding: 4px;
}
.scope-project-menu {
  border-right: var(--hairline) solid var(--border);
  background: color-mix(in srgb, var(--text-primary) 3%, var(--surface-overlay));
}
.scope-project-menu button,
.scope-session-menu button {
  display: flex;
  align-items: center;
  width: 100%;
  min-height: 32px;
  padding: 6px 8px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-primary);
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.scope-project-menu button.active,
.scope-project-menu button:hover,
.scope-session-menu button:hover {
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
}
.scope-project-menu button.selected {
  font-weight: 600;
}
.scope-project-menu button svg {
  flex: 0 0 auto;
  width: 13px;
  height: 13px;
  margin-left: auto;
  color: var(--text-secondary);
}
.scope-project-label,
.scope-session-label {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.scope-session-menu button.selected {
  background: color-mix(in srgb, var(--accent) 15%, transparent);
  font-weight: 600;
}
.scope-check {
  flex: 0 0 15px;
  width: 15px;
  visibility: hidden;
}
.scope-session-menu button.selected .scope-check {
  visibility: visible;
}
.scope-divider {
  height: var(--hairline);
  margin: 4px 6px;
  background: var(--border);
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
  min-width: 300px;
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
  width: 360px;
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
.delete-context {
  display: grid;
  gap: 6px;
  margin: -6px 0 18px;
  padding: 9px 10px;
  border-radius: var(--radius-sm, 8px);
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
  font-size: 11px;
}
.delete-context div {
  display: grid;
  grid-template-columns: auto minmax(0, 1fr);
  gap: 8px;
}
.delete-context dt {
  color: var(--text-secondary);
}
.delete-context dd {
  min-width: 0;
  margin: 0;
  overflow: hidden;
  text-align: right;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dialog .dialog-error {
  margin-top: -8px;
  color: #ff453a;
  word-break: break-word;
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
.btn-ghost:disabled,
.btn-danger:disabled {
  cursor: default;
  opacity: 0.55;
}
</style>
