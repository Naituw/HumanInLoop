<script setup lang="ts">
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { startDrag } from "@crabnebula/tauri-plugin-drag";
import MarkdownContent from "./MarkdownContent.vue";
import {
  closePreview,
  fileIconDataUrl,
  openPath,
  previewAttachments,
  readImageDataUrl,
  showAttachmentMenu,
} from "../lib/ipc";
import type { FileAttachment, HistoryAnswer, HistoryEntry } from "../lib/types";
import {
  agentKindOf,
  customSourceOf,
  historySessionOf,
  shortSessionId,
  workspaceNameOf,
} from "../lib/history";

const props = defineProps<{ entry: HistoryEntry; sessionTitle?: string }>();
const { t, locale } = useI18n();

// —— 来源 meta（agent 家族 / 自定义来源名 / workspace）——
const agentLabel = computed(() => {
  const k = agentKindOf(props.entry);
  if (!k) return "";
  const label = t(`agents.kind.${k}`);
  return label === `agents.kind.${k}` ? k : label;
});
const customSource = computed(() => customSourceOf(props.entry, agentLabel.value));
const workspaceName = computed(() => workspaceNameOf(props.entry));
const sessionRef = computed(() => historySessionOf(props.entry));
const sessionLabel = computed(() => {
  const session = sessionRef.value;
  if (session.type === "agent") {
    const id = shortSessionId(session.sessionId);
    return props.sessionTitle ? `${props.sessionTitle} · ${id}` : id;
  }
  if (session.type === "mcp") {
    return t("history.sessionMcpDetail", { id: shortSessionId(session.instanceId) });
  }
  return "";
});
const sessionTooltip = computed(() => {
  const session = sessionRef.value;
  if (session.type === "agent") {
    return [agentLabel.value, props.sessionTitle, session.sessionId].filter(Boolean).join(" · ");
  }
  if (session.type === "mcp") {
    return `${t("history.sessionMcpFull")} · ${session.instanceId} · ${session.project}`;
  }
  return "";
});
const sessionPlainText = computed(() => {
  const session = sessionRef.value;
  if (session.type === "agent") {
    return [props.sessionTitle, session.sessionId].filter(Boolean).join(" · ");
  }
  if (session.type === "mcp") {
    return `${t("history.sessionMcpFull")} · ${session.instanceId}`;
  }
  return "";
});
const hasMeta = computed(
  () =>
    !!agentLabel.value ||
    !!customSource.value ||
    !!sessionLabel.value ||
    !!workspaceName.value
);

function openWorkspace() {
  if (props.entry.project) openPath(props.entry.project).catch(() => {});
}

const isMulti = computed(() => props.entry.questions.length > 1);
const isCancel = computed(() => props.entry.action === "cancel");

const showMessage = computed(
  () =>
    props.entry.message.text.trim() !== "" ||
    props.entry.message.files.length > 0
);

function channelName(id: string): string {
  const key = `history.channel.${id}`;
  const name = t(key);
  return name === key ? t("history.channel.unknown") : name;
}

const statusText = computed(() => {
  const ch = channelName(props.entry.channel);
  return isCancel.value
    ? t("history.statusCancelled", { channel: ch })
    : t("history.statusSubmitted", { channel: ch });
});

const absoluteTime = computed(() => {
  const d = new Date(props.entry.timestampMs);
  try {
    return new Intl.DateTimeFormat(locale.value, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(d);
  } catch {
    return d.toLocaleString();
  }
});

function answerOf(i: number): HistoryAnswer | null {
  return props.entry.answers[i] ?? null;
}

// —— 复制整条记录为纯文本（便于直接粘贴给 agent）——
const copied = ref(false);

function buildPlainText(): string {
  const e = props.entry;
  const lines: string[] = [];
  lines.push(`${statusText.value} · ${absoluteTime.value}`);
  const meta: string[] = [];
  if (agentLabel.value) meta.push(agentLabel.value);
  if (customSource.value) meta.push(customSource.value);
  if (sessionPlainText.value) {
    meta.push(t("history.copySession", { session: sessionPlainText.value }));
  }
  if (e.project) meta.push(e.project);
  if (meta.length) lines.push(meta.join(" · "));

  if (e.message.text.trim()) {
    lines.push("", t("history.copyMessage"), e.message.text.trimEnd());
  }
  if (e.message.files.length) {
    lines.push("", t("history.attachments", { n: e.message.files.length }));
    for (const f of e.message.files) lines.push(`- ${f.name}`);
  }

  e.questions.forEach((q, i) => {
    lines.push("");
    lines.push(
      isMulti.value
        ? t("history.questionIndexed", { i: i + 1, n: e.questions.length })
        : t("history.question")
    );
    if (q.message.trim()) lines.push(q.message.trimEnd());

    if (q.predefinedOptions.length) {
      const a = answerOf(i);
      const sel = a?.selectedOptions ?? [];
      lines.push(t("history.copyOptions"));
      for (const opt of q.predefinedOptions) {
        const mark = sel.includes(opt.text) ? "x" : " ";
        const rec = opt.recommended ? ` (${t("popup.recommended")})` : "";
        lines.push(`- [${mark}] ${opt.text}${rec}`);
      }
    }
    const a = answerOf(i);
    if (isAnswerEmpty(a)) {
      lines.push(t("history.unanswered"));
      return;
    }
    if ((a?.userInput ?? "").trim()) {
      lines.push(t("history.copyReply"), (a?.userInput ?? "").trimEnd());
    }
    if ((a?.images ?? []).length) {
      lines.push(t("history.copyReplyImages"));
      for (const img of a?.images ?? []) lines.push(`- ${img}`);
    }
    if ((a?.files ?? []).length) {
      lines.push(t("history.copyReplyFiles"));
      for (const f of a?.files ?? []) lines.push(`- ${f}`);
    }
  });

  return lines.join("\n");
}

async function copyEntry() {
  try {
    await navigator.clipboard.writeText(buildPlainText());
  } catch {
    /* 忽略：剪贴板不可用时静默 */
  }
  copied.value = true;
  setTimeout(() => (copied.value = false), 1500);
}

function isAnswerEmpty(a: HistoryAnswer | null): boolean {
  if (!a) return true;
  return (
    a.selectedOptions.length === 0 &&
    (a.userInput ?? "").trim() === "" &&
    a.images.length === 0 &&
    a.files.length === 0
  );
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function open(path: string) {
  openPath(path).catch(() => {});
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

// —— Message 附件交互（与弹窗一致：选中 / 空格预览 + 方向键切换 / 双击打开 / 右键菜单 / 拖出）——
const attachments = computed(() => props.entry.message.files);
const selectedFile = ref<number | null>(null);
const attRefs = ref<HTMLElement[]>([]);
const dragIcons = ref<Record<string, string>>({});
const previewing = ref(false);

function setAttRef(el: Element | null, i: number) {
  if (el) attRefs.value[i] = el as HTMLElement;
}

function focusAttachment(index: number) {
  selectedFile.value = index;
  attRefs.value[index]?.focus();
}

function selectFile(index: number) {
  focusAttachment(index);
}

function previewSelected(index: number) {
  focusAttachment(index);
  previewing.value = true;
  previewAttachments(
    attachments.value.map((f) => f.path),
    index
  ).catch(() => {});
}

function stopPreview() {
  if (!previewing.value) return;
  previewing.value = false;
  closePreview().catch(() => {});
}

function onAttachmentContextMenu(file: FileAttachment, i: number, e: MouseEvent) {
  e.preventDefault();
  selectFile(i);
  showAttachmentMenu(file.path).catch(() => {});
}

function onAttachmentDragStart(file: FileAttachment, e: DragEvent) {
  e.preventDefault();
  const icon = dragIcons.value[file.path] || thumbs.value[file.path] || "";
  startDrag({ item: [file.path], icon }, () => {}).catch(() => {});
}

function handleAttachmentKey(e: KeyboardEvent): boolean {
  if (!attachments.value.length) return false;
  const i = selectedFile.value;
  if (i === null) return false;
  if (e.key === "Enter") {
    open(attachments.value[i].path);
  } else if (e.key === " ") {
    previewSelected(i);
  } else if (e.key === "ArrowRight" || e.key === "ArrowDown") {
    if (i < attachments.value.length - 1) focusAttachment(i + 1);
  } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
    if (i > 0) focusAttachment(i - 1);
  } else {
    return false;
  }
  e.preventDefault();
  return true;
}

function onKeydown(e: KeyboardEvent) {
  handleAttachmentKey(e);
}

// 点击附件以外区域：取消选中并关闭预览。
function onDetailClick(e: MouseEvent) {
  if ((e.target as HTMLElement).closest(".attachment")) return;
  if (selectedFile.value !== null) selectedFile.value = null;
  stopPreview();
}

async function loadDragIcons() {
  for (const f of attachments.value) {
    if (dragIcons.value[f.path]) continue;
    try {
      dragIcons.value[f.path] = await fileIconDataUrl(f.path);
    } catch {
      /* 取图标失败：拖出时回退缩略图或无预览 */
    }
  }
}

let unlistenIndex: UnlistenFn | null = null;
let unlistenClosed: UnlistenFn | null = null;

onMounted(async () => {
  window.addEventListener("keydown", onKeydown);
  unlistenIndex = await listen<number>("preview-index", (e) => {
    const i = e.payload;
    if (i >= 0 && i < attachments.value.length) {
      selectedFile.value = i;
      nextTick(() => {
        const el = attRefs.value[i];
        el?.focus();
        el?.scrollIntoView({ block: "nearest" });
      });
    }
  });
  unlistenClosed = await listen("preview-closed", () => {
    previewing.value = false;
    const i = selectedFile.value;
    if (i !== null) nextTick(() => attRefs.value[i]?.focus());
  });
  loadDragIcons();
});

onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKeydown);
  unlistenIndex?.();
  unlistenClosed?.();
  stopPreview();
});

// Image thumbnails: load best-effort; failures render a placeholder.
const thumbs = ref<Record<string, string>>({});
const failed = ref<Record<string, boolean>>({});

watch(
  () => props.entry,
  async (entry) => {
    thumbs.value = {};
    failed.value = {};
    const paths = entry.answers.flatMap((a) => a.images);
    const attachImgs = entry.message.files
      .filter((f) => f.isImage)
      .map((f) => f.path);
    for (const p of [...paths, ...attachImgs]) {
      if (thumbs.value[p] || failed.value[p]) continue;
      try {
        thumbs.value[p] = await readImageDataUrl(p);
      } catch {
        failed.value[p] = true;
      }
    }
  },
  { immediate: true }
);
</script>

<template>
  <div class="detail" @click="onDetailClick">
    <!-- Status banner -->
    <div class="status-banner" :class="{ cancel: isCancel }">
      <span class="status-dot"></span>
      <span class="status-text">{{ statusText }}</span>
      <span class="status-time">{{ absoluteTime }}</span>
      <button
        class="copy-btn"
        type="button"
        :title="t('history.copy')"
        @click.stop="copyEntry"
      >
        <svg v-if="copied" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
        <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2" /><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" /></svg>
        <span>{{ copied ? t("history.copied") : t("history.copy") }}</span>
      </button>
    </div>

    <!-- Source meta: agent family / session / custom source name / workspace -->
    <div v-if="hasMeta" class="meta-row">
      <span v-if="agentLabel" class="meta-chip">{{ agentLabel }}</span>
      <span
        v-if="sessionLabel"
        class="meta-chip meta-session"
        :title="sessionTooltip"
      >
        {{ t("history.sessionMeta", { value: sessionLabel }) }}
      </span>
      <span v-if="customSource" class="meta-chip">{{ customSource }}</span>
      <button
        v-if="workspaceName"
        type="button"
        class="meta-chip meta-workspace"
        :title="entry.project"
        @click.stop="openWorkspace"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 20h16a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13c0 1.1.9 2 2 2Z" /></svg>
        <span class="meta-ws-name">{{ workspaceName }}</span>
        <svg class="meta-arrow" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M7 17 17 7M8 7h9v9" /></svg>
      </button>
    </div>

    <!-- Shared message -->
    <template v-if="showMessage">
      <MarkdownContent
        v-if="entry.message.text && entry.isMarkdown"
        :source="entry.message.text"
      />
      <pre v-else-if="entry.message.text" class="plain-body">{{ entry.message.text }}</pre>

      <div v-if="attachments.length" class="attachments">
        <div class="att-caption">
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
            <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
          </svg>
          <span>{{ t("history.attachments", { n: attachments.length }) }}</span>
        </div>
        <div class="att-list">
          <div
            v-for="(file, i) in attachments"
            :key="file.path"
            :ref="(el) => setAttRef(el as Element | null, i)"
            class="attachment"
            :class="{ selected: selectedFile === i }"
            tabindex="0"
            draggable="true"
            :title="file.path"
            @click="selectFile(i)"
            @dblclick="open(file.path)"
            @dragstart="onAttachmentDragStart(file, $event)"
            @contextmenu="onAttachmentContextMenu(file, i, $event)"
          >
            <span class="att-icon" :class="{ 'is-image': file.isImage && thumbs[file.path] }">
              <img v-if="file.isImage && thumbs[file.path]" :src="thumbs[file.path]" alt="" />
              <svg v-else viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round">
                <path d="M14 3v4a1 1 0 0 0 1 1h4" />
                <path d="M17 21H7a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h7l5 5v11a2 2 0 0 1-2 2z" />
              </svg>
            </span>
            <span class="att-meta">
              <span class="att-name">{{ file.name }}</span>
              <span class="att-size">{{ formatBytes(file.size) }}</span>
            </span>
          </div>
        </div>
      </div>
    </template>

    <!-- Per-question + answer (read-only). Cancelled requests still show the original prompt. -->
    <div
      v-for="(q, i) in entry.questions"
      :key="i"
      class="q-block"
      :class="{ 'with-divider': showMessage || isMulti || i > 0 }"
    >
      <div class="q-header">
        <span class="q-label">{{
          isMulti
            ? t("history.questionIndexed", { i: i + 1, n: entry.questions.length })
            : t("history.question")
        }}</span>
      </div>

      <MarkdownContent
        v-if="entry.isMarkdown && q.message"
        :source="q.message"
      />
      <pre v-else-if="q.message" class="plain-body">{{ q.message }}</pre>

      <!-- Options (selected highlighted, read-only) -->
      <div v-if="q.predefinedOptions.length" class="options">
        <div
          v-for="(opt, oi) in q.predefinedOptions"
          :key="oi"
          class="option"
          :class="{ selected: (answerOf(i)?.selectedOptions ?? []).includes(opt.text) }"
        >
          <span class="check">{{ (answerOf(i)?.selectedOptions ?? []).includes(opt.text) ? "✓" : "" }}</span>
          <span class="label"><span v-if="opt.recommended" class="rec-badge"><span class="rec-badge-pill"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 9V5a3 3 0 0 0-3-3l-4 9v11h11.28a2 2 0 0 0 2-1.7l1.38-9a2 2 0 0 0-2-2.3z"></path><path d="M7 22H4a2 2 0 0 1-2-2v-7a2 2 0 0 1 2-2h3"></path></svg>{{ t("popup.recommended") }}</span></span>{{ opt.text }}</span>
        </div>
      </div>

      <!-- Unanswered -->
      <p v-if="isAnswerEmpty(answerOf(i))" class="unanswered">{{ t("history.unanswered") }}</p>

      <template v-else>
        <!-- Reply text -->
        <div v-if="(answerOf(i)?.userInput ?? '').trim()" class="reply-block">
          <div class="reply-caption">{{ t("history.reply") }}</div>
          <pre class="reply-text">{{ answerOf(i)?.userInput }}</pre>
        </div>

        <!-- Reply images -->
        <div v-if="(answerOf(i)?.images ?? []).length" class="thumbs">
          <div v-for="(img, ii) in answerOf(i)?.images ?? []" :key="ii" class="thumb" :title="img" @click="open(img)">
            <img v-if="thumbs[img]" :src="thumbs[img]" alt="" />
            <div v-else class="thumb-missing">{{ t("history.imageUnavailable") }}</div>
          </div>
        </div>

        <!-- Reply files -->
        <div v-if="(answerOf(i)?.files ?? []).length" class="reply-files">
          <div
            v-for="(f, fi) in answerOf(i)?.files ?? []"
            :key="fi"
            class="reply-file"
            :title="f"
            @click="open(f)"
          >
            <span class="rf-icon">📄</span>
            <span class="rf-name">{{ fileName(f) }}</span>
          </div>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
  padding: var(--space-4);
}
/* Status banner */
.status-banner {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 12px;
  border-radius: var(--radius-sm, 8px);
  background: color-mix(in srgb, var(--accent) 12%, transparent);
  color: var(--text-primary);
  font-size: 13px;
  font-weight: 600;
}
.status-banner.cancel {
  background: color-mix(in srgb, #ff453a 14%, transparent);
}
.status-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: #30d158;
  flex: 0 0 auto;
}
.status-banner.cancel .status-dot {
  background: #ff453a;
}
.status-text {
  flex: 1 1 auto;
}
.status-time {
  font-weight: 500;
  color: var(--text-secondary);
  font-variant-numeric: tabular-nums;
}
.copy-btn {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  height: 24px;
  padding: 0 9px;
  border: 1px solid transparent;
  border-radius: 999px;
  background: var(--bg-elevated);
  color: var(--text-primary);
  font-size: 11px;
  font-weight: 600;
  cursor: pointer;
}
.copy-btn:hover {
  background: color-mix(in srgb, var(--text-primary) 8%, var(--bg-elevated));
}
.copy-btn svg {
  width: 12px;
  height: 12px;
}
/* Source meta chips */
.meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: -4px;
}
.meta-chip {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  max-width: 260px;
  /* 固定高度 + line-height:1：span（继承正文行高）与 button（UA 默认行高）混排时保持等高。 */
  height: 20px;
  padding: 0 9px;
  border: none;
  border-radius: 999px;
  background: color-mix(in srgb, var(--text-primary) 8%, transparent);
  color: var(--text-secondary);
  font-size: 11px;
  font-weight: 600;
  line-height: 1;
  white-space: nowrap;
}
.meta-chip svg {
  flex: 0 0 auto;
  width: 11px;
  height: 11px;
}
.meta-session {
  max-width: 320px;
  overflow: hidden;
  text-overflow: ellipsis;
}
.meta-workspace {
  cursor: pointer;
}
.meta-workspace:hover {
  background: color-mix(in srgb, var(--text-primary) 14%, transparent);
}
.meta-ws-name {
  overflow: hidden;
  text-overflow: ellipsis;
}
.meta-arrow {
  opacity: 0.7;
}
/* Plain (non-markdown) message / question body */
.plain-body {
  margin: 0;
  font-family: inherit;
  font-size: 14px;
  color: var(--text-primary);
  white-space: pre-wrap;
  word-break: break-word;
}
/* Attachments */
.attachments {
  display: flex;
  flex-direction: column;
  gap: 7px;
}
.att-caption {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.3px;
  color: var(--text-secondary);
}
.att-caption svg {
  width: 13px;
  height: 13px;
}
.att-list {
  display: flex;
  flex-flow: row wrap;
  gap: 8px;
}
.attachment {
  display: inline-flex;
  align-items: center;
  gap: 9px;
  max-width: 100%;
  padding: 5px 12px 5px 5px;
  border: 1px solid transparent;
  border-radius: 999px;
  background: var(--control-bg);
  cursor: default;
  outline: none;
  box-shadow: var(--clickable-shadow);
}
.attachment:hover {
  background: var(--control-hover-bg);
}
.attachment.selected,
.attachment:focus-visible {
  box-shadow: 0 0 0 2px var(--accent);
}
.att-icon {
  flex: 0 0 auto;
  width: 28px;
  height: 28px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  color: var(--accent);
  border-radius: 50%;
  overflow: hidden;
  background: color-mix(in srgb, var(--accent) 16%, transparent);
}
.att-icon.is-image {
  border-radius: 7px;
  background: var(--card-bg);
}
.att-icon img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.att-icon svg {
  width: 15px;
  height: 15px;
}
.att-meta {
  display: inline-flex;
  flex-direction: column;
  min-width: 0;
  gap: 1px;
}
.att-name {
  font-size: 13px;
  color: var(--text-primary);
  max-width: 200px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.att-size {
  font-size: 11px;
  color: var(--text-secondary);
}
/* Question block */
.q-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.q-block.with-divider {
  margin-top: 4px;
  padding-top: 14px;
  border-top: var(--hairline) solid var(--border);
}
.q-header {
  font-size: 14px;
  font-weight: 600;
  color: var(--text-primary);
}
.q-label {
  color: var(--accent);
}
/* `.option` / `.check` / `.label` reuse the shared styles in controls.css
   (checkbox-style box: empty when unselected, accent square + white ✓ when selected),
   so the read-only detail matches the live popup exactly. */
.options {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
/* Reply text */
.reply-block {
  display: flex;
  flex-direction: column;
  gap: 5px;
}
.reply-caption,
.unanswered {
  font-size: 11px;
  font-weight: 600;
  letter-spacing: 0.3px;
  color: var(--text-secondary);
}
.unanswered {
  font-style: italic;
}
.reply-text {
  margin: 0;
  padding: 10px 12px;
  border-radius: var(--radius-sm, 8px);
  background: var(--bg-elevated);
  border: 1px solid transparent;
  font-family: inherit;
  font-size: 13px;
  color: var(--text-primary);
  white-space: pre-wrap;
  word-break: break-word;
}
/* Thumbs */
.thumbs {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
}
.thumb {
  width: 72px;
  height: 72px;
  border-radius: var(--radius-sm, 8px);
  overflow: hidden;
  border: var(--hairline) solid var(--border);
  background: var(--bg-elevated);
  cursor: default;
}
.thumb img {
  width: 100%;
  height: 100%;
  object-fit: cover;
}
.thumb-missing {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  text-align: center;
  font-size: 10px;
  color: var(--text-secondary);
  padding: 4px;
}
/* Reply files */
.reply-files {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.reply-file {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  max-width: 240px;
  padding: 4px 10px 4px 8px;
  border-radius: var(--radius-sm, 8px);
  border: 1px solid transparent;
  background: var(--bg-elevated);
  font-size: 12px;
  cursor: default;
}
.reply-file .rf-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  color: var(--text-primary);
}
</style>
