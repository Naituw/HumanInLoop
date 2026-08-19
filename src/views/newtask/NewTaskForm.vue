<script setup lang="ts">
// 「新建 Agent 任务」共享表单主体（spec gui-agent-task-launch + gui-agent-console C4）：
// 项目 → 任务来源（直接输入 / 项目待办 + 可选补充）→ Agent → 权限 → 启动。
// 窗口版（NewTaskView）与控制台内嵌版（NewTaskPane）共用；宿主负责主题/语言与启动后的收尾
// （关窗 / 选中新会话），本组件成功后仅 emit `launched`。
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  agentTaskReadiness,
  newTaskInit,
  newTaskLaunch,
  newTaskProjects,
  newTaskProjectsRefreshed,
  openPath,
  openSettings,
  projectKeyOf,
  todosList,
} from "../../lib/ipc";
import type {
  AgentKind,
  AgentTaskReadiness,
  NewTaskProject,
  PopupSubmitKey,
  TodoEntry,
} from "../../lib/types";
import { AGENT_INSTALL_DOCS } from "../settings/useAgentTasks";
import { primaryModifierPressed, primaryShortcutLabel } from "../../lib/platform";
import LaunchPermission from "./LaunchPermission.vue";

const { t } = useI18n();

const props = defineProps<{
  /** 锁定项目（控制台内嵌版）：项目区只读展示、不加载候选下拉。 */
  lockProject?: string | null;
  /** 初始预选（窗口版 URL 参数）；挂载时应用。 */
  initialProject?: string | null;
  initialTodo?: string | null;
}>();

const emit = defineEmits<{ launched: [] }>();

// ===== init =====
const popupSubmitKey = ref<PopupSubmitKey>("cmdEnter");
const submitWithBareEnter = computed(() => popupSubmitKey.value === "enter");
const submitKeyLabel = computed(() =>
  submitWithBareEnter.value ? "↵" : primaryShortcutLabel("enter")
);
/** `agentTasks.permissionPrompt`（G6）：ask 显示单选，另两态为固定模式。 */
const permissionPrompt = ref<string>("ask");
const loaded = ref(false);

const locked = computed(() => !!props.lockProject);

// ===== 项目 =====
const projects = ref<NewTaskProject[]>([]);
const selectedProject = ref("");
/** 所选 workspace 的 git 根 project key（待办按此读取 / 出队）。 */
const projectKey = ref("");

const projectsWorkspace = computed(() =>
  projects.value.filter((p) => p.source === "workspace")
);
const projectsTodos = computed(() =>
  projects.value.filter((p) => p.source !== "workspace")
);

function basename(key: string): string {
  const parts = key.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || key;
}

const lockedLabel = computed(() =>
  props.lockProject ? basename(props.lockProject) : ""
);

/** 预选项目不在候选中（如待办项目未入 workspace 索引）→ 兜底追加，保持选中稳定。 */
function applyProjects(list: NewTaskProject[]): void {
  if (
    selectedProject.value &&
    !list.some((p) => p.path === selectedProject.value)
  ) {
    list = list.concat({
      path: selectedProject.value,
      label: basename(selectedProject.value),
      pinned: false,
      source: "todos",
    });
  }
  projects.value = list;
  if (!selectedProject.value) {
    selectedProject.value = list[0]?.path ?? "";
  }
}

// ===== 任务来源（G3/G7/G8）=====
const todos = ref<TodoEntry[]>([]);
/** 选中的待办**快照**（IM D31 语义：并发删除仍按快照启动）；null = 直接输入。 */
const selectedTodo = ref<{
  id: string;
  text: string;
  auto: boolean;
  project: string;
  attachments: NonNullable<TodoEntry["attachments"]>;
} | null>(null);
/** 选中的待办已被别处删除（仍可按快照启动，仅提示）。 */
const todoMissing = ref(false);
const inputText = ref("");

async function loadTodosFor(path: string): Promise<void> {
  if (!path) {
    projectKey.value = "";
    todos.value = [];
    return;
  }
  try {
    projectKey.value = await projectKeyOf(path);
    todos.value = projectKey.value ? await todosList(projectKey.value) : [];
  } catch (err) {
    console.warn("new-task todos load failed", err);
    todos.value = [];
  }
}

async function onProjectChange(): Promise<void> {
  // 切换项目：清除待办预选（G8），任务来源回到「直接输入」。
  selectedTodo.value = null;
  todoMissing.value = false;
  launchError.value = "";
  await loadTodosFor(selectedProject.value);
}

function selectManual(): void {
  selectedTodo.value = null;
  todoMissing.value = false;
}

function selectTodo(entry: TodoEntry): void {
  selectedTodo.value = {
    id: entry.id,
    text: entry.text,
    auto: entry.auto ?? false,
    project: projectKey.value,
    attachments: (entry.attachments ?? []).map((attachment) => ({ ...attachment })),
  };
  todoMissing.value = false;
}

// ===== Agent（G4/G5）=====
const readiness = ref<AgentTaskReadiness[] | null>(null);
const selectedKind = ref<AgentKind | null>(null);

async function loadReadiness(): Promise<void> {
  try {
    const list = await agentTaskReadiness();
    readiness.value = list;
    // 已选 Agent 不再就绪（重查后）→ 取消选择。
    if (
      selectedKind.value &&
      !list.some((r) => r.kind === selectedKind.value && r.ready)
    ) {
      selectedKind.value = null;
    }
  } catch (err) {
    console.warn("new-task readiness failed", err);
    readiness.value = [];
  }
}

function selectAgent(item: AgentTaskReadiness): void {
  if (!item.ready) return;
  selectedKind.value = item.kind;
}

/** Missing readiness links: binary opens vendor docs; integration capabilities open the Agent card. */
async function openIssue(
  kind: AgentKind,
  issue: "binary" | "lifecycle" | "integration"
): Promise<void> {
  try {
    if (issue === "binary") {
      await openPath(AGENT_INSTALL_DOCS[kind]);
    } else if (issue === "lifecycle") {
      await openSettings(`integration#lifecycle-${kind}`);
    } else {
      await openSettings(`integration#integration-${kind}`);
    }
  } catch (err) {
    console.warn("new-task open issue failed", err);
  }
}

// ===== 权限（G6）=====
/** ask 模式下的用户选择（不预选，IM D19 同语义）；固定模式下忽略。 */
const permissionChoice = ref<"agent-default" | "yolo" | null>(null);
const effectivePermission = computed<"agent-default" | "yolo" | null>(() => {
  if (selectedKind.value === "pi") return "agent-default";
  if (permissionPrompt.value === "agent-default") return "agent-default";
  if (permissionPrompt.value === "yolo") return "yolo";
  return permissionChoice.value;
});

// ===== 任务拼装与校验（spec §3.3）=====
const MAX_TASK_CHARS = 3000;

/** 最终任务 = 待办原文 + 空行 + 补充（补充为空则仅原文）；直接输入 = 输入本身。 */
const finalTask = computed(() => {
  const input = inputText.value.trim();
  if (selectedTodo.value) {
    const base = selectedTodo.value.text.trim();
    return input ? `${base}\n\n${input}` : base;
  }
  return input;
});
const taskChars = computed(() => [...finalTask.value].length);
const tooLong = computed(() => taskChars.value > MAX_TASK_CHARS);

const canLaunch = computed(
  () =>
    !launching.value &&
    !!selectedProject.value &&
    !!selectedKind.value &&
    !!effectivePermission.value &&
    finalTask.value.length > 0 &&
    !tooLong.value
);

// ===== 启动 =====
const launching = ref(false);
const launchError = ref("");

async function launch(): Promise<void> {
  if (!canLaunch.value || !selectedKind.value || !effectivePermission.value) {
    return;
  }
  launching.value = true;
  launchError.value = "";
  try {
    await newTaskLaunch({
      workspace: selectedProject.value,
      kind: selectedKind.value,
      permission: effectivePermission.value,
      task: finalTask.value,
      todoProject: selectedTodo.value?.project ?? null,
      todoId: selectedTodo.value?.id ?? null,
      todoAttachments:
        selectedTodo.value?.attachments.map((attachment) => ({
          id: attachment.id,
          name: attachment.name,
          path: attachment.path,
          sourcePath: attachment.sourcePath,
          storage: attachment.storage,
        })) ?? [],
    });
  } catch (err) {
    launchError.value = t("newTask.launchFailed", { e: String(err) });
    return;
  } finally {
    launching.value = false;
  }
  // 成功收尾由宿主决定（窗口版关窗 / 控制台版选中新会话）。
  emit("launched");
}

function onInputKeydown(e: KeyboardEvent): void {
  // 与弹窗/待办窗口一致的提交快捷键；IME 组词中的 Enter 不当提交。
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
  void launch();
}

// ===== 预选与重置（窗口版 goto 事件经 defineExpose 调用）=====
async function applyPreselect(
  project: string | null,
  todoId: string | null
): Promise<void> {
  if (project) {
    selectedProject.value = project;
    applyProjects(projects.value.slice());
  }
  selectedTodo.value = null;
  todoMissing.value = false;
  launchError.value = "";
  inputText.value = "";
  await loadTodosFor(selectedProject.value);
  if (todoId) {
    const entry = todos.value.find((e) => e.id === todoId);
    if (entry) selectTodo(entry);
  }
}

defineExpose({ applyPreselect });

function applyPopupSubmitKey(value: unknown): void {
  if (value === "enter" || value === "cmdEnter") {
    popupSubmitKey.value = value;
  }
}

let unlistenSettings: UnlistenFn | null = null;
let unlistenTodos: UnlistenFn | null = null;

onMounted(async () => {
  try {
    const init = await newTaskInit();
    applyPopupSubmitKey(init.popupSubmitKey);
    permissionPrompt.value = init.permissionPrompt;
  } catch {
    /* 读取失败：保持兜底默认 */
  }
  if (props.lockProject) {
    selectedProject.value = props.lockProject;
  } else if (props.initialProject) {
    selectedProject.value = props.initialProject;
  }

  unlistenSettings = await listen<{ popupSubmitKey?: PopupSubmitKey }>(
    "settings-updated",
    (e) => {
      applyPopupSubmitKey(e.payload.popupSubmitKey);
    }
  );
  // todos.json 被任意进程改写 → 重载所选项目待办；选中待办被删时保留快照并提示（IM D31）。
  unlistenTodos = await listen("todos-updated", async () => {
    await loadTodosFor(selectedProject.value);
    if (
      selectedTodo.value &&
      !todos.value.some((e) => e.id === selectedTodo.value?.id)
    ) {
      todoMissing.value = true;
    }
  });

  // 首屏：本地项目列表 + 所选项目待办（瞬时）；Agent 就绪与冷扫描合并放后台。
  // 锁定项目（控制台内嵌）不加载候选下拉。
  try {
    if (!locked.value) {
      applyProjects(await newTaskProjects());
    }
    await applyPreselect(null, props.initialTodo ?? null);
  } catch (err) {
    console.warn("new-task init load failed", err);
  } finally {
    loaded.value = true;
  }
  void loadReadiness();
  if (!locked.value) {
    void (async () => {
      try {
        applyProjects(await newTaskProjectsRefreshed());
      } catch (err) {
        console.warn("new-task projects refresh failed", err);
      }
    })();
  }
});

onBeforeUnmount(() => {
  unlistenSettings?.();
  unlistenTodos?.();
});
</script>

<template>
  <div class="nt-form">
    <div class="nt-body">
      <div v-if="!loaded" class="nt-empty">
        <span class="nt-spinner" />
      </div>

      <div v-else-if="!locked && !projects.length" class="nt-empty">
        <p class="nt-empty-title">{{ t("newTask.noProjects") }}</p>
        <p class="nt-empty-hint">{{ t("newTask.noProjectsHint") }}</p>
      </div>

      <template v-else>
        <!-- 项目 -->
        <section class="nt-section">
          <label class="nt-label" for="nt-project">{{ t("newTask.projectLabel") }}</label>
          <p v-if="locked" class="nt-locked-project" :title="lockProject ?? ''">
            {{ lockedLabel }}
            <span class="nt-locked-path">{{ lockProject }}</span>
          </p>
          <select
            v-else
            id="nt-project"
            v-model="selectedProject"
            class="nt-select"
            @change="onProjectChange"
          >
            <optgroup
              v-if="projectsWorkspace.length"
              :label="t('newTask.projectSectionWorkspace')"
            >
              <option
                v-for="p in projectsWorkspace"
                :key="p.path"
                :value="p.path"
                :title="p.path"
              >
                {{ p.pinned ? "★ " : "" }}{{ p.label }}
              </option>
            </optgroup>
            <optgroup
              v-if="projectsTodos.length"
              :label="t('newTask.projectSectionTodos')"
            >
              <option
                v-for="p in projectsTodos"
                :key="p.path"
                :value="p.path"
                :title="p.path"
              >
                {{ p.label }}
              </option>
            </optgroup>
          </select>
        </section>

        <!-- 任务来源：直接输入 / 项目待办（全部列出、可滚动，G9） -->
        <section class="nt-section">
          <span class="nt-label">{{ t("newTask.taskLabel") }}</span>
          <div v-if="todos.length || selectedTodo" class="nt-choices">
            <button
              type="button"
              class="nt-choice"
              :class="{ active: !selectedTodo }"
              @click="selectManual"
            >
              <span class="nt-radio" :class="{ on: !selectedTodo }" />
              <span class="nt-choice-text">{{ t("newTask.manualEntry") }}</span>
            </button>
            <button
              v-for="e in todos"
              :key="e.id"
              type="button"
              class="nt-choice"
              :class="{ active: selectedTodo?.id === e.id }"
              @click="selectTodo(e)"
            >
              <span class="nt-radio" :class="{ on: selectedTodo?.id === e.id }" />
              <span
                class="nt-choice-text"
                :class="{ clamp: selectedTodo?.id !== e.id }"
              >
                <!-- 待办前缀：与飞书/钉钉任务卡的琥珀色【TODO】标记同语义（IM D29）。 -->
                <span class="nt-todo-tag">{{ t("newTask.todoTag") }}</span
                ><span v-if="e.auto" class="nt-auto">⚡</span>{{ e.text }}
                <span v-if="e.attachments?.length" class="nt-file-count">
                  {{ t("common.attachmentBadge", { n: e.attachments.length }) }}
                </span>
              </span>
            </button>
            <!-- 快照兜底：选中的待办已被删除（不再出现在列表）仍显示为选中项。 -->
            <button
              v-if="selectedTodo && !todos.some((e) => e.id === selectedTodo?.id)"
              type="button"
              class="nt-choice active"
            >
              <span class="nt-radio on" />
              <span class="nt-choice-text">
                <span class="nt-todo-tag">{{ t("newTask.todoTag") }}</span
                ><span v-if="selectedTodo.auto" class="nt-auto">⚡</span
                >{{ selectedTodo.text }}
                <span
                  v-if="selectedTodo.attachments?.length"
                  class="nt-file-count"
                >
                  {{ t("common.attachmentBadge", { n: selectedTodo.attachments.length }) }}
                </span>
              </span>
            </button>
          </div>
          <div
            v-if="selectedTodo?.attachments.length"
            class="nt-selected-files"
          >
            <span
              v-for="attachment in selectedTodo.attachments"
              :key="attachment.id"
              class="nt-selected-file"
              :title="attachment.sourcePath"
            >
              📎 {{ attachment.name }}
            </span>
          </div>
          <p v-if="todoMissing" class="nt-note">{{ t("newTask.todoMissing") }}</p>
          <label class="nt-sublabel" for="nt-input">
            {{ selectedTodo ? t("newTask.supplementLabel") : todos.length ? t("newTask.taskLabel") : "" }}
          </label>
          <textarea
            id="nt-input"
            v-model="inputText"
            class="nt-input"
            rows="3"
            :placeholder="
              selectedTodo
                ? t('newTask.supplementPlaceholder')
                : t('newTask.taskPlaceholder')
            "
            @keydown="onInputKeydown"
          />
          <p v-if="tooLong" class="nt-error">
            {{ t("newTask.tooLong", { n: taskChars }) }}
          </p>
        </section>

        <!-- Agent：五家全列（G5 + Pi） -->
        <section class="nt-section">
          <span class="nt-label">{{ t("newTask.agentLabel") }}</span>
          <div v-if="readiness === null" class="nt-agent-loading">
            <span class="nt-spinner small" />
            <span>{{ t("newTask.agentLoading") }}</span>
          </div>
          <div v-else class="nt-agents">
            <button
              v-for="item in readiness"
              :key="item.kind"
              type="button"
              class="nt-agent"
              :class="{
                active: selectedKind === item.kind,
                disabled: !item.ready,
              }"
              @click="selectAgent(item)"
            >
              <span class="nt-radio" :class="{ on: selectedKind === item.kind }" />
              <span class="nt-agent-main">
                <span class="nt-agent-name">{{ item.label }}</span>
                <span v-if="item.ready" class="nt-agent-sub" :title="item.executable ?? ''">
                  {{ item.integrationMode.toUpperCase() }} · {{ item.executable }}
                </span>
                <span v-else class="nt-agent-issues">
                  <template
                    v-for="issue in (['binary', 'lifecycle', 'integration'] as const)"
                    :key="issue"
                  >
                    <span
                      v-if="
                        issue === 'binary'
                          ? item.binaryReady
                          : issue === 'lifecycle'
                            ? item.lifecycleReady
                            : item.integrationReady
                      "
                      class="nt-issue ok"
                    >
                      {{ t(`newTask.ready${issue.charAt(0).toUpperCase() + issue.slice(1)}`) }} ✓
                    </span>
                    <a
                      v-else
                      class="nt-issue bad"
                      href="#"
                      @click.prevent.stop="openIssue(item.kind, issue)"
                    >
                      {{ t(`newTask.ready${issue.charAt(0).toUpperCase() + issue.slice(1)}`) }} ✗
                    </a>
                  </template>
                </span>
              </span>
            </button>
          </div>
        </section>

        <LaunchPermission
          v-if="selectedKind !== 'pi'"
          v-model="permissionChoice"
          :permission-prompt="permissionPrompt"
        />
        <p v-else class="nt-sublabel">{{ t("newTask.piPermissionHint") }}</p>
      </template>
    </div>

    <footer v-if="loaded && (locked || projects.length)" class="nt-footer">
      <p v-if="launchError" class="nt-error">{{ launchError }}</p>
      <div class="nt-footer-row">
        <span class="nt-note">{{ t("newTask.launchNote") }}</span>
        <button
          type="button"
          class="nt-btn nt-btn-launch"
          :disabled="!canLaunch"
          @click="launch"
        >
          {{ launching ? t("newTask.launching") : t("newTask.launch") }}
          <kbd v-if="!launching" class="sc">{{ submitKeyLabel }}</kbd>
        </button>
      </div>
    </footer>
  </div>
</template>

<style scoped>
.nt-form {
  display: flex;
  flex-direction: column;
  flex: 1 1 auto;
  min-height: 0;
  color: var(--text-primary);
}
.nt-body {
  flex: 1 1 auto;
  min-height: 0;
  overflow-y: auto;
  padding: 12px 14px;
  display: flex;
  flex-direction: column;
  gap: 14px;
}
.nt-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  height: 100%;
  text-align: center;
}
.nt-empty-title {
  font-size: 14px;
  font-weight: 600;
  margin: 0;
}
.nt-empty-hint {
  font-size: 12px;
  color: var(--text-secondary);
  margin: 0;
  max-width: 320px;
}
.nt-spinner {
  width: 20px;
  height: 20px;
  border-radius: 50%;
  border: 2px solid color-mix(in srgb, var(--text-primary) 18%, transparent);
  border-top-color: var(--text-secondary);
  animation: nt-spin 0.7s linear infinite;
}
.nt-spinner.small {
  width: 14px;
  height: 14px;
}
@keyframes nt-spin {
  to {
    transform: rotate(360deg);
  }
}
.nt-section {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.nt-label {
  font-size: 12px;
  font-weight: 600;
  color: var(--text-secondary);
}
.nt-sublabel {
  font-size: 11px;
  color: var(--text-secondary);
}
.nt-sublabel:empty {
  display: none;
}
.nt-locked-project {
  margin: 0;
  font-size: 12.5px;
  font-weight: 600;
}
.nt-locked-path {
  display: block;
  font-size: 11px;
  font-weight: 400;
  color: var(--text-tertiary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.nt-select {
  width: 100%;
  min-width: 0;
  appearance: auto;
  border: var(--hairline) solid var(--border);
  border-radius: 7px;
  background: var(--control-bg);
  color: var(--text-primary);
  font-size: 12px;
  padding: 5px 8px;
  box-shadow: var(--clickable-shadow);
}
/* 任务来源 / 权限：radio 卡片列表（待办全部列出、区域内滚动，G9）。 */
.nt-choices {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-height: 180px;
  overflow-y: auto;
}
.nt-choice {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  text-align: left;
  padding: 7px 10px;
  border: var(--hairline) solid var(--border);
  border-radius: 8px;
  background: var(--bg-elevated);
  color: var(--text-primary);
  font: inherit;
  font-size: 12.5px;
  line-height: 1.4;
  cursor: pointer;
}
.nt-choice:hover {
  background: var(--control-hover-bg);
}
.nt-choice.active {
  border-color: color-mix(in srgb, #0a84ff 55%, var(--border));
  background: color-mix(in srgb, #0a84ff 8%, var(--bg-elevated));
}
.nt-choice.danger.active {
  border-color: color-mix(in srgb, #ff453a 55%, var(--border));
  background: color-mix(in srgb, #ff453a 8%, var(--bg-elevated));
}
.nt-radio {
  flex: 0 0 auto;
  width: 12px;
  height: 12px;
  margin-top: 2px;
  border-radius: 50%;
  border: 1.4px solid var(--text-secondary);
  box-sizing: border-box;
}
.nt-radio.on {
  border-color: #0a84ff;
  background: radial-gradient(circle, #0a84ff 0 3.5px, transparent 4px);
}
.nt-choice.danger .nt-radio.on {
  border-color: #ff453a;
  background: radial-gradient(circle, #ff453a 0 3.5px, transparent 4px);
}
.nt-choice-text {
  min-width: 0;
  white-space: pre-wrap;
  word-break: break-word;
}
.nt-choice-text.clamp {
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.nt-choice-sub {
  display: block;
  font-size: 11px;
  color: var(--text-secondary);
  margin-top: 1px;
}
.nt-auto {
  color: #ff9f0a;
  margin-right: 3px;
}
/* Todo prefix tag: same semantics as the amber 【TODO】 marker on Feishu/DingTalk task cards (IM D29). */
.nt-todo-tag {
  color: #ff9f0a;
  font-weight: 600;
  margin-right: 2px;
}
.nt-file-count {
  margin-left: 6px;
  color: var(--text-secondary);
  font-size: inherit;
  white-space: nowrap;
}
.nt-selected-files {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
  margin-top: 7px;
}
.nt-selected-file {
  max-width: 220px;
  overflow: hidden;
  padding: 3px 7px;
  border: var(--hairline) solid var(--border);
  border-radius: 6px;
  color: var(--text-secondary);
  font-size: 10px;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.nt-input {
  display: block;
  width: 100%;
  min-width: 0;
  min-height: 60px;
  max-height: 150px;
  resize: vertical;
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
.nt-input:focus,
.nt-input:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring), var(--clickable-shadow);
}
/* Agent 区（G5）：五家全列；未就绪灰显 + 原因链接。 */
.nt-agent-loading {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--text-secondary);
  padding: 6px 2px;
}
.nt-agents {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.nt-agent {
  display: flex;
  align-items: flex-start;
  gap: 8px;
  text-align: left;
  padding: 7px 10px;
  border: var(--hairline) solid var(--border);
  border-radius: 8px;
  background: var(--bg-elevated);
  color: var(--text-primary);
  font: inherit;
  cursor: pointer;
}
.nt-agent:hover:not(.disabled) {
  background: var(--control-hover-bg);
}
.nt-agent.active {
  border-color: color-mix(in srgb, #0a84ff 55%, var(--border));
  background: color-mix(in srgb, #0a84ff 8%, var(--bg-elevated));
}
.nt-agent.disabled {
  cursor: default;
}
.nt-agent.disabled .nt-agent-name,
.nt-agent.disabled .nt-radio {
  opacity: 0.45;
}
.nt-agent-main {
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.nt-agent-name {
  font-size: 12.5px;
  font-weight: 600;
}
.nt-agent-sub {
  font-size: 11px;
  color: var(--text-secondary);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.nt-agent-issues {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  font-size: 11px;
}
.nt-issue.ok {
  color: var(--text-secondary);
  opacity: 0.6;
}
.nt-issue.bad {
  color: #ff453a;
  text-decoration: none;
}
.nt-issue.bad:hover {
  text-decoration: underline;
}
.nt-badge-danger {
  display: inline-block;
  margin-left: 6px;
  padding: 0 5px;
  border-radius: 5px;
  background: color-mix(in srgb, #ff453a 16%, transparent);
  color: #ff453a;
  font-size: 10px;
  font-weight: 700;
  line-height: 1.6;
  vertical-align: 1px;
}
.nt-permission-fixed {
  margin: 0;
  font-size: 12.5px;
}
.nt-note {
  margin: 0;
  font-size: 11px;
  color: var(--text-secondary);
}
.nt-error {
  margin: 0;
  font-size: 11.5px;
  color: #ff453a;
  white-space: pre-wrap;
  word-break: break-word;
}
.nt-footer {
  flex: 0 0 auto;
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 14px 12px;
  border-top: var(--hairline) solid var(--border);
}
.nt-footer-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
}
.nt-btn {
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
.nt-btn:disabled {
  opacity: 0.45;
  cursor: default;
}
.nt-btn-launch {
  border-color: transparent;
  background: #0a84ff;
  color: #fff;
}
.nt-btn-launch:hover:not(:disabled) {
  background: #0071e3;
}
.nt-btn .sc {
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
</style>
