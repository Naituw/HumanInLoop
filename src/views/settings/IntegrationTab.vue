<script setup lang="ts">
// 「Agent 集成」tab：自动集成（每家一张卡，CLI|MCP|未集成三态）+ 手动集成（参考提示词、
// MCP 配置示例）。
import { useI18n } from "vue-i18n";
import { useSettingsContext } from "./context";

const { t } = useI18n();
const {
  config,
  revealLabel,
  prompt,
  promptCopied,
  promptVariant,
  collabBusy,
  collabError,
  changeCollaborationStyle,
  saveCustomCollaborationText,
  AGENTS,
  modes,
  integrationLoading,
  piVersionLoading,
  modeBusy,
  modeMessage,
  modeError,
  setMode,
  togglePermission,
  toggleLifecycle,
  toggleStop,
  toggleAskQuestion,
  permissionBlockedText,
  updateArtifact,
  updateSummary,
  updateAllBusy,
  updateAll,
  openMenuKey,
  toggleOpenMenu,
  closeOpenMenu,
  revealFile,
  openFile,
  setPromptVariant,
  mcpExampleJson,
  mcpExampleToml,
  mcpJsonCopied,
  mcpTomlCopied,
  copyMcpExample,
  copyPrompt,
  settingsTargetHighlight,
} = useSettingsContext();
</script>

<template>
  <div
    v-if="openMenuKey"
    class="menu-backdrop"
    @click="closeOpenMenu"
  ></div>
  <p class="section-intro">{{ t("settings.integration.overviewDesc") }}</p>

  <!-- 协作风格（全局）：对齐 / 自主 / 自定义 -->
  <div class="card collab-style-card">
    <p class="card-title">{{ t("settings.integration.collabTitle") }}</p>
    <div class="row">
      <div class="segmented">
        <button
          type="button"
          class="seg"
          :class="{ active: (config?.general.collaborationStyle ?? 'aligned') === 'aligned' }"
          :disabled="collabBusy"
          @click="changeCollaborationStyle('aligned')"
        >
          {{ t("settings.integration.collabAligned") }}
        </button>
        <button
          type="button"
          class="seg"
          :class="{ active: config?.general.collaborationStyle === 'autonomous' }"
          :disabled="collabBusy"
          @click="changeCollaborationStyle('autonomous')"
        >
          {{ t("settings.integration.collabAutonomous") }}
        </button>
        <button
          type="button"
          class="seg"
          :class="{ active: config?.general.collaborationStyle === 'custom' }"
          :disabled="collabBusy"
          @click="changeCollaborationStyle('custom')"
        >
          {{ t("settings.integration.collabCustom") }}
        </button>
      </div>
    </div>
    <p
      v-if="(config?.general.collaborationStyle ?? 'aligned') === 'aligned'"
      class="card-desc"
    >
      {{ t("settings.integration.collabAlignedDesc") }}
    </p>
    <p
      v-else-if="config?.general.collaborationStyle === 'autonomous'"
      class="card-desc"
    >
      {{ t("settings.integration.collabAutonomousDesc") }}
    </p>
    <template v-else-if="config?.general.collaborationStyle === 'custom' && config">
      <p class="card-desc">{{ t("settings.integration.collabCustomDesc") }}</p>
      <textarea
        v-model="config.general.collaborationStyleCustomText"
        class="input collab-custom-input"
        rows="8"
        spellcheck="false"
      />
      <div class="row" style="margin-top: 8px">
        <span class="spacer"></span>
        <button
          type="button"
          class="btn"
          :disabled="collabBusy"
          @click="saveCustomCollaborationText"
        >
          {{ t("settings.integration.collabSaveCustom") }}
        </button>
      </div>
    </template>
    <p v-if="collabError" class="error-text">{{ collabError }}</p>
  </div>

  <div class="integration-manual">
  <!-- 手动集成：参考提示词（CLI / MCP 双版本 + MCP 配置示例） -->
  <p class="section-title">{{ t("settings.integration.manualTitle") }}</p>
  <p class="section-intro">{{ t("settings.integration.manualLifecycleHint") }}</p>
  <div class="card">
    <div class="row">
      <p class="card-title">{{ t("settings.integration.promptTitle") }}</p>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          type="button"
          class="seg"
          :class="{ active: promptVariant === 'cli' }"
          @click="setPromptVariant('cli')"
        >
          {{ t("settings.integration.modeCli") }}
        </button>
        <button
          type="button"
          class="seg"
          :class="{ active: promptVariant === 'mcp' }"
          @click="setPromptVariant('mcp')"
        >
          {{ t("settings.integration.modeMcp") }}
        </button>
      </div>
      <button class="btn" type="button" @click="copyPrompt">
        {{
          promptCopied
            ? t("settings.integration.copied")
            : t("settings.integration.copy")
        }}
      </button>
    </div>
    <pre class="code-area">{{ prompt }}</pre>

    <template v-if="promptVariant === 'mcp'">
      <hr class="divider" />
      <p class="card-desc agent-hint">
        {{ t("settings.integration.mcpExampleHint") }}
      </p>
      <div class="row mcp-example-head">
        <p class="label mcp-example-label">
          {{ t("settings.integration.mcpExampleJson") }}
        </p>
        <button
          class="btn"
          type="button"
          @click="copyMcpExample('json')"
        >
          {{
            mcpJsonCopied
              ? t("settings.integration.copied")
              : t("settings.integration.copy")
          }}
        </button>
      </div>
      <pre class="code-area">{{ mcpExampleJson }}</pre>
      <p class="card-desc agent-hint">
        {{ t("settings.integration.mcpTimeoutNote") }}
      </p>
      <div class="row mcp-example-head">
        <p class="label mcp-example-label">
          {{ t("settings.integration.mcpExampleToml") }}
        </p>
        <button
          class="btn"
          type="button"
          @click="copyMcpExample('toml')"
        >
          {{
            mcpTomlCopied
              ? t("settings.integration.copied")
              : t("settings.integration.copy")
          }}
        </button>
      </div>
      <pre class="code-area">{{ mcpExampleToml }}</pre>
    </template>
  </div>

  </div>
  <div class="integration-auto">
  <!-- 自动集成：每个 Agent 一张卡，CLI | MCP | 未集成 三态切换 -->
  <p class="section-title">{{ t("settings.integration.autoTitle") }}</p>

  <div v-if="integrationLoading" class="card settings-section-loading">
    <span class="permission-state-spinner" aria-hidden="true"></span>
    <p>{{ t("common.loading") }}</p>
  </div>

  <template v-else>
  <!-- 待更新总览（跨所有 Agent）：有任意产物过期/缺失时出现，附「全部更新」按钮 -->
  <div v-if="updateSummary.total > 0" class="card update-overview">
    <div class="row">
      <span class="dot-update"></span>
      <div class="overview-text">
        <p class="overview-title">
          {{ t("settings.integration.updatesAvailable") }}
        </p>
        <p class="overview-counts">
          <template v-if="updateSummary.rule > 0"
            >{{ t("settings.integration.rulesLabel") }} ×{{
              updateSummary.rule
            }}</template
          ><template v-if="updateSummary.hook > 0"
            ><span v-if="updateSummary.rule > 0" class="sep">·</span
            >{{ t("settings.integration.hookLabel") }} ×{{
              updateSummary.hook
            }}</template
          ><template v-if="updateSummary.mcp > 0"
            ><span
              v-if="updateSummary.rule > 0 || updateSummary.hook > 0"
              class="sep"
              >·</span
            >{{ t("settings.integration.mcpConfigLabel") }} ×{{
              updateSummary.mcp
            }}</template
          >
        </p>
      </div>
      <span class="spacer"></span>
      <button
        class="btn btn-update"
        type="button"
        :disabled="updateAllBusy"
        @click="updateAll"
      >
        {{ t("settings.integration.updateAll") }}
      </button>
    </div>
  </div>

  <div
    v-for="a in AGENTS"
    :id="`integration-${a.id}`"
    :key="a.id"
    class="card agent-card"
    :class="{ 'settings-target-highlight': settingsTargetHighlight === `integration-${a.id}` }"
  >
    <div class="row agent-row">
      <p class="card-title">{{ a.title }}</p>
      <span class="spacer"></span>
      <div class="segmented">
        <button
          v-if="a.hasCli"
          type="button"
          class="seg"
          :class="{ active: modes[a.id].mode === 'cli' }"
          :disabled="modeBusy[a.id]"
          @click="setMode(a.id, 'cli')"
        >
          {{ t("settings.integration.modeCli")
          }}<span v-if="a.recommended === 'cli'" class="seg-rec">{{
            t("settings.integration.recommendedTag")
          }}</span>
        </button>
        <button
          v-if="a.hasMcp"
          type="button"
          class="seg"
          :class="{ active: modes[a.id].mode === 'mcp' }"
          :disabled="modeBusy[a.id]"
          @click="setMode(a.id, 'mcp')"
        >
          {{ t("settings.integration.modeMcp")
          }}<span v-if="a.recommended === 'mcp'" class="seg-rec">{{
            t("settings.integration.recommendedTag")
          }}</span>
        </button>
        <button
          type="button"
          class="seg"
          :class="{ active: modes[a.id].mode === 'none' }"
          :disabled="modeBusy[a.id]"
          @click="setMode(a.id, 'none')"
        >
          {{ t("settings.integration.modeNone") }}
        </button>
      </div>
    </div>

    <div
      v-if="modes[a.id].mode === 'none' && modes[a.id].lifecycle.cleanupRequired"
      :id="`lifecycle-${a.id}`"
      class="result err lifecycle-cleanup row"
      :class="{ 'settings-target-highlight': settingsTargetHighlight === `lifecycle-${a.id}` }"
    >
      <span>{{ t("settings.integration.lifecycleCleanupHint") }}</span>
      <span class="spacer"></span>
      <button
        class="btn btn-update"
        type="button"
        :disabled="modeBusy[a.id]"
        @click="updateArtifact(a.id, 'hook')"
      >
        {{ t("settings.integration.update") }}
      </button>
    </div>

    <template v-if="modes[a.id].mode !== 'none'">
      <hr class="divider" />

      <p
        v-if="a.id === 'pi' && piVersionLoading"
        class="card-desc settings-inline-loading"
      >
        <span class="permission-state-spinner" aria-hidden="true"></span>
        {{ t("common.loading") }}
      </p>
      <p
        v-else-if="a.id === 'pi' && !modes[a.id].versionSupported"
        class="result err"
      >
        {{
          modes[a.id].agentVersion
            ? t("settings.integration.piVersionUnsupported", {
                version: modes[a.id].agentVersion,
                minimum: modes[a.id].minimumVersion,
              })
            : t("settings.integration.piVersionMissing", {
                minimum: modes[a.id].minimumVersion,
              })
        }}
      </p>

      <!-- Rules / Skill（CLI / MCP 共有；Grok 为 skill） -->
      <div class="row agent-row">
        <span class="label">{{
          a.instructionKind === "skill"
            ? t("settings.integration.skillLabel")
            : t("settings.integration.rulesLabel")
        }}</span>
        <span class="badge">
          <span
            class="dot"
            :class="modes[a.id].ruleInstalled ? 'on' : 'off'"
          ></span>
          {{
            modes[a.id].ruleInstalled
              ? t("settings.integration.installed")
              : t("settings.integration.notInstalled")
          }}
        </span>
        <span class="spacer"></span>
        <button
          v-if="modes[a.id].ruleNeedsUpdate"
          class="btn btn-update"
          type="button"
          :disabled="modeBusy[a.id]"
          @click="updateArtifact(a.id, 'rule')"
        >
          <span class="dot-update"></span
          >{{ t("settings.integration.update") }}
        </button>
        <div v-if="modes[a.id].ruleInstalled" class="menu-wrap">
          <button
            class="btn"
            type="button"
            @click.stop="toggleOpenMenu(a.id + ':rule')"
          >
            {{ t("settings.integration.openFile") }}
          </button>
          <div v-if="openMenuKey === a.id + ':rule'" class="menu-pop">
            <button
              class="menu-item"
              type="button"
              @click="revealFile(a.id, 'rule')"
            >
              {{ revealLabel }}
            </button>
            <button
              class="menu-item"
              type="button"
              @click="openFile(a.id, 'rule')"
            >
              {{ t("settings.integration.openFileAction") }}
            </button>
          </div>
        </div>
      </div>
      <p v-if="modes[a.id].rulePath" class="agent-path">
        {{ modes[a.id].rulePath }}
      </p>
      <p v-if="a.id === 'cursor'" class="card-desc agent-hint">
        {{ t("settings.integration.cursorRulesHint") }}
      </p>
      <p v-if="a.id === 'grok'" class="card-desc agent-hint">
        {{ t("settings.integration.grokSkillHint") }}
      </p>

      <!-- CLI 模式：超时 Hook（Codex 无 Hook 给提示） -->
      <template v-if="modes[a.id].mode === 'cli'">
        <hr class="divider" />
        <template v-if="a.hasTimeoutHook">
          <div class="row agent-row">
            <span class="label">{{
              modes[a.id].runtimeArtifactKind === "extension"
                ? t("settings.integration.extensionLabel")
                : t("settings.integration.hookLabel")
            }}</span>
            <span class="badge">
              <span
                class="dot"
                :class="modes[a.id].timeoutHookInstalled ? 'on' : 'off'"
              ></span>
              {{
                modes[a.id].timeoutHookInstalled
                  ? t("settings.integration.installed")
                  : t("settings.integration.notInstalled")
              }}
            </span>
            <span class="spacer"></span>
            <button
              v-if="modes[a.id].hookNeedsUpdate && !modes[a.id].lifecycle.needsUpdate"
              class="btn btn-update"
              type="button"
              :disabled="modeBusy[a.id]"
              @click="updateArtifact(a.id, 'hook')"
            >
              <span class="dot-update"></span
              >{{ t("settings.integration.update") }}
            </button>
            <div v-if="modes[a.id].timeoutHookInstalled" class="menu-wrap">
              <button
                class="btn"
                type="button"
                @click.stop="toggleOpenMenu(a.id + ':hook')"
              >
                {{ t("settings.integration.openFile") }}
              </button>
              <div v-if="openMenuKey === a.id + ':hook'" class="menu-pop">
                <button
                  class="menu-item"
                  type="button"
                  @click="revealFile(a.id, 'hook')"
                >
                  {{ revealLabel }}
                </button>
                <button
                  class="menu-item"
                  type="button"
                  @click="openFile(a.id, 'hook')"
                >
                  {{ t("settings.integration.openFileAction") }}
                </button>
              </div>
            </div>
          </div>
          <p class="card-desc agent-hint">
            {{
              modes[a.id].runtimeArtifactKind === "extension"
                ? t("settings.integration.piExtensionHint")
                : t("settings.integration.hookShort")
            }}
          </p>
          <p
            v-if="!modes[a.id].timeoutHookSupported"
            class="result err"
          >
            {{ t("settings.integration.hookUnsupported") }}
          </p>
        </template>
        <template v-else>
          <div class="row agent-row">
            <span class="label">{{
              t("settings.integration.contextRecoveryHookLabel")
            }}</span>
            <span class="badge">
              <span
                class="dot"
                :class="modes[a.id].recoveryHookInstalled ? 'on' : 'off'"
              ></span>
              {{
                modes[a.id].recoveryHookInstalled
                  ? t("settings.integration.installed")
                  : t("settings.integration.notInstalled")
              }}
            </span>
            <span class="spacer"></span>
            <button
              v-if="modes[a.id].hookNeedsUpdate && !modes[a.id].lifecycle.needsUpdate"
              class="btn btn-update"
              type="button"
              :disabled="modeBusy[a.id]"
              @click="updateArtifact(a.id, 'hook')"
            >
              <span class="dot-update"></span
              >{{ t("settings.integration.update") }}
            </button>
          </div>
          <p class="card-desc agent-hint">
            {{ t("settings.integration.codexRecoveryHookHint") }}
          </p>
        </template>
      </template>

      <!-- MCP 模式：MCP 配置 -->
      <template v-if="modes[a.id].mode === 'mcp' && modes[a.id].mcpSupported">
        <hr class="divider" />
        <div class="row agent-row">
          <span class="label">{{
            t("settings.integration.mcpConfigLabel")
          }}</span>
          <span class="badge">
            <span
              class="dot"
              :class="modes[a.id].mcpConfigInstalled ? 'on' : 'off'"
            ></span>
            {{
              modes[a.id].mcpConfigInstalled
                ? t("settings.integration.installed")
                : t("settings.integration.notInstalled")
            }}
          </span>
          <span class="spacer"></span>
          <button
            v-if="modes[a.id].mcpNeedsUpdate"
            class="btn btn-update"
            type="button"
            :disabled="modeBusy[a.id]"
            @click="updateArtifact(a.id, 'mcp')"
          >
            <span class="dot-update"></span
            >{{ t("settings.integration.update") }}
          </button>
          <div v-if="modes[a.id].mcpConfigInstalled" class="menu-wrap">
            <button
              class="btn"
              type="button"
              @click.stop="toggleOpenMenu(a.id + ':mcp')"
            >
              {{ t("settings.integration.openFile") }}
            </button>
            <div v-if="openMenuKey === a.id + ':mcp'" class="menu-pop">
              <button
                class="menu-item"
                type="button"
                @click="revealFile(a.id, 'mcp')"
              >
                {{ revealLabel }}
              </button>
              <button
                class="menu-item"
                type="button"
                @click="openFile(a.id, 'mcp')"
              >
                {{ t("settings.integration.openFileAction") }}
              </button>
            </div>
          </div>
        </div>
        <p v-if="modes[a.id].mcpConfigPath" class="agent-path">
          {{ modes[a.id].mcpConfigPath }}
        </p>
        <p class="card-desc agent-hint">
          {{ t("settings.integration.mcpModeHint") }}
        </p>
      </template>

    </template>

    <template v-if="modes[a.id].mode !== 'none'">
      <hr class="divider" />
      <template v-if="modes[a.id].stop.supported">
        <div class="row agent-row">
          <span class="label">{{ t("settings.integration.stopTitle") }}</span>
          <span class="badge">
            <span
              class="dot"
              :class="modes[a.id].stop.installed ? 'on' : 'off'"
            ></span>
            {{
              modes[a.id].stop.installed
                ? t("settings.integration.configured")
                : t("settings.integration.notConfigured")
            }}
          </span>
          <span class="spacer"></span>
          <button
            v-if="modes[a.id].stop.outdated"
            class="btn btn-update"
            type="button"
            :disabled="modeBusy[a.id]"
            @click="toggleStop(a.id, true)"
          >
            <span class="dot-update"></span
            >{{ t("settings.integration.update") }}
          </button>
          <label class="switch">
            <input
              type="checkbox"
              :checked="modes[a.id].stop.enabled"
              :disabled="modeBusy[a.id]"
              @change="
                toggleStop(
                  a.id,
                  ($event.target as HTMLInputElement).checked
                )
              "
            />
            <span class="track"></span>
          </label>
        </div>
        <p class="card-desc agent-hint">
          {{ t("settings.integration.stopHint") }}
        </p>
        <p
          v-if="modes[a.id].stop.otherHandlersDetected"
          class="result err"
        >
          {{ t("settings.integration.stopCoexist") }}
        </p>
      </template>
      <p v-else class="card-desc agent-hint">
        {{ t("settings.integration.stopUnsupported") }}
      </p>

      <!-- 接管 Claude 内置的 AskUserQuestion（仅 Claude Code 有这个工具） -->
      <template v-if="modes[a.id].askQuestion.supported">
        <hr class="divider" />
        <div class="row agent-row">
          <span class="label">{{
            t("settings.integration.askQuestionTitle")
          }}</span>
          <span class="badge">
            <span
              class="dot"
              :class="modes[a.id].askQuestion.installed ? 'on' : 'off'"
            ></span>
            {{
              modes[a.id].askQuestion.installed
                ? t("settings.integration.configured")
                : t("settings.integration.notConfigured")
            }}
          </span>
          <span class="spacer"></span>
          <button
            v-if="modes[a.id].askQuestion.outdated"
            class="btn btn-update"
            type="button"
            :disabled="modeBusy[a.id]"
            @click="toggleAskQuestion(a.id, true)"
          >
            <span class="dot-update"></span
            >{{ t("settings.integration.update") }}
          </button>
          <label class="switch">
            <input
              type="checkbox"
              :checked="modes[a.id].askQuestion.enabled"
              :disabled="modeBusy[a.id]"
              @change="
                toggleAskQuestion(
                  a.id,
                  ($event.target as HTMLInputElement).checked
                )
              "
            />
            <span class="track"></span>
          </label>
        </div>
        <p class="card-desc agent-hint">
          {{ t("settings.integration.askQuestionHint") }}
        </p>
      </template>

      <hr class="divider" />
      <template v-if="modes[a.id].permission.supported">
        <div class="row agent-row">
          <span class="label">{{
            t("settings.integration.permissionTitle")
          }}</span>
          <span class="badge">
            <span
              class="dot"
              :class="modes[a.id].permission.configured ? 'on' : 'off'"
            ></span>
            {{
              modes[a.id].permission.configured
                ? t("settings.integration.configured")
                : t("settings.integration.notConfigured")
            }}
          </span>
          <span class="spacer"></span>
          <button
            v-if="modes[a.id].permissionNeedsUpdate"
            class="btn btn-update"
            type="button"
            :disabled="modeBusy[a.id]"
            @click="updateArtifact(a.id, 'hook')"
          >
            <span class="dot-update"></span
            >{{ t("settings.integration.update") }}
          </button>
          <label class="switch">
            <input
              type="checkbox"
              :checked="modes[a.id].permission.enabled"
              :disabled="modeBusy[a.id]"
              @change="
                togglePermission(
                  a.id,
                  ($event.target as HTMLInputElement).checked
                )
              "
            />
            <span class="track"></span>
          </label>
        </div>
        <p class="card-desc agent-hint">
          {{
            a.id === 'claude'
              ? t("settings.integration.permissionClaudeHint")
              : t("settings.integration.permissionCodexHint")
          }}
        </p>
        <p class="card-desc agent-hint">
          {{ t("settings.integration.permissionInflightHint") }}
        </p>
        <p
          v-if="modes[a.id].permission.knownBlockedReason"
          class="result err"
        >
          {{
            permissionBlockedText(
              modes[a.id].permission.knownBlockedReason as string
            )
          }}
        </p>
        <p
          v-if="modes[a.id].permission.otherHandlersDetected"
          class="result err"
        >
          {{
            a.id === 'claude'
              ? t("settings.integration.permissionClaudeCoexist")
              : t("settings.integration.permissionCodexCoexist")
          }}
        </p>
      </template>
      <p v-else class="card-desc agent-hint">
        {{
          a.id === 'pi'
            ? t("settings.integration.piPermissionUnsupported")
            : t("settings.integration.permissionUnsupported")
        }}
      </p>

      <hr class="divider" />
      <div
        :id="`lifecycle-${a.id}`"
        class="row agent-row readiness-target-row"
        :class="{ 'settings-target-highlight': settingsTargetHighlight === `lifecycle-${a.id}` }"
      >
        <span class="label">{{ t("settings.integration.lifecycleTitle") }}</span>
        <span class="badge">
          <span
            class="dot"
            :class="modes[a.id].lifecycle.installed ? 'on' : 'off'"
          ></span>
          {{
            modes[a.id].lifecycle.installed
              ? t("settings.integration.configured")
              : t("settings.integration.notConfigured")
          }}
        </span>
        <span class="spacer"></span>
        <button
          v-if="modes[a.id].lifecycle.needsUpdate"
          class="btn btn-update"
          type="button"
          :disabled="modeBusy[a.id]"
          @click="updateArtifact(a.id, 'hook')"
        >
          <span class="dot-update"></span
          >{{ t("settings.integration.update") }}
        </button>
        <label class="switch">
          <input
            type="checkbox"
            :checked="modes[a.id].lifecycle.enabled"
            :disabled="modeBusy[a.id] || !modes[a.id].lifecycle.supported"
            @change="
              toggleLifecycle(
                a.id,
                ($event.target as HTMLInputElement).checked
              )
            "
          />
          <span class="track"></span>
        </label>
      </div>
      <p class="card-desc agent-hint">
        {{ t("settings.integration.lifecycleHint") }}
      </p>
    </template>

    <p
      v-if="modeMessage[a.id]"
      class="result"
      :class="modeError[a.id] ? 'err' : 'ok'"
    >
      {{ modeMessage[a.id] }}
    </p>
  </div>
  </template>
  </div>
</template>
