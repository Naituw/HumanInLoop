<script setup lang="ts">
import { useI18n } from "vue-i18n";

defineProps<{
  permissionPrompt: string;
  modelValue: string | null;
}>();
const emit = defineEmits<{ "update:modelValue": [value: string] }>();
const { t } = useI18n();
</script>

<template>
  <section class="nt-section">
    <span class="nt-label">{{ t("newTask.permissionLabel") }}</span>
    <div v-if="permissionPrompt === 'ask'" class="nt-choices">
      <button
        type="button"
        class="nt-choice"
        :class="{ active: modelValue === 'agent-default' }"
        @click="emit('update:modelValue', 'agent-default')"
      >
        <span class="nt-radio" :class="{ on: modelValue === 'agent-default' }" />
        <span class="nt-choice-text">
          {{ t("newTask.permissionAgentDefault") }}
          <span class="nt-choice-sub">{{ t("newTask.permissionAgentDefaultDesc") }}</span>
        </span>
      </button>
      <button
        type="button"
        class="nt-choice danger"
        :class="{ active: modelValue === 'yolo' }"
        @click="emit('update:modelValue', 'yolo')"
      >
        <span class="nt-radio" :class="{ on: modelValue === 'yolo' }" />
        <span class="nt-choice-text">
          {{ t("newTask.permissionYolo") }}
          <span class="nt-badge-danger">{{ t("newTask.permissionYoloBadge") }}</span>
          <span class="nt-choice-sub">{{ t("newTask.permissionYoloDesc") }}</span>
        </span>
      </button>
    </div>
    <p v-else class="nt-permission-fixed">
      {{
        permissionPrompt === "yolo"
          ? t("newTask.permissionYolo")
          : t("newTask.permissionAgentDefault")
      }}
      <span v-if="permissionPrompt === 'yolo'" class="nt-badge-danger">
        {{ t("newTask.permissionYoloBadge") }}
      </span>
    </p>
  </section>
</template>

<style>
.nt-section { display: flex; flex-direction: column; gap: 6px; }
.nt-label { font-size: 12px; font-weight: 600; color: var(--text-secondary); }
.nt-choices { display: flex; flex-direction: column; gap: 4px; max-height: 180px; overflow-y: auto; }
.nt-choice { display: flex; align-items: flex-start; gap: 8px; text-align: left; padding: 7px 10px; border: var(--hairline) solid var(--border); border-radius: 8px; background: var(--bg-elevated); color: var(--text-primary); font: inherit; font-size: 12.5px; line-height: 1.4; cursor: pointer; }
.nt-choice:hover { background: var(--control-hover-bg); }
.nt-choice.active { border-color: color-mix(in srgb, #0a84ff 55%, var(--border)); background: color-mix(in srgb, #0a84ff 8%, var(--bg-elevated)); }
.nt-choice.danger.active { border-color: color-mix(in srgb, #ff453a 55%, var(--border)); background: color-mix(in srgb, #ff453a 8%, var(--bg-elevated)); }
.nt-radio { flex: 0 0 auto; width: 12px; height: 12px; margin-top: 2px; border-radius: 50%; border: 1.4px solid var(--text-secondary); box-sizing: border-box; }
.nt-radio.on { border-color: #0a84ff; background: radial-gradient(circle, #0a84ff 0 3.5px, transparent 4px); }
.nt-choice.danger .nt-radio.on { border-color: #ff453a; background: radial-gradient(circle, #ff453a 0 3.5px, transparent 4px); }
.nt-choice-text { min-width: 0; white-space: pre-wrap; word-break: break-word; }
.nt-choice-sub { display: block; margin-top: 1px; color: var(--text-secondary); font-size: 11px; }
.nt-badge-danger { display: inline-block; margin-left: 6px; padding: 0 5px; border-radius: 5px; background: color-mix(in srgb, #ff453a 16%, transparent); color: #ff453a; font-size: 10px; font-weight: 700; line-height: 1.6; vertical-align: 1px; }
.nt-permission-fixed { margin: 0; font-size: 12.5px; }
</style>
