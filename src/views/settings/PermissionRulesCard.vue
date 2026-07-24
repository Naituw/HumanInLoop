<script setup lang="ts">
// 「高级」tab 最后一张卡：Codex 会话授权管理（spec codex-permission-remember §6.3）。
// 渐进加载：卡片本身全静态，点「管理」才连 daemon 取摘要，展开分组才取该组规则详情。
// 顶部附宽松模式全局开关（D52）。
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import { permissionRulesPanel } from "../../lib/ipc";
import { useSettingsContext } from "./context";
import type {
  PermissionRuleInfo,
  PermissionSessionGroup,
} from "../../lib/types";

const { t } = useI18n();
const ctx = useSettingsContext();
const { persist } = ctx;
// 父组件仅在 config 加载后渲染本卡，这里可安全断言非空。
const config = computed(() => ctx.config.value!);

/** 跨会话授权分组在本组件内部使用的伪 id（不会与 Codex session id 冲突）。 */
const GLOBAL_ID = "__global__";

const opened = ref(false);
const loading = ref(false);
const error = ref("");
const sessions = ref<PermissionSessionGroup[]>([]);
const globalCount = ref(0);
const details = ref<Record<string, PermissionRuleInfo[] | "loading">>({});
const armedReset = ref<string | null>(null);
const resetBusy = ref<string | null>(null);
const yoloBusy = ref<string | null>(null);
let disarmTimer: number | undefined;

async function load() {
  loading.value = true;
  error.value = "";
  details.value = {};
  armedReset.value = null;
  try {
    const result = await permissionRulesPanel({ op: "summaries" });
    if (result.kind === "summaries") {
      sessions.value = result.sessions;
      globalCount.value = result.globalCount;
    }
  } catch (e) {
    error.value = String(e);
  } finally {
    loading.value = false;
  }
}

function openPanel() {
  opened.value = true;
  void load();
}

async function toggleDetail(id: string) {
  if (details.value[id] && details.value[id] !== "loading") {
    const next = { ...details.value };
    delete next[id];
    details.value = next;
    return;
  }
  details.value = { ...details.value, [id]: "loading" };
  try {
    const result = await permissionRulesPanel(
      id === GLOBAL_ID
        ? { op: "globalDetail" }
        : { op: "sessionDetail", sessionId: id },
    );
    if (result.kind === "rules") {
      details.value = { ...details.value, [id]: result.rules };
    }
  } catch (e) {
    error.value = String(e);
    const next = { ...details.value };
    delete next[id];
    details.value = next;
  }
}

/** 两段式重置：第一次点击进入「确认重置？」，3 秒内再点才执行。 */
function requestReset(id: string) {
  if (armedReset.value !== id) {
    armedReset.value = id;
    window.clearTimeout(disarmTimer);
    disarmTimer = window.setTimeout(() => {
      armedReset.value = null;
    }, 3000);
    return;
  }
  window.clearTimeout(disarmTimer);
  armedReset.value = null;
  void doReset(id);
}

async function doReset(id: string) {
  resetBusy.value = id;
  try {
    await permissionRulesPanel(
      id === GLOBAL_ID
        ? { op: "resetGlobal" }
        : { op: "resetSession", sessionId: id },
    );
    // Daemon 已原子落盘：本地移除该分组即可，无需整表刷新。
    if (id === GLOBAL_ID) {
      globalCount.value = 0;
    } else {
      sessions.value = sessions.value.filter(
        (g) => g.summary.sessionId !== id,
      );
    }
    const next = { ...details.value };
    delete next[id];
    details.value = next;
  } catch (e) {
    error.value = String(e);
  } finally {
    resetBusy.value = null;
  }
}

function groupTitle(group: PermissionSessionGroup): string {
  if (group.title) return group.title;
  if (group.projectName) return group.projectName;
  return t("settings.permissionRules.sessionFallback", {
    id: shortId(group.summary.sessionId),
  });
}

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

/** 只关 YOLO（D53）：不动该会话其它授权；成功后本地改摘要即可。 */
async function disableYolo(id: string) {
  yoloBusy.value = id;
  try {
    await permissionRulesPanel({ op: "disableYolo", sessionId: id });
    const group = sessions.value.find((g) => g.summary.sessionId === id);
    if (group) {
      group.summary.yolo = false;
      group.summary.ruleCount = Math.max(0, group.summary.ruleCount - 1);
    }
    // 详情已展开时刷新该组规则列表。
    if (details.value[id] && details.value[id] !== "loading") {
      const result = await permissionRulesPanel({
        op: "sessionDetail",
        sessionId: id,
      });
      if (result.kind === "rules") {
        details.value = { ...details.value, [id]: result.rules };
      }
    }
  } catch (e) {
    error.value = String(e);
  } finally {
    yoloBusy.value = null;
  }
}

function scopeText(group: PermissionSessionGroup): string {
  const s = group.summary;
  const parts: string[] = [];
  if (s.yolo) parts.push(t("settings.permissionRules.scopeYolo"));
  if (s.fileExactCount > 0)
    parts.push(t("settings.permissionRules.scopeFiles", { n: s.fileExactCount }));
  for (const root of s.projectRoots)
    parts.push(t("settings.permissionRules.scopeProject", { root: shortPath(root) }));
  if (s.fullDisk) parts.push(t("settings.permissionRules.scopeDisk"));
  if (s.shellCount > 0)
    parts.push(t("settings.permissionRules.scopeShell", { n: s.shellCount }));
  if (s.networkCount > 0)
    parts.push(t("settings.permissionRules.scopeNetwork", { n: s.networkCount }));
  if (s.mcpCount > 0)
    parts.push(t("settings.permissionRules.scopeMcp", { n: s.mcpCount }));
  const meta = [
    t("settings.permissionRules.lastUsed", { time: formatMs(s.lastUsedAtMs) }),
  ];
  return [parts.join(" · "), meta.join(" · ")].filter(Boolean).join(" — ");
}

function shortPath(path: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : path;
}

function formatMs(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : "";
}

function kindLabel(kind: PermissionRuleInfo["kind"]): string {
  const key = `settings.permissionRules.kind${kind.charAt(0).toUpperCase()}${kind.slice(1)}`;
  return t(key);
}
</script>

<template>
  <div class="card">
    <p class="card-title">{{ t("settings.permissionRules.title") }}</p>
    <p class="card-desc">{{ t("settings.permissionRules.desc") }}</p>
    <!-- 宽松模式全局开关（D52）：会话级开关在权限弹窗里。 -->
    <div class="row">
      <span class="label">{{ t("settings.permissionRules.relaxedTitle") }}</span>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.permissions.codexRelaxedShell"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p class="card-desc">{{ t("settings.permissionRules.relaxedDesc") }}</p>
    <hr class="divider" />
    <div class="row">
      <span class="spacer"></span>
      <button
        class="btn"
        type="button"
        :disabled="loading"
        @click="opened ? load() : openPanel()"
      >
        {{
          opened
            ? t("settings.permissionRules.refresh")
            : t("settings.permissionRules.manage")
        }}
      </button>
    </div>

    <template v-if="opened">
      <hr class="divider" />
      <p v-if="loading" class="card-desc">
        {{ t("settings.permissionRules.loading") }}
      </p>
      <p v-else-if="error" class="card-desc err">{{ error }}</p>
      <p
        v-else-if="sessions.length === 0 && globalCount === 0"
        class="card-desc"
      >
        {{ t("settings.permissionRules.empty") }}
      </p>
      <template v-else>
        <!-- 跨会话授权（D41）：单独分组，不属于任何对话。 -->
        <template v-if="globalCount > 0">
          <div class="row">
            <div class="col">
              <span class="label">{{
                t("settings.permissionRules.globalGroup")
              }}</span>
              <p class="card-desc">
                {{ t("settings.permissionRules.scopeMcp", { n: globalCount }) }}
              </p>
            </div>
            <span class="spacer"></span>
            <button class="btn" type="button" @click="toggleDetail(GLOBAL_ID)">
              {{
                details[GLOBAL_ID] && details[GLOBAL_ID] !== "loading"
                  ? t("settings.permissionRules.collapse")
                  : t("settings.permissionRules.detail")
              }}
            </button>
            <button
              class="btn"
              type="button"
              :disabled="resetBusy === GLOBAL_ID"
              @click="requestReset(GLOBAL_ID)"
            >
              {{
                armedReset === GLOBAL_ID
                  ? t("settings.permissionRules.confirmReset")
                  : t("settings.permissionRules.reset")
              }}
            </button>
          </div>
          <div
            v-if="details[GLOBAL_ID] && details[GLOBAL_ID] !== 'loading'"
            class="sub-setting"
          >
            <p
              v-for="(rule, i) in details[GLOBAL_ID] as PermissionRuleInfo[]"
              :key="i"
              class="card-desc"
            >
              {{ kindLabel(rule.kind) }}
              <template v-if="rule.display"> · {{ rule.display }}</template>
              · {{ t("settings.permissionRules.expires", { time: formatMs(rule.expiresAtMs) }) }}
            </p>
          </div>
          <hr class="divider" />
        </template>

        <!-- 按 Codex 对话分组，最近使用在前。 -->
        <template
          v-for="(group, index) in sessions"
          :key="group.summary.sessionId"
        >
          <hr v-if="index > 0" class="divider" />
          <div class="row">
            <div class="col">
              <span class="label">{{ groupTitle(group) }}</span>
              <p class="card-desc">{{ scopeText(group) }}</p>
            </div>
            <span class="spacer"></span>
            <button
              v-if="group.summary.yolo"
              class="btn"
              type="button"
              :disabled="yoloBusy === group.summary.sessionId"
              @click="disableYolo(group.summary.sessionId)"
            >
              {{ t("settings.permissionRules.yoloOff") }}
            </button>
            <button
              class="btn"
              type="button"
              @click="toggleDetail(group.summary.sessionId)"
            >
              {{
                details[group.summary.sessionId] &&
                details[group.summary.sessionId] !== "loading"
                  ? t("settings.permissionRules.collapse")
                  : t("settings.permissionRules.detail")
              }}
            </button>
            <button
              class="btn"
              type="button"
              :disabled="resetBusy === group.summary.sessionId"
              @click="requestReset(group.summary.sessionId)"
            >
              {{
                armedReset === group.summary.sessionId
                  ? t("settings.permissionRules.confirmReset")
                  : t("settings.permissionRules.reset")
              }}
            </button>
          </div>
          <div
            v-if="
              details[group.summary.sessionId] &&
              details[group.summary.sessionId] !== 'loading'
            "
            class="sub-setting"
          >
            <p
              v-for="(rule, i) in details[group.summary.sessionId] as PermissionRuleInfo[]"
              :key="i"
              class="card-desc"
            >
              {{ kindLabel(rule.kind) }}
              <template v-if="rule.display"> · {{ rule.display }}</template>
              · {{ t("settings.permissionRules.expires", { time: formatMs(rule.expiresAtMs) }) }}
            </p>
          </div>
        </template>
      </template>
    </template>
  </div>
</template>
