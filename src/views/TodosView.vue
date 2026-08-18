<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { applyTheme, fileToDataUrl } from "../lib/theme";
import {
  isWindows,
  primaryModifierPressed,
  primaryShortcutLabel,
} from "../lib/platform";
import { applyLanguage } from "../i18n";
import ComposerAttachments from "../components/ComposerAttachments.vue";
import TodoAttachmentList from "./todos/TodoAttachmentList.vue";
import {
  clipboardImageFiles,
  nextTodoSelection,
} from "./todos/todoInteraction";
import {
  openNewTask,
  readImageDataUrl,
  todosAdd,
  todosClear,
  todosComplete,
  todosHistory,
  todosHistoryClear,
  todosInit,
  todosList,
  todosProjects,
  todosProjectsEnriched,
  todosRemove,
  todosReorder,
  todosRestore,
  todosSetAuto,
  todosAttachPastedImages,
  todosUpdate,
  todosUpdateAttachments,
} from "../lib/ipc";
import type {
  PopupSubmitKey,
  ThemeMode,
  ImageAttachment,
  TodoDoneEntry,
  TodoEntry,
  TodoProjectInfo,
} from "../lib/types";

const { t, locale } = useI18n();

/** Same setting as the popup submit shortcut (`settings.popupBehavior.submitKey`). */
const popupSubmitKey = ref<PopupSubmitKey>("cmdEnter");
const submitWithBareEnter = computed(() => popupSubmitKey.value === "enter");
/** Badge on the Add button — matches the platform's popup submit shortcut. */
const submitKeyLabel = computed(() =>
  submitWithBareEnter.value ? "↵" : primaryShortcutLabel("enter")
);

function applyPopupSubmitKey(value: unknown): void {
  if (value === "enter" || value === "cmdEnter") {
    popupSubmitKey.value = value;
  }
}

// Show the create-task entry when the platform has a supported terminal.
const newTaskSupported = ref(false);

async function createTaskFrom(e: TodoEntry): Promise<void> {
  if (!selected.value) return;
  try {
    await openNewTask(selected.value, e.id);
  } catch (err) {
    console.warn("open new task failed", err);
  }
}

const projects = ref<TodoProjectInfo[]>([]);
const selected = ref<string>("");
const entries = ref<TodoEntry[]>([]);
const selectedTodoId = ref<string | null>(null);

/** Selector section: projects that currently have pending todos. */
const projectsWithTodos = computed(() =>
  projects.value.filter((p) => p.section === "withTodos")
);
/** Selector section: recent workspaces / live agents (keys already in withTodos excluded). */
const projectsRecent = computed(() =>
  projects.value.filter((p) => p.section === "recent")
);
// 首次加载完成前显示 Loading（避免空态闪现误导）。
const loaded = ref(false);
const newText = ref("");
const newFiles = ref<string[]>([]);
interface PastedImageDraft {
  key: string;
  image: ImageAttachment;
}
const newPastedImages = ref<PastedImageDraft[]>([]);
const newFileThumbs = ref<Record<string, string>>({});
const newAttachmentBusy = ref(false);
let pastedImageDraftSequence = 0;
interface NewComposerImage {
  key: string;
  data: string;
  filename: string;
  source: "file" | "pasted";
  sourceKey: string;
}
const newComposerImages = computed<NewComposerImage[]>(() => [
  ...newFiles.value.flatMap((path) => {
    const data = newFileThumbs.value[path];
    return data
      ? [
          {
            key: `file:${path}`,
            data,
            filename: draftName(path),
            source: "file" as const,
            sourceKey: path,
          },
        ]
      : [];
  }),
  ...newPastedImages.value.map((draft) => ({
    key: draft.key,
    data: draft.image.data,
    filename: draft.image.filename ?? t("todosWin.attachment"),
    source: "pasted" as const,
    sourceKey: draft.key,
  })),
]);
const newComposerFiles = computed(() =>
  newFiles.value
    .filter((path) => !newFileThumbs.value[path])
    .map((path) => ({ key: path, path, name: draftName(path) }))
);
const attachmentError = ref("");
const operationError = ref("");
// 清空确认改为模态 alert（第 18 轮定案：危险操作需要光标移动后再确认，且提示无法恢复）；
// 'todos'＝清空待办队列，'history'＝清空执行历史。
const confirmKind = ref<"todos" | "history" | null>(null);
// 防连点重复新增（Enter 连击）。
const adding = ref(false);
const addInputRef = ref<HTMLTextAreaElement | null>(null);

// Multi-line add box: ~2.5 lines default, grow to ~6 lines, then scroll inside.
const ADD_INPUT_MIN_PX = 56;
const ADD_INPUT_MAX_PX = 132;

// 带预选项目打开（托盘 agent「添加待办」/ AgentsView 入口）＝为该项目记想法而来 →
// 自动聚焦新增输入框（footer 在数据加载后才渲染，故 nextTick）。
async function focusAddInput(): Promise<void> {
  await nextTick();
  addInputRef.value?.focus();
  syncAddInputHeight();
}

function syncTextareaHeight(
  el: HTMLTextAreaElement | null | undefined,
  minPx: number,
  maxPx: number
): void {
  if (!el) return;
  // height:auto + overflow:hidden so scrollHeight is the full content height
  // (min-height / fixed height would clamp the measurement).
  el.style.height = "auto";
  el.style.overflow = "hidden";
  const content = el.scrollHeight;
  el.style.overflow = "auto";
  el.style.height = `${Math.min(maxPx, Math.max(minPx, content))}px`;
}

function syncAddInputHeight(): void {
  syncTextareaHeight(addInputRef.value, ADD_INPUT_MIN_PX, ADD_INPUT_MAX_PX);
}

function basename(key: string): string {
  const parts = key.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || key;
}

function agentLabel(kind?: string | null): string {
  if (!kind) return "";
  const key = `agents.kind.${kind}`;
  const label = t(key);
  return label === key ? kind : label;
}

/** Apply a project candidate list without clobbering an existing selection. */
function applyProjects(list: TodoProjectInfo[]): void {
  // 预选项目不在候选中（如 daemon 刚退、agent 项目未入 workspace 索引）→ 兜底追加，保持选中稳定。
  if (selected.value && !list.some((p) => p.key === selected.value)) {
    list = list.concat({
      key: selected.value,
      name: basename(selected.value),
      count: 0,
      section: "recent",
    });
  }
  projects.value = list;
  if (!selected.value) {
    selected.value = list[0]?.key ?? "";
  }
}

/** Local fast path: todos.json + workspaces only (no daemon). */
async function reloadProjectsLocal(): Promise<void> {
  const list = await todosProjects();
  applyProjects(list);
}

/**
 * Background enrich: merge live agent projects. Safe to call after first paint;
 * keeps current selection if still present.
 */
async function enrichProjectsInBackground(): Promise<void> {
  try {
    const list = await todosProjectsEnriched();
    const prev = selected.value;
    applyProjects(list);
    // Prefer keeping the user's selection when the enriched list still has it.
    if (prev && list.some((p) => p.key === prev)) {
      selected.value = prev;
    }
  } catch (err) {
    console.warn("todos projects enrich failed", err);
  }
}

async function reloadEntries(): Promise<void> {
  if (!selected.value) {
    entries.value = [];
    history.value = [];
    return;
  }
  [entries.value, history.value] = await Promise.all([
    todosList(selected.value),
    todosHistory(selected.value),
  ]);
  if (
    selectedTodoId.value &&
    !entries.value.some((entry) => entry.id === selectedTodoId.value)
  ) {
    selectedTodoId.value = null;
  }
  const pendingAttachmentRemoval = confirmAttachmentRemoval.value;
  if (
    pendingAttachmentRemoval &&
    !entries.value.some(
      (entry) =>
        entry.id === pendingAttachmentRemoval.todoId &&
        (entry.attachments ?? []).some(
          (attachment) => attachment.id === pendingAttachmentRemoval.attachmentId
        )
    )
  ) {
    confirmAttachmentRemoval.value = null;
  }
  if (editingId.value && !entries.value.some((entry) => entry.id === editingId.value)) {
    cancelEdit();
  }
  // Drop optimistic complete state for ids that no longer exist in the queue.
  for (const id of [...pendingCompleteTimers.keys()]) {
    if (!entries.value.some((e) => e.id === id)) {
      clearPendingComplete(id);
    }
  }
}

async function reloadAll(): Promise<void> {
  try {
    // Phase 1: local lists only so the window / dropdown is interactive immediately.
    await reloadProjectsLocal();
    await reloadEntries();
  } catch (err) {
    console.warn("todos reload failed", err);
  } finally {
    // Always clear the first-load spinner, even when a command rejects or times out.
    loaded.value = true;
  }
  // Phase 2: agent projects in the background (may take a few hundred ms).
  void enrichProjectsInBackground();
}

async function onSelect(): Promise<void> {
  confirmKind.value = null;
  confirmDeleteId.value = null;
  operationError.value = "";
  selectedTodoId.value = null;
  confirmAttachmentRemoval.value = null;
  cancelEdit();
  histOpen.value = false;
  clearAllPendingComplete();
  await reloadEntries();
}

// 添加行「自动执行」开关（第 17 轮定案）：勾选后新增的待办在 whats-next 时直接派发。
const newAuto = ref(false);
const newAutoHintHovered = ref(false);
const newAutoHintKeyboardFocused = ref(false);
const newAutoHintPinned = ref(false);
let newAutoHintTimer: number | undefined;

function onNewAutoFocus(e: FocusEvent): void {
  const target = e.currentTarget;
  newAutoHintKeyboardFocused.value =
    target instanceof HTMLElement && target.matches(":focus-visible");
}

function toggleNewAuto(): void {
  newAuto.value = !newAuto.value;
  newAutoHintPinned.value = true;
  if (newAutoHintTimer !== undefined) window.clearTimeout(newAutoHintTimer);
  newAutoHintTimer = window.setTimeout(() => {
    newAutoHintPinned.value = false;
    newAutoHintTimer = undefined;
  }, 500);
}

async function addEntry(): Promise<void> {
  const text = newText.value.trim();
  if (!text || !selected.value || adding.value || newAttachmentBusy.value) return;
  adding.value = true;
  try {
    await todosAdd(
      selected.value,
      text,
      newAuto.value,
      newFiles.value,
      newPastedImages.value.map((draft) => draft.image)
    );
    newText.value = "";
    newFiles.value = [];
    newPastedImages.value = [];
    newFileThumbs.value = {};
    attachmentError.value = "";
    await nextTick();
    syncAddInputHeight();
    await reloadAll();
  } catch (err) {
    console.warn("todo add failed", err);
    attachmentError.value = String(err);
  } finally {
    adding.value = false;
  }
}

// 行内 ⚡ 切换自动执行。
async function toggleAuto(e: TodoEntry): Promise<void> {
  if (!selected.value) return;
  try {
    await todosSetAuto(selected.value, e.id, !e.auto);
  } catch (err) {
    console.warn("todo set auto failed", err);
  }
  await reloadAll();
}

function onNewKeydown(e: KeyboardEvent): void {
  // Align with popup submit: cmdEnter (default) = ⌘/Ctrl+Enter; enter = bare Enter.
  // Non-submit Enter (or mod+Enter in enter mode) inserts a newline in the textarea.
  // isComposing / keyCode 229：IME 组词中的 Enter 不当提交。
  if (e.key !== "Enter") return;
  if (e.isComposing || (e as KeyboardEvent & { keyCode?: number }).keyCode === 229) {
    return;
  }
  const mod = primaryModifierPressed(e);
  const anyMod = e.metaKey || e.ctrlKey || e.shiftKey || e.altKey;
  const isPrimarySendMod = mod && !e.shiftKey && !e.altKey;
  const shouldSubmit = submitWithBareEnter.value ? !anyMod : isPrimarySendMod;
  if (!shouldSubmit) return;
  e.preventDefault();
  void addEntry();
}

function onNewInput(): void {
  syncAddInputHeight();
}

// ===== 行内编辑：复制旁「编辑」进入；变成「保存 / 取消」；提交键保存、Esc 取消 =====
const editingId = ref<string | null>(null);
const editText = ref("");
const editExpectedText = ref("");
const editExpectedAttachmentIds = ref<string[]>([]);
const editSaving = ref(false);
/** Function ref — plain `ref` inside `v-for` becomes an array in Vue 3. */
const editInputRef = ref<HTMLTextAreaElement | null>(null);

function setEditInputRef(el: unknown): void {
  editInputRef.value = el instanceof HTMLTextAreaElement ? el : null;
}

// Inline edit box: at least ~2 lines, grow with content up to ~6 lines.
const EDIT_INPUT_MIN_PX = 48;
const EDIT_INPUT_MAX_PX = 132;

async function beginEdit(e: TodoEntry): Promise<void> {
  if (isCompleting(e.id) || editSaving.value) return;
  if (editingId.value === e.id) return;
  // Switching rows: discard the previous draft (explicit Save only).
  if (editingId.value) {
    cancelEdit();
  }
  confirmDeleteId.value = null;
  selectedTodoId.value = e.id;
  editingId.value = e.id;
  editText.value = e.text;
  editExpectedText.value = e.text;
  editExpectedAttachmentIds.value = (e.attachments ?? []).map((attachment) => attachment.id);
  await nextTick();
  const el = editInputRef.value;
  if (el) {
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    syncEditInputHeight();
  }
}

function syncEditInputHeight(): void {
  syncTextareaHeight(editInputRef.value, EDIT_INPUT_MIN_PX, EDIT_INPUT_MAX_PX);
}

function onEditInput(): void {
  // Remeasure after the value is in the DOM.
  void nextTick(() => syncEditInputHeight());
}

function cancelEdit(): void {
  editingId.value = null;
  editText.value = "";
  editExpectedText.value = "";
  editExpectedAttachmentIds.value = [];
  attachmentError.value = "";
}

async function commitEdit(): Promise<void> {
  const id = editingId.value;
  if (!id || !selected.value || editSaving.value) return;
  const text = editText.value.trim();
  if (!text) return; // Keep editing; Save stays disabled in UI when empty.
  if (text === editExpectedText.value) {
    cancelEdit();
    return;
  }
  editSaving.value = true;
  try {
    const stored = await todosUpdate(
      selected.value,
      id,
      editExpectedText.value,
      editExpectedAttachmentIds.value,
      text,
      editExpectedAttachmentIds.value,
      []
    );
    const rowIndex = entries.value.findIndex((entry) => entry.id === id);
    if (rowIndex >= 0) entries.value[rowIndex] = stored;
    cancelEdit();
  } catch (err) {
    console.warn("todo update failed", err);
    attachmentError.value = String(err);
  } finally {
    editSaving.value = false;
  }
}

const IMAGE_FILE_EXT = /\.(png|jpe?g|gif|webp|bmp|heic|heif|tiff?|svg)$/i;

async function loadNewFileThumbnail(path: string): Promise<void> {
  if (!IMAGE_FILE_EXT.test(path) || newFileThumbs.value[path]) return;
  try {
    const data = await readImageDataUrl(path);
    if (newFiles.value.includes(path)) newFileThumbs.value[path] = data;
  } catch (err) {
    console.warn("todo draft image preview failed", path, err);
  }
}

function appendNewPaths(paths: string[]): void {
  const list = newFiles.value;
  let exceeded = false;
  const added: string[] = [];
  for (const path of paths) {
    if (list.includes(path)) continue;
    if (list.length + newPastedImages.value.length >= 20) {
      exceeded = true;
      continue;
    }
    list.push(path);
    added.push(path);
  }
  attachmentError.value = exceeded ? t("todosWin.attachmentLimit", { n: 20 }) : "";
  for (const path of added) void loadNewFileThumbnail(path);
}

function removeNewFile(path: string): void {
  const index = newFiles.value.indexOf(path);
  if (index >= 0) newFiles.value.splice(index, 1);
  delete newFileThumbs.value[path];
}

function removeNewComposerImage(index: number): void {
  const item = newComposerImages.value[index];
  if (!item) return;
  if (item.source === "file") {
    removeNewFile(item.sourceKey);
    return;
  }
  const draftIndex = newPastedImages.value.findIndex(
    (draft) => draft.key === item.sourceKey
  );
  if (draftIndex >= 0) newPastedImages.value.splice(draftIndex, 1);
}

function removeNewComposerFile(index: number): void {
  const item = newComposerFiles.value[index];
  if (item) removeNewFile(item.path);
}

async function chooseNewFiles(): Promise<void> {
  try {
    const result = await openDialog({ multiple: true, directory: false });
    const paths = Array.isArray(result) ? result : result ? [result] : [];
    appendNewPaths(paths);
  } catch (err) {
    attachmentError.value = String(err);
  }
}

const attachmentBusyIds = ref<Set<string>>(new Set());
const attachmentRowErrors = ref<Record<string, string>>({});

function isAttachmentBusy(id: string): boolean {
  return attachmentBusyIds.value.has(id);
}

function setAttachmentBusy(id: string, busy: boolean): void {
  const next = new Set(attachmentBusyIds.value);
  if (busy) next.add(id);
  else next.delete(id);
  attachmentBusyIds.value = next;
}

function applyStoredEntry(stored: TodoEntry): void {
  const rowIndex = entries.value.findIndex((entry) => entry.id === stored.id);
  if (rowIndex >= 0) entries.value[rowIndex] = stored;
  if (editingId.value === stored.id) {
    editExpectedAttachmentIds.value = (stored.attachments ?? []).map(
      (attachment) => attachment.id
    );
  }
}

async function updateTodoAttachments(
  entry: TodoEntry,
  addPaths: string[],
  removeAttachmentIds: string[] = []
): Promise<void> {
  if (!selected.value || isCompleting(entry.id) || isAttachmentBusy(entry.id)) return;
  const knownPaths = new Set(
    (entry.attachments ?? []).flatMap((attachment) => [attachment.path, attachment.sourcePath])
  );
  const uniquePaths = [...new Set(addPaths)].filter((path) => !knownPaths.has(path));
  const remainingCount = (entry.attachments?.length ?? 0) - removeAttachmentIds.length;
  if (remainingCount + uniquePaths.length > 20) {
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.attachmentLimit", { n: 20 }),
    };
    return;
  }
  if (!uniquePaths.length && !removeAttachmentIds.length) return;
  setAttachmentBusy(entry.id, true);
  try {
    const stored = await todosUpdateAttachments(
      selected.value,
      entry.id,
      uniquePaths,
      removeAttachmentIds
    );
    applyStoredEntry(stored);
    const nextErrors = { ...attachmentRowErrors.value };
    delete nextErrors[entry.id];
    attachmentRowErrors.value = nextErrors;
  } catch (err) {
    console.warn("todo attachment update failed", err);
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.operationFailed", { e: String(err) }),
    };
  } finally {
    setAttachmentBusy(entry.id, false);
  }
}

async function chooseTodoFiles(entry: TodoEntry): Promise<void> {
  selectedTodoId.value = entry.id;
  try {
    const result = await openDialog({ multiple: true, directory: false });
    const paths = Array.isArray(result) ? result : result ? [result] : [];
    await updateTodoAttachments(entry, paths);
  } catch (err) {
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.operationFailed", { e: String(err) }),
    };
  }
}

const confirmAttachmentRemoval = ref<{
  todoId: string;
  attachmentId: string;
} | null>(null);

function confirmedAttachmentId(todoId: string): string | null {
  return confirmAttachmentRemoval.value?.todoId === todoId
    ? confirmAttachmentRemoval.value.attachmentId
    : null;
}

function removeTodoAttachment(entry: TodoEntry, attachmentId: string): void {
  const pending = confirmAttachmentRemoval.value;
  if (pending?.todoId !== entry.id || pending.attachmentId !== attachmentId) {
    confirmAttachmentRemoval.value = { todoId: entry.id, attachmentId };
    return;
  }
  confirmAttachmentRemoval.value = null;
  void updateTodoAttachments(entry, [], [attachmentId]);
}

function focusTodoRow(id: string): void {
  const index = entries.value.findIndex((entry) => entry.id === id);
  if (index < 0) return;
  const row = document.querySelectorAll<HTMLElement>(".td-list > .td-row")[index];
  row?.focus({ preventScroll: true });
  row?.scrollIntoView({ block: "nearest" });
}

function selectTodo(id: string, focus = false): void {
  selectedTodoId.value = id;
  if (focus) void nextTick(() => focusTodoRow(id));
}

function onTodoNavigation(event: KeyboardEvent): void {
  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
  if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return;
  const target = event.target;
  if (
    target instanceof HTMLElement &&
    target.closest("textarea, input, select, [contenteditable='true'], .todo-attachments-list")
  ) {
    return;
  }
  if (!entries.value.length) return;
  event.preventDefault();
  const id = nextTodoSelection(
    entries.value.map((entry) => entry.id),
    selectedTodoId.value,
    event.key === "ArrowDown" ? 1 : -1
  );
  if (!id) return;
  selectedTodoId.value = id;
  void nextTick(() => focusTodoRow(id));
}

async function onTodoPaste(event: ClipboardEvent): Promise<void> {
  const files = clipboardImageFiles(event.clipboardData?.items);
  if (!files.length) return;
  if (event.target === addInputRef.value) {
    event.preventDefault();
    if (adding.value || newAttachmentBusy.value) return;
    if (newFiles.value.length + newPastedImages.value.length + files.length > 20) {
      attachmentError.value = t("todosWin.attachmentLimit", { n: 20 });
      return;
    }
    if (files.some((file) => file.size > 10 * 1024 * 1024)) {
      attachmentError.value = t("todosWin.pastedImageTooLarge");
      return;
    }
    newAttachmentBusy.value = true;
    try {
      const images = await imageAttachmentsFromFiles(files);
      newPastedImages.value.push(
        ...images.map((image) => ({
          key: `pasted-${++pastedImageDraftSequence}`,
          image,
        }))
      );
      attachmentError.value = "";
    } catch (err) {
      attachmentError.value = t("todosWin.operationFailed", { e: String(err) });
    } finally {
      newAttachmentBusy.value = false;
    }
    return;
  }
  const id = selectedTodoId.value;
  if (!id || !selected.value || isAttachmentBusy(id)) return;
  const entry = entries.value.find((item) => item.id === id);
  if (!entry) return;
  event.preventDefault();
  if ((entry.attachments?.length ?? 0) + files.length > 20) {
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.attachmentLimit", { n: 20 }),
    };
    return;
  }
  if (files.some((file) => file.size > 10 * 1024 * 1024)) {
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.pastedImageTooLarge"),
    };
    return;
  }
  setAttachmentBusy(entry.id, true);
  try {
    const images = await imageAttachmentsFromFiles(files);
    const stored = await todosAttachPastedImages(selected.value, entry.id, images);
    applyStoredEntry(stored);
    const nextErrors = { ...attachmentRowErrors.value };
    delete nextErrors[entry.id];
    attachmentRowErrors.value = nextErrors;
  } catch (err) {
    console.warn("todo clipboard image failed", err);
    attachmentRowErrors.value = {
      ...attachmentRowErrors.value,
      [entry.id]: t("todosWin.operationFailed", { e: String(err) }),
    };
  } finally {
    setAttachmentBusy(entry.id, false);
  }
}

async function imageAttachmentsFromFiles(files: File[]): Promise<ImageAttachment[]> {
  const pastedAt = Date.now();
  return Promise.all(
    files.map(async (file, index) => ({
      data: await fileToDataUrl(file),
      mediaType: file.type || "image/png",
      filename: file.name || `pasted-image-${pastedAt}-${index + 1}.png`,
    }))
  );
}

function draftName(path: string): string {
  return path.replace(/\\/g, "/").split("/").pop() || path;
}

function onEditKeydown(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.preventDefault();
    cancelEdit();
    return;
  }
  // Same submit shortcut as add / popup.
  if (e.key !== "Enter") return;
  if (e.isComposing || (e as KeyboardEvent & { keyCode?: number }).keyCode === 229) {
    return;
  }
  const mod = primaryModifierPressed(e);
  const anyMod = e.metaKey || e.ctrlKey || e.shiftKey || e.altKey;
  const isPrimarySendMod = mod && !e.shiftKey && !e.altKey;
  const shouldSubmit = submitWithBareEnter.value ? !anyMod : isPrimarySendMod;
  if (!shouldSubmit) return;
  e.preventDefault();
  void commitEdit();
}

// 删除二次确认（第 13 轮定案，防误删）：首次点 ✕ 该行按钮变「确认删除」文字，再点才删；
// 点其它行的 ✕ 把确认焦点挪过去，点删除按钮之外的任意区域取消确认态（删除按钮 @click.stop，
// 其余点击冒泡到根容器复位），切项目/删除后复位。
const confirmDeleteId = ref<string | null>(null);

function onRootClick(): void {
  confirmDeleteId.value = null;
  confirmAttachmentRemoval.value = null;
}

async function removeEntry(id: string): Promise<void> {
  if (!selected.value) return;
  if (confirmDeleteId.value !== id) {
    confirmDeleteId.value = id;
    return;
  }
  confirmDeleteId.value = null;
  if (confirmAttachmentRemoval.value?.todoId === id) {
    confirmAttachmentRemoval.value = null;
  }
  clearPendingComplete(id);
  try {
    await todosRemove(selected.value, id);
    operationError.value = "";
  } catch (err) {
    console.warn("todo remove failed", err);
    operationError.value = t("todosWin.operationFailed", { e: String(err) });
  }
  await reloadAll();
}

async function doConfirmedClear(): Promise<void> {
  const kind = confirmKind.value;
  if (!selected.value || !kind) return;
  if (kind === "todos") clearAllPendingComplete();
  try {
    if (kind === "todos") {
      await todosClear(selected.value);
    } else {
      await todosHistoryClear(selected.value);
    }
    operationError.value = "";
    confirmKind.value = null;
  } catch (err) {
    console.warn("todo clear failed", err);
    operationError.value = t("todosWin.operationFailed", { e: String(err) });
  }
  await reloadAll();
}

// ===== 勾选完成：乐观 UI + 1s 可撤回，超时 take 进 history =====
const COMPLETE_DELAY_MS = 1000;
/** id → timer handle for optimistic complete. */
const pendingCompleteTimers = new Map<string, number>();
/** Reactive set of ids currently in the completing (checked / strikethrough) state. */
const completingIds = ref<Set<string>>(new Set());

function isCompleting(id: string): boolean {
  return completingIds.value.has(id);
}

function clearPendingComplete(id: string): void {
  const timer = pendingCompleteTimers.get(id);
  if (timer !== undefined) {
    window.clearTimeout(timer);
    pendingCompleteTimers.delete(id);
  }
  if (completingIds.value.has(id)) {
    const next = new Set(completingIds.value);
    next.delete(id);
    completingIds.value = next;
  }
}

function clearAllPendingComplete(): void {
  for (const timer of pendingCompleteTimers.values()) {
    window.clearTimeout(timer);
  }
  pendingCompleteTimers.clear();
  completingIds.value = new Set();
}

function onPendingCheck(id: string): void {
  if (!selected.value) return;
  if (isCompleting(id)) {
    // Uncheck within the grace window → cancel.
    clearPendingComplete(id);
    return;
  }
  const next = new Set(completingIds.value);
  next.add(id);
  completingIds.value = next;
  const timer = window.setTimeout(() => {
    pendingCompleteTimers.delete(id);
    void commitComplete(id);
  }, COMPLETE_DELAY_MS);
  pendingCompleteTimers.set(id, timer);
}

async function commitComplete(id: string): Promise<void> {
  if (!selected.value) {
    clearPendingComplete(id);
    return;
  }
  try {
    await todosComplete(selected.value, id);
  } catch (err) {
    console.warn("todo complete failed", err);
    clearPendingComplete(id);
  }
  // Reload (or todos-updated) will drop the id from completingIds once gone from the queue.
  await reloadAll();
}

// ===== 时间：相对显示 + 自定义即时绝对时间 tooltip =====
const timeTipId = ref<string | null>(null);

function absoluteTime(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : "";
}

function relativeTime(ms: number): string {
  if (!ms) return "";
  const now = Date.now();
  const diff = Math.max(0, now - ms);
  const sec = Math.floor(diff / 1000);
  if (sec < 5) return t("todosWin.time.justNow");
  if (sec < 60) return t("todosWin.time.secondsAgo", { n: sec });
  const min = Math.floor(sec / 60);
  if (min < 60) return t("todosWin.time.minutesAgo", { n: min });
  const hr = Math.floor(min / 60);
  if (hr < 24) return t("todosWin.time.hoursAgo", { n: hr });
  const d = new Date(ms);
  const yd = new Date(now - 86400000);
  if (
    d.getFullYear() === yd.getFullYear() &&
    d.getMonth() === yd.getMonth() &&
    d.getDate() === yd.getDate()
  ) {
    return t("todosWin.time.yesterday");
  }
  const day = Math.floor(hr / 24);
  if (day < 7) return t("todosWin.time.daysAgo", { n: day });
  try {
    return new Intl.DateTimeFormat(locale.value, {
      month: "numeric",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    }).format(d);
  } catch {
    return d.toLocaleString();
  }
}

// ===== 执行历史（执行出队 + GUI 勾选完成；可一键恢复回队列末尾）=====
const history = ref<TodoDoneEntry[]>([]);
const histOpen = ref(false);

async function restoreEntry(id: string): Promise<void> {
  if (!selected.value) return;
  try {
    await todosRestore(selected.value, id);
  } catch (err) {
    console.warn("todo restore failed", err);
  }
  await reloadAll();
}

/** id of the row whose copy button just succeeded (brief visual feedback). */
const copiedId = ref<string | null>(null);
let copiedTimer: number | undefined;

async function copyTodoText(id: string, text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    copiedId.value = id;
    if (copiedTimer !== undefined) window.clearTimeout(copiedTimer);
    copiedTimer = window.setTimeout(() => {
      if (copiedId.value === id) copiedId.value = null;
      copiedTimer = undefined;
    }, 1200);
  } catch (err) {
    console.warn("todo copy failed", err);
  }
}

// ===== Pointer 排序（不使用 HTML5 DnD，避免与 Tauri 原生文件拖入互斥）=====
// 手柄按下后经过其它行时本地 splice 实时预览；松手一次性持久化。
const dragIndex = ref<number | null>(null);

function onSortPointerDown(i: number, event: PointerEvent): void {
  if (event.button !== 0) return;
  event.preventDefault();
  dragIndex.value = i;
  confirmDeleteId.value = null;
}

function onSortPointerEnter(i: number): void {
  const from = dragIndex.value;
  if (from === null || from === i) return;
  const moved = entries.value.splice(from, 1)[0];
  entries.value.splice(i, 0, moved);
  dragIndex.value = i;
}

async function onSortPointerEnd(): Promise<void> {
  if (dragIndex.value === null) return;
  dragIndex.value = null;
  if (!selected.value) return;
  try {
    await todosReorder(
      selected.value,
      entries.value.map((e) => e.id)
    );
  } catch (err) {
    console.warn("todo reorder failed", err);
    await reloadAll();
  }
}

let unlistenUpdated: UnlistenFn | null = null;
let unlistenGoto: UnlistenFn | null = null;
let unlistenSettings: UnlistenFn | null = null;
let unlistenFileDrop: UnlistenFn | null = null;
let unlistenPreviewIndex: UnlistenFn | null = null;
let unlistenPreviewClosed: UnlistenFn | null = null;
const fileDropTarget = ref<string | null>(null);
const todoPreviewContext = ref<{
  todoId: string;
  attachmentIds: string[];
} | null>(null);
const todoPreviewSelection = ref<{
  todoId: string;
  attachmentId: string;
} | null>(null);

function startTodoPreview(todoId: string, attachmentIds: string[]): void {
  todoPreviewContext.value = { todoId, attachmentIds };
  todoPreviewSelection.value = null;
}

function previewSelectedAttachmentId(todoId: string): string | null {
  return todoPreviewSelection.value?.todoId === todoId
    ? todoPreviewSelection.value.attachmentId
    : null;
}

function fileDropTargetAt(x: number, y: number): string | null {
  const dpr = window.devicePixelRatio || 1;
  const candidates: [number, number][] = isWindows
    ? [
        [x / dpr, y / dpr],
        [x, y],
      ]
    : [
        [x, y],
        [x / dpr, y / dpr],
      ];
  for (const [cx, cy] of candidates) {
    const element = document.elementFromPoint(cx, cy);
    const target = element
      ?.closest<HTMLElement>("[data-todo-file-drop]")
      ?.dataset.todoFileDrop;
    if (target) return target;
  }
  return null;
}

onMounted(async () => {
  window.addEventListener("pointerup", onSortPointerEnd);
  window.addEventListener("pointercancel", onSortPointerEnd);
  window.addEventListener("blur", onSortPointerEnd);
  window.addEventListener("keydown", onTodoNavigation);
  window.addEventListener("paste", onTodoPaste);
  document.addEventListener("mouseleave", onSortPointerEnd);
  try {
    const init = await todosInit();
    applyTheme(init.theme);
    applyLanguage(init.lang);
    applyPopupSubmitKey(init.popupSubmitKey);
    newTaskSupported.value = init.newTaskSupported === true;
  } catch {
    /* 读取失败：保持兜底外观 */
  }
  const preselect = new URLSearchParams(window.location.search).get("project");
  if (preselect) selected.value = preselect;
  // 设置变更实时生效（主题/语言/提交快捷键与设置窗口同宿主进程广播）。
  unlistenSettings = await listen<{
    theme?: ThemeMode;
    language?: string;
    popupSubmitKey?: PopupSubmitKey;
  }>("settings-updated", (e) => {
    if (typeof e.payload.theme === "string") applyTheme(e.payload.theme);
    if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
    applyPopupSubmitKey(e.payload.popupSubmitKey);
  });
  // todos.json 被任意进程改写（CLI/弹窗/出队）→ 宿主文件监听推事件 → 重载。
  unlistenUpdated = await listen("todos-updated", () => {
    void reloadAll();
  });
  // 窗口已开时再次带预选项目打开（托盘 agent「添加待办」/ Agent 卡片入口）→ 切换选中项目。
  unlistenGoto = await listen<string>("todos-goto-project", async (e) => {
    if (typeof e.payload === "string" && e.payload) {
      selected.value = e.payload;
      selectedTodoId.value = null;
      confirmAttachmentRemoval.value = null;
      confirmKind.value = null;
      confirmDeleteId.value = null;
      histOpen.value = false;
      clearAllPendingComplete();
      await reloadAll();
      await focusAddInput();
    }
  });
  unlistenPreviewIndex = await listen<number>("preview-index", (event) => {
    const context = todoPreviewContext.value;
    if (!context) return;
    const attachmentId = context.attachmentIds[event.payload];
    if (!attachmentId) return;
    todoPreviewSelection.value = { todoId: context.todoId, attachmentId };
  });
  unlistenPreviewClosed = await listen("preview-closed", () => {
    todoPreviewContext.value = null;
    todoPreviewSelection.value = null;
  });
  unlistenFileDrop = await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "leave") {
      fileDropTarget.value = null;
      return;
    }
    const pos = event.payload.position;
    const targetAtPoint = fileDropTargetAt(pos.x, pos.y);
    if (event.payload.type === "enter" || event.payload.type === "over") {
      fileDropTarget.value = targetAtPoint;
      return;
    }
    const target = fileDropTarget.value ?? targetAtPoint;
    fileDropTarget.value = null;
    if (target === "new") {
      appendNewPaths(event.payload.paths);
      return;
    }
    const entry = entries.value.find((item) => item.id === target);
    if (entry) {
      void updateTodoAttachments(entry, event.payload.paths);
    }
  });
  await reloadAll();
  if (preselect) await focusAddInput();
});

onBeforeUnmount(() => {
  if (newAutoHintTimer !== undefined) window.clearTimeout(newAutoHintTimer);
  if (copiedTimer !== undefined) window.clearTimeout(copiedTimer);
  clearAllPendingComplete();
  window.removeEventListener("pointerup", onSortPointerEnd);
  window.removeEventListener("pointercancel", onSortPointerEnd);
  window.removeEventListener("blur", onSortPointerEnd);
  window.removeEventListener("keydown", onTodoNavigation);
  window.removeEventListener("paste", onTodoPaste);
  document.removeEventListener("mouseleave", onSortPointerEnd);
  unlistenUpdated?.();
  unlistenGoto?.();
  unlistenSettings?.();
  unlistenFileDrop?.();
  unlistenPreviewIndex?.();
  unlistenPreviewClosed?.();
});
</script>

<template>
  <div class="todos-win" @click="onRootClick">
    <header class="td-header" data-tauri-drag-region>
      <span class="td-title" data-tauri-drag-region>{{ t("todosWin.title") }}</span>
      <select
        v-if="projects.length"
        v-model="selected"
        class="td-select"
        :aria-label="t('todosWin.projectLabel')"
        @change="onSelect"
      >
        <optgroup
          v-if="projectsWithTodos.length"
          :label="t('todosWin.sectionWithTodos')"
        >
          <option
            v-for="p in projectsWithTodos"
            :key="p.key"
            :value="p.key"
            :title="p.key"
          >
            {{ p.name }}{{ p.count ? ` (${p.count})` : "" }}
          </option>
        </optgroup>
        <optgroup
          v-if="projectsRecent.length"
          :label="t('todosWin.sectionRecent')"
        >
          <option
            v-for="p in projectsRecent"
            :key="p.key"
            :value="p.key"
            :title="p.key"
          >
            {{ p.name }}
          </option>
        </optgroup>
      </select>
    </header>

    <div class="td-body">
      <div v-if="!loaded" class="empty">
        <span class="spinner" />
      </div>

      <div v-else-if="!projects.length" class="empty">
        <p class="empty-title">{{ t("todosWin.noProjects") }}</p>
        <p class="empty-hint">{{ t("todosWin.noProjectsHint") }}</p>
      </div>

      <template v-else>
      <div v-if="!entries.length" class="empty" :class="{ compact: history.length }">
        <p class="empty-title">{{ t("todosWin.empty") }}</p>
        <p class="empty-hint">{{ t("todosWin.emptyHint") }}</p>
      </div>

      <ul v-else class="td-list">
        <li
          v-for="(e, i) in entries"
          :key="e.id"
          class="td-row"
          :class="{
            dragging: dragIndex === i,
            completing: isCompleting(e.id),
            selected: selectedTodoId === e.id,
            'file-drop-target': fileDropTarget === e.id,
            'attachment-busy': isAttachmentBusy(e.id),
          }"
          :data-todo-file-drop="e.id"
          :data-todo-row-id="e.id"
          tabindex="0"
          :aria-label="e.text"
          @click="selectTodo(e.id, true)"
          @focus="selectTodo(e.id)"
          @pointerdown.capture="selectTodo(e.id)"
          @pointerenter="onSortPointerEnter(i)"
        >
          <!-- 手柄列始终占位，与历史行复选框对齐。 -->
          <span
            class="td-handle-slot"
            :class="{ active: entries.length > 1 }"
          >
            <span
              v-if="entries.length > 1"
              class="td-handle"
              :title="t('todosWin.dragHint')"
              @pointerdown="onSortPointerDown(i, $event)"
            >
              <svg viewBox="0 0 10 14" aria-hidden="true">
                <circle cx="3" cy="3" r="1.1" fill="currentColor" />
                <circle cx="7" cy="3" r="1.1" fill="currentColor" />
                <circle cx="3" cy="7" r="1.1" fill="currentColor" />
                <circle cx="7" cy="7" r="1.1" fill="currentColor" />
                <circle cx="3" cy="11" r="1.1" fill="currentColor" />
                <circle cx="7" cy="11" r="1.1" fill="currentColor" />
              </svg>
            </span>
          </span>

          <button
            type="button"
            class="td-check"
            :class="{ checked: isCompleting(e.id) }"
            :aria-label="
              isCompleting(e.id) ? t('todosWin.undoComplete') : t('todosWin.complete')
            "
            :aria-pressed="isCompleting(e.id)"
            @click.stop="onPendingCheck(e.id)"
          >
            <svg v-if="isCompleting(e.id)" viewBox="0 0 16 16" aria-hidden="true">
              <circle cx="8" cy="8" r="7" fill="currentColor" />
              <path
                d="M5 8.2 L7.1 10.3 L11.2 5.8"
                fill="none"
                stroke="#fff"
                stroke-width="1.6"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
            <svg v-else viewBox="0 0 16 16" aria-hidden="true">
              <circle
                cx="8"
                cy="8"
                r="6.25"
                fill="none"
                stroke="currentColor"
                stroke-width="1.4"
              />
            </svg>
          </button>

          <div class="td-main">
            <div class="td-top">
              <textarea
                v-if="editingId === e.id"
                :ref="setEditInputRef"
                v-model="editText"
                class="td-edit"
                rows="1"
                :disabled="editSaving"
                :aria-label="t('todosWin.editLabel')"
                @click.stop
                @keydown="onEditKeydown"
                @input="onEditInput"
              />
              <span v-else class="td-text">{{ e.text }}</span>
              <button
                v-if="confirmDeleteId === e.id"
                type="button"
                class="td-del-confirm"
                @click.stop="removeEntry(e.id)"
              >
                {{ t("todosWin.deleteConfirm") }}
              </button>
              <button
                v-else
                type="button"
                class="td-del"
                :title="t('todosWin.delete')"
                :aria-label="t('todosWin.delete')"
                @click.stop="removeEntry(e.id)"
              >
                <svg viewBox="0 0 12 12" aria-hidden="true">
                  <path
                    d="M3 3 L9 9 M9 3 L3 9"
                    stroke="currentColor"
                    stroke-width="1.4"
                    stroke-linecap="round"
                  />
                </svg>
              </button>
            </div>
            <TodoAttachmentList
              :project="selected"
              :todo-id="e.id"
              :attachments="e.attachments ?? []"
              removable
              :busy="isAttachmentBusy(e.id)"
              :confirm-remove-id="confirmedAttachmentId(e.id)"
              :preview-selected-id="previewSelectedAttachmentId(e.id)"
              @remove="removeTodoAttachment(e, $event)"
              @cancel-remove="confirmAttachmentRemoval = null"
              @preview-started="startTodoPreview(e.id, $event)"
            />
            <p
              v-if="attachmentRowErrors[e.id]"
              class="td-attachment-error td-row-attachment-error"
              role="alert"
            >
              {{ attachmentRowErrors[e.id] }}
            </p>
            <div class="td-meta">
              <span
                class="td-time-anchor"
                @mouseenter="timeTipId = `p-${e.id}`"
                @mouseleave="timeTipId = null"
              >
                <span class="td-time">{{ relativeTime(e.createdAtMs) }}</span>
                <span
                  v-show="timeTipId === `p-${e.id}` && e.createdAtMs"
                  class="td-time-tip"
                  role="tooltip"
                >
                  {{ absoluteTime(e.createdAtMs) }}
                </span>
              </span>
              <span v-if="agentLabel(e.agentKind)" class="td-source">
                · {{ agentLabel(e.agentKind) }}
              </span>
              <button
                type="button"
                class="td-auto"
                :class="{ on: e.auto }"
                :title="e.auto ? t('todosWin.autoOff') : t('todosWin.autoOn')"
                :aria-label="e.auto ? t('todosWin.autoOff') : t('todosWin.autoOn')"
                @click.stop="toggleAuto(e)"
              >
                <svg viewBox="0 0 12 12" aria-hidden="true">
                  <path
                    d="M6.8 1 L2.5 7 H5.6 L5.2 11 L9.5 5 H6.4 Z"
                    :fill="e.auto ? 'currentColor' : 'none'"
                    stroke="currentColor"
                    stroke-width="1"
                    stroke-linejoin="round"
                  />
                </svg>
              </button>
              <button
                type="button"
                class="td-copy"
                :class="{ done: copiedId === e.id }"
                :title="
                  copiedId === e.id ? t('todosWin.copied') : t('todosWin.copy')
                "
                :aria-label="
                  copiedId === e.id ? t('todosWin.copied') : t('todosWin.copy')
                "
                @click.stop="copyTodoText(e.id, editingId === e.id ? editText : e.text)"
              >
                <svg
                  v-if="copiedId === e.id"
                  viewBox="0 0 12 12"
                  aria-hidden="true"
                >
                  <path
                    d="M2.5 6.2 L5 8.7 L9.5 3.5"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.4"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
                <svg v-else viewBox="0 0 12 12" aria-hidden="true">
                  <rect
                    x="4"
                    y="4"
                    width="6"
                    height="6"
                    rx="1.2"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.2"
                  />
                  <path
                    d="M3 8 V3.2 A1.2 1.2 0 0 1 4.2 2 H8"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.2"
                    stroke-linecap="round"
                  />
                </svg>
              </button>
              <!-- Edit (right of copy) → becomes Save + Cancel while editing. -->
              <template v-if="editingId === e.id">
                <button
                  type="button"
                  class="td-edit-action td-edit-save"
                  :disabled="editSaving || !editText.trim()"
                  @click.stop="commitEdit"
                >
                  {{ t("todosWin.save") }}
                </button>
                <button
                  type="button"
                  class="td-edit-action td-edit-cancel"
                  :disabled="editSaving"
                  @click.stop="cancelEdit"
                >
                  {{ t("todosWin.editCancel") }}
                </button>
              </template>
              <button
                v-else
                type="button"
                class="td-edit-btn"
                :title="t('todosWin.edit')"
                :aria-label="t('todosWin.edit')"
                :disabled="isCompleting(e.id)"
                @click.stop="beginEdit(e)"
              >
                <svg viewBox="0 0 12 12" aria-hidden="true">
                  <path
                    d="M8.4 1.6 L10.4 3.6 L4.2 9.8 L2 10 L2.2 7.8 Z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.2"
                    stroke-linejoin="round"
                  />
                  <path
                    d="M7.3 2.7 L9.3 4.7"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.2"
                    stroke-linecap="round"
                  />
                </svg>
              </button>
              <button
                type="button"
                class="td-edit-btn td-attach-btn"
                :title="t('todosWin.addAttachment')"
                :aria-label="t('todosWin.addAttachment')"
                :disabled="isCompleting(e.id) || isAttachmentBusy(e.id)"
                @click.stop="chooseTodoFiles(e)"
              >
                <svg
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.7"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
                </svg>
              </button>
              <!-- 创建 Agent 任务（spec gui-agent-task-launch G12）：带该项目 + 该待办打开新建任务窗口。 -->
              <button
                v-if="newTaskSupported"
                type="button"
                class="td-newtask"
                :title="t('todosWin.newTask')"
                :aria-label="t('todosWin.newTask')"
                :disabled="isCompleting(e.id)"
                @click.stop="createTaskFrom(e)"
              >
                <svg viewBox="0 0 12 12" aria-hidden="true">
                  <path
                    d="M3.2 1.8 L9.8 6 L3.2 10.2 Z"
                    fill="none"
                    stroke="currentColor"
                    stroke-width="1.2"
                    stroke-linejoin="round"
                  />
                </svg>
              </button>
            </div>
          </div>
          <div
            v-if="fileDropTarget === e.id"
            class="td-file-drop-overlay"
            aria-hidden="true"
          >
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
              <path d="M12 16V5M8 9l4-4 4 4" />
              <path d="M5 14v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4" />
            </svg>
            <span>{{ t("todosWin.dropOnTodo") }}</span>
          </div>
        </li>
      </ul>

      <!-- 清空（第 18 轮定案：列表区末尾、历史区上方，紧邻其作用对象、远离新增行；
           点击弹模态确认，需要移动光标二次确认）。 -->
      <div v-if="entries.length" class="td-clear-row">
        <button type="button" class="td-clear" @click="confirmKind = 'todos'">
          {{ t("todosWin.clear") }}
        </button>
      </div>

      <!-- 执行历史折叠区：执行出队 + GUI 勾选完成；可一键恢复回队列末尾。 -->
      <section v-if="history.length" class="td-hist">
        <div class="td-hist-head">
          <button type="button" class="td-hist-toggle" @click="histOpen = !histOpen">
            <span class="td-hist-caret" :class="{ open: histOpen }">▸</span>
            <span>{{ t("todosWin.historyTitle") }}</span>
            <span class="td-hist-count">{{ history.length }}</span>
          </button>
          <button
            v-if="histOpen"
            type="button"
            class="td-clear"
            @click="confirmKind = 'history'"
          >
            {{ t("todosWin.clearHist") }}
          </button>
        </div>
        <ul v-if="histOpen" class="td-list td-hist-list">
          <li v-for="h in history" :key="h.id" class="td-row td-hist-row">
            <!-- 与待办行手柄列同宽留白，复选框上下对齐。 -->
            <span class="td-handle-slot" />
            <button
              type="button"
              class="td-check td-check-hist"
              :aria-label="t('todosWin.restore')"
              @click.stop="restoreEntry(h.id)"
            >
              <!-- 默认：实心已勾选；hover：同尺寸圆内换成恢复图标 + 蓝色 tint。 -->
              <svg class="td-check-done" viewBox="0 0 16 16" aria-hidden="true">
                <circle cx="8" cy="8" r="7" fill="currentColor" />
                <path
                  d="M5 8.2 L7.1 10.3 L11.2 5.8"
                  fill="none"
                  stroke="#fff"
                  stroke-width="1.6"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
              <svg class="td-check-restore" viewBox="0 0 16 16" aria-hidden="true">
                <circle cx="8" cy="8" r="7" fill="currentColor" />
                <!-- Restore glyph inset inside the disc so it does not fill the circle. -->
                <path
                  d="M7 5.1 L5.4 6.8 L7 8.5 M5.4 6.8 H9.6 A2.1 2.1 0 0 1 9.6 11 H7.4"
                  fill="none"
                  stroke="#fff"
                  stroke-width="1.25"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </button>
            <div class="td-main">
              <div class="td-top">
                <span class="td-text">{{ h.text }}</span>
              </div>
              <TodoAttachmentList
                :project="selected"
                :todo-id="h.id"
                :attachments="h.attachments ?? []"
                :preview-selected-id="previewSelectedAttachmentId(h.id)"
                @preview-started="startTodoPreview(h.id, $event)"
              />
              <div class="td-meta">
                <span
                  class="td-time-anchor"
                  @mouseenter="timeTipId = `h-${h.id}`"
                  @mouseleave="timeTipId = null"
                >
                  <span class="td-time">{{ relativeTime(h.doneAtMs) }}</span>
                  <span
                    v-show="timeTipId === `h-${h.id}` && h.doneAtMs"
                    class="td-time-tip"
                    role="tooltip"
                  >
                    {{ absoluteTime(h.doneAtMs) }}
                  </span>
                </span>
                <span v-if="agentLabel(h.agentKind)" class="td-source">
                  · {{ agentLabel(h.agentKind) }}
                </span>
                <button
                  type="button"
                  class="td-copy"
                  :class="{ done: copiedId === h.id }"
                  :title="
                    copiedId === h.id ? t('todosWin.copied') : t('todosWin.copy')
                  "
                  :aria-label="
                    copiedId === h.id ? t('todosWin.copied') : t('todosWin.copy')
                  "
                  @click.stop="copyTodoText(h.id, h.text)"
                >
                  <svg
                    v-if="copiedId === h.id"
                    viewBox="0 0 12 12"
                    aria-hidden="true"
                  >
                    <path
                      d="M2.5 6.2 L5 8.7 L9.5 3.5"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.4"
                      stroke-linecap="round"
                      stroke-linejoin="round"
                    />
                  </svg>
                  <svg v-else viewBox="0 0 12 12" aria-hidden="true">
                    <rect
                      x="4"
                      y="4"
                      width="6"
                      height="6"
                      rx="1.2"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.2"
                    />
                    <path
                      d="M3 8 V3.2 A1.2 1.2 0 0 1 4.2 2 H8"
                      fill="none"
                      stroke="currentColor"
                      stroke-width="1.2"
                      stroke-linecap="round"
                    />
                  </svg>
                </button>
              </div>
            </div>
          </li>
        </ul>
      </section>
      </template>
    </div>

    <footer
      v-if="projects.length"
      class="td-footer"
      @pointerdown="selectedTodoId = null"
    >
      <div
        class="td-add"
        :class="{ 'file-drop-target': fileDropTarget === 'new' }"
        data-todo-file-drop="new"
      >
        <textarea
          ref="addInputRef"
          v-model="newText"
          class="td-input"
          rows="2"
          :placeholder="t('todosWin.addPlaceholder')"
          @focus="selectedTodoId = null"
          @keydown="onNewKeydown"
          @input="onNewInput"
        />
        <div
          v-if="newComposerImages.length || newComposerFiles.length"
          class="td-composer-attachments"
        >
          <ComposerAttachments
            :images="newComposerImages"
            :files="newComposerFiles"
            @remove-image="removeNewComposerImage"
            @remove-file="removeNewComposerFile"
          />
        </div>
        <p v-if="attachmentError" class="td-attachment-error">{{ attachmentError }}</p>
        <p v-if="operationError" class="td-attachment-error" role="alert">
          {{ operationError }}
        </p>
        <div class="td-add-actions">
          <button
            type="button"
            class="td-btn td-attach-picker"
            :title="t('todosWin.addAttachment')"
            @click="chooseNewFiles"
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
              aria-hidden="true"
            >
              <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
            </svg>
            <span>{{ t("todosWin.attachment") }}</span>
          </button>
          <div
            class="td-auto-hint-anchor"
            @mouseenter="newAutoHintHovered = true"
            @mouseleave="newAutoHintHovered = false"
          >
            <button
              type="button"
              class="td-auto td-auto-new"
              :class="{ on: newAuto }"
              :aria-label="newAuto ? t('todosWin.autoOff') : t('todosWin.autoOn')"
              aria-describedby="todo-auto-new-hint"
              :aria-pressed="newAuto"
              @focus="onNewAutoFocus"
              @blur="newAutoHintKeyboardFocused = false"
              @click="toggleNewAuto"
            >
              <svg viewBox="0 0 12 12" aria-hidden="true">
                <path
                  d="M6.8 1 L2.5 7 H5.6 L5.2 11 L9.5 5 H6.4 Z"
                  :fill="newAuto ? 'currentColor' : 'none'"
                  stroke="currentColor"
                  stroke-width="1"
                  stroke-linejoin="round"
                />
              </svg>
              <span>{{ t("todosWin.autoLabel") }}</span>
            </button>
            <div
              v-show="
                newAutoHintHovered || newAutoHintKeyboardFocused || newAutoHintPinned
              "
              id="todo-auto-new-hint"
              class="td-auto-hint"
              role="tooltip"
              aria-live="polite"
            >
              {{ t("todosWin.autoNewHint") }}
            </div>
          </div>
          <button
            type="button"
            class="td-btn td-btn-add"
            :disabled="!newText.trim() || adding || newAttachmentBusy"
            @click="addEntry"
          >
            {{ t("todosWin.add") }}
            <kbd class="sc">{{ submitKeyLabel }}</kbd>
          </button>
        </div>
        <div
          v-if="fileDropTarget === 'new'"
          class="td-add-drop-overlay"
          aria-hidden="true"
        >
          <svg viewBox="0 0 24 24" fill="none" stroke="currentColor">
            <path d="M12 16V5M8 9l4-4 4 4" />
            <path d="M5 14v4a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-4" />
          </svg>
          <span>{{ t("todosWin.dropOnNew") }}</span>
        </div>
      </div>
    </footer>

    <!-- 清空确认（模态 alert）：清空不可恢复，强制光标移动后再确认。 -->
    <div
      v-if="confirmKind"
      class="td-overlay"
      @click.self="confirmKind = null; operationError = ''"
    >
      <div class="td-dialog" role="alertdialog">
        <h3>
          {{ confirmKind === "todos" ? t("todosWin.clearTitle") : t("todosWin.clearHistTitle") }}
        </h3>
        <p>
          {{
            confirmKind === "todos"
              ? t("todosWin.clearDesc", { n: entries.length })
              : t("todosWin.clearHistDesc", { n: history.length })
          }}
        </p>
        <p v-if="operationError" class="td-attachment-error" role="alert">
          {{ operationError }}
        </p>
        <div class="td-dialog-actions">
          <button
            type="button"
            class="td-btn"
            @click="confirmKind = null; operationError = ''"
          >
            {{ t("todosWin.confirmCancel") }}
          </button>
          <button type="button" class="td-btn td-btn-danger" @click="doConfirmedClear">
            {{ t("todosWin.clearOk") }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.todos-win {
  display: flex;
  flex-direction: column;
  height: 100%;
  color: var(--text-primary);
}
.td-header {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  padding: 10px 14px;
  border-bottom: var(--hairline) solid var(--border);
}
.macos .td-header {
  padding-top: 30px;
}
.td-title {
  font-size: 14px;
  font-weight: 600;
  white-space: nowrap;
}
.td-select {
  flex: 0 1 auto;
  min-width: 0;
  max-width: 60%;
  appearance: auto;
  border: var(--hairline) solid var(--border);
  border-radius: 7px;
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  padding: 3px 8px;
  box-shadow: var(--clickable-shadow);
}
.td-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 12px 14px;
}
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 100%;
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
  animation: td-spin 0.7s linear infinite;
}
@keyframes td-spin {
  to {
    transform: rotate(360deg);
  }
}
.td-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.td-row {
  position: relative;
  display: flex;
  align-items: flex-start;
  gap: 8px;
  padding: 9px 12px;
  border: 1px solid transparent;
  border-radius: var(--radius-sm, 8px);
  background: var(--bg-elevated);
  transition: border-color 0.12s ease, background 0.12s ease, box-shadow 0.12s ease;
}
.td-row.file-drop-target {
  border-color: color-mix(in srgb, #0a84ff 72%, transparent);
  background: color-mix(in srgb, #0a84ff 7%, var(--bg-elevated));
  box-shadow: inset 0 0 0 1px color-mix(in srgb, #0a84ff 22%, transparent);
}
.td-row.selected {
  border-color: color-mix(in srgb, #0a84ff 48%, transparent);
  background: color-mix(in srgb, #0a84ff 7%, var(--bg-elevated));
  box-shadow: inset 0 0 0 1px color-mix(in srgb, #0a84ff 12%, transparent);
  outline: none;
}
.td-row:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}
.td-row.dragging {
  opacity: 0.55;
  border-style: dashed;
  border-color: var(--control-border);
}
.td-row.completing .td-text {
  text-decoration: line-through;
  color: var(--text-secondary);
  opacity: 0.72;
}
.td-row.completing .td-meta {
  opacity: 0.55;
}

/* 手柄列固定宽度：历史行同宽留白，上下复选框对齐。 */
.td-handle-slot {
  flex: 0 0 14px;
  width: 14px;
  height: 20px;
  margin: 1px -2px 0 -4px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}
.td-handle {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 14px;
  height: 20px;
  color: var(--text-secondary);
  opacity: 0.55;
  cursor: grab;
  touch-action: none;
  user-select: none;
}
.td-handle:active {
  cursor: grabbing;
}
.td-row:hover .td-handle,
.td-row.selected .td-handle {
  opacity: 1;
}
.td-handle svg {
  width: 10px;
  height: 14px;
}

/* 圆形复选框。 */
.td-check {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  margin-top: 1px;
  padding: 0;
  border: none;
  border-radius: 50%;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
}
.td-check:hover {
  color: #0a84ff;
}
.td-check.checked {
  color: #0a84ff;
}
.td-check svg {
  width: 16px;
  height: 16px;
  display: block;
}
.td-check-hist {
  /* Solid checked disc, slightly muted; same 16px glyph size for both states. */
  color: color-mix(in srgb, var(--text-secondary) 55%, transparent);
  position: relative;
  border-radius: 50%;
}
.td-check-hist .td-check-done,
.td-check-hist .td-check-restore {
  width: 16px;
  height: 16px;
}
.td-check-hist .td-check-done {
  display: block;
}
.td-check-hist .td-check-restore {
  display: none;
}
/* Hover: same circle size, only icon + blue tint on the solid fill — no bg plate. */
.td-check-hist:hover {
  color: color-mix(in srgb, #0a84ff 72%, var(--text-secondary));
  background: transparent;
}
.td-check-hist:hover .td-check-done {
  display: none;
}
.td-check-hist:hover .td-check-restore {
  display: block;
}

.td-main {
  flex: 1 1 auto;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 3px;
}
.td-top {
  display: flex;
  align-items: flex-start;
  gap: 8px;
}
.td-text {
  flex: 1 1 auto;
  min-width: 0;
  font-size: 13px;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
}
/* Inline edit field: control look; height driven by JS (min ~2 lines, max ~6). */
.td-edit {
  flex: 1 1 auto;
  min-width: 0;
  /* Do not set CSS min-height here — it clamps scrollHeight and blocks auto-grow. */
  max-height: 132px;
  height: 48px; /* initial floor before first JS measure */
  resize: none;
  overflow-y: auto;
  margin: -2px 0 0;
  padding: 6px 8px;
  border: var(--hairline) solid color-mix(in srgb, #0a84ff 50%, var(--control-border));
  border-radius: 7px;
  background: var(--control-bg);
  color: var(--text-primary);
  font: inherit;
  font-size: 13px;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
  box-sizing: border-box;
  box-shadow: var(--clickable-shadow);
}
.td-edit:focus,
.td-edit:focus-visible {
  outline: none;
  border-color: color-mix(in srgb, #0a84ff 70%, var(--control-border));
  box-shadow: var(--focus-ring), var(--clickable-shadow);
}
/* Pencil — same hover-only visibility as .td-copy (right of copy). */
.td-edit-btn {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--text-secondary);
  opacity: 0;
  cursor: pointer;
}
.td-row:hover .td-edit-btn,
.td-row.selected .td-edit-btn {
  opacity: 0.75;
}
.td-edit-btn:hover:not(:disabled),
.td-edit-btn:focus-visible:not(:disabled) {
  opacity: 1;
  color: #0a84ff;
  background: color-mix(in srgb, #0a84ff 14%, transparent);
}
.td-edit-btn:disabled {
  opacity: 0;
  cursor: default;
}
.td-row:hover .td-edit-btn:disabled,
.td-row.selected .td-edit-btn:disabled {
  opacity: 0.3;
}
.td-edit-btn svg {
  width: 12px;
  height: 12px;
}
/* 创建 Agent 任务 ▶：编辑右侧，同 hover-only 交互；绿色 tint 区分「执行」语义。 */
.td-newtask {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--text-secondary);
  opacity: 0;
  cursor: pointer;
}
.td-row:hover .td-newtask,
.td-row.selected .td-newtask {
  opacity: 0.75;
}
.td-newtask:hover:not(:disabled),
.td-newtask:focus-visible:not(:disabled) {
  opacity: 1;
  color: #30d158;
  background: color-mix(in srgb, #30d158 14%, transparent);
}
.td-newtask:disabled {
  opacity: 0;
  cursor: default;
}
.td-row:hover .td-newtask:disabled,
.td-row.selected .td-newtask:disabled {
  opacity: 0.3;
}
.td-newtask svg {
  width: 12px;
  height: 12px;
}
/* Save / Cancel replace the edit control while a row is being edited. */
.td-edit-action {
  flex: 0 0 auto;
  appearance: none;
  height: 18px;
  padding: 0 7px;
  border: var(--hairline) solid var(--control-border);
  border-radius: 5px;
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 11px;
  font-weight: 600;
  line-height: 1;
  white-space: nowrap;
  cursor: pointer;
}
.td-edit-action:disabled {
  opacity: 0.45;
  cursor: default;
}
.td-edit-save {
  border-color: transparent;
  background: #0a84ff;
  color: #fff;
}
.td-edit-save:hover:not(:disabled) {
  background: #0071e3;
}
.td-edit-cancel:hover:not(:disabled) {
  background: var(--control-hover-bg);
}
.td-meta {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: 4px 6px;
  min-height: 18px;
  color: var(--text-muted, #8e8e93);
  font-size: 11px;
  line-height: 1.3;
}
.td-time-anchor {
  position: relative;
  display: inline-flex;
  align-items: center;
}
.td-time {
  cursor: default;
}
.td-time-tip {
  position: absolute;
  left: 0;
  bottom: calc(100% + 6px);
  z-index: 20;
  width: max-content;
  max-width: min(280px, calc(100vw - 28px));
  padding: 5px 8px;
  border: var(--hairline) solid var(--border);
  border-radius: 6px;
  background: var(--surface-overlay);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.2);
  color: var(--text-primary);
  font-size: 11px;
  font-weight: 400;
  line-height: 1.35;
  white-space: nowrap;
  pointer-events: none;
}
.td-source {
  color: var(--text-muted, #8e8e93);
}

/* 自动执行 ⚡：跟在 meta 文字后；off 时随行 hover 淡入，on 时常显琥珀色。 */
.td-auto {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--text-secondary);
  opacity: 0;
  cursor: pointer;
}
.td-row:hover .td-auto,
.td-row.selected .td-auto {
  opacity: 0.75;
}
.td-auto.on {
  opacity: 1;
  color: #ff9f0a;
}
.td-auto:hover {
  opacity: 1;
  background: color-mix(in srgb, #ff9f0a 14%, transparent);
  color: #ff9f0a;
}
.td-auto svg {
  width: 12px;
  height: 12px;
}
/* 复制正文：跟在 ⚡ 右侧，仅行 hover 出现；成功后短暂勾号反馈。 */
.td-copy {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 18px;
  height: 18px;
  padding: 0;
  border: none;
  border-radius: 5px;
  background: transparent;
  color: var(--text-secondary);
  opacity: 0;
  cursor: pointer;
}
.td-row:hover .td-copy,
.td-row.selected .td-copy {
  opacity: 0.75;
}
.td-copy:hover {
  opacity: 1;
  background: color-mix(in srgb, #0a84ff 14%, transparent);
  color: #0a84ff;
}
.td-copy.done {
  opacity: 1;
  color: #30d158;
}
.td-copy svg {
  width: 12px;
  height: 12px;
}
/* 添加行的 ⚡ 开关：常显（不依赖行 hover），带「自动」文字标签便于理解。 */
.td-auto-hint-anchor {
  position: relative;
  display: inline-flex;
  align-items: center;
  flex: 0 0 auto;
}
.td-auto-new {
  opacity: 0.75;
  align-self: center;
  border: var(--hairline) solid var(--border);
  width: auto;
  height: 28px;
  padding: 0 8px;
  gap: 3px;
  border-radius: 7px;
  font-size: 11px;
  font-weight: 600;
  white-space: nowrap;
}
.td-auto-new.on {
  opacity: 1;
  border-color: color-mix(in srgb, #ff9f0a 55%, transparent);
  background: color-mix(in srgb, #ff9f0a 12%, transparent);
}
.td-auto-hint {
  position: absolute;
  right: 0;
  bottom: calc(100% + 8px);
  z-index: 20;
  width: max-content;
  max-width: min(280px, calc(100vw - 28px));
  padding: 7px 9px;
  border: var(--hairline) solid var(--border);
  border-radius: 7px;
  background: var(--surface-overlay);
  box-shadow: 0 5px 18px rgba(0, 0, 0, 0.22);
  color: var(--text-primary);
  font-size: 11px;
  font-weight: 400;
  line-height: 1.45;
  text-align: left;
  white-space: normal;
  pointer-events: none;
}
.td-auto-hint::after {
  content: "";
  position: absolute;
  right: 18px;
  bottom: -5px;
  width: 8px;
  height: 8px;
  border-right: var(--hairline) solid var(--border);
  border-bottom: var(--hairline) solid var(--border);
  background: var(--surface-overlay);
  transform: rotate(45deg);
}
.td-del {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 20px;
  height: 20px;
  margin-top: 1px;
  padding: 0;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
  opacity: 0;
}
.td-row:hover .td-del,
.td-row.selected .td-del {
  opacity: 1;
}
.td-del:hover {
  background: color-mix(in srgb, #ff453a 14%, transparent);
  color: #ff453a;
}
.td-del svg {
  width: 12px;
  height: 12px;
}
/* 二次确认态：✕ 变红色文字按钮，再点才真正删除。 */
.td-del-confirm {
  flex: 0 0 auto;
  margin-top: 1px;
  padding: 1px 8px;
  border: 1px solid color-mix(in srgb, #ff453a 45%, transparent);
  border-radius: 6px;
  background: color-mix(in srgb, #ff453a 12%, transparent);
  color: #ff453a;
  font-size: 11px;
  font-weight: 600;
  white-space: nowrap;
  cursor: pointer;
}
.td-del-confirm:hover {
  background: color-mix(in srgb, #ff453a 22%, transparent);
}
.td-footer {
  flex: 0 0 auto;
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 14px 12px;
  border-top: var(--hairline) solid var(--border);
}
.td-add {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.td-input {
  display: block;
  width: 100%;
  min-width: 0;
  /* Height from JS (min/max); avoid CSS min-height clamping scrollHeight. */
  max-height: 132px;
  height: 56px;
  resize: none;
  overflow-y: auto;
  border: var(--hairline) solid var(--control-border);
  border-radius: 7px;
  background: var(--control-bg);
  color: var(--text-primary);
  font: inherit;
  font-size: 12px;
  line-height: 1.45;
  padding: 7px 9px;
  box-shadow: var(--clickable-shadow);
  box-sizing: border-box;
}
.td-input:focus,
.td-input:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring), var(--clickable-shadow);
}
/* B1: actions under the full-width textarea, right-aligned. */
.td-add-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
}
.td-btn {
  appearance: none;
  flex: 0 0 auto;
  border: var(--hairline) solid var(--control-border);
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  font-weight: 600;
  padding: 5px 12px;
  border-radius: 7px;
  cursor: pointer;
  box-shadow: var(--clickable-shadow);
}
.td-btn:hover:not(:disabled) {
  background: var(--control-hover-bg);
}
.td-btn:disabled {
  opacity: 0.45;
  cursor: default;
}
.td-btn-add {
  border-color: transparent;
  background: #0a84ff;
  color: #fff;
}
.td-btn-add:hover:not(:disabled) {
  background: #0071e3;
}
/* Shortcut badge on Add — same style idea as popup footer `.btn .sc`. */
.td-btn .sc {
  margin-left: 6px;
  font-size: 11px;
  line-height: 1;
  opacity: 0.85;
  font-family: inherit;
  border: none;
  background: transparent;
  padding: 0;
  color: inherit;
}
.td-btn-danger {
  border-color: transparent;
  background: #ff453a;
  color: #fff;
}
.td-btn-danger:hover {
  background: #e0352b;
}
.td-clear-row {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 6px;
}
.td-clear {
  appearance: none;
  border: none;
  background: transparent;
  color: var(--text-secondary);
  font-size: 11px;
  font-weight: 600;
  padding: 2px 6px;
  border-radius: 5px;
  cursor: pointer;
}
.td-clear:hover {
  background: color-mix(in srgb, #ff453a 12%, transparent);
  color: #ff453a;
}
/* 空态与历史并存时不再全高居中。 */
.empty.compact {
  height: auto;
  padding: 28px 0 20px;
}
/* ===== 执行历史折叠区 ===== */
.td-hist {
  margin-top: 14px;
}
.td-hist-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}
.td-hist-toggle {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  border: none;
  background: transparent;
  padding: 2px 4px;
  color: var(--text-secondary);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  border-radius: 5px;
}
.td-hist-toggle:hover {
  color: var(--text-primary);
}
.td-hist-caret {
  display: inline-block;
  font-size: 10px;
  transition: transform 0.15s ease;
}
.td-hist-caret.open {
  transform: rotate(90deg);
}
.td-hist-count {
  padding: 0 6px;
  border-radius: 8px;
  background: color-mix(in srgb, var(--text-primary) 10%, transparent);
  font-size: 11px;
  font-weight: 600;
}
.td-hist-list {
  margin-top: 8px;
}
.td-hist-row {
  opacity: 0.78;
}
.td-hist-row .td-text {
  color: var(--text-secondary);
}
.td-file-drop-overlay,
.td-add-drop-overlay {
  position: absolute;
  inset: 2px;
  z-index: 12;
  display: flex;
  align-items: center;
  justify-content: center;
  gap: 7px;
  border: 1.5px dashed color-mix(in srgb, #0a84ff 78%, transparent);
  border-radius: 8px;
  background: color-mix(in srgb, var(--bg-elevated) 80%, #0a84ff 20%);
  color: #0a84ff;
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.01em;
  pointer-events: none;
  box-shadow: 0 5px 18px color-mix(in srgb, #0a84ff 15%, transparent);
}
.td-file-drop-overlay svg,
.td-add-drop-overlay svg {
  width: 18px;
  height: 18px;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}
.td-add-drop-overlay {
  inset: -4px;
  border-radius: 9px;
  background: color-mix(in srgb, var(--control-bg) 84%, #0a84ff 16%);
}
.td-row-attachment-error {
  margin-top: 2px;
}
.td-composer-attachments {
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
  margin: 0;
  padding: 0 2px;
}
.td-composer-attachments :deep(.reply-files) {
  margin-top: 0;
}
.td-attachment-error {
  margin: 0;
  color: #ff453a;
  font-size: 11px;
}
.td-attach-picker {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  margin-right: auto;
}
.td-attach-picker svg {
  width: 14px;
  height: 14px;
}
.td-attach-btn {
  width: auto;
  min-width: 24px;
  padding: 0 4px;
  font-size: 12px;
}
/* ===== 清空确认模态（与历史窗口 .overlay/.dialog 同构） ===== */
.td-overlay {
  position: fixed;
  inset: 0;
  z-index: 50;
  display: flex;
  align-items: center;
  justify-content: center;
  background: rgba(0, 0, 0, 0.32);
}
.td-dialog {
  width: 300px;
  padding: 20px;
  border-radius: var(--radius, 12px);
  background: var(--surface-overlay);
  border: var(--hairline) solid var(--border);
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3);
}
.td-dialog h3 {
  margin: 0 0 8px;
  font-size: 14px;
}
.td-dialog p {
  margin: 0 0 18px;
  font-size: 12px;
  color: var(--text-secondary);
}
.td-dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
</style>
