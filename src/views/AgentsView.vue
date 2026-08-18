<script setup lang="ts">
// Agent 控制台（spec gui-agent-console）：双栏布局——边栏（项目分组会话 + 最近项目 + ＋）
// + 详情区（Watch 帧 / 交互区插槽）。数据面：daemon 快照订阅（agents-updated）+ 焦点会话
// 详情帧（agent-detail，签名变化才推）。
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { applyTheme } from "../lib/theme";
import { applyLanguage } from "../i18n";
import {
  agentForceIdle,
  agentsFocus,
  agentsInit,
  agentsStartSubscription,
  focusAgentTerminal,
  focusRequest,
  interjectAppend,
  interjectClear,
  interjectPeek,
  newTaskProjects,
  openTodos,
  openForkTask,
  todosProjects,
} from "../lib/ipc";
import { isFocusableTerminal } from "../lib/terminals";
import type {
  AgentDetailFrame,
  AgentRecord,
  ImageAttachment,
  ThemeMode,
} from "../lib/types";
import Sidebar from "./console/Sidebar.vue";
import StatusInd from "./console/StatusInd.vue";
import WatchPane from "./console/WatchPane.vue";
import TranscriptPane from "./console/TranscriptPane.vue";
import DiffBar from "./console/DiffBar.vue";
import InteractSlot from "./console/InteractSlot.vue";
import NewTaskPane from "./console/NewTaskPane.vue";
import {
  anchor,
  basename,
  indState,
  stateWeight,
  type ConsoleFilter,
  type ProjectGroup,
  UNKNOWN_PROJECT_KEY,
} from "./console/model";

const { t } = useI18n();

// ===== 数据 =====
const agents = ref<AgentRecord[]>([]);
// 是否已收到首帧快照（在此之前显示 Loading，而非"暂无 Agent"，避免误导）。
const loaded = ref(false);
// 每秒重算一次相对时间与累计时长（与数据推送解耦）。
const nowMs = ref(Date.now());
const newTaskSupported = ref(false);
const submitBareEnter = ref(false);

// ===== 过滤（C6）=====
const FILTERS: ConsoleFilter[] = ["all", "working", "idle"];
const filter = ref<ConsoleFilter>("all");

function passFilter(a: AgentRecord): boolean {
  if (filter.value === "all") return true;
  if (filter.value === "working") return a.state === "working";
  return a.state === "idle";
}

// ===== 边栏分组（R3）=====
const groups = computed<ProjectGroup[]>(() => {
  const map = new Map<string, AgentRecord[]>();
  for (const a of agents.value) {
    if (!passFilter(a)) continue;
    const key = a.cwd || UNKNOWN_PROJECT_KEY;
    const arr = map.get(key) ?? [];
    arr.push(a);
    map.set(key, arr);
  }
  const result: ProjectGroup[] = [];
  for (const [key, items] of map) {
    items.sort((x, y) => {
      const w = stateWeight(x) - stateWeight(y);
      return w !== 0 ? w : anchor(y) - anchor(x);
    });
    result.push({
      key,
      label: key === UNKNOWN_PROJECT_KEY ? t("agents.unknownProject") : basename(key),
      path: key === UNKNOWN_PROJECT_KEY ? "" : key,
      items,
    });
  }
  // 组按组内最近活动倒序；未知项目组沉底。
  result.sort((x, y) => {
    const xu = x.key === UNKNOWN_PROJECT_KEY;
    const yu = y.key === UNKNOWN_PROJECT_KEY;
    if (xu !== yu) return xu ? 1 : -1;
    return anchor(y.items[0]) - anchor(x.items[0]);
  });
  return result;
});

// 「最近项目」（C5）：workspace 索引里不在会话分组中的项目。
const recentAll = ref<{ path: string; label: string }[]>([]);
const recent = computed(() => {
  const used = new Set(agents.value.map((a) => a.cwd).filter(Boolean) as string[]);
  return recentAll.value.filter((p) => !used.has(p.path)).slice(0, 8);
});

// 项目待办数徽标。
const todoCounts = ref<Record<string, number>>({});

async function loadSidebarExtras(): Promise<void> {
  try {
    const projects = await newTaskProjects();
    recentAll.value = projects
      .filter((p) => p.source === "workspace")
      .map((p) => ({ path: p.path, label: p.label }));
  } catch {
    recentAll.value = [];
  }
  try {
    const infos = await todosProjects();
    const counts: Record<string, number> = {};
    for (const info of infos) {
      if (info.count > 0) counts[info.key] = info.count;
    }
    todoCounts.value = counts;
  } catch {
    todoCounts.value = {};
  }
}

// ===== 选中与焦点订阅（C8）=====
const selectedId = ref<string | null>(null);
const sel = computed<AgentRecord | null>(
  () => agents.value.find((a) => a.sessionId === selectedId.value) ?? null
);
const frame = ref<AgentDetailFrame | null>(null);
// 收到当前帧的时刻（累计时长本地走秒的基准）。
const frameAtMs = ref(0);

/** 详情正文视图：最近动态（默认）/ 完整会话（C14）；切换会话自动回默认。 */
const viewMode = ref<"latest" | "transcript">("latest");

/** diff 状态条刷新信号（C16：帧含编辑步时前沿节流 2s 后递增）。 */
const editTick = ref(0);
let lastDiffBumpMs = 0;

function selectSession(id: string): void {
  paneMode.value = "detail";
  if (selectedId.value === id) return;
  selectedId.value = id;
  confirmIdle.value = false;
  viewMode.value = "latest";
}

watch(selectedId, (id) => {
  frame.value = null;
  agentsFocus(id).catch(() => {});
  void refreshPending();
});

/** 键盘 ↑↓ 导航的可见会话平铺序（分组渲染序）。 */
const flatVisible = computed<string[]>(() =>
  groups.value.flatMap((g) => g.items.map((a) => a.sessionId))
);

function navigate(delta: number): void {
  const list = flatVisible.value;
  if (list.length === 0) return;
  const idx = list.indexOf(selectedId.value ?? "");
  const next = idx < 0 ? 0 : Math.min(list.length - 1, Math.max(0, idx + delta));
  selectSession(list[next]);
}

function onKeydown(e: KeyboardEvent): void {
  const target = e.target as HTMLElement | null;
  if (target && (target.tagName === "TEXTAREA" || target.tagName === "INPUT")) return;
  if (e.key === "ArrowDown") {
    e.preventDefault();
    navigate(1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    navigate(-1);
  }
}

// ===== 详情头部 =====
function kindLabel(kind: string): string {
  return t(`agents.kind.${kind}`);
}

const detailState = computed(() => (sel.value ? indState(sel.value) : "ended"));

function stateLabel(): string {
  const s = detailState.value;
  if (s === "waiting") return t("console.waitingTitle");
  return t(`agents.state.${s === "ended" ? "ended" : s}`);
}

/** 累计有效工作时长：帧值（回退快照值）为基准，工作中时本地走秒（帧签名不含时长）。 */
const elapsedSecs = computed<number | null>(() => {
  const base = frame.value?.activeElapsedSecs ?? sel.value?.activeElapsedSecs ?? null;
  if (base === null || base === undefined) return null;
  const working = sel.value?.state === "working";
  if (!working || frameAtMs.value === 0) return base;
  return base + Math.max(0, Math.floor((nowMs.value - frameAtMs.value) / 1000));
});

function fmtDuration(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return t("console.durationHM", { h, m });
  if (m > 0) return t("console.durationM", { m });
  return t("console.durationS", { s: secs });
}

// macOS focus requires a live pid; Windows Terminal focus requires a registered launch UUID.
function canFocusTerminal(a: AgentRecord): boolean {
  return (
    (!!a.pid || !!a.launchId) &&
    a.state !== "ended" &&
    isFocusableTerminal(a.terminal)
  );
}

async function onFocusTerminal(a: AgentRecord): Promise<void> {
  if (!a.pid && !a.launchId) return;
  try {
    await focusAgentTerminal(a.pid, a.launchId);
  } catch (err) {
    console.warn("focus terminal failed", err);
  }
}

async function onOpenTodos(a: AgentRecord): Promise<void> {
  if (!a.cwd) return;
  await onOpenTodosPath(a.cwd);
}

async function onFork(a: AgentRecord): Promise<void> {
  if (!a.forkReady) return;
  try {
    await openForkTask(a.sessionId);
  } catch (err) {
    console.warn("open fork task failed", err);
  }
}

function forkParentLabel(a: AgentRecord): string {
  const parentId = a.forkedFromSessionId;
  if (!parentId) return "";
  const parent = agents.value.find((candidate) => candidate.sessionId === parentId);
  return parent?.seq ? `#${parent.seq}` : parentId.slice(0, 8);
}

/** 打开某项目的待办窗口（边栏项目头待办徽标 / 详情头待办按钮共用）。 */
async function onOpenTodosPath(path: string): Promise<void> {
  try {
    await openTodos(path);
  } catch (err) {
    console.warn("open todos failed", err);
  }
}

// 手动置空闲（行内二次确认）。
const confirmIdle = ref(false);

async function doForceIdle(a: AgentRecord): Promise<void> {
  confirmIdle.value = false;
  try {
    await agentForceIdle(a.sessionId);
  } catch (err) {
    console.warn("force idle failed", err);
  }
}

// 「去回答」（C7）：聚焦对应提问弹窗。
async function onGoAnswer(a: AgentRecord): Promise<void> {
  if (!a.waitingRequestId) return;
  try {
    await focusRequest(a.waitingRequestId);
  } catch (err) {
    console.warn("focus request failed", err);
  }
}

// ===== 插话（C3 追加语义）=====
const pendingText = ref("");
const pendingCount = ref(0);
const pendingAttachmentCount = ref(0);

async function refreshPending(): Promise<void> {
  const id = selectedId.value;
  if (!id) {
    pendingText.value = "";
    pendingCount.value = 0;
    pendingAttachmentCount.value = 0;
    return;
  }
  try {
    const pending = await interjectPeek(id);
    if (selectedId.value === id) {
      pendingText.value = pending.text;
      pendingCount.value = pending.entries;
      pendingAttachmentCount.value = pending.attachments.length;
    }
  } catch {
    /* daemon 不可达：保持现状 */
  }
}

// 快照里选中会话的 pendingInterject 变化 → 重新取气泡内容。
watch(
  () => sel.value?.pendingInterject ?? false,
  () => {
    void refreshPending();
  }
);

async function onSend(
  text: string,
  filePaths: string[],
  pastedImages: ImageAttachment[],
): Promise<void> {
  const a = sel.value;
  if (!a) return;
  try {
    await interjectAppend(a.sessionId, text, filePaths, pastedImages);
  } catch (err) {
    console.warn("interject append failed", err);
  }
  // daemon 广播快照会触发 refreshPending；这里再直接刷一次兜底（广播先于落队列极罕见）。
  window.setTimeout(() => void refreshPending(), 200);
}

async function onRevoke(): Promise<void> {
  const a = sel.value;
  if (!a) return;
  try {
    await interjectClear(a.sessionId);
  } catch (err) {
    console.warn("interject revoke failed", err);
  }
  window.setTimeout(() => void refreshPending(), 200);
}

// ===== 「＋」新建任务（C4：内嵌右栏，项目锁定）=====
/** 右栏面板：会话详情 / 内嵌新建任务表单。 */
const paneMode = ref<"detail" | "newtask">("detail");
const ntProject = ref("");
/** 启动成功后 best-effort 自动选中新会话：按 cwd + 启动时间（±180s 内最新）匹配。 */
let pendingLaunch: { path: string; sinceSecs: number } | null = null;

function onPlus(projectPath: string): void {
  ntProject.value = projectPath;
  paneMode.value = "newtask";
}

function onLaunched(): void {
  pendingLaunch = { path: ntProject.value, sinceSecs: Math.floor(Date.now() / 1000) };
  paneMode.value = "detail";
}

/** 快照更新时消费 pendingLaunch：命中即选中新会话并清除。 */
function trySelectLaunched(): void {
  const p = pendingLaunch;
  if (!p) return;
  const now = Math.floor(Date.now() / 1000);
  if (now - p.sinceSecs > 180) {
    pendingLaunch = null;
    return;
  }
  const candidates = agents.value.filter(
    (a) =>
      a.startedAt >= p.sinceSecs - 5 &&
      !!a.cwd &&
      (a.cwd === p.path || a.cwd.startsWith(`${p.path}/`))
  );
  if (candidates.length === 0) return;
  candidates.sort((x, y) => y.startedAt - x.startedAt);
  pendingLaunch = null;
  selectSession(candidates[0].sessionId);
}

// ===== 生命周期 =====
const isLoading = computed(() => !loaded.value);
const isEmpty = computed(
  () => agents.value.length === 0 && recent.value.length === 0
);

let unlistenAgents: UnlistenFn | null = null;
let unlistenDetail: UnlistenFn | null = null;
let unlistenSettings: UnlistenFn | null = null;
let unlistenGoto: UnlistenFn | null = null;
let ticker: number | undefined;
// URL 预选（open_agents(session) 寻址，R4）：首帧快照到达后生效。
let pendingSelect: string | null = null;

onMounted(async () => {
  pendingSelect = new URLSearchParams(window.location.search).get("session");

  // 先注册监听，再触发后端订阅：daemon 一连上就推首帧立即快照，监听必须先就绪才不丢帧。
  unlistenAgents = await listen<AgentRecord[]>("agents-updated", (e) => {
    agents.value = Array.isArray(e.payload) ? e.payload : [];
    loaded.value = true;
    trySelectLaunched();
    // 预选（URL / goto 事件）或默认选中最近活动的会话。
    if (pendingSelect && agents.value.some((a) => a.sessionId === pendingSelect)) {
      selectSession(pendingSelect);
      pendingSelect = null;
    } else if (!selectedId.value && agents.value.length > 0) {
      const sorted = [...agents.value].sort(
        (x, y) => stateWeight(x) - stateWeight(y) || anchor(y) - anchor(x)
      );
      selectSession(sorted[0].sessionId);
    }
  });
  unlistenDetail = await listen<AgentDetailFrame>("agent-detail", (e) => {
    if (e.payload?.sessionId === selectedId.value) {
      frame.value = e.payload;
      frameAtMs.value = Date.now();
      // C16：帧含「编辑/写入」步 → 触发 diff 状态条刷新（2s 前沿节流）。
      if (e.payload.steps?.some((s) => s.kind === "write")) {
        const now = Date.now();
        if (now - lastDiffBumpMs > 2000) {
          lastDiffBumpMs = now;
          editTick.value += 1;
        }
      }
    }
  });
  unlistenGoto = await listen<{ session?: string | null }>("agents-goto", (e) => {
    const session = e.payload?.session;
    if (!session) return;
    if (agents.value.some((a) => a.sessionId === session)) {
      selectSession(session);
    } else {
      pendingSelect = session;
    }
  });
  try {
    const init = await agentsInit();
    applyTheme(init.theme);
    applyLanguage(init.lang);
    newTaskSupported.value = init.newTaskSupported;
    submitBareEnter.value = init.popupSubmitKey === "enter";
  } catch {
    /* 读取失败：保持兜底外观 */
  }
  unlistenSettings = await listen<{ theme?: ThemeMode; language?: string }>(
    "settings-updated",
    (e) => {
      if (typeof e.payload.theme === "string") applyTheme(e.payload.theme);
      if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
    }
  );
  // 监听已就绪，启动到 daemon 的快照订阅。
  try {
    await agentsStartSubscription();
  } catch {
    /* 订阅启动失败：窗口停留在 Loading，由后端重连逻辑兜底 */
  }
  void loadSidebarExtras();
  window.addEventListener("keydown", onKeydown);
  ticker = window.setInterval(() => {
    nowMs.value = Date.now();
  }, 1000);
});

onBeforeUnmount(() => {
  unlistenAgents?.();
  unlistenDetail?.();
  unlistenSettings?.();
  unlistenGoto?.();
  window.removeEventListener("keydown", onKeydown);
  if (ticker) window.clearInterval(ticker);
  agentsFocus(null).catch(() => {});
});
</script>

<template>
  <div class="console">
    <header class="con-header" data-tauri-drag-region>
      <span class="con-title" data-tauri-drag-region>{{ t("agents.title") }}</span>
      <div v-if="!isLoading && !isEmpty" class="seg" role="tablist">
        <button
          v-for="f in FILTERS"
          :key="f"
          class="seg-btn"
          :class="{ active: filter === f }"
          role="tab"
          :aria-selected="filter === f"
          @click="filter = f"
        >
          {{ t(`console.filter.${f}`) }}
        </button>
      </div>
      <span class="spacer" data-tauri-drag-region />
    </header>

    <div v-if="isLoading" class="empty">
      <span class="spinner" />
      <p class="empty-hint">{{ t("agents.loading") }}</p>
    </div>

    <div v-else-if="isEmpty" class="empty">
      <p class="empty-title">{{ t("agents.empty") }}</p>
      <p class="empty-hint">{{ t("agents.emptyHint") }}</p>
    </div>

    <div v-else class="con-body">
      <Sidebar
        :groups="groups"
        :recent="recent"
        :selected-id="selectedId"
        :new-task-supported="newTaskSupported"
        :todo-counts="todoCounts"
        :now-ms="nowMs"
        @select="selectSession"
        @plus="onPlus"
        @todos="onOpenTodosPath"
      />

      <main class="detail">
        <NewTaskPane
          v-if="paneMode === 'newtask'"
          :project="ntProject"
          @launched="onLaunched"
          @close="paneMode = 'detail'"
        />
        <template v-else-if="sel">
          <div class="dt-head">
            <div class="dt-title-row">
              <span class="kind-badge">{{ kindLabel(sel.kind) }}</span>
              <span class="dt-title">{{ sel.title || t("agents.untitled") }}</span>
              <span class="spacer" />
              <button
                v-if="sel.forkReady"
                class="icon-btn"
                :title="t('agents.fork')"
                :aria-label="t('agents.fork')"
                @click="onFork(sel)"
              >
                <svg viewBox="0 0 16 16"><path d="M4 3 V6.2 C4 8 5.5 9 7.2 9 H9.5 M8 4 L11 7 L8 10 M4 6.5 V13" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/></svg>
              </button>
              <button
                v-if="canFocusTerminal(sel)"
                class="icon-btn"
                :title="t('agents.focusTerminal')"
                :aria-label="t('agents.focusTerminal')"
                @click="onFocusTerminal(sel)"
              >
                <svg viewBox="0 0 16 16"><rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M4 6 L6.5 8 L4 10" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 10.2 H11.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
              <button
                v-if="sel.cwd"
                class="icon-btn"
                :title="t('agents.openTodos')"
                :aria-label="t('agents.openTodos')"
                @click="onOpenTodos(sel)"
              >
                <svg viewBox="0 0 16 16"><rect x="2" y="2.5" width="12" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M4.6 6 L6 7.4 L8.2 5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/><path d="M9.8 6.6 H11.6 M4.8 10.4 H11.6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
              <button
                v-if="sel.state === 'working'"
                class="icon-btn warn"
                :title="t('agents.markIdle')"
                :aria-label="t('agents.markIdle')"
                @click="confirmIdle = true"
              >
                <svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M6 5.6 V10.4 M10 5.6 V10.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
            </div>
            <div class="dt-meta">
              <StatusInd v-if="detailState !== 'ended'" :state="detailState" />
              <span class="dt-state" :class="detailState">{{ stateLabel() }}</span>
              <span v-if="elapsedSecs !== null && elapsedSecs >= 60" class="dt-elapsed">
                · {{ t("console.elapsed", { t: fmtDuration(elapsedSecs) }) }}
              </span>
              <span v-if="sel.forkedFromSessionId" class="dt-elapsed">
                · {{ t("agents.forkedFrom", { id: forkParentLabel(sel) }) }}
              </span>
              <span class="spacer" />
              <span v-if="sel.cwd" class="dt-path" :title="sel.cwd">{{ sel.cwd }}</span>
            </div>
            <div v-if="confirmIdle && sel.state === 'working'" class="idle-confirm">
              <span>{{ t("agents.markIdleConfirm") }}</span>
              <span class="spacer" />
              <button class="ic-btn" @click="confirmIdle = false">
                {{ t("agents.confirmCancel") }}
              </button>
              <button class="ic-btn ic-ok" @click="doForceIdle(sel)">
                {{ t("agents.confirmOk") }}
              </button>
            </div>
          </div>

          <!-- 等待回答横幅（C7）：两种正文视图下都常驻。 -->
          <div v-if="sel.waitingRequestId" class="wait-banner">
            <span class="wait-icon">🙋</span>
            <span class="wait-main">
              <span class="wait-title">{{ t("console.waitingTitle") }}</span>
              <span v-if="sel.waitingPreview" class="wait-q">{{ sel.waitingPreview }}</span>
            </span>
            <button class="btn sm primary" @click="onGoAnswer(sel)">
              {{ t("console.goAnswer") }}
            </button>
          </div>

          <div v-if="viewMode === 'latest'" class="dt-body">
            <WatchPane :frame="frame">
              <template #heading-actions>
                <button class="tx-open" @click="viewMode = 'transcript'">
                  <svg viewBox="0 0 14 14"><path d="M2.5 3.5 H11.5 M2.5 7 H11.5 M2.5 10.5 H8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
                  {{ t("console.tx.open") }}
                </button>
              </template>
            </WatchPane>
          </div>
          <TranscriptPane
            v-else
            :kind="sel.kind"
            :session-id="sel.sessionId"
            @close="viewMode = 'latest'"
          />

          <DiffBar v-if="sel.cwd" :project="sel.cwd" :edit-tick="editTick" />

          <InteractSlot
            :record="sel"
            :submit-bare-enter="submitBareEnter"
            :pending-text="pendingText"
            :pending-count="pendingCount"
            :pending-attachment-count="pendingAttachmentCount"
            :new-task-supported="newTaskSupported"
            @send="onSend"
            @revoke="onRevoke"
            @new-task="onPlus"
          />
        </template>

        <div v-else class="dt-empty">
          <p>{{ t("console.selectHint") }}</p>
        </div>
      </main>
    </div>
  </div>
</template>

<style scoped>
.console {
  display: flex;
  flex-direction: column;
  height: 100%;
  color: var(--text-primary);
}
.spacer {
  flex: 1 1 auto;
}
.con-header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.macos .con-header {
  padding-top: 30px;
}
.con-title {
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
}
.seg {
  display: inline-flex;
  padding: 2px;
  border-radius: 8px;
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
}
.seg-btn {
  appearance: none;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 12px;
  font-weight: 500;
  padding: 3px 12px;
  border-radius: 6px;
  cursor: pointer;
  white-space: nowrap;
}
.seg-btn:hover {
  color: var(--text-primary);
}
.seg-btn.active {
  background: var(--bg-elevated);
  color: var(--text-primary);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
}
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  flex: 1 1 auto;
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
  max-width: 320px;
}
.spinner {
  width: 20px;
  height: 20px;
  border-radius: 50%;
  border: 2px solid color-mix(in srgb, var(--text-primary) 18%, transparent);
  border-top-color: var(--text-secondary);
  animation: ag-spin 0.7s linear infinite;
}
@keyframes ag-spin {
  to {
    transform: rotate(360deg);
  }
}
.con-body {
  flex: 1 1 auto;
  display: flex;
  min-height: 0;
}
.detail {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.dt-head {
  flex: 0 0 auto;
  padding: 12px 16px 10px;
  border-bottom: var(--hairline) solid var(--border);
}
.dt-title-row {
  display: flex;
  align-items: center;
  gap: 8px;
}
.kind-badge {
  flex: 0 0 auto;
  padding: 1px 8px;
  border-radius: 5px;
  font-size: 10.5px;
  font-weight: 600;
  background: color-mix(in srgb, var(--text-primary) 9%, transparent);
  color: var(--text-secondary);
  white-space: nowrap;
}
.dt-title {
  min-width: 0;
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.icon-btn {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 24px;
  height: 24px;
  padding: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
}
.icon-btn:hover {
  background: color-mix(in srgb, var(--text-primary) 10%, transparent);
  color: var(--text-primary);
}
.icon-btn.warn:hover {
  background: color-mix(in srgb, #ff9f0a 18%, transparent);
  color: #c77700;
}
.icon-btn svg {
  width: 15px;
  height: 15px;
}
.dt-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 7px;
  font-size: 12px;
}
.dt-state {
  font-weight: 600;
}
.dt-state.working {
  color: var(--ind-working);
}
.dt-state.waiting {
  color: var(--accent);
}
.dt-state.idle {
  color: var(--text-secondary);
}
.dt-state.ended {
  color: var(--text-secondary);
}
.dt-elapsed {
  color: var(--text-secondary);
}
.dt-path {
  font-size: 11px;
  color: var(--text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 46%;
}
.idle-confirm {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 8px;
  padding: 6px 10px;
  border-radius: 7px;
  font-size: 12px;
  background: color-mix(in srgb, #ff9f0a 12%, transparent);
}
.ic-btn {
  appearance: none;
  border: var(--hairline) solid var(--control-border);
  background: var(--bg-elevated);
  color: var(--text-primary);
  font-size: 11px;
  font-weight: 600;
  padding: 3px 10px;
  border-radius: 6px;
  cursor: pointer;
}
.ic-btn:hover {
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
}
.ic-ok {
  border-color: transparent;
  background: #ff9f0a;
  color: #fff;
}
.ic-ok:hover {
  background: #f59300;
}
.dt-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 16px;
}
.wait-banner {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  margin: 12px 16px 0;
  border-radius: 9px;
  background: color-mix(in srgb, var(--accent) 10%, transparent);
}
.tx-open {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 11.5px;
  font-weight: 600;
  padding: 2px 8px;
  border-radius: 6px;
  cursor: pointer;
}
.tx-open:hover {
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
  color: var(--text-primary);
}
.tx-open svg {
  width: 12px;
  height: 12px;
}
.wait-icon {
  font-size: 17px;
}
.wait-main {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.wait-title {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--accent);
}
.wait-q {
  font-size: 12px;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.btn {
  appearance: none;
  border: var(--hairline) solid var(--control-border);
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  color: var(--text-primary);
  font-size: 12.5px;
  font-weight: 600;
  padding: 5px 14px;
  border-radius: 7px;
  cursor: pointer;
}
.btn.sm {
  font-size: 11.5px;
  padding: 3px 10px;
}
.btn.primary {
  border-color: transparent;
  background: var(--accent);
  color: #fff;
}
.btn.primary:hover {
  background: #0071e3;
}
.dt-empty {
  flex: 1 1 auto;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-tertiary);
  font-size: 13px;
}
</style>
