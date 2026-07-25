<script setup lang="ts">
// 统一状态指示器（spec gui-agent-console C13）：
// 工作中＝3×3 点阵动画（Cursor sine_3x3 同参）/ 等待＝🙋 / 空闲＝静态 4 点 / 已结束＝无。
import { computed, onBeforeUnmount, onMounted } from "vue";
import {
  acquireMatrixTicker,
  IDLE_MASK,
  MATRIX_CELLS,
  MATRIX_FRAMES,
  matrixFrame,
  releaseMatrixTicker,
} from "./model";

const props = defineProps<{
  state: "working" | "waiting" | "idle" | "ended" | string;
}>();

const mask = computed<number>(() => {
  if (props.state === "working") return MATRIX_FRAMES[matrixFrame.value];
  if (props.state === "idle") return IDLE_MASK;
  return 0;
});

function on(cell: number): boolean {
  return (mask.value & (1 << cell)) !== 0;
}

onMounted(acquireMatrixTicker);
onBeforeUnmount(releaseMatrixTicker);
</script>

<template>
  <span class="ind" :class="state">
    <template v-if="state === 'waiting'">🙋</template>
    <svg v-else-if="state === 'working' || state === 'idle'" class="matrix" viewBox="0 0 12 12">
      <circle
        v-for="(c, i) in MATRIX_CELLS"
        :key="i"
        :cx="c.x"
        :cy="c.y"
        r="1.3"
        :class="{ on: on(i) }"
      />
    </svg>
  </span>
</template>

<style scoped>
.ind {
  flex: 0 0 auto;
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 17px;
  line-height: 1;
}
.ind.working {
  color: var(--ind-working);
}
.ind.waiting {
  font-size: 13.5px;
  animation: hand-bounce 1.3s ease-in-out infinite;
}
.ind.idle {
  color: var(--text-tertiary);
}
.matrix {
  width: 12px;
  height: 12px;
}
.matrix circle {
  fill: currentColor;
  opacity: 0;
  transition: opacity 0.07s linear;
}
.matrix circle.on {
  opacity: 1;
}
@keyframes hand-bounce {
  0%,
  100% {
    transform: translateY(0);
  }
  50% {
    transform: translateY(-1.5px);
  }
}
</style>
