// Generated and managed by AskHuman. Changes inside this file will be replaced.
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";

const CONFIG = /* ASKHUMAN_CONFIG_BEGIN */ __ASKHUMAN_CONFIG_JSON__ /* ASKHUMAN_CONFIG_END */;
const ASK_TIMEOUT_MS = 24 * 60 * 60 * 1000;
const SHORT_TIMEOUT_MS = 30 * 1000;
const RECOVERY_PROMPT =
  "[ASKHUMAN CONTEXT RECOVERY] Continue following the AskHuman mandatory interaction protocol from AGENTS.md. Ask questions only through the AskHuman CLI. If the exact prior AskHuman question or answer is uncertain after compaction, run AskHuman --show-last before continuing.";

type JsonObject = Record<string, unknown>;
type RunningCommand = {
  child: ChildProcessWithoutNullStreams;
  result: Promise<JsonObject | undefined>;
};

let generation = 0;
let recoveryPending = false;
let lastRun:
  | { generation: number; stopReason?: string; message?: string }
  | undefined;
let pendingStop:
  | { generation: number; child: ChildProcessWithoutNullStreams }
  | undefined;

function sessionPayload(ctx: ExtensionContext, extra: JsonObject = {}): JsonObject {
  const manager = ctx.sessionManager;
  const sessionId = process.env.PI_SESSION_ID || manager.getSessionId();
  const sessionFile = process.env.PI_SESSION_FILE || manager.getSessionFile();
  return {
    session_id: sessionId,
    transcript_path: sessionFile ?? null,
    cwd: ctx.cwd,
    ...extra,
  };
}

function commandEnv(ctx: ExtensionContext): NodeJS.ProcessEnv {
  const payload = sessionPayload(ctx);
  return {
    ...process.env,
    PI_CODING_AGENT: "true",
    PI_SESSION_ID: String(payload.session_id ?? ""),
    PI_SESSION_FILE: String(payload.transcript_path ?? ""),
  };
}

function spawnJson(
  args: string[],
  payload: JsonObject,
  ctx: ExtensionContext,
  timeoutMs: number,
): RunningCommand {
  const child = spawn(CONFIG.executable, args, {
    cwd: ctx.cwd,
    env: commandEnv(ctx),
    stdio: ["pipe", "pipe", "pipe"],
    windowsHide: true,
  });
  const result = new Promise<JsonObject | undefined>((resolve) => {
    let stdout = "";
    let settled = false;
    const finish = (value?: JsonObject) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve(value);
    };
    const timer = setTimeout(() => {
      child.kill();
      finish();
    }, timeoutMs);
    child.stdout.setEncoding("utf8");
    child.stdout.on("data", (chunk: string) => {
      if (stdout.length < 1024 * 1024) stdout += chunk;
    });
    child.stderr.resume();
    child.on("error", () => finish());
    child.on("close", () => {
      const trimmed = stdout.trim();
      if (!trimmed) return finish();
      try {
        const parsed = JSON.parse(trimmed);
        finish(parsed && typeof parsed === "object" ? parsed : undefined);
      } catch {
        finish();
      }
    });
  });
  child.stdin.on("error", () => {});
  child.stdin.end(JSON.stringify(payload));
  return { child, result };
}

function report(ctx: ExtensionContext, event: string, extra: JsonObject = {}): void {
  const command = spawnJson(
    ["__agent-hook", "pi", event],
    sessionPayload(ctx, extra),
    ctx,
    SHORT_TIMEOUT_MS,
  );
  void command.result;
}

function shellTokens(segment: string): string[] {
  const tokens: string[] = [];
  const pattern = /"([^"]*)"|'([^']*)'|([^\s]+)/g;
  for (const match of segment.matchAll(pattern)) {
    tokens.push(match[1] ?? match[2] ?? match[3] ?? "");
  }
  return tokens;
}

function basename(value: string): string {
  const unquoted = value.replace(/^['"]|['"]$/g, "");
  const name = unquoted.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  return name.endsWith(".exe") ? name.slice(0, -4) : name;
}

function isAskHumanCommand(command: string): boolean {
  const allowed = new Set([
    "askhuman",
    "humaninloop",
    basename(CONFIG.programName),
    basename(CONFIG.executable),
  ]);
  for (const segment of command.split(/&&|\|\||[;|\n]/)) {
    const tokens = shellTokens(segment.trim());
    let index = 0;
    while (index < tokens.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[index])) index += 1;
    if (["command", "exec"].includes(tokens[index]?.toLowerCase())) index += 1;
    if (tokens[index]?.toLowerCase() === "env") {
      index += 1;
      while (index < tokens.length && /^[A-Za-z_][A-Za-z0-9_]*=/.test(tokens[index])) index += 1;
    }
    if (allowed.has(basename(tokens[index] ?? ""))) return true;
  }
  return false;
}

function boundedInput(input: unknown): unknown {
  try {
    const encoded = JSON.stringify(input);
    return encoded.length <= 64 * 1024 ? input : {};
  } catch {
    return {};
  }
}

function assistantSummary(messages: unknown[]): { stopReason?: string; message?: string } {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index] as JsonObject;
    if (message?.role !== "assistant") continue;
    const content = Array.isArray(message.content) ? message.content : [];
    const text = content
      .filter((part): part is { type: "text"; text: string } =>
        Boolean(part && typeof part === "object" && (part as JsonObject).type === "text"),
      )
      .map((part) => part.text)
      .join("\n")
      .trim();
    return {
      stopReason: typeof message.stopReason === "string" ? message.stopReason : undefined,
      message: text || undefined,
    };
  }
  return {};
}

function cancelPendingStop(): void {
  const pending = pendingStop;
  pendingStop = undefined;
  if (pending && !pending.child.killed) pending.child.kill();
}

export default function askHumanExtension(pi: ExtensionAPI) {
  pi.on("session_start", async (_event, ctx) => {
    generation += 1;
    recoveryPending = false;
    lastRun = undefined;
    cancelPendingStop();
    if (CONFIG.lifecycle) report(ctx, "session-start");
  });

  pi.on("before_agent_start", async (event) => {
    if (!CONFIG.cli || !recoveryPending) return;
    recoveryPending = false;
    return { systemPrompt: `${event.systemPrompt}\n\n${RECOVERY_PROMPT}` };
  });

  pi.on("session_compact", async () => {
    if (CONFIG.cli) recoveryPending = true;
  });

  pi.on("agent_start", async (_event, ctx) => {
    generation += 1;
    lastRun = undefined;
    cancelPendingStop();
    if (CONFIG.lifecycle) report(ctx, "turn-start");
  });

  pi.on("tool_call", async (event, ctx) => {
    const input = event.input as JsonObject;
    if (
      CONFIG.cli &&
      event.toolName === "bash" &&
      typeof input.command === "string" &&
      isAskHumanCommand(input.command)
    ) {
      input.timeout = 86400;
    }
    if (!CONFIG.lifecycle) return;
    const command = spawnJson(
      ["__agent-hook", "pi", "activity"],
      sessionPayload(ctx, {
        hook_event_name: "PreToolUse",
        tool_name: event.toolName,
        tool_input: boundedInput(event.input),
      }),
      ctx,
      ASK_TIMEOUT_MS,
    );
    const decision = await command.result;
    if (decision?.block === true && typeof decision.reason === "string") {
      return { block: true, reason: decision.reason };
    }
  });

  pi.on("tool_result", async (event, ctx) => {
    if (!CONFIG.lifecycle) return;
    report(ctx, "activity", {
      hook_event_name: "PostToolUse",
      tool_name: event.toolName,
      tool_response: true,
    });
  });

  pi.on("agent_end", async (event) => {
    lastRun = { generation, ...assistantSummary(event.messages as unknown[]) };
  });

  pi.on("agent_settled", async (_event, ctx) => {
    const run = lastRun;
    if (!CONFIG.stop || !run || run.generation !== generation || run.stopReason !== "stop") {
      if (CONFIG.lifecycle) report(ctx, "turn-end");
      return;
    }

    const token = generation;
    cancelPendingStop();
    const command = spawnJson(
      ["__stop-hook", "pi", "confirm"],
      sessionPayload(ctx, {
        hook_event_name: "Stop",
        stop_reason: run.stopReason,
        last_assistant_message: run.message ?? "",
      }),
      ctx,
      ASK_TIMEOUT_MS,
    );
    pendingStop = { generation: token, child: command.child };
    void command.result.then(async (decision) => {
      if (
        !pendingStop ||
        pendingStop.child !== command.child ||
        pendingStop.generation !== token ||
        generation !== token
      ) {
        return;
      }
      pendingStop = undefined;
      const prompt = decision?.followup_message;
      if (
        typeof prompt === "string" &&
        prompt.trim() &&
        ctx.isIdle() &&
        !ctx.hasPendingMessages()
      ) {
        try {
          await pi.sendUserMessage(prompt);
          return;
        } catch {
          // Fail open and report the settled turn below.
        }
      }
      if (CONFIG.lifecycle) report(ctx, "turn-end");
    });
  });

  pi.on("session_shutdown", async (_event, ctx) => {
    generation += 1;
    lastRun = undefined;
    cancelPendingStop();
    if (CONFIG.lifecycle) report(ctx, "session-end");
  });
}
