// 「高级」tab 的 Agent 生命周期追踪开关域（安装/卸载/更新各家 lifecycle hook）。
import { ref } from "vue";
import {
  agentLifecycleStatus,
  agentLifecycleInstall,
  agentLifecycleUninstall,
} from "../../lib/ipc";
import type { AgentKind, LifecycleStatus } from "../../lib/types";
import { supportsAgentTasks } from "../../lib/platform";

export function useLifecycleSettings(
  refreshAgentTaskSettings: (scan?: boolean) => Promise<void>,
) {
  const LIFECYCLE_KINDS: AgentKind[] = ["claude", "codex", "cursor", "grok", "pi"];

  const lifecycleStatus = ref<Record<AgentKind, LifecycleStatus>>({
    claude: { installed: false, outdated: false, supported: true },
    codex: { installed: false, outdated: false, supported: true },
    cursor: { installed: false, outdated: false, supported: true },
    grok: { installed: false, outdated: false, supported: true },
    pi: { installed: false, outdated: false, supported: true },
  });
  const lifecycleBusy = ref<Record<AgentKind, boolean>>({
    claude: false,
    codex: false,
    cursor: false,
    grok: false,
    pi: false,
  });
  const lifecycleError = ref<Record<AgentKind, string | null>>({
    claude: null,
    codex: null,
    cursor: null,
    grok: null,
    pi: null,
  });

  async function refreshLifecycle() {
    for (const kind of LIFECYCLE_KINDS) {
      try {
        lifecycleStatus.value[kind] = await agentLifecycleStatus(kind);
      } catch (e) {
        lifecycleError.value[kind] = String(e);
      }
    }
  }

  // 开关切换：开 = 安装，关 = 卸载。失败时回滚显示并展示错误。
  async function toggleLifecycle(kind: AgentKind, on: boolean) {
    if (lifecycleBusy.value[kind]) return;
    lifecycleBusy.value[kind] = true;
    lifecycleError.value[kind] = null;
    try {
      if (on) await agentLifecycleInstall(kind);
      else await agentLifecycleUninstall(kind);
      lifecycleStatus.value[kind] = await agentLifecycleStatus(kind);
      if (supportsAgentTasks) await refreshAgentTaskSettings(false);
    } catch (e) {
      lifecycleError.value[kind] = String(e);
      // 回滚到后端真实状态，避免开关与实际不一致。
      try {
        lifecycleStatus.value[kind] = await agentLifecycleStatus(kind);
      } catch {
        /* 状态查询也失败时保留现状 */
      }
    } finally {
      lifecycleBusy.value[kind] = false;
    }
  }

  // 过期 lifecycle hook 一键更新：幂等重装（补齐新增事件 / 修正命令路径）后刷新状态。
  async function updateLifecycle(kind: AgentKind) {
    if (lifecycleBusy.value[kind]) return;
    lifecycleBusy.value[kind] = true;
    lifecycleError.value[kind] = null;
    try {
      await agentLifecycleInstall(kind);
      lifecycleStatus.value[kind] = await agentLifecycleStatus(kind);
      if (supportsAgentTasks) await refreshAgentTaskSettings(false);
    } catch (e) {
      lifecycleError.value[kind] = String(e);
    } finally {
      lifecycleBusy.value[kind] = false;
    }
  }

  return {
    LIFECYCLE_KINDS,
    lifecycleStatus,
    lifecycleBusy,
    lifecycleError,
    refreshLifecycle,
    toggleLifecycle,
    updateLifecycle,
  };
}
