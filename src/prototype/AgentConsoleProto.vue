<script setup lang="ts">
// Agent 控制台原型（spec gui-agent-console C1–C12 的可点击 Demo）：
// 双栏布局 + mock 数据 + 活会话模拟。纯浏览器运行，不依赖 Tauri / daemon。
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { renderMarkdown } from "../lib/markdown";
import {
  initialSessions,
  KIND_LABEL,
  LIVE_SCRIPT,
  LIVE_SESSION_ID,
  makeTranscript,
  richAskEvent,
  PROJECTS,
  PROJECT_DIFFS,
  PROJECT_TODOS,
  READINESS,
  RECENT_PROJECTS,
  TX_COUNTS,
  type DiffFileM,
  type Kind,
  type SessionM,
  type TxEvent,
} from "./mock";

// ===== 主题（demo 控件）=====
const theme = ref<"light" | "dark">(
  window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"
);
function applyThemeClass(): void {
  const el = document.documentElement;
  el.classList.toggle("theme-dark", theme.value === "dark");
  el.classList.toggle("theme-light", theme.value === "light");
}
function toggleTheme(): void {
  theme.value = theme.value === "dark" ? "light" : "dark";
  applyThemeClass();
}

// ===== 数据与模拟 =====
const sessions = ref<SessionM[]>(initialSessions());
const nowMs = ref(Date.now());
let scriptIdx = 0;
let tickTimer: number | undefined;
let liveTimer: number | undefined;
let matrixTimer: number | undefined;

// 工作中指示器：移植 Cursor 的 ui-ascii-loading-indicator（sine_3x3）——
// 3×3 点阵、8 帧位掩码、175ms/帧循环；全部工作中行共用同一帧（同步动画）。
const MATRIX_FRAMES = [189, 220, 90, 78, 45, 291, 306, 433];
const MATRIX_FRAME_MS = 175;
/** 9 个点的圆心坐标（12×12 视图，行优先，bit i = 第 i 格）。 */
const MATRIX_CELLS = Array.from({ length: 9 }, (_, i) => ({
  x: 2 + (i % 3) * 4,
  y: 2 + Math.floor(i / 3) * 4,
}));
const matrixFrame = ref(0);

/** 空闲的静态帧：中间一行 3 个点 + 第三点上方 1 个点（共 4 点）——同一套点阵
 *  停住并褪灰，传达「之前在动、现在歇着」；比纯「⋯」更不像「更多」按钮。 */
const IDLE_MASK = 0b000111100;

function matrixOn(s: SessionM, cell: number): boolean {
  const mask = s.state === "working" ? MATRIX_FRAMES[matrixFrame.value] : IDLE_MASK;
  return (mask & (1 << cell)) !== 0;
}

function nowSecs(): number {
  return Math.floor(nowMs.value / 1000);
}

function advanceLive(): void {
  const s = sessions.value.find((x) => x.id === LIVE_SESSION_ID);
  if (!s || s.state !== "working") return;
  const f = LIVE_SCRIPT[scriptIdx % LIVE_SCRIPT.length];
  scriptIdx += 1;
  if (f.text !== undefined) s.text = f.text;
  if (f.steps !== undefined) s.steps = f.steps.map((x) => ({ ...x }));
  if (f.stepsOmitted !== undefined) s.stepsOmitted = f.stepsOmitted;
  if (f.todos !== undefined) s.todos = f.todos.map((x) => ({ ...x }));
  s.lastActivity = Math.floor(Date.now() / 1000);
}

// ===== 过滤 =====
type Filter = "all" | "working" | "idle";
const FILTERS: Filter[] = ["all", "working", "idle"];
const FILTER_LABEL: Record<Filter, string> = {
  all: "全部",
  working: "工作中",
  idle: "空闲",
};
const filter = ref<Filter>("all");

function passFilter(s: SessionM): boolean {
  if (filter.value === "all") return true;
  if (filter.value === "working") return s.state === "working";
  return s.state === "idle";
}

// ===== 边栏分组（R3：通用 key 分组）=====
interface Group {
  key: string;
  label: string;
  path: string;
  todoCount: number;
  items: SessionM[];
}

function stateWeight(s: SessionM): number {
  if (s.state === "working") return s.waiting ? 1 : 0;
  return s.state === "idle" ? 2 : 3;
}

const groups = computed<Group[]>(() => {
  const gs: Group[] = PROJECTS.map((p) => ({
    key: p.path,
    label: p.name,
    path: p.path,
    todoCount: PROJECT_TODOS[p.path]?.length ?? 0,
    items: sessions.value
      .filter((s) => s.projectPath === p.path && passFilter(s))
      .sort((a, b) => {
        const w = stateWeight(a) - stateWeight(b);
        return w !== 0 ? w : b.lastActivity - a.lastActivity;
      }),
  }));
  // 组按组内最近活动倒序；无会话的组沉底（过滤后可能为空）。
  return gs.sort((a, b) => {
    const am = a.items[0]?.lastActivity ?? 0;
    const bm = b.items[0]?.lastActivity ?? 0;
    return bm - am;
  });
});

function workingCount(g: Group): number {
  return g.items.filter((s) => s.state === "working").length;
}

const collapsed = ref<Set<string>>(new Set());
function toggleCollapse(key: string): void {
  const next = new Set(collapsed.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  collapsed.value = next;
}

/** 组收起时仍显示工作中的会话（含等待回答）——只收起安静的。 */
function visibleItems(g: Group): SessionM[] {
  if (!collapsed.value.has(g.key)) return g.items;
  return g.items.filter((s) => s.state === "working");
}

const recentOpen = ref(true);

// ===== 选中与详情 =====
const selectedId = ref<string | null>("s-cursor-refactor");
const sel = computed<SessionM | null>(
  () => sessions.value.find((s) => s.id === selectedId.value) ?? null
);

type Mode = "detail" | "newtask";
const mode = ref<Mode>("detail");

function selectSession(id: string): void {
  selectedId.value = id;
  mode.value = "detail";
  confirmIdle.value = false;
  viewMode.value = "latest";
  diffOpen.value = false;
  diffFileOpen.value = null;
  confirmStageAll.value = false;
}

/** 键盘 ↑↓ 导航用的可见会话平铺序。 */
const flatVisible = computed<string[]>(() =>
  groups.value.flatMap((g) => visibleItems(g).map((s) => s.id))
);

function navigate(delta: number): void {
  const list = flatVisible.value;
  if (list.length === 0) return;
  const idx = list.indexOf(selectedId.value ?? "");
  const next = idx < 0 ? 0 : Math.min(list.length - 1, Math.max(0, idx + delta));
  selectSession(list[next]);
}

function onKeydown(e: KeyboardEvent): void {
  const t = e.target as HTMLElement | null;
  if (t && (t.tagName === "TEXTAREA" || t.tagName === "INPUT")) return;
  if (e.key === "ArrowDown") {
    e.preventDefault();
    navigate(1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    navigate(-1);
  }
}

// ===== 完整会话（transcript 分页加载）=====
// 默认「最近动态」；切到「完整会话」先加载最新一页（200 条），顶部按钮向上补更早的页，
// 滚动位置锚定在原内容处。真实实现对应 daemon 侧 transcript_full 的分页游标。
const TX_PAGE = 200;
const txCache = new Map<string, TxEvent[]>();
const viewMode = ref<"latest" | "transcript">("latest");
const txLoaded = ref(0);
const dtBody = ref<HTMLElement | null>(null);

function txAll(s: SessionM): TxEvent[] {
  let ev = txCache.get(s.id);
  if (!ev) {
    ev = makeTranscript(s.id, TX_COUNTS[s.id] ?? 36, s.lastActivity);
    if (s.id === LIVE_SESSION_ID) {
      // 演示：末尾追加一条「多问题 + 长 message」的 AskHuman 问答。
      ev.push(richAskEvent(s.lastActivity));
    }
    txCache.set(s.id, ev);
  }
  return ev;
}

// 长 message 折叠：默认 5 行截断，按事件的绝对序号记录展开态。
const txExpanded = ref<Set<number>>(new Set());

function isLongAsk(e: TxEvent): boolean {
  return (e.question?.length ?? 0) > 220;
}

function toggleAskExpand(key: number): void {
  const next = new Set(txExpanded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  txExpanded.value = next;
}

const txTotal = computed(() => (sel.value ? txAll(sel.value).length : 0));
const txEvents = computed<TxEvent[]>(() => {
  const s = sel.value;
  if (!s || viewMode.value !== "transcript") return [];
  const all = txAll(s);
  return all.slice(Math.max(0, all.length - txLoaded.value));
});
const txBase = computed(() => txTotal.value - txEvents.value.length);
const txRemaining = computed(() => Math.max(0, txTotal.value - txLoaded.value));
const txNextPage = computed(() => Math.min(TX_PAGE, txRemaining.value));

async function openTranscript(): Promise<void> {
  if (!sel.value) return;
  txLoaded.value = Math.min(TX_PAGE, txAll(sel.value).length);
  viewMode.value = "transcript";
  await nextTick();
  const el = dtBody.value;
  if (el) el.scrollTop = el.scrollHeight; // 进入即定位到最新
}

function closeTranscript(): void {
  viewMode.value = "latest";
}

async function loadOlder(): Promise<void> {
  const el = dtBody.value;
  const prevHeight = el?.scrollHeight ?? 0;
  const prevTop = el?.scrollTop ?? 0;
  txLoaded.value = Math.min(txTotal.value, txLoaded.value + TX_PAGE);
  await nextTick();
  // 锚定：新内容插入顶部后，视野仍停在原来看到的位置。
  if (el) el.scrollTop = el.scrollHeight - prevHeight + prevTop;
}

// ===== 项目 diff 状态条（输入框上方；真实实现 = gitutil::build_diff_model）=====
const diffOpen = ref(false);
const diffFileOpen = ref<string | null>(null);
const confirmStageAll = ref(false);
/** mock 对象就地修改后手动触发响应式。 */
const diffVersion = ref(0);

const unstagedFiles = computed<DiffFileM[]>(() => {
  void diffVersion.value;
  const s = sel.value;
  if (!s) return [];
  return (PROJECT_DIFFS[s.projectPath] ?? []).filter((f) => !f.staged);
});

const diffTotals = computed(() => {
  let adds = 0;
  let dels = 0;
  let added = 0;
  for (const f of unstagedFiles.value) {
    adds += f.adds;
    dels += f.dels;
    if (f.kind === "A") added += 1;
  }
  return { files: unstagedFiles.value.length, adds, dels, added };
});

function stageFile(f: DiffFileM): void {
  f.staged = true;
  diffVersion.value += 1;
  showToast(`已暂存 ${f.path.split("/").pop()}（模拟）`);
}

function stageAll(): void {
  confirmStageAll.value = false;
  for (const f of unstagedFiles.value) f.staged = true;
  diffVersion.value += 1;
  diffOpen.value = false;
  showToast("已暂存全部变更（模拟）");
}

// ===== 详情区展示 =====
const bodyHtml = computed(() => {
  const text = sel.value?.text;
  return text ? renderMarkdown(text) : "";
});

const todosOpen = ref(true);

function todoSummary(s: SessionM): string {
  const done = s.todos.filter((t) => t.state === "completed").length;
  const cur = s.todos.find((t) => t.state === "inProgress");
  const head = `TODO ${done}/${s.todos.length}`;
  return cur ? `${head} · 当前：${cur.content}` : head;
}

function stateLabel(s: SessionM): string {
  if (s.state === "ended") return "已结束";
  if (s.waiting) return "正在等待你的回答";
  return s.state === "working" ? "工作中" : "空闲";
}

/** 统一状态指示器：工作中=3×3 点阵动画 / 等待=🙋 / 空闲=月亮 / 已结束=无（只留灰字）。 */
function indClass(s: SessionM): string {
  if (s.state === "ended") return "ended";
  if (s.waiting) return "waiting";
  return s.state;
}

function fmtDuration(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分钟`;
  return `${secs} 秒`;
}

function relativeTime(secs?: number | null): string {
  if (!secs) return "";
  const diff = Math.max(0, nowSecs() - secs);
  if (diff < 5) return "刚刚";
  if (diff < 60) return `${diff} 秒前`;
  const min = Math.floor(diff / 60);
  if (min < 60) return `${min} 分钟前`;
  const hr = Math.floor(min / 60);
  if (hr < 24) return `${hr} 小时前`;
  return `${Math.floor(hr / 24)} 天前`;
}

function clockTime(secs?: number | null): string {
  if (!secs) return "";
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

// ===== 详情动作 =====
const toast = ref("");
let toastTimer: number | undefined;
function showToast(msg: string): void {
  toast.value = msg;
  if (toastTimer) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (toast.value = ""), 2600);
}

const confirmIdle = ref(false);
function doForceIdle(s: SessionM): void {
  confirmIdle.value = false;
  s.state = "idle";
  s.waiting = false;
  s.steps = s.steps.map((st) => ({ ...st, state: st.state === "running" ? "done" : st.state }));
  showToast("已手动置为空闲");
}

// ===== 插话输入（C3 追加语义 + R1 交互区插槽）=====
const draft = ref("");
function canSend(s: SessionM): boolean {
  return s.state === "working" && s.kind !== "grok";
}
function sendMsg(): void {
  const s = sel.value;
  if (!s || !canSend(s) || !draft.value.trim()) return;
  s.pendingMsgs.push(draft.value.trim());
  draft.value = "";
  showToast("已排队 · 将在 Agent 下一次工具调用时送达");
}
function revokeMsgs(s: SessionM): void {
  s.pendingMsgs = [];
  showToast("已撤回待送达消息");
}
function onDraftKeydown(e: KeyboardEvent): void {
  if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
    e.preventDefault();
    sendMsg();
  }
}

// ===== 新建任务（C4：嵌入右栏）=====
const ntProject = ref("");
const ntSource = ref<number>(-1); // -1 = 直接输入；否则待办下标
const ntText = ref("");
const ntAgent = ref<Kind | null>(null);
const ntPermission = ref<"default" | "yolo" | null>(null);
const ntBusy = ref(false);

function openNewTask(path: string): void {
  ntProject.value = path;
  ntSource.value = -1;
  ntText.value = "";
  ntAgent.value = null;
  ntPermission.value = null;
  mode.value = "newtask";
}

const ntProjectName = computed(
  () =>
    [...PROJECTS, ...RECENT_PROJECTS].find((p) => p.path === ntProject.value)?.name ??
    ntProject.value
);
const ntTodos = computed(() => PROJECT_TODOS[ntProject.value] ?? []);

const ntValid = computed(() => {
  if (!ntAgent.value || !ntPermission.value) return false;
  if (ntSource.value >= 0) return true; // 选待办时补充可空
  return ntText.value.trim().length > 0;
});

function launchTask(): void {
  if (!ntValid.value || ntBusy.value) return;
  ntBusy.value = true;
  const todo = ntSource.value >= 0 ? ntTodos.value[ntSource.value] : null;
  const title = (todo ? todo.text : ntText.value.trim()).slice(0, 24);
  window.setTimeout(() => {
    ntBusy.value = false;
    const t = Math.floor(Date.now() / 1000);
    const id = `s-new-${t}`;
    sessions.value.push({
      id,
      kind: ntAgent.value as Kind,
      title,
      projectPath: ntProject.value,
      state: "working",
      waiting: false,
      startedAt: t,
      lastActivity: t,
      activeElapsedSecs: 0,
      text: "任务已启动，我先通读相关代码，稍后给出执行计划。",
      steps: [{ label: "读取", object: "README.md", state: "running" }],
      stepsOmitted: 0,
      todos: [],
      pendingMsgs: [],
    });
    selectSession(id); // 启动成功 → 自动选中新会话（C4）
    showToast("已在新的 Terminal 窗口启动任务（模拟）");
  }, 900);
}

// ===== 生命周期 =====
onMounted(() => {
  applyThemeClass();
  window.addEventListener("keydown", onKeydown);
  tickTimer = window.setInterval(() => {
    nowMs.value = Date.now();
    for (const s of sessions.value) {
      if (s.state === "working") s.activeElapsedSecs += 1;
    }
  }, 1000);
  liveTimer = window.setInterval(advanceLive, 2600);
  matrixTimer = window.setInterval(() => {
    matrixFrame.value = (matrixFrame.value + 1) % MATRIX_FRAMES.length;
  }, MATRIX_FRAME_MS);
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKeydown);
  if (tickTimer) window.clearInterval(tickTimer);
  if (liveTimer) window.clearInterval(liveTimer);
  if (matrixTimer) window.clearInterval(matrixTimer);
});
</script>

<template>
  <div class="console">
    <!-- 顶栏 -->
    <header class="top">
      <span class="app-title">Agents</span>
      <div class="seg" role="tablist">
        <button
          v-for="f in FILTERS"
          :key="f"
          class="seg-btn"
          :class="{ active: filter === f }"
          role="tab"
          @click="filter = f"
        >
          {{ FILTER_LABEL[f] }}
        </button>
      </div>
      <span class="spacer" />
      <span class="proto-tag">原型 · mock 数据</span>
      <button class="icon-btn" :title="theme === 'dark' ? '切浅色' : '切深色'" @click="toggleTheme">
        <svg v-if="theme === 'dark'" viewBox="0 0 16 16"><circle cx="8" cy="8" r="3.4" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M8 1.4v1.8M8 12.8v1.8M1.4 8h1.8M12.8 8h1.8M3.3 3.3l1.3 1.3M11.4 11.4l1.3 1.3M12.7 3.3l-1.3 1.3M4.6 11.4l-1.3 1.3" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
        <svg v-else viewBox="0 0 16 16"><path d="M13.2 9.6A5.6 5.6 0 0 1 6.4 2.8a5.6 5.6 0 1 0 6.8 6.8Z" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"/></svg>
      </button>
    </header>

    <div class="body">
      <!-- 边栏 -->
      <aside class="sidebar">
        <section v-for="g in groups" :key="g.key" class="group">
          <div class="proj-head">
            <button class="proj-toggle" @click="toggleCollapse(g.key)">
              <svg class="chevron" :class="{ closed: collapsed.has(g.key) }" viewBox="0 0 12 12">
                <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
          <span class="proj-name">{{ g.label }}</span>
          <span v-if="workingCount(g)" class="badge badge-working">{{ workingCount(g) }}</span>
          <span
            v-if="g.todoCount"
            class="badge badge-todo"
            role="button"
            title="项目待办"
            @click.stop="showToast('（原型）打开该项目待办窗口')"
          >☑{{ g.todoCount }}</span>
        </button>
            <button class="plus-btn" title="在此项目新建任务" @click="openNewTask(g.path)">
              <svg viewBox="0 0 12 12"><path d="M6 2v8M2 6h8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
            </button>
          </div>
          <ul v-show="visibleItems(g).length || !collapsed.has(g.key)" class="sess-list">
            <li
              v-for="s in visibleItems(g)"
              :key="s.id"
              class="sess"
              :class="{ selected: s.id === selectedId, ended: s.state === 'ended' }"
              @click="selectSession(s.id)"
            >
              <span class="ind" :class="indClass(s)">
                <template v-if="indClass(s) === 'waiting'">🙋</template>
                <svg v-else-if="indClass(s) !== 'ended'" class="matrix" viewBox="0 0 12 12"><circle v-for="(c, i) in MATRIX_CELLS" :key="i" :cx="c.x" :cy="c.y" r="1.3" :class="{ on: matrixOn(s, i) }"/></svg>
              </span>
              <span class="sess-main">
                <span class="sess-title">{{ s.title }}</span>
                <span class="sess-sub">
                  {{ KIND_LABEL[s.kind] }} · {{ relativeTime(s.state === 'ended' ? s.endedAt : s.lastActivity) }}
                  <span v-if="s.pendingMsgs.length" class="ij-mini" title="有待送达插话">✉ {{ s.pendingMsgs.length }}</span>
                </span>
              </span>
            </li>
            <li v-if="!collapsed.has(g.key) && g.items.length === 0" class="sess-empty">当前过滤下无会话</li>
          </ul>
        </section>

        <!-- 最近项目折叠区（C5）-->
        <section class="group recent">
          <div class="proj-head">
            <button class="proj-toggle" @click="recentOpen = !recentOpen">
              <svg class="chevron" :class="{ closed: !recentOpen }" viewBox="0 0 12 12">
                <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
              </svg>
              <span class="proj-name secondary">最近项目</span>
            </button>
          </div>
          <ul v-show="recentOpen" class="sess-list">
            <li v-for="p in RECENT_PROJECTS" :key="p.path" class="recent-row">
              <span class="recent-name" :title="p.path">{{ p.name }}</span>
              <button class="plus-btn always" title="在此项目新建任务" @click="openNewTask(p.path)">
                <svg viewBox="0 0 12 12"><path d="M6 2v8M2 6h8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
              </button>
            </li>
          </ul>
        </section>
      </aside>

      <!-- 详情区 -->
      <main class="detail">
        <!-- 新建任务表单（C4）-->
        <template v-if="mode === 'newtask'">
          <div class="nt">
            <div class="nt-head">
              <span class="nt-title">新建 Agent 任务</span>
              <span class="nt-proj" :title="ntProject">{{ ntProjectName }}</span>
              <span class="spacer" />
              <button class="icon-btn" title="返回" @click="mode = 'detail'">
                <svg viewBox="0 0 12 12"><path d="M3 3l6 6M9 3l-6 6" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
              </button>
            </div>
            <div class="nt-body">
              <div class="nt-field">
                <label class="nt-label">任务来源</label>
                <div class="nt-source">
                  <label class="nt-radio">
                    <input type="radio" :checked="ntSource === -1" @change="ntSource = -1" />
                    <span>直接输入新任务</span>
                  </label>
                  <label v-for="(td, i) in ntTodos" :key="i" class="nt-radio">
                    <input type="radio" :checked="ntSource === i" @change="ntSource = i" />
                    <span class="nt-todo-text">{{ td.auto ? "⚡ " : "" }}{{ td.text }}</span>
                  </label>
                  <p v-if="ntTodos.length === 0" class="nt-hint">该项目暂无待办</p>
                </div>
              </div>
              <div class="nt-field">
                <label class="nt-label">{{ ntSource >= 0 ? "补充说明（可选）" : "任务内容" }}</label>
                <textarea
                  v-model="ntText"
                  class="nt-input"
                  rows="4"
                  :placeholder="ntSource >= 0 ? '在待办原文基础上补充要求…' : '描述要执行的任务…'"
                />
              </div>
              <div class="nt-field">
                <label class="nt-label">Agent</label>
                <div class="nt-agents">
                  <button
                    v-for="r in READINESS"
                    :key="r.kind"
                    class="nt-agent"
                    :class="{ active: ntAgent === r.kind, disabled: !r.ready }"
                    :disabled="!r.ready"
                    @click="ntAgent = r.kind"
                  >
                    <span class="nt-agent-name">{{ KIND_LABEL[r.kind] }}</span>
                    <span v-if="r.ready" class="nt-agent-ok">✓ 就绪</span>
                    <span v-else class="nt-agent-no">{{ r.reason }}</span>
                  </button>
                </div>
              </div>
              <div class="nt-field">
                <label class="nt-label">权限</label>
                <div class="nt-source row">
                  <label class="nt-radio">
                    <input type="radio" :checked="ntPermission === 'default'" @change="ntPermission = 'default'" />
                    <span>Agent 默认</span>
                  </label>
                  <label class="nt-radio">
                    <input type="radio" :checked="ntPermission === 'yolo'" @change="ntPermission = 'yolo'" />
                    <span class="danger">YOLO（危险）</span>
                  </label>
                </div>
              </div>
            </div>
            <div class="nt-foot">
              <span class="nt-hint">启动后将打开新的 Terminal 窗口并自动选中新会话</span>
              <span class="spacer" />
              <button class="btn" @click="mode = 'detail'">取消</button>
              <button class="btn primary" :disabled="!ntValid || ntBusy" @click="launchTask">
                {{ ntBusy ? "启动中…" : "启动任务" }}
              </button>
            </div>
          </div>
        </template>

        <!-- 会话详情（C2 Watch 帧）-->
        <template v-else-if="sel">
          <div class="dt-head">
            <div class="dt-title-row">
              <span class="kind-badge">{{ KIND_LABEL[sel.kind] }}</span>
              <span class="dt-title">{{ sel.title }}</span>
              <span class="spacer" />
              <button class="icon-btn" title="聚焦终端" @click="showToast('（原型）聚焦该会话所在终端')">
                <svg viewBox="0 0 16 16"><rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M4 6 L6.5 8 L4 10" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/><path d="M8 10.2 H11.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
              <button class="icon-btn" title="项目待办" @click="showToast('（原型）打开该项目待办窗口')">
                <svg viewBox="0 0 16 16"><rect x="2" y="2.5" width="12" height="11" rx="2" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M4.6 6 L6 7.4 L8.2 5" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"/><path d="M9.8 6.6 H11.6 M4.8 10.4 H11.6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
              <button
                v-if="sel.state === 'working'"
                class="icon-btn warn"
                title="手动置空闲"
                @click="confirmIdle = true"
              >
                <svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" stroke-width="1.3"/><path d="M6 5.6 V10.4 M10 5.6 V10.4" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
              </button>
            </div>
            <div class="dt-meta">
              <span v-if="sel.state !== 'ended'" class="ind" :class="indClass(sel)">
                <template v-if="indClass(sel) === 'waiting'">🙋</template>
                <svg v-else class="matrix" viewBox="0 0 12 12"><circle v-for="(c, i) in MATRIX_CELLS" :key="i" :cx="c.x" :cy="c.y" r="1.3" :class="{ on: matrixOn(sel, i) }"/></svg>
              </span>
              <span class="dt-state" :class="indClass(sel)">{{ stateLabel(sel) }}</span>
              <span v-if="sel.activeElapsedSecs >= 60" class="dt-elapsed">· 累计工作 {{ fmtDuration(sel.activeElapsedSecs) }}</span>
              <span class="spacer" />
              <span class="dt-path" :title="sel.projectPath">{{ sel.projectPath }}</span>
            </div>
            <div v-if="confirmIdle" class="idle-confirm">
              <span>该 Agent 可能仍在工作，确认标记为空闲？</span>
              <span class="spacer" />
              <button class="btn sm" @click="confirmIdle = false">取消</button>
              <button class="btn sm warn-solid" @click="doForceIdle(sel)">确认</button>
            </div>
          </div>

          <div ref="dtBody" class="dt-body">
            <!-- 等待回答横幅（C7）-->
            <div v-if="sel.waiting" class="wait-banner">
              <span class="wait-icon">🙋</span>
              <span class="wait-main">
                <span class="wait-title">正在等待你的回答</span>
                <span v-if="sel.waitingQuestion" class="wait-q">{{ sel.waitingQuestion }}</span>
              </span>
              <button class="btn sm primary" @click="showToast('（原型）聚焦对应提问弹窗')">去回答</button>
            </div>

            <!-- 最近动态（默认视图）-->
            <template v-if="viewMode === 'latest'">
              <div class="act-heading-row">
                <span class="act-heading">最近动态（{{ clockTime(sel.lastActivity) }}）</span>
                <button class="tx-btn" @click="openTranscript">
                  <svg viewBox="0 0 14 14"><path d="M2.5 3.5 H11.5 M2.5 7 H11.5 M2.5 10.5 H8" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>
                  完整会话
                </button>
              </div>
              <div v-if="bodyHtml" class="markdown-body act-text" v-html="bodyHtml" />
              <p v-else class="act-none">暂无活动</p>

              <div v-if="sel.steps.length" class="steps">
                <div v-if="sel.stepsOmitted > 0" class="step omitted">… 已省略 {{ sel.stepsOmitted }} 步</div>
                <div v-for="(st, i) in sel.steps" :key="i" class="step">
                  <span class="step-dot" :class="st.state" />
                  <span class="step-label">{{ st.label }}</span>
                  <span v-if="st.object" class="step-obj">{{ st.object }}</span>
                </div>
              </div>

              <div v-if="sel.todos.length" class="todos">
                <button class="todos-head" @click="todosOpen = !todosOpen">
                  <svg class="chevron" :class="{ closed: !todosOpen }" viewBox="0 0 12 12">
                    <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                  </svg>
                  <span>📋 {{ todoSummary(sel) }}</span>
                </button>
                <ul v-show="todosOpen" class="todo-list">
                  <li v-for="(td, i) in sel.todos" :key="i" class="todo" :class="td.state">
                    <span class="todo-dot" :class="td.state" />
                    <span class="todo-text">{{ td.content }}</span>
                  </li>
                </ul>
              </div>
            </template>

            <!-- 完整会话（分页加载，向上补更早）-->
            <template v-else>
              <div class="act-heading-row tx-sticky">
                <span class="act-heading">完整会话 · 已加载 {{ txEvents.length }}/{{ txTotal }} 条</span>
                <button class="tx-btn" @click="closeTranscript">
                  <svg viewBox="0 0 14 14"><path d="M8.5 3.5 L4.5 7 L8.5 10.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
                  返回最近动态
                </button>
              </div>
              <div class="tx-top">
                <button v-if="txRemaining > 0" class="btn sm" @click="loadOlder">
                  ↑ 加载更早的 {{ txNextPage }} 条（还有 {{ txRemaining }} 条）
                </button>
                <span v-else class="tx-begin">已到会话开头</span>
              </div>
              <div class="tx-list">
                <template v-for="(e, i) in txEvents" :key="txBase + i">
                  <div v-if="e.kind === 'user'" class="tx-user">
                    <div class="tx-user-head">
                      <span class="tx-role">你</span>
                      <span class="tx-time">{{ clockTime(e.at) }}</span>
                    </div>
                    <div class="tx-user-text">{{ e.text }}</div>
                  </div>
                  <div v-else-if="e.kind === 'assistant'" class="markdown-body tx-assistant" v-html="renderMarkdown(e.text || '')" />
                  <div v-else-if="e.kind === 'ask'" class="tx-ask">
                    <div class="tx-ask-q">
                      <span class="tx-ask-badge">🙋 AskHuman</span>
                      <span class="tx-time">{{ clockTime(e.at) }}</span>
                    </div>
                    <div class="tx-ask-qtext">
                      <div
                        class="markdown-body ask-md"
                        :class="{ clamped: isLongAsk(e) && !txExpanded.has(txBase + i) }"
                        v-html="renderMarkdown(e.question || '')"
                      />
                      <button v-if="isLongAsk(e)" class="ask-expand" @click="toggleAskExpand(txBase + i)">
                        {{ txExpanded.has(txBase + i) ? "收起" : "展开全文" }}
                      </button>
                    </div>
                    <!-- 多问题：每题独立子块，各带自己的回答 -->
                    <template v-if="e.questions?.length">
                      <div v-for="(qa, qi) in e.questions" :key="qi" class="tx-ask-sub">
                        <div class="tx-ask-subq">
                          <span class="tx-ask-qn">Q{{ qi + 1 }}</span>
                          <span>{{ qa.q }}</span>
                        </div>
                        <div v-if="qa.a" class="tx-ask-a">
                          <span class="tx-role">你</span>
                          <span class="tx-ask-atext">{{ qa.a }}</span>
                        </div>
                        <div v-else class="tx-ask-a pending-a">（未回答）</div>
                      </div>
                    </template>
                    <template v-else>
                      <div v-if="e.answer" class="tx-ask-a">
                        <span class="tx-role">你</span>
                        <span class="tx-ask-atext">{{ e.answer }}</span>
                      </div>
                      <div v-else class="tx-ask-a pending-a">（未回答）</div>
                    </template>
                  </div>
                  <div v-else class="step tx-step">
                    <span class="step-dot" :class="e.failed ? 'failed' : 'done'" />
                    <span class="step-label">{{ e.label }}</span>
                    <span v-if="e.object" class="step-obj">{{ e.object }}</span>
                  </div>
                </template>
              </div>
            </template>
          </div>

          <!-- 项目未暂存变更状态条（/diff·/stage 的 GUI 形态）-->
          <div v-if="unstagedFiles.length" class="diffbar">
            <div class="diffbar-row">
              <button class="diffbar-head" @click="diffOpen = !diffOpen">
                <svg class="chevron" :class="{ closed: !diffOpen }" viewBox="0 0 12 12">
                  <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" />
                </svg>
                <span class="diffbar-title">未暂存变更</span>
                <span class="diffbar-stat">
                  {{ diffTotals.files }} 个文件<template v-if="diffTotals.added">（{{ diffTotals.added }} 新增）</template>
                </span>
                <span class="d-adds">+{{ diffTotals.adds }}</span>
                <span class="d-dels">−{{ diffTotals.dels }}</span>
              </button>
              <template v-if="confirmStageAll">
                <span class="stage-confirm-text">暂存全部 {{ diffTotals.files }} 个文件？</span>
                <button class="btn sm" @click="confirmStageAll = false">取消</button>
                <button class="btn sm primary" @click="stageAll">确认</button>
              </template>
              <button v-else class="btn sm stage-all" @click="confirmStageAll = true">全部暂存</button>
            </div>
            <div v-show="diffOpen" class="diff-panel">
              <div v-for="f in unstagedFiles" :key="f.path" class="diff-file">
                <div class="diff-file-row">
                  <button class="diff-file-main" @click="diffFileOpen = diffFileOpen === f.path ? null : f.path">
                    <span class="fkind" :class="f.kind">{{ f.kind }}</span>
                    <span class="fpath">{{ f.path }}</span>
                    <span class="d-adds">+{{ f.adds }}</span>
                    <span class="d-dels">−{{ f.dels }}</span>
                  </button>
                  <button class="btn sm" @click="stageFile(f)">暂存</button>
                </div>
                <div v-show="diffFileOpen === f.path" class="hunks">
                  <template v-for="(h, hi) in f.hunks" :key="hi">
                    <div class="hunk-header mono">{{ h.header }}</div>
                    <div v-for="(ln, li) in h.lines" :key="li" class="dline mono" :class="ln.kind">
                      <span class="dsign">{{ ln.kind === "add" ? "+" : ln.kind === "del" ? "−" : " " }}</span>{{ ln.text }}
                    </div>
                  </template>
                </div>
              </div>
            </div>
          </div>

          <!-- 交互区插槽（R1）：插话输入 / 提示 / 新建任务快捷入口 -->
          <div class="interact">
            <template v-if="canSend(sel)">
              <div v-if="sel.pendingMsgs.length" class="ij-pending">
                <span class="pending-badge">待送达 {{ sel.pendingMsgs.length }} 条</span>
                <span class="pending-text" :title="sel.pendingMsgs.join('\n')">{{ sel.pendingMsgs[sel.pendingMsgs.length - 1] }}</span>
                <button class="pending-revoke" @click="revokeMsgs(sel)">撤回</button>
              </div>
              <div class="composer">
                <textarea
                  v-model="draft"
                  class="composer-input"
                  rows="2"
                  placeholder="给这个 Agent 发消息（下一次工具调用时送达）…"
                  @keydown="onDraftKeydown"
                />
                <button class="btn primary send" :disabled="!draft.trim()" @click="sendMsg">
                  发送 <span class="kbd">⌘↵</span>
                </button>
              </div>
            </template>
            <p v-else-if="sel.state === 'working' && sel.kind === 'grok'" class="interact-hint">
              Grok 不支持发送消息
            </p>
            <p v-else-if="sel.state === 'idle'" class="interact-hint">
              会话已空闲，无法发送消息 ·
              <a href="#" @click.prevent="openNewTask(sel.projectPath)">在此项目新建任务</a>
            </p>
            <p v-else class="interact-hint">会话已结束</p>
          </div>
        </template>

        <div v-else class="dt-empty">
          <p>选择左侧会话查看实时状态</p>
        </div>
      </main>
    </div>

    <transition name="toast">
      <div v-if="toast" class="toast">{{ toast }}</div>
    </transition>
  </div>
</template>

<style scoped>
.console {
  display: flex;
  flex-direction: column;
  height: 100vh;
  color: var(--text-primary);
  overflow: hidden;
}
.spacer {
  flex: 1 1 auto;
}

/* ── 顶栏 ── */
.top {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.app-title {
  font-size: 14px;
  font-weight: 600;
}
.proto-tag {
  font-size: 11px;
  color: var(--text-tertiary);
  border: var(--hairline) solid var(--border);
  border-radius: 999px;
  padding: 1px 8px;
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
.seg-btn.active {
  background: var(--bg-elevated);
  color: var(--text-primary);
  box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
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

/* ── 双栏 ── */
.body {
  flex: 1 1 auto;
  display: flex;
  min-height: 0;
}
.sidebar {
  flex: 0 0 248px;
  min-width: 0;
  overflow-y: auto;
  padding: 10px 8px 16px;
  border-right: var(--hairline) solid var(--border);
}
.detail {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
}

/* ── 边栏分组 ── */
.group {
  margin-bottom: 10px;
}
.group.recent {
  margin-top: 18px;
}
.proj-head {
  display: flex;
  align-items: center;
  gap: 2px;
}
.proj-head:hover .plus-btn {
  opacity: 1;
}
.proj-toggle {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 5px;
  border: none;
  background: transparent;
  padding: 3px 4px;
  border-radius: 6px;
  font-size: 12px;
  font-weight: 700;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
}
.proj-toggle:hover {
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
}
.proj-name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.proj-name.secondary {
  text-transform: uppercase;
  letter-spacing: 0.04em;
  font-size: 11px;
}
.chevron {
  flex: 0 0 auto;
  width: 11px;
  height: 11px;
  transform: rotate(90deg);
  transition: transform 0.15s ease;
}
.chevron.closed {
  transform: rotate(0deg);
}
.badge {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  min-width: 16px;
  height: 16px;
  padding: 0 4px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 600;
}
.badge-working {
  background: color-mix(in srgb, #30d158 20%, transparent);
  color: #248a3d;
}
.badge-todo {
  cursor: pointer;
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
  color: var(--text-tertiary);
  font-weight: 500;
}
.badge-todo:hover {
  background: color-mix(in srgb, var(--accent) 16%, transparent);
  color: var(--accent);
}
.plus-btn {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  padding: 0;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.12s ease;
}
.plus-btn.always,
.recent-row:hover .plus-btn {
  opacity: 1;
}
.plus-btn:hover {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
}
.plus-btn svg {
  width: 11px;
  height: 11px;
}

/* ── 会话行 ── */
.sess-list {
  list-style: none;
  margin: 2px 0 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 1px;
}
.sess {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 8px;
  border-radius: 7px;
  cursor: pointer;
}
.sess:hover {
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
}
.sess.selected {
  background: color-mix(in srgb, var(--accent) 14%, transparent);
}
.sess.ended {
  opacity: 0.55;
}
.sess-main {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.sess-title {
  font-size: 12.5px;
  font-weight: 500;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.sess-sub {
  font-size: 11px;
  color: var(--text-tertiary);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.ij-mini {
  color: var(--accent);
  margin-left: 4px;
}
.sess-empty {
  padding: 4px 8px;
  font-size: 11px;
  color: var(--text-tertiary);
}
.recent-row {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 4px 8px 4px 20px;
  border-radius: 7px;
  font-size: 12px;
  color: var(--text-secondary);
}
.recent-row:hover {
  background: color-mix(in srgb, var(--text-primary) 5%, transparent);
}
.recent-name {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* ── 状态指示器（旋转圆弧 / 🙋 / 月亮 / 无）── */
.ind {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 17px;
  line-height: 1;
}
.ind.working {
  /* 低饱和绿（按主题取值，见下方非 scoped 块）。 */
  color: var(--ind-working);
}
.ind .matrix {
  width: 12px;
  height: 12px;
}
.ind .matrix circle {
  fill: currentColor;
  opacity: 0;
  transition: opacity 0.07s linear;
}
.ind .matrix circle.on {
  opacity: 1;
}
.ind.waiting {
  font-size: 13.5px;
  animation: hand-bounce 1.3s ease-in-out infinite;
}
.ind.idle {
  color: var(--text-tertiary);
}
@keyframes hand-bounce {
  0%,
  100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(-1.5px);
  }
}

/* ── 详情头部 ── */
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
.dt-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-top: 7px;
  font-size: 12px;
}
.dt-state.working {
  color: var(--ind-working);
  font-weight: 600;
}
.dt-state.waiting {
  color: var(--accent);
  font-weight: 600;
}
.dt-state.idle {
  color: var(--text-secondary);
  font-weight: 600;
}
.dt-state.ended {
  color: var(--text-secondary);
  font-weight: 600;
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

/* ── 详情正文 ── */
.dt-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 16px;
}
.wait-banner {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 9px 12px;
  margin-bottom: 14px;
  border-radius: 9px;
  background: color-mix(in srgb, var(--accent) 10%, transparent);
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
.act-heading {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-tertiary);
}
.act-heading-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  margin-bottom: 8px;
}
.act-heading-row.tx-sticky {
  position: sticky;
  top: -14px;
  margin-top: -8px;
  padding: 8px 0;
  background: var(--bg);
  z-index: 2;
}
.tx-btn {
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
.tx-btn:hover {
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
  color: var(--text-primary);
}
.tx-btn svg {
  width: 12px;
  height: 12px;
}
.tx-top {
  display: flex;
  justify-content: center;
  padding: 2px 0 12px;
}
.tx-begin {
  font-size: 11px;
  color: var(--text-tertiary);
}
.tx-list {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.tx-user {
  margin: 6px 0 2px;
  padding: 7px 10px;
  border-radius: 9px;
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}
.tx-user-head {
  display: flex;
  align-items: baseline;
  gap: 8px;
  margin-bottom: 2px;
}
.tx-role {
  font-size: 10.5px;
  font-weight: 700;
  color: var(--accent);
}
.tx-time {
  font-size: 10.5px;
  color: var(--text-tertiary);
}
.tx-user-text {
  font-size: 13px;
}
.tx-assistant {
  font-size: 13px;
  margin: 2px 0;
}
.tx-ask {
  margin: 6px 0 2px;
  border: var(--hairline) solid color-mix(in srgb, var(--accent) 30%, transparent);
  border-radius: 9px;
  overflow: hidden;
}
.tx-ask-q {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 8px;
  padding: 6px 10px 0;
}
.tx-ask-badge {
  font-size: 10.5px;
  font-weight: 700;
  color: var(--accent);
}
.tx-ask-qtext {
  padding: 2px 10px 7px;
  font-size: 13px;
}
.ask-md {
  font-size: 13px;
}
.ask-md.clamped {
  display: -webkit-box;
  -webkit-line-clamp: 4;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.ask-expand {
  appearance: none;
  border: none;
  background: transparent;
  color: var(--accent);
  font-size: 11.5px;
  font-weight: 600;
  padding: 3px 0 0;
  cursor: pointer;
}
.tx-ask-sub {
  border-top: var(--hairline) solid color-mix(in srgb, var(--accent) 16%, transparent);
}
.tx-ask-subq {
  display: flex;
  align-items: baseline;
  gap: 7px;
  padding: 7px 10px 5px;
  font-size: 13px;
  font-weight: 500;
}
.tx-ask-qn {
  flex: 0 0 auto;
  padding: 0 6px;
  border-radius: 5px;
  font-size: 10.5px;
  font-weight: 700;
  background: color-mix(in srgb, var(--accent) 14%, transparent);
  color: var(--accent);
}
.tx-ask-a {
  display: flex;
  align-items: baseline;
  gap: 8px;
  padding: 6px 10px 7px;
  background: color-mix(in srgb, var(--accent) 8%, transparent);
}
.tx-ask-atext {
  font-size: 13px;
}
.tx-ask-a.pending-a {
  font-size: 12px;
  color: var(--text-tertiary);
}
.act-text {
  font-size: 13px;
  margin-bottom: 12px;
}
.act-none {
  font-size: 12px;
  color: var(--text-tertiary);
}
.steps {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin-bottom: 14px;
}
.step {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}
.step.omitted {
  color: var(--text-tertiary);
  font-size: 11.5px;
  padding-left: 16px;
}
.step-dot {
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
}
.step-dot.running {
  background: #30d158;
  animation: pulse-green 1.4s ease-in-out infinite;
}
.step-dot.done {
  background: var(--text-tertiary);
}
.step-dot.failed {
  background: #ff453a;
}
@keyframes pulse-green {
  0%,
  100% {
    box-shadow: 0 0 0 0 color-mix(in srgb, #30d158 30%, transparent);
  }
  50% {
    box-shadow: 0 0 0 4px color-mix(in srgb, #30d158 14%, transparent);
  }
}
.step-label {
  font-weight: 600;
}
.step-obj {
  min-width: 0;
  font-style: italic;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.todos {
  border: var(--hairline) solid var(--border);
  border-radius: 9px;
  overflow: hidden;
}
.todos-head {
  display: flex;
  align-items: center;
  gap: 6px;
  width: 100%;
  border: none;
  background: color-mix(in srgb, var(--text-primary) 4%, transparent);
  padding: 7px 10px;
  font-size: 12px;
  font-weight: 600;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
}
.todo-list {
  list-style: none;
  margin: 0;
  padding: 6px 12px 8px;
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.todo {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}
.todo-dot {
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
  border: 1.4px solid var(--text-tertiary);
  background: transparent;
}
.todo-dot.inProgress {
  background: #30d158;
  border-color: #30d158;
}
.todo-dot.completed {
  background: var(--text-tertiary);
  border-color: var(--text-tertiary);
}
.todo.inProgress .todo-text {
  font-weight: 600;
}
.todo.completed .todo-text {
  color: var(--text-tertiary);
  text-decoration: line-through;
}

/* ── 项目未暂存变更状态条 ── */
.diffbar {
  flex: 0 0 auto;
  border-top: var(--hairline) solid var(--border);
}
.diffbar-row {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 16px 4px 10px;
}
.diffbar-head {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 7px;
  border: none;
  background: transparent;
  padding: 4px 6px;
  border-radius: 6px;
  font-size: 12px;
  color: var(--text-secondary);
  cursor: pointer;
  text-align: left;
}
.diffbar-head:hover {
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
}
.diffbar-title {
  font-weight: 600;
  color: var(--text-primary);
}
.d-adds {
  color: var(--accent-green);
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.d-dels {
  color: #ff453a;
  font-weight: 600;
  font-variant-numeric: tabular-nums;
}
.stage-confirm-text {
  font-size: 11.5px;
  color: var(--text-primary);
  white-space: nowrap;
}
.diff-panel {
  max-height: 300px;
  overflow-y: auto;
  padding: 2px 16px 10px;
}
.diff-file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  /* 长 diff 内滚动时文件头钉在面板顶部：随时可点击收起。 */
  position: sticky;
  top: 0;
  z-index: 1;
  background: var(--bg);
}
.diff-file-main {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  border: none;
  background: transparent;
  padding: 4px 6px;
  border-radius: 6px;
  cursor: pointer;
  font-size: 12px;
  color: var(--text-primary);
  text-align: left;
}
.diff-file-main:hover {
  background: color-mix(in srgb, var(--text-primary) 6%, transparent);
}
.fkind {
  flex: 0 0 auto;
  width: 16px;
  height: 16px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 4px;
  font-size: 10px;
  font-weight: 700;
}
.fkind.M {
  background: color-mix(in srgb, #ff9f0a 18%, transparent);
  color: #c77700;
}
.fkind.A {
  background: color-mix(in srgb, #30d158 18%, transparent);
  color: #248a3d;
}
.fkind.D {
  background: color-mix(in srgb, #ff453a 16%, transparent);
  color: #ff453a;
}
.fpath {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  direction: rtl;
  text-align: left;
}
.hunks {
  margin: 2px 0 8px 30px;
  border: var(--hairline) solid var(--border);
  border-radius: 7px;
  overflow: hidden;
}
.hunk-header {
  padding: 3px 10px;
  font-size: 11px;
  color: var(--text-tertiary);
  background: color-mix(in srgb, var(--text-primary) 4%, transparent);
}
.dline {
  padding: 1px 10px;
  font-size: 11.5px;
  white-space: pre-wrap;
  word-break: break-all;
}
.dline.add {
  background: var(--diff-add-bg);
}
.dline.del {
  background: var(--diff-delete-bg);
}
.dsign {
  display: inline-block;
  width: 12px;
  color: var(--text-tertiary);
}
.mono {
  font-family: var(--font-mono);
}

/* ── 交互区插槽（R1）── */
.interact {
  flex: 0 0 auto;
  padding: 10px 16px 12px;
  border-top: var(--hairline) solid var(--border);
}
.ij-pending {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 9px;
  margin-bottom: 8px;
  border-radius: 7px;
  background: color-mix(in srgb, var(--accent) 10%, transparent);
  font-size: 12px;
}
.pending-badge {
  flex: 0 0 auto;
  padding: 1px 8px;
  border-radius: 999px;
  font-size: 10px;
  font-weight: 600;
  background: color-mix(in srgb, var(--accent) 18%, transparent);
  color: var(--accent);
  white-space: nowrap;
}
.pending-text {
  flex: 1 1 auto;
  min-width: 0;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.pending-revoke {
  flex: 0 0 auto;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 11px;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 5px;
  cursor: pointer;
}
.pending-revoke:hover {
  background: color-mix(in srgb, var(--text-primary) 10%, transparent);
  color: var(--text-primary);
}
.composer {
  display: flex;
  align-items: flex-end;
  gap: 8px;
}
.composer-input {
  flex: 1 1 auto;
  resize: none;
  border: var(--hairline) solid var(--control-border);
  border-radius: 9px;
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  color: var(--text-primary);
  font-family: var(--font-sans);
  font-size: 13px;
  line-height: 1.45;
  padding: 7px 10px;
}
.composer-input:focus-visible {
  box-shadow: var(--clickable-shadow), var(--focus-ring);
}
.interact-hint {
  margin: 2px 0;
  font-size: 12px;
  color: var(--text-tertiary);
}
.interact-hint a {
  color: var(--accent);
  text-decoration: none;
}
.dt-empty {
  flex: 1 1 auto;
  display: flex;
  align-items: center;
  justify-content: center;
  color: var(--text-tertiary);
  font-size: 13px;
}

/* ── 按钮 ── */
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
.btn:hover:not(:disabled) {
  background: var(--control-hover-bg);
}
.btn:disabled {
  opacity: 0.45;
  cursor: default;
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
.btn.primary:hover:not(:disabled) {
  background: #0071e3;
}
.btn.warn-solid {
  border-color: transparent;
  background: #ff9f0a;
  color: #fff;
}
.btn.send {
  flex: 0 0 auto;
}
.kbd {
  font-size: 10.5px;
  opacity: 0.75;
  margin-left: 2px;
}
.danger {
  color: #ff453a;
}

/* ── 新建任务表单 ── */
.nt {
  flex: 1 1 auto;
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.nt-head {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 12px 16px 10px;
  border-bottom: var(--hairline) solid var(--border);
}
.nt-title {
  font-size: 14px;
  font-weight: 600;
}
.nt-proj {
  font-size: 12px;
  color: var(--text-secondary);
  padding: 1px 8px;
  border-radius: 5px;
  background: color-mix(in srgb, var(--text-primary) 7%, transparent);
}
.nt-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 14px 16px;
  display: flex;
  flex-direction: column;
  gap: 16px;
}
.nt-field {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.nt-label {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-tertiary);
}
.nt-source {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.nt-source.row {
  flex-direction: row;
  gap: 18px;
}
.nt-radio {
  display: flex;
  align-items: center;
  gap: 7px;
  font-size: 13px;
  cursor: pointer;
}
.nt-todo-text {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.nt-input {
  resize: vertical;
  border: var(--hairline) solid var(--control-border);
  border-radius: 9px;
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  color: var(--text-primary);
  font-family: var(--font-sans);
  font-size: 13px;
  line-height: 1.45;
  padding: 8px 10px;
}
.nt-input:focus-visible {
  box-shadow: var(--clickable-shadow), var(--focus-ring);
}
.nt-agents {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(170px, 1fr));
  gap: 8px;
}
.nt-agent {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 8px 11px;
  border: var(--hairline) solid var(--control-border);
  border-radius: 9px;
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  cursor: pointer;
  text-align: left;
  color: var(--text-primary);
}
.nt-agent:hover:not(.disabled) {
  background: var(--control-hover-bg);
}
.nt-agent.active {
  border-color: var(--accent);
  box-shadow: 0 0 0 1px var(--accent), var(--clickable-shadow);
}
.nt-agent.disabled {
  opacity: 0.55;
  cursor: default;
}
.nt-agent-name {
  font-size: 13px;
  font-weight: 600;
}
.nt-agent-ok {
  font-size: 11px;
  color: #248a3d;
}
.nt-agent-no {
  font-size: 11px;
  color: var(--text-tertiary);
}
.nt-foot {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 16px 12px;
  border-top: var(--hairline) solid var(--border);
}
.nt-hint {
  font-size: 11.5px;
  color: var(--text-tertiary);
  margin: 0;
}

/* ── Toast ── */
.toast {
  position: fixed;
  left: 50%;
  bottom: 26px;
  transform: translateX(-50%);
  padding: 7px 16px;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text-primary) 85%, transparent);
  color: var(--bg);
  font-size: 12.5px;
  font-weight: 500;
  box-shadow: 0 4px 18px rgba(0, 0, 0, 0.25);
  z-index: 50;
}
.toast-enter-active,
.toast-leave-active {
  transition: opacity 0.18s ease, transform 0.18s ease;
}
.toast-enter-from,
.toast-leave-to {
  opacity: 0;
  transform: translateX(-50%) translateY(6px);
}
</style>

<style>
/* 工作中指示器颜色（按主题）：深色＝近白带一点绿；浅色＝更深的灰绿。 */
:root,
.theme-light {
  --ind-working: color-mix(in srgb, #248a3d 70%, #3c3c3e);
}
.theme-dark {
  --ind-working: color-mix(in srgb, #30d158 42%, #f5f5f7);
}
</style>
