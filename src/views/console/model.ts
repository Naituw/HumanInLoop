// Agent 控制台共享模型与纯函数（spec gui-agent-console）。
import { ref } from "vue";
import type { AgentRecord } from "../../lib/types";

export type ConsoleFilter = "all" | "working" | "idle";

/** 边栏分组（R3 通用 key 分组）：项目组 key=cwd 路径；未知项目组用固定 key。 */
export interface ProjectGroup {
  key: string;
  label: string;
  /** 项目路径（「＋」新建任务用；未知项目组为空串）。 */
  path: string;
  items: AgentRecord[];
}

export const UNKNOWN_PROJECT_KEY = "__unknown__";

/** 组内排序权重：工作中 → 等待回答 → 空闲 → 已结束（C6）。 */
export function stateWeight(a: AgentRecord): number {
  if (a.state === "working") return a.waitingRequestId ? 1 : 0;
  return a.state === "idle" ? 2 : 3;
}

/** 排序/相对时间的时间锚点（秒）。 */
export function anchor(a: AgentRecord): number {
  if (a.state === "ended") return a.endedAt ?? a.lastActivity;
  return a.lastActivity;
}

export function basename(p?: string | null): string {
  if (!p) return "";
  const parts = p.replace(/\/+$/, "").split("/");
  return parts[parts.length - 1] || p;
}

/** 会话行的指示器状态（等待覆盖工作中）。 */
export function indState(a: AgentRecord): "working" | "waiting" | "idle" | "ended" {
  if (a.state === "ended") return "ended";
  if (a.waitingRequestId) return "waiting";
  return a.state === "working" ? "working" : "idle";
}

// ── 状态指示器（C13：Cursor ui-ascii-loading-indicator / sine_3x3 移植）──
// 帧表与帧率来自 Cursor 原始实现；所有工作中指示器共享同一帧（同步动画）。

export const MATRIX_FRAMES = [189, 220, 90, 78, 45, 291, 306, 433];
export const MATRIX_FRAME_MS = 175;
/** 空闲静态帧：中行 3 点 + 右上 1 点（「动停了」隐喻，见 spec C13）。 */
export const IDLE_MASK = 0b000111100;
/** 9 个点的圆心坐标（12×12 视图，行优先，bit i = 第 i 格）。 */
export const MATRIX_CELLS = Array.from({ length: 9 }, (_, i) => ({
  x: 2 + (i % 3) * 4,
  y: 2 + Math.floor(i / 3) * 4,
}));

/** 模块级共享帧（多个指示器实例同步；有实例挂载时才走针）。 */
export const matrixFrame = ref(0);
let matrixTimer: number | undefined;
let matrixRefs = 0;

export function acquireMatrixTicker(): void {
  matrixRefs += 1;
  if (matrixTimer === undefined) {
    matrixTimer = window.setInterval(() => {
      matrixFrame.value = (matrixFrame.value + 1) % MATRIX_FRAMES.length;
    }, MATRIX_FRAME_MS);
  }
}

export function releaseMatrixTicker(): void {
  matrixRefs -= 1;
  if (matrixRefs <= 0 && matrixTimer !== undefined) {
    window.clearInterval(matrixTimer);
    matrixTimer = undefined;
    matrixRefs = 0;
  }
}
