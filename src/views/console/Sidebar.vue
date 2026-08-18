<script setup lang="ts">
// 控制台边栏（spec gui-agent-console C5/C6/R3）：项目分组会话 + 「最近项目」折叠区 + 「＋」。
import { ref } from "vue";
import { useI18n } from "vue-i18n";
import type { AgentRecord } from "../../lib/types";
import StatusInd from "./StatusInd.vue";
import { indState, type ProjectGroup, UNKNOWN_PROJECT_KEY } from "./model";

const { t } = useI18n();

const props = defineProps<{
  groups: ProjectGroup[];
  /** 无活跃会话的最近项目（workspace 索引）。 */
  recent: { path: string; label: string }[];
  selectedId: string | null;
  /** Whether a supported platform terminal is available for the plus entry. */
  newTaskSupported: boolean;
  /** 项目 key（git 根）→ 待办数。 */
  todoCounts: Record<string, number>;
  /** 相对时间重算锚点（父级 1s 心跳）。 */
  nowMs: number;
}>();

const emit = defineEmits<{
  select: [sessionId: string];
  plus: [projectPath: string];
  /** 点击项目头待办徽标：打开该项目的待办窗口（待办是项目级的）。 */
  todos: [projectPath: string];
}>();

// 分组折叠（localStorage 持久化；key = 分组 key）。
const COLLAPSE_KEY = "askhuman.console.collapsed";
function loadCollapsed(): Set<string> {
  try {
    const v = localStorage.getItem(COLLAPSE_KEY);
    if (v) return new Set(JSON.parse(v) as string[]);
  } catch {
    /* 忽略 */
  }
  return new Set();
}
const collapsed = ref<Set<string>>(loadCollapsed());
function toggleCollapse(key: string): void {
  const next = new Set(collapsed.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  collapsed.value = next;
  try {
    localStorage.setItem(COLLAPSE_KEY, JSON.stringify([...next]));
  } catch {
    /* 忽略持久化失败 */
  }
}

const RECENT_KEY = "__recent__";

function workingCount(g: ProjectGroup): number {
  return g.items.filter((a) => a.state === "working").length;
}

/** 组收起时仍显示工作中的会话（含等待回答；用户定案）——只收起安静的。 */
function visibleItems(g: ProjectGroup): AgentRecord[] {
  if (!collapsed.value.has(g.key)) return g.items;
  return g.items.filter((a) => a.state === "working");
}

/** 项目待办数（组 key 为 cwd，可能是 git 根的子目录：前缀匹配兜底）。 */
function todoCount(g: ProjectGroup): number {
  if (props.todoCounts[g.key] !== undefined) return props.todoCounts[g.key];
  for (const [key, count] of Object.entries(props.todoCounts)) {
    if (g.key === key || g.key.startsWith(`${key}/`)) return count;
  }
  return 0;
}

function kindLabel(kind: string): string {
  return t(`agents.kind.${kind}`);
}

function relativeTime(secs?: number | null): string {
  if (!secs) return "";
  const diff = Math.max(0, Math.floor(props.nowMs / 1000) - secs);
  if (diff < 5) return t("agents.time.justNow");
  if (diff < 60) return t("agents.time.secondsAgo", { n: diff });
  const min = Math.floor(diff / 60);
  if (min < 60) return t("agents.time.minutesAgo", { n: min });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t("agents.time.hoursAgo", { n: hr });
  return t("agents.time.daysAgo", { n: Math.floor(hr / 24) });
}

function rowTime(a: AgentRecord): string {
  return relativeTime(a.state === "ended" ? a.endedAt : a.lastActivity);
}

function forkParentLabel(a: AgentRecord): string {
  const parentId = a.forkedFromSessionId;
  if (!parentId) return "";
  const parent = props.groups
    .flatMap((group) => group.items)
    .find((candidate) => candidate.sessionId === parentId);
  return parent?.seq ? `#${parent.seq}` : parentId.slice(0, 8);
}
</script>

<template>
  <aside class="sidebar">
    <section v-for="g in groups" :key="g.key" class="group">
      <div class="proj-head">
        <button class="proj-toggle" @click="toggleCollapse(g.key)">
          <svg class="chevron" :class="{ closed: collapsed.has(g.key) }" viewBox="0 0 12 12">
            <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6"
              stroke-linecap="round" stroke-linejoin="round" />
          </svg>
          <span class="proj-name" :title="g.path || undefined">{{ g.label }}</span>
          <span v-if="workingCount(g)" class="badge badge-working">{{ workingCount(g) }}</span>
          <!-- 待办徽标：钉在组头行内（working 徽标之后），独立可点（打开该项目待办窗口）。
               嵌套在 toggle 按钮内故用 span + click.stop（避免非法嵌套 button）。 -->
          <span
            v-if="g.key !== UNKNOWN_PROJECT_KEY && todoCount(g)"
            class="badge badge-todo"
            role="button"
            :title="t('agents.openTodos')"
            :aria-label="t('agents.openTodos')"
            @click.stop="emit('todos', g.path)"
          >☑{{ todoCount(g) }}</span>
        </button>
        <button
          v-if="newTaskSupported && g.path"
          class="plus-btn"
          :title="t('console.newTask')"
          :aria-label="t('console.newTask')"
          @click="emit('plus', g.path)"
        >
          <svg viewBox="0 0 12 12"><path d="M6 2v8M2 6h8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
        </button>
      </div>
      <ul v-show="visibleItems(g).length || !collapsed.has(g.key)" class="sess-list">
        <li
          v-for="a in visibleItems(g)"
          :key="a.sessionId"
          class="sess"
          :class="{ selected: a.sessionId === selectedId, ended: a.state === 'ended' }"
          @click="emit('select', a.sessionId)"
        >
          <StatusInd :state="indState(a)" />
          <span class="sess-main">
            <span class="sess-title">{{ a.title || t("agents.untitled") }}</span>
            <span class="sess-sub">
              <span v-if="a.forkedFromSessionId" class="fork-mini">
                {{ t("agents.forkedFrom", { id: forkParentLabel(a) }) }}
              </span>
              <span v-if="a.forkedFromSessionId" aria-hidden="true"> · </span>
              {{ kindLabel(a.kind) }} · {{ rowTime(a) }}
              <span v-if="a.pendingInterject" class="ij-mini" :title="t('agents.pendingInterject')">✉</span>
            </span>
          </span>
        </li>
        <li
          v-if="!collapsed.has(g.key) && g.items.length === 0"
          class="sess-empty"
        >
          {{ t("console.noSessions") }}
        </li>
      </ul>
    </section>

    <!-- 最近项目折叠区（C5）-->
    <section v-if="newTaskSupported && recent.length" class="group recent">
      <div class="proj-head">
        <button class="proj-toggle" @click="toggleCollapse(RECENT_KEY)">
          <svg class="chevron" :class="{ closed: collapsed.has(RECENT_KEY) }" viewBox="0 0 12 12">
            <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6"
              stroke-linecap="round" stroke-linejoin="round" />
          </svg>
          <span class="proj-name secondary">{{ t("console.recentProjects") }}</span>
        </button>
      </div>
      <ul v-show="!collapsed.has(RECENT_KEY)" class="sess-list">
        <li v-for="p in recent" :key="p.path" class="recent-row">
          <span class="recent-name" :title="p.path">{{ p.label }}</span>
          <button
            class="plus-btn always"
            :title="t('console.newTask')"
            :aria-label="t('console.newTask')"
            @click="emit('plus', p.path)"
          >
            <svg viewBox="0 0 12 12"><path d="M6 2v8M2 6h8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/></svg>
          </button>
        </li>
      </ul>
    </section>
  </aside>
</template>

<style scoped>
.sidebar {
  flex: 0 0 248px;
  min-width: 0;
  overflow-y: auto;
  padding: 10px 8px 16px;
  border-right: var(--hairline) solid var(--border);
}
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
/* 待办徽标：组头行内的可点击元素（打开该项目的待办窗口），独立 hover 高亮。 */
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
.fork-mini {
  color: var(--accent);
  font-weight: 600;
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
</style>
