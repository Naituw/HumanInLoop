<script setup lang="ts">
// 项目未暂存变更状态条（spec gui-agent-console C15/C16）：输入框上方细条，展开为文件列表，
// 单文件再展开 hunk 视图；单文件暂存直接执行、「全部暂存」行内二次确认。
// 刷新时机（C16）：项目变化 / 展开面板 / 帧含编辑步（父级防抖后 bump editTick）/ 窗口重获焦点；
// 上次请求未返回不重发（前端 inflight + 后端互斥双保险），不做常驻轮询。
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useI18n } from "vue-i18n";
import { consoleDiffFile, consoleDiffStat, consoleStage } from "../../lib/ipc";
import type { DiffFileStat, DiffFileView } from "../../lib/types";

const { t } = useI18n();

const props = defineProps<{
  /** 项目内目录（会话 cwd；后端映射 git 根）。 */
  project: string;
  /** 帧含编辑/写入步时父级递增（已防抖）。 */
  editTick: number;
}>();

const files = ref<DiffFileStat[]>([]);
const gitRoot = ref("");
const open = ref(false);
const openFile = ref<string | null>(null);
const hunks = ref<DiffFileView | null>(null);
const confirmAll = ref(false);
const inflight = ref(false);
const statError = ref(false);

const totals = computed(() => {
  let adds = 0;
  let dels = 0;
  let added = 0;
  for (const f of files.value) {
    adds += f.adds;
    dels += f.dels;
    if (f.kind === "A") added += 1;
  }
  return { files: files.value.length, adds, dels, added };
});

async function refresh(): Promise<void> {
  if (inflight.value || !props.project) return;
  inflight.value = true;
  try {
    const page = await consoleDiffStat(props.project);
    files.value = page.files;
    gitRoot.value = page.root;
    statError.value = false;
    if (openFile.value && !page.files.some((f) => f.path === openFile.value)) {
      openFile.value = null;
      hunks.value = null;
    }
  } catch (err) {
    // busy → 静默跳过本次；其它错误（非 git 仓库/超时）→ 隐藏状态条。
    if (String(err) !== "busy") {
      files.value = [];
      statError.value = true;
    }
  } finally {
    inflight.value = false;
  }
}

async function toggleFile(path: string): Promise<void> {
  if (openFile.value === path) {
    openFile.value = null;
    hunks.value = null;
    return;
  }
  openFile.value = path;
  hunks.value = null;
  try {
    const view = await consoleDiffFile(props.project, path);
    if (openFile.value === path) hunks.value = view;
  } catch {
    if (openFile.value === path) openFile.value = null;
  }
}

async function stage(paths: string[]): Promise<void> {
  confirmAll.value = false;
  try {
    await consoleStage(props.project, paths);
  } catch (err) {
    console.warn("stage failed", err);
  }
  await refresh();
}

function onToggleOpen(): void {
  open.value = !open.value;
  if (open.value) void refresh();
}

watch(
  () => props.project,
  () => {
    files.value = [];
    open.value = false;
    openFile.value = null;
    hunks.value = null;
    confirmAll.value = false;
    void refresh();
  }
);

watch(
  () => props.editTick,
  () => void refresh()
);

function onWindowFocus(): void {
  void refresh();
}

onMounted(() => {
  window.addEventListener("focus", onWindowFocus);
  void refresh();
});

onBeforeUnmount(() => {
  window.removeEventListener("focus", onWindowFocus);
});
</script>

<template>
  <div v-if="files.length" class="diffbar">
    <div class="diffbar-row">
      <button class="diffbar-head" @click="onToggleOpen">
        <svg class="chevron" :class="{ closed: !open }" viewBox="0 0 12 12">
          <path d="M4 2.5 L8 6 L4 9.5" fill="none" stroke="currentColor" stroke-width="1.6"
            stroke-linecap="round" stroke-linejoin="round" />
        </svg>
        <span class="diffbar-title">{{ t("console.diff.title") }}</span>
        <span class="diffbar-stat">
          {{ t("console.diff.files", { n: totals.files }) }}<template v-if="totals.added">{{ t("console.diff.added", { n: totals.added }) }}</template>
        </span>
        <span class="d-adds">+{{ totals.adds }}</span>
        <span class="d-dels">−{{ totals.dels }}</span>
      </button>
      <template v-if="confirmAll">
        <span class="stage-confirm-text">{{ t("console.diff.stageAllConfirm", { n: totals.files }) }}</span>
        <button class="btn sm" @click="confirmAll = false">{{ t("agents.confirmCancel") }}</button>
        <button class="btn sm primary" @click="stage(files.map((f) => f.path))">
          {{ t("console.diff.confirm") }}
        </button>
      </template>
      <button v-else class="btn sm" @click="confirmAll = true">
        {{ t("console.diff.stageAll") }}
      </button>
    </div>
    <div v-show="open" class="diff-panel">
      <div v-for="f in files" :key="f.path" class="diff-file">
        <div class="diff-file-row">
          <button class="diff-file-main" @click="toggleFile(f.path)">
            <span class="fkind" :class="f.kind">{{ f.kind }}</span>
            <span class="fpath">{{ f.path }}</span>
            <span class="d-adds">+{{ f.adds }}</span>
            <span class="d-dels">−{{ f.dels }}</span>
          </button>
          <button class="btn sm" @click="stage([f.path])">{{ t("console.diff.stage") }}</button>
        </div>
        <div v-if="openFile === f.path" class="hunks">
          <div v-if="!hunks" class="hunk-loading">{{ t("console.tx.loading") }}</div>
          <template v-else>
            <div v-if="hunks.skipped" class="hunk-skip">{{ hunks.skipReason || t("console.diff.skipped") }}</div>
            <template v-for="(ln, li) in hunks.lines" :key="li">
              <div v-if="ln.kind === 'header'" class="hunk-header mono">{{ ln.text }}</div>
              <div v-else class="dline mono" :class="ln.kind">
                <span class="dsign">{{ ln.kind === "add" ? "+" : ln.kind === "del" ? "−" : " " }}</span>{{ ln.text }}
              </div>
            </template>
          </template>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
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
.diff-panel {
  max-height: 300px;
  overflow-y: auto;
  padding: 2px 16px 10px;
}
.diff-file-row {
  display: flex;
  align-items: center;
  gap: 8px;
  /* 长 diff 内滚动时文件头钉在面板顶部：随时可点击收起（用户验收反馈）。 */
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
.fkind.D,
.fkind.B {
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
.hunk-loading,
.hunk-skip {
  padding: 5px 10px;
  font-size: 11.5px;
  color: var(--text-tertiary);
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
</style>
