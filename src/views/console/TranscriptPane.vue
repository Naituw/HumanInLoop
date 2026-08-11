<script setup lang="ts">
// 完整会话视图（spec gui-agent-console C14）：进入定位最新、每页 200 条向上分页
// （滚动位置锚定）、AskHuman 结构化问答卡（多问题 Q1/Qn 子块、长 message 折叠）。
// 本组件自身是滚动容器（占满详情区正文）。
import { nextTick, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import MarkdownContent from "../../components/MarkdownContent.vue";
import { consoleTranscript } from "../../lib/ipc";
import type { TranscriptEventJson } from "../../lib/types";

const { t } = useI18n();

const props = defineProps<{
  kind: string;
  sessionId: string;
}>();

const emit = defineEmits<{ close: [] }>();

const events = ref<TranscriptEventJson[]>([]);
/** 已加载窗口首事件的绝对下标。 */
const start = ref(0);
const total = ref(0);
const truncatedHead = ref(false);
const loading = ref(false);
const error = ref("");
const root = ref<HTMLElement | null>(null);
const stickToBottom = ref(true);

// 长 message 折叠：按事件绝对下标记录展开态。
const expanded = ref<Set<number>>(new Set());

function isLongAsk(text: string): boolean {
  return text.length > 220;
}

function toggleExpand(key: number): void {
  const next = new Set(expanded.value);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  expanded.value = next;
}

async function loadLatest(): Promise<void> {
  loading.value = true;
  error.value = "";
  try {
    const page = await consoleTranscript(props.kind, props.sessionId, null);
    events.value = page.events;
    start.value = page.start;
    total.value = page.total;
    truncatedHead.value = page.truncatedHead;
    await nextTick();
    if (root.value) root.value.scrollTop = root.value.scrollHeight; // 进入即定位到最新
    stickToBottom.value = true;
  } catch (err) {
    error.value = String(err);
  } finally {
    loading.value = false;
  }
}

async function loadOlder(): Promise<void> {
  if (loading.value || start.value === 0) return;
  loading.value = true;
  const el = root.value;
  const prevHeight = el?.scrollHeight ?? 0;
  const prevTop = el?.scrollTop ?? 0;
  try {
    const page = await consoleTranscript(props.kind, props.sessionId, start.value);
    events.value = [...page.events, ...events.value];
    start.value = page.start;
    total.value = page.total;
    truncatedHead.value = page.truncatedHead;
    await nextTick();
    // 锚定：新内容插入顶部后，视野停在原内容处。
    if (el) el.scrollTop = el.scrollHeight - prevHeight + prevTop;
  } catch (err) {
    error.value = String(err);
  } finally {
    loading.value = false;
  }
}

function clockTime(secs?: number | null, label?: string | null): string {
  if (label) return label;
  if (!secs) return "";
  const d = new Date(secs * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

function thinkingSnippet(text: string): string {
  const one = [...text].slice(0, 120).join("");
  return one.length < text.length ? `${one}…` : one;
}

function onScroll(): void {
  const element = root.value;
  if (!element) return;
  stickToBottom.value =
    element.scrollHeight - element.scrollTop - element.clientHeight < 80;
}

function onMarkdownUpdated(event: Event): void {
  const element = root.value;
  if (!element) return;
  if (stickToBottom.value) {
    void nextTick(() => {
      if (root.value) root.value.scrollTop = root.value.scrollHeight;
    });
    return;
  }

  const detail = (event as CustomEvent<{ heightDelta?: number }>).detail;
  const delta = detail?.heightDelta ?? 0;
  const changed = event.target as HTMLElement | null;
  if (!delta || !changed) return;
  const viewport = element.getBoundingClientRect();
  const changedRect = changed.getBoundingClientRect();
  if (changedRect.bottom <= viewport.top) element.scrollTop += delta;
}

onMounted(loadLatest);
watch(() => props.sessionId, loadLatest);
</script>

<template>
  <div
    ref="root"
    class="tx-root"
    @scroll.passive="onScroll"
    @markdown-content-updated="onMarkdownUpdated"
  >
    <div class="tx-bar">
      <span class="tx-count">{{
        t("console.tx.loaded", { loaded: events.length, total })
      }}</span>
      <button class="tx-btn" @click="emit('close')">
        <svg viewBox="0 0 14 14"><path d="M8.5 3.5 L4.5 7 L8.5 10.5" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
        {{ t("console.tx.back") }}
      </button>
    </div>

    <div v-if="error" class="tx-error">{{ t("console.tx.error") }} · {{ error }}</div>

    <div class="tx-top">
      <button v-if="start > 0" class="load-btn" :disabled="loading" @click="loadOlder">
        {{ t("console.tx.loadOlder", { n: Math.min(200, start), r: start }) }}
      </button>
      <span v-else-if="truncatedHead" class="tx-begin">{{ t("console.tx.truncated") }}</span>
      <span v-else-if="!loading && events.length" class="tx-begin">{{ t("console.tx.begin") }}</span>
      <span v-if="loading" class="tx-begin">{{ t("console.tx.loading") }}</span>
    </div>

    <div class="tx-list">
      <template v-for="(e, i) in events" :key="start + i">
        <div v-if="e.type === 'user'" class="tx-user">
          <div class="tx-user-head">
            <span class="tx-role">{{ t("console.tx.you") }}</span>
            <span class="tx-time">{{ clockTime(e.at, e.atLabel) }}</span>
          </div>
          <div class="tx-user-text">{{ e.text }}</div>
        </div>

        <MarkdownContent
          v-else-if="e.type === 'assistant'"
          class="tx-assistant"
          :source="e.text"
          lazy-mermaid
        />

        <div v-else-if="e.type === 'thinking'" class="tx-thinking">
          {{ thinkingSnippet(e.text) }}
        </div>

        <div v-else-if="e.type === 'ask'" class="tx-ask">
          <div class="tx-ask-head">
            <span class="tx-ask-badge">🙋 AskHuman</span>
            <span class="tx-time">{{ clockTime(e.at, e.atLabel) }}</span>
          </div>
          <div v-if="e.message" class="tx-ask-qtext">
            <MarkdownContent
              class="ask-md"
              :class="{ clamped: isLongAsk(e.message) && !expanded.has(start + i) }"
              :source="e.message"
              lazy-mermaid
            />
            <button v-if="isLongAsk(e.message)" class="ask-expand" @click="toggleExpand(start + i)">
              {{ expanded.has(start + i) ? t("console.tx.collapse") : t("console.tx.expand") }}
            </button>
          </div>
          <template v-for="(qa, qi) in e.questions" :key="qi">
            <div v-if="qa.text || e.kind === 'whatsNext'" class="tx-ask-sub">
              <div class="tx-ask-subq">
                <span v-if="e.kind !== 'whatsNext'" class="tx-ask-qn">Q{{ qi + 1 }}</span>
                <span>{{ qa.text || t("console.tx.whatsNextQuestion") }}</span>
              </div>
              <div v-if="qa.answer" class="tx-ask-a">
                <span class="tx-role">{{ t("console.tx.you") }}</span>
                <span class="tx-ask-atext">{{ qa.answer }}</span>
              </div>
              <div v-else class="tx-ask-a pending-a">{{ t("console.tx.unanswered") }}</div>
            </div>
            <template v-else>
              <div v-if="qa.answer" class="tx-ask-a bordered">
                <span class="tx-role">{{ t("console.tx.you") }}</span>
                <span class="tx-ask-atext">{{ qa.answer }}</span>
              </div>
              <div v-else class="tx-ask-a pending-a bordered">{{ t("console.tx.unanswered") }}</div>
            </template>
          </template>
        </div>

        <div v-else-if="e.type === 'tool'" class="tx-step">
          <span class="step-dot" :class="e.isError ? 'failed' : 'done'" />
          <span class="step-label">{{ e.label }}</span>
          <span v-if="e.object" class="step-obj">{{ e.object }}</span>
        </div>

        <div v-else class="tx-meta">{{ e.text }}</div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.tx-root {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 0 16px 14px;
}
.tx-bar {
  position: sticky;
  top: 0;
  z-index: 2;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 10px 0 8px;
  background: var(--bg);
}
.tx-count {
  font-size: 11px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  color: var(--text-tertiary);
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
.tx-error {
  font-size: 12px;
  color: #ff453a;
  padding: 4px 0 8px;
}
.tx-top {
  display: flex;
  justify-content: center;
  padding: 2px 0 12px;
}
.load-btn {
  appearance: none;
  border: var(--hairline) solid var(--control-border);
  background: var(--control-bg);
  box-shadow: var(--clickable-shadow);
  color: var(--text-primary);
  font-size: 11.5px;
  font-weight: 600;
  padding: 3px 12px;
  border-radius: 999px;
  cursor: pointer;
}
.load-btn:disabled {
  opacity: 0.5;
  cursor: default;
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
  white-space: pre-wrap;
  word-break: break-word;
}
.tx-assistant {
  font-size: 13px;
  margin: 2px 0;
}
.tx-thinking {
  font-size: 12px;
  font-style: italic;
  color: var(--text-tertiary);
}
.tx-ask {
  margin: 6px 0 2px;
  border: var(--hairline) solid color-mix(in srgb, var(--accent) 30%, transparent);
  border-radius: 9px;
  overflow: hidden;
}
.tx-ask-head {
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
.tx-ask-a.bordered {
  border-top: var(--hairline) solid color-mix(in srgb, var(--accent) 16%, transparent);
}
.tx-ask-atext {
  font-size: 13px;
  white-space: pre-wrap;
  word-break: break-word;
}
.tx-ask-a.pending-a {
  font-size: 12px;
  color: var(--text-tertiary);
}
.tx-step {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12.5px;
}
.step-dot {
  flex: 0 0 auto;
  width: 7px;
  height: 7px;
  border-radius: 50%;
}
.step-dot.done {
  background: var(--text-tertiary);
}
.step-dot.failed {
  background: #ff453a;
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
.tx-meta {
  font-size: 11.5px;
  font-style: italic;
  color: var(--text-tertiary);
}
</style>
