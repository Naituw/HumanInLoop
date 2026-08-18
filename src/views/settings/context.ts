// 设置页共享上下文：SettingsView 在 setup 里调用 createSettingsContext() 组装各域
// 组合式函数（provide），各 tab 子组件用 useSettingsContext() 注入取用。类型由
// createSettingsContext 的返回值推导，无须手工维护巨型 interface。
import { inject, provide, ref, type InjectionKey, type Ref } from "vue";
import { saveSettings } from "../../lib/ipc";
import type {
  AppConfig,
  SecretAction,
  SecretActions,
  SecretsPresent,
} from "../../lib/types";
import { useGeneralSettings } from "./useGeneralSettings";
import { useAboutUpdates } from "./useAboutUpdates";
import { useIntegration } from "./useIntegration";
import { useAgentTasks } from "./useAgentTasks";
import { useLifecycleSettings } from "./useLifecycleSettings";
import { useChannels } from "./useChannels";
import { useSettingsSearch } from "./useSearch";
import { isMac, isWindows, supportsAgentTasks } from "../../lib/platform";

export { isMac, isWindows, supportsAgentTasks };

export type Tab = "general" | "integration" | "channel" | "advanced" | "experimental";
export const TABS: readonly Tab[] = ["general", "integration", "channel", "advanced", "experimental"];

function parseInitialTab(): Tab {
  // 初始定位 tab 经窗口 URL 传入（如托盘「渠道异常」行 → ?tab=channel），无监听时序问题。
  // 支持 `tab#elementId` 锚点后缀（跨窗口定位，spec gui-agent-task-launch G5）：此处只取 tab 段，
  // 锚点滚动由 SettingsView 挂载完成后处理。
  const tab = (new URLSearchParams(window.location.search).get("tab") ?? "").split("#")[0];
  return TABS.includes(tab as Tab) ? (tab as Tab) : "general";
}

export interface SettingsCore {
  config: Ref<AppConfig | null>;
  activeTab: Ref<Tab>;
  secretsPresent: Ref<SecretsPresent>;
  persist: () => Promise<void>;
}

type ClearedKey = "dingding" | "feishu" | "telegram" | "slackBot" | "slackApp";

function createCore() {
  const config = ref<AppConfig | null>(null);
  const activeTab = ref<Tab>(parseInitialTab());

  // Secrets are never loaded into the UI; we only know whether each is configured (for the
  // placeholder) and track an explicit "cleared" intent until the next save.
  const secretsPresent = ref<SecretsPresent>({
    dingdingSecret: false,
    feishuSecret: false,
    telegramToken: false,
    slackBotToken: false,
    slackAppToken: false,
  });
  const secretCleared = ref({
    dingding: false,
    feishu: false,
    telegram: false,
    slackBot: false,
    slackApp: false,
  });
  const SECRET_PLACEHOLDER = "••••••••";

  // Build a secret's edit intent: a typed value wins (set); else an explicit clear; else unchanged.
  function secretActionFor(value: string, cleared: boolean): SecretAction {
    if (value && value.length > 0) return { kind: "set", value };
    if (cleared) return { kind: "clear" };
    return { kind: "unchanged" };
  }

  async function persist() {
    if (!config.value) return;
    const c = config.value.channels;
    const actions: SecretActions = {
      dingdingSecret: secretActionFor(
        c.dingding.clientSecret,
        secretCleared.value.dingding
      ),
      feishuSecret: secretActionFor(c.feishu.appSecret, secretCleared.value.feishu),
      telegramToken: secretActionFor(
        c.telegram.botToken,
        secretCleared.value.telegram
      ),
      slackBotToken: secretActionFor(
        c.slack.botToken,
        secretCleared.value.slackBot
      ),
      slackAppToken: secretActionFor(
        c.slack.appToken,
        secretCleared.value.slackApp
      ),
    };
    await saveSettings(config.value, actions);
    // Reflect the saved state: a set secret becomes a "Saved" placeholder, a cleared one becomes
    // empty. Wipe the field so the secret is never re-sent on subsequent saves.
    finalizeSecret(actions.dingdingSecret, "dingdingSecret", "dingding");
    finalizeSecret(actions.feishuSecret, "feishuSecret", "feishu");
    finalizeSecret(actions.telegramToken, "telegramToken", "telegram");
    finalizeSecret(actions.slackBotToken, "slackBotToken", "slackBot");
    finalizeSecret(actions.slackAppToken, "slackAppToken", "slackApp");
  }

  function finalizeSecret(
    action: SecretAction,
    presentKey: keyof SecretsPresent,
    clearedKey: ClearedKey
  ) {
    if (!config.value) return;
    if (action.kind === "set") secretsPresent.value[presentKey] = true;
    else if (action.kind === "clear") secretsPresent.value[presentKey] = false;
    if (action.kind !== "unchanged") {
      const c = config.value.channels;
      if (clearedKey === "dingding") c.dingding.clientSecret = "";
      else if (clearedKey === "feishu") c.feishu.appSecret = "";
      else if (clearedKey === "slackBot") c.slack.botToken = "";
      else if (clearedKey === "slackApp") c.slack.appToken = "";
      else c.telegram.botToken = "";
    }
    secretCleared.value[clearedKey] = false;
  }

  // "Clear" button: drop the saved secret (deletes the keychain entry on save) and re-persist so
  // the daemon reloads with the secret gone.
  function clearSecret(channel: ClearedKey) {
    if (!config.value) return;
    const c = config.value.channels;
    if (channel === "dingding") c.dingding.clientSecret = "";
    else if (channel === "feishu") c.feishu.appSecret = "";
    else if (channel === "slackBot") c.slack.botToken = "";
    else if (channel === "slackApp") c.slack.appToken = "";
    else c.telegram.botToken = "";
    secretCleared.value[channel] = true;
    persist();
  }

  return {
    config,
    activeTab,
    secretsPresent,
    SECRET_PLACEHOLDER,
    persist,
    clearSecret,
  };
}

export function createSettingsContext() {
  const core = createCore();
  const general = useGeneralSettings(core);
  const updates = useAboutUpdates();
  const integration = useIntegration(core);
  const tasks = useAgentTasks(core);
  const lifecycle = useLifecycleSettings(tasks.refreshAgentTaskSettings);
  const channels = useChannels(core);
  const search = useSettingsSearch({
    config: core.config,
    activeTab: core.activeTab,
  });

  const ctx = {
    isMac,
    isWindows,
    supportsAgentTasks,
    ...core,
    ...general,
    ...updates,
    ...integration,
    ...tasks,
    ...lifecycle,
    ...channels,
    ...search,
  };
  provide(SettingsCtxKey, ctx);
  return ctx;
}

export type SettingsContext = ReturnType<typeof createSettingsContext>;

const SettingsCtxKey: InjectionKey<SettingsContext> = Symbol("settings-ctx");

export function useSettingsContext(): SettingsContext {
  const ctx = inject(SettingsCtxKey);
  if (!ctx) throw new Error("settings context not provided");
  return ctx;
}
