<script setup lang="ts">
// Advanced: daemon lifecycle, on-demand IM delivery, Agent task launch, and permission grants.
// Only the task-launch card is limited to platforms with a supported terminal.
import { computed } from "vue";
import { useI18n } from "vue-i18n";
import { useSettingsContext } from "./context";
import PermissionRulesCard from "./PermissionRulesCard.vue";

const { t } = useI18n();
const ctx = useSettingsContext();
const {
  isWindows,
  supportsAgentTasks,
  persist,
  changeDaemonLifecycle,
  toggleAgentTasks,
  testAgentTaskTerminal,
  taskSettingsBusy,
  taskSettingsMessage,
  refreshAgentTaskSettings,
  taskReadiness,
  taskWorkspaces,
  openReadinessIssue,
  openWorkspacePanel,
} = ctx;
// 父组件仅在 config 加载后渲染本 tab，这里可安全断言非空。
const config = computed(() => ctx.config.value!);
</script>

<template>
  <!-- 守护进程生命周期（默认按活动启动/空闲退出；保活=常驻+开机自启） -->
  <div class="card">
    <p class="card-title">
      {{ t("settings.experimental.daemonLifecycleTitle") }}
    </p>
    <div class="row">
      <span class="label">{{
        t("settings.experimental.daemonLifecycleLabel")
      }}</span>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          :class="{ active: config.general.daemonLifecycle === 'activity' }"
          :disabled="config.agentTasks.enabled"
          @click="changeDaemonLifecycle('activity')"
        >
          {{ t("settings.experimental.daemonLifecycleActivity") }}
        </button>
        <button
          :class="{ active: config.general.daemonLifecycle === 'keepalive' }"
          :disabled="config.agentTasks.enabled"
          @click="changeDaemonLifecycle('keepalive')"
        >
          {{ t("settings.experimental.daemonLifecycleKeepalive") }}
        </button>
      </div>
    </div>
    <!-- 「从 IM 创建 Agent 任务」依赖保活：功能开启期间锁定本控件，并就地说明原因，
         避免用户改了不生效以为是 bug（save_settings 会静默强制回 keepalive）。 -->
    <p v-if="config.agentTasks.enabled" class="card-desc warn">
      {{ t("settings.experimental.daemonLifecycleLockedByTasks") }}
    </p>
    <p class="card-desc">
      {{ t("settings.experimental.daemonLifecycleHint") }}
    </p>
  </div>

  <!-- IM 渠道按需发送（归入「高级」Tab；配置键仍为 autoActivation） -->
  <div class="card">
    <div class="row">
      <p class="card-title">
        {{ t("settings.channels.autoActivationTitle") }}
      </p>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.channels.autoActivation"
          @change="persist"
        />
        <span class="track"></span>
      </label>
    </div>
    <p class="card-desc">
      {{ t("settings.channels.autoActivationDesc") }}
    </p>
    <p class="card-desc hint">
      {{ t("settings.channels.autoActivationLifecycleHint") }}
    </p>
    <!-- 子开关：自动结束 watch（缩进以示为「按需发送」子项；仅父开时可用，父关置灰禁用） -->
    <div
      class="sub-setting"
      :style="{ opacity: config.channels.autoActivation ? 1 : 0.5 }"
    >
      <div class="row">
        <span class="label">
          {{ t("settings.channels.autoEndWatchTitle") }}
        </span>
        <span class="spacer"></span>
        <label class="switch">
          <input
            type="checkbox"
            v-model="config.channels.autoEndWatch"
            :disabled="!config.channels.autoActivation"
            @change="persist"
          />
          <span class="track"></span>
        </label>
      </div>
      <p class="card-desc">
        {{ t("settings.channels.autoEndWatchDesc") }}
      </p>
    </div>
  </div>

  <!-- IM Agent task launch is available with Terminal.app or Windows Terminal. -->
  <div v-if="supportsAgentTasks" class="card">
    <div class="row">
      <div class="col">
        <p class="card-title">{{ t("settings.agentTasks.title") }}</p>
        <p class="card-desc">{{ t("settings.agentTasks.description") }}</p>
      </div>
      <span class="spacer"></span>
      <label class="switch">
        <input
          type="checkbox"
          v-model="config.agentTasks.enabled"
          @change="toggleAgentTasks"
        />
        <span class="track"></span>
      </label>
    </div>
    <template v-if="config.agentTasks.enabled">
      <hr class="divider" />
      <div class="row">
        <span class="label">{{ t("settings.agentTasks.permission") }}</span>
        <span class="spacer"></span>
        <select class="select" v-model="config.agentTasks.permissionPrompt" @change="persist">
          <option value="ask">{{ t("settings.agentTasks.permissionAsk") }}</option>
          <option value="agent-default">{{ t("settings.agentTasks.permissionDefault") }}</option>
          <option value="yolo">{{ t("settings.agentTasks.permissionYolo") }}</option>
        </select>
      </div>
      <p v-if="config.agentTasks.permissionPrompt === 'yolo'" class="result err">
        {{ t("settings.agentTasks.yoloWarning") }}
      </p>
      <hr class="divider" />
      <div class="row">
        <span class="label">{{ isWindows ? "Windows Terminal" : "Terminal.app" }}</span>
        <span class="spacer"></span>
        <button class="btn" type="button" @click="testAgentTaskTerminal">
          {{ t("settings.agentTasks.testTerminal") }}
        </button>
        <button class="btn" type="button" :disabled="taskSettingsBusy" @click="refreshAgentTaskSettings(true)">
          {{ t("settings.agentTasks.refresh") }}
        </button>
      </div>
      <hr class="divider" />
      <p class="label">{{ t("settings.agentTasks.readiness") }}</p>
      <div
        v-if="taskSettingsBusy && taskReadiness.length === 0"
        class="settings-section-loading"
      >
        <span class="permission-state-spinner" aria-hidden="true"></span>
        <p>{{ t("common.loading") }}</p>
      </div>
      <div v-for="item in taskReadiness" :key="item.kind" class="row agent-row">
        <span class="label">{{ item.label }}</span>
        <span class="badge"><span class="dot" :class="item.ready ? 'on' : 'off'"></span>{{ item.ready ? t("settings.agentTasks.ready") : t("settings.agentTasks.notReady") }}</span>
        <span class="spacer"></span>
        <span class="card-desc readiness-conditions">
          <span v-if="item.binaryReady">CLI ✓</span>
          <button
            v-else
            type="button"
            :title="t('settings.agentTasks.openInstallDocs')"
            @click="openReadinessIssue(item.kind, 'binary')"
          >CLI ×</button>
          <span class="readiness-separator">·</span>
          <span v-if="item.lifecycleReady">Lifecycle ✓</span>
          <button
            v-else
            type="button"
            :title="t('settings.agentTasks.goToSetting')"
            @click="openReadinessIssue(item.kind, 'lifecycle')"
          >Lifecycle ×</button>
          <span class="readiness-separator">·</span>
          <span v-if="item.integrationReady">Integration ✓</span>
          <button
            v-else
            type="button"
            :title="t('settings.agentTasks.goToSetting')"
            @click="openReadinessIssue(item.kind, 'integration')"
          >Integration ×</button>
        </span>
      </div>
      <hr class="divider" />
      <div class="row">
        <div class="col">
          <span class="label">{{ t("settings.agentTasks.workspaces") }}</span>
          <span
            v-if="taskSettingsBusy && taskWorkspaces.length === 0"
            class="card-desc"
          >{{ t("common.loading") }}</span>
          <span v-else class="card-desc">{{ t("settings.agentTasks.workspaceCount", { n: taskWorkspaces.length }) }}</span>
        </div>
        <span class="spacer"></span>
        <button class="btn" type="button" @click="openWorkspacePanel">
          {{ t("settings.agentTasks.manageWorkspaces") }}
        </button>
      </div>
      <p v-if="taskSettingsMessage" class="result">{{ taskSettingsMessage }}</p>
    </template>
  </div>

  <!-- Codex 权限授权管理（spec codex-permission-remember §6.3） -->
  <PermissionRulesCard />
</template>
