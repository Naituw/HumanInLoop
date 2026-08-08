<script setup lang="ts">
// Codex permission preferences stay in the Advanced tab; remembered grants are managed in a
// separate macOS-style sheet so a growing session list never expands the settings page itself.
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
const config = computed(() => ctx.config.value!);

const GLOBAL_ID = "__global__";

type ScopeBadge = {
  text: string;
  tone?: "danger" | "warning";
};

type ResetTarget = {
  id: string;
  title: string;
  count: number;
  global: boolean;
};

const opened = ref(false);
const loading = ref(false);
const error = ref("");
const sessions = ref<PermissionSessionGroup[]>([]);
const globalCount = ref(0);
const details = ref<Record<string, PermissionRuleInfo[] | "loading">>({});
const menuId = ref<string | null>(null);
const resetTarget = ref<ResetTarget | null>(null);
const resetBusy = ref(false);
const yoloBusy = ref<string | null>(null);

async function load() {
  loading.value = true;
  error.value = "";
  details.value = {};
  menuId.value = null;
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

function closePanel() {
  menuId.value = null;
  resetTarget.value = null;
  opened.value = false;
}

async function toggleDetail(id: string) {
  menuId.value = null;
  if (details.value[id] && details.value[id] !== "loading") {
    const next = { ...details.value };
    delete next[id];
    details.value = next;
    return;
  }
  if (details.value[id] === "loading") return;
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

function toggleMenu(id: string) {
  menuId.value = menuId.value === id ? null : id;
}

function askReset(id: string, title: string, count: number, global = false) {
  menuId.value = null;
  resetTarget.value = { id, title, count, global };
}

async function confirmReset() {
  const target = resetTarget.value;
  if (!target || resetBusy.value) return;
  resetBusy.value = true;
  error.value = "";
  let succeeded = false;
  try {
    await permissionRulesPanel(
      target.global
        ? { op: "resetGlobal" }
        : { op: "resetSession", sessionId: target.id },
    );
    if (target.global) {
      globalCount.value = 0;
    } else {
      sessions.value = sessions.value.filter(
        (group) => group.summary.sessionId !== target.id,
      );
    }
    const next = { ...details.value };
    delete next[target.id];
    details.value = next;
    succeeded = true;
  } catch (e) {
    error.value = String(e);
  } finally {
    resetBusy.value = false;
    if (succeeded) resetTarget.value = null;
  }
}

async function disableYolo(group: PermissionSessionGroup) {
  const id = group.summary.sessionId;
  menuId.value = null;
  yoloBusy.value = id;
  error.value = "";
  try {
    await permissionRulesPanel({ op: "disableYolo", sessionId: id });
    group.summary.yolo = false;
    group.summary.ruleCount = Math.max(0, group.summary.ruleCount - 1);
    if (group.summary.ruleCount === 0) {
      sessions.value = sessions.value.filter(
        (candidate) => candidate.summary.sessionId !== id,
      );
      const next = { ...details.value };
      delete next[id];
      details.value = next;
      return;
    }
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

function groupTitle(group: PermissionSessionGroup): string {
  if (group.title) return group.title;
  if (group.projectName) return group.projectName;
  return t("settings.permissionRules.untitled");
}

function shortId(id: string): string {
  return id.length > 12 ? `${id.slice(0, 8)}…` : id;
}

function groupBadges(group: PermissionSessionGroup): ScopeBadge[] {
  const summary = group.summary;
  const badges: ScopeBadge[] = [];
  if (summary.yolo) {
    badges.push({
      text: t("settings.permissionRules.badgeYolo"),
      tone: "danger",
    });
  }
  if (summary.fullDisk) {
    badges.push({
      text: t("settings.permissionRules.badgeDisk"),
      tone: "warning",
    });
  }
  if (summary.fileExactCount > 0) {
    badges.push({
      text: t("settings.permissionRules.badgeFiles", {
        n: summary.fileExactCount,
      }),
    });
  }
  if (summary.projectRoots.length > 0) {
    badges.push({
      text: t("settings.permissionRules.badgeProjects", {
        n: summary.projectRoots.length,
      }),
    });
  }
  if (summary.shellCount > 0) {
    badges.push({
      text: t("settings.permissionRules.badgeShell", {
        n: summary.shellCount,
      }),
    });
  }
  if (summary.networkCount > 0) {
    badges.push({
      text: t("settings.permissionRules.badgeNetwork", {
        n: summary.networkCount,
      }),
    });
  }
  if (summary.mcpCount > 0) {
    badges.push({
      text: t("settings.permissionRules.badgeMcp", {
        n: summary.mcpCount,
      }),
    });
  }
  return badges;
}

function formatMs(ms: number): string {
  return ms ? new Date(ms).toLocaleString() : "";
}

function kindLabel(kind: PermissionRuleInfo["kind"]): string {
  const key = `settings.permissionRules.kind${kind.charAt(0).toUpperCase()}${kind.slice(1)}`;
  return t(key);
}

const resetDescription = computed(() => {
  const target = resetTarget.value;
  if (!target) return "";
  return target.global
    ? t("settings.permissionRules.resetGlobalDesc", { n: target.count })
    : t("settings.permissionRules.resetSessionDesc", {
        title: target.title,
        n: target.count,
      });
});
</script>

<template>
  <div class="card permission-rules-card">
    <p class="card-title">{{ t("settings.permissionRules.title") }}</p>
    <p class="card-desc">{{ t("settings.permissionRules.desc") }}</p>
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
      <div class="col">
        <span class="label">{{ t("settings.permissionRules.savedTitle") }}</span>
        <span class="card-desc">{{ t("settings.permissionRules.manageHint") }}</span>
      </div>
      <span class="spacer"></span>
      <button class="btn" type="button" @click="openPanel">
        {{ t("settings.permissionRules.manage") }}
      </button>
    </div>
  </div>

  <Teleport to=".settings">
    <div v-if="opened" class="workspace-panel-backdrop">
      <section
        class="workspace-panel permission-panel"
        role="dialog"
        aria-modal="true"
        :aria-label="t('settings.permissionRules.panelTitle')"
      >
        <header class="workspace-panel-toolbar">
          <button class="workspace-toolbar-done" type="button" @click="closePanel">
            {{ t("settings.agentTasks.done") }}
          </button>
          <h2>{{ t("settings.permissionRules.panelTitle") }}</h2>
          <button
            class="workspace-toolbar-icon"
            type="button"
            :disabled="loading"
            :aria-label="t('settings.permissionRules.refresh')"
            :title="t('settings.permissionRules.refresh')"
            @click="load"
          >
            <svg viewBox="0 0 20 20" aria-hidden="true">
              <path d="M15.9 7.3A6.4 6.4 0 1 0 16 12" />
              <path d="M15.9 3.8v3.8h-3.8" />
            </svg>
          </button>
        </header>

        <div class="workspace-panel-content permission-panel-content">
          <div v-if="loading" class="workspace-panel-empty permission-panel-state">
            <span class="permission-state-spinner" aria-hidden="true"></span>
            <p>{{ t("settings.permissionRules.loading") }}</p>
          </div>
          <div
            v-else-if="sessions.length === 0 && globalCount === 0"
            class="workspace-panel-empty permission-panel-state"
          >
            <span class="workspace-empty-icon permission-empty-icon" aria-hidden="true">
              <svg viewBox="0 0 24 24">
                <path d="M12 3.4 19 6v5.1c0 4.5-2.9 7.8-7 9.5-4.1-1.7-7-5-7-9.5V6l7-2.6Z" />
                <path d="m8.8 12 2.1 2.1 4.5-4.6" />
              </svg>
            </span>
            <p>{{ t("settings.permissionRules.empty") }}</p>
            <span>{{ t("settings.permissionRules.emptyHint") }}</span>
          </div>
          <template v-else>
            <p v-if="error" class="result err permission-panel-error">{{ error }}</p>
            <div class="permission-list">
              <div
                v-if="globalCount > 0"
                class="permission-list-row"
                :class="{ 'is-expanded': Boolean(details[GLOBAL_ID]) }"
              >
                <div class="permission-row-main">
                  <button
                    class="permission-row-disclosure"
                    type="button"
                    :aria-expanded="Boolean(details[GLOBAL_ID])"
                    @click="toggleDetail(GLOBAL_ID)"
                  >
                  <span class="permission-row-icon global" aria-hidden="true">
                    <svg viewBox="0 0 24 24">
                      <circle cx="12" cy="12" r="8.5" />
                      <path d="M3.8 12h16.4M12 3.5c2.3 2.3 3.4 5.1 3.4 8.5S14.3 18.2 12 20.5C9.7 18.2 8.6 15.4 8.6 12S9.7 5.8 12 3.5Z" />
                    </svg>
                  </span>
                  <span class="permission-row-copy">
                    <span class="permission-row-title">
                      <span>{{ t("settings.permissionRules.globalGroup") }}</span>
                      <span class="permission-scope-badge">
                        {{ t("settings.permissionRules.badgeMcp", { n: globalCount }) }}
                      </span>
                    </span>
                    <span class="permission-row-meta">{{ t("settings.permissionRules.globalHint") }}</span>
                  </span>
                  <span
                    class="permission-disclosure-chevron"
                    :class="{ expanded: Boolean(details[GLOBAL_ID]) }"
                    aria-hidden="true"
                  >
                    <svg viewBox="0 0 16 16"><path d="m5.5 3.5 4.5 4.5-4.5 4.5" /></svg>
                  </span>
                  </button>
                  <div class="workspace-row-menu permission-row-menu">
                    <button
                      class="workspace-more-button"
                      type="button"
                      :aria-label="t('settings.permissionRules.actions')"
                      @click.stop="toggleMenu(GLOBAL_ID)"
                    >
                      <svg viewBox="0 0 20 20" aria-hidden="true">
                        <circle cx="4" cy="10" r="1.35" />
                        <circle cx="10" cy="10" r="1.35" />
                        <circle cx="16" cy="10" r="1.35" />
                      </svg>
                    </button>
                    <div v-if="menuId === GLOBAL_ID" class="workspace-menu-pop permission-menu-pop">
                      <button
                        class="menu-item workspace-menu-danger"
                        type="button"
                        @click="askReset(GLOBAL_ID, t('settings.permissionRules.globalGroup'), globalCount, true)"
                      >
                        {{ t("settings.permissionRules.reset") }}
                      </button>
                    </div>
                  </div>
                </div>
                <div v-if="details[GLOBAL_ID]" class="permission-rule-list">
                  <p v-if="details[GLOBAL_ID] === 'loading'" class="permission-rule-loading">
                    {{ t("settings.permissionRules.loading") }}
                  </p>
                  <div
                    v-for="(rule, index) in details[GLOBAL_ID] as PermissionRuleInfo[]"
                    v-else
                    :key="index"
                    class="permission-rule-row"
                  >
                    <span class="permission-rule-kind">{{ kindLabel(rule.kind) }}</span>
                    <span v-if="rule.display" class="permission-rule-value" :title="rule.display">{{ rule.display }}</span>
                    <span class="permission-rule-expiry">{{ t("settings.permissionRules.expires", { time: formatMs(rule.expiresAtMs) }) }}</span>
                  </div>
                </div>
              </div>

              <div
                v-for="group in sessions"
                :key="group.summary.sessionId"
                class="permission-list-row"
                :class="{
                  'has-yolo': group.summary.yolo,
                  'is-expanded': Boolean(details[group.summary.sessionId]),
                }"
              >
                <div class="permission-row-main">
                  <button
                    class="permission-row-disclosure"
                    type="button"
                    :aria-expanded="Boolean(details[group.summary.sessionId])"
                    @click="toggleDetail(group.summary.sessionId)"
                  >
                  <span class="permission-row-icon" :class="{ danger: group.summary.yolo }" aria-hidden="true">
                    <svg viewBox="0 0 24 24">
                      <path d="M12 3.4 19 6v5.1c0 4.5-2.9 7.8-7 9.5-4.1-1.7-7-5-7-9.5V6l7-2.6Z" />
                      <path d="m8.8 12 2.1 2.1 4.5-4.6" />
                    </svg>
                  </span>
                  <span class="permission-row-copy">
                    <span class="permission-row-title">
                      <span :title="group.title || groupTitle(group)">{{ groupTitle(group) }}</span>
                      <span
                        v-for="badge in groupBadges(group)"
                        :key="badge.text"
                        class="permission-scope-badge"
                        :class="badge.tone"
                      >
                        {{ badge.text }}
                      </span>
                    </span>
                    <span class="permission-row-meta">
                      <template v-if="group.projectName">{{ group.projectName }} · </template>
                      {{ t("settings.permissionRules.lastUsed", { time: formatMs(group.summary.lastUsedAtMs) }) }}
                    </span>
                    <span class="permission-session-id" :title="group.summary.sessionId">
                      {{ shortId(group.summary.sessionId) }}
                    </span>
                  </span>
                  <span
                    class="permission-disclosure-chevron"
                    :class="{ expanded: Boolean(details[group.summary.sessionId]) }"
                    aria-hidden="true"
                  >
                    <svg viewBox="0 0 16 16"><path d="m5.5 3.5 4.5 4.5-4.5 4.5" /></svg>
                  </span>
                  </button>
                  <div class="workspace-row-menu permission-row-menu">
                    <button
                      class="workspace-more-button"
                      type="button"
                      :disabled="yoloBusy === group.summary.sessionId"
                      :aria-label="t('settings.permissionRules.actions')"
                      @click.stop="toggleMenu(group.summary.sessionId)"
                    >
                      <svg viewBox="0 0 20 20" aria-hidden="true">
                        <circle cx="4" cy="10" r="1.35" />
                        <circle cx="10" cy="10" r="1.35" />
                        <circle cx="16" cy="10" r="1.35" />
                      </svg>
                    </button>
                    <div
                      v-if="menuId === group.summary.sessionId"
                      class="workspace-menu-pop permission-menu-pop"
                    >
                      <button
                        v-if="group.summary.yolo"
                        class="menu-item"
                        type="button"
                        @click="disableYolo(group)"
                      >
                        {{ t("settings.permissionRules.yoloOff") }}
                      </button>
                      <button
                        class="menu-item workspace-menu-danger"
                        type="button"
                        @click="askReset(group.summary.sessionId, groupTitle(group), group.summary.ruleCount)"
                      >
                        {{ t("settings.permissionRules.reset") }}
                      </button>
                    </div>
                  </div>
                </div>
                <div v-if="details[group.summary.sessionId]" class="permission-rule-list">
                  <p
                    v-if="details[group.summary.sessionId] === 'loading'"
                    class="permission-rule-loading"
                  >
                    {{ t("settings.permissionRules.loading") }}
                  </p>
                  <div
                    v-for="(rule, index) in details[group.summary.sessionId] as PermissionRuleInfo[]"
                    v-else
                    :key="index"
                    class="permission-rule-row"
                  >
                    <span class="permission-rule-kind">{{ kindLabel(rule.kind) }}</span>
                    <span v-if="rule.display" class="permission-rule-value" :title="rule.display">{{ rule.display }}</span>
                    <span class="permission-rule-expiry">{{ t("settings.permissionRules.expires", { time: formatMs(rule.expiresAtMs) }) }}</span>
                  </div>
                </div>
              </div>
            </div>
          </template>
          <p v-if="error && (loading || (sessions.length === 0 && globalCount === 0))" class="result err permission-panel-error">
            {{ error }}
          </p>
        </div>
        <div
          v-if="menuId"
          class="workspace-menu-backdrop"
          @click="menuId = null"
        ></div>
      </section>
    </div>

    <div v-if="resetTarget" class="workspace-panel-backdrop permission-confirm-backdrop">
      <section
        class="confirm-dialog permission-reset-dialog"
        role="alertdialog"
        aria-modal="true"
        :aria-label="t('settings.permissionRules.resetTitle')"
      >
        <h2>{{ t("settings.permissionRules.resetTitle") }}</h2>
        <p class="confirm-intro">{{ resetDescription }}</p>
        <p class="confirm-note">{{ t("settings.permissionRules.resetPermanentHint") }}</p>
        <div class="confirm-actions">
          <button class="btn" type="button" :disabled="resetBusy" @click="resetTarget = null">
            {{ t("settings.agentTasks.confirmCancel") }}
          </button>
          <button class="btn btn-danger" type="button" :disabled="resetBusy" @click="confirmReset">
            {{ t("settings.permissionRules.resetConfirm") }}
          </button>
        </div>
      </section>
    </div>
  </Teleport>
</template>
