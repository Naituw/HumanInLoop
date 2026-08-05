import { mount } from "@vue/test-utils";
import { nextTick, ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import type { ConfirmChoice, ConfirmRequest } from "../../lib/types";
import ConfirmPane from "./ConfirmPane.vue";
import { PopupCtxKey, type PopupContext } from "./context";

function choice(level: number, segmentLabel: string, recommended = false): ConfirmChoice {
  const prefix = ["cargo", "build", "--release"].slice(0, level + 1).join(" ");
  return {
    id: `prefix-${level}`,
    label: `Allow ${prefix}`,
    description: "This conversation",
    role: "default",
    variant: {
      group: "shell-prefix-session",
      level,
      levelLabel: prefix,
      segmentLabel,
      recommended,
    },
  };
}

function request(choices: ConfirmChoice[]): ConfirmRequest {
  return {
    id: "confirm-1",
    title: "Run command?",
    context: [],
    detail: { summary: "Needed for verification", bodyMd: "`cargo test`" },
    choices,
    presentation: { type: "singleSelectSubmit", submitLabel: "Submit" },
    dismissActionId: "deny",
    createdAtMs: 1,
    expiresAtMs: 2,
  };
}

describe("ConfirmPane prefix scope", () => {
  beforeEach(() => {
    i18n.global.locale.value = "en";
  });

  it("renders cumulative token segments with preview, commit, and reset states", async () => {
    const choices = [choice(0, "cargo"), choice(1, "build", true), choice(2, "--release")];
    const selectedLevel = ref(1);
    const selectLevel = vi.fn((level: number) => {
      selectedLevel.value = level;
    });
    const context = {
      confirmRequest: ref(request(choices)),
      confirmChoiceIndex: ref(1),
      confirmComment: ref(""),
      confirmInput: ref(null),
      showConfirmInput: ref(false),
      confirmDetailHtml: ref("<code>cargo test</code>"),
      confirmToolName: ref("Shell"),
      confirmRows: ref([{ index: 1, choice: choices[1], group: "shell-prefix-session" }]),
      confirmVariantLevel: selectedLevel,
      confirmVariantLevels: ref([
        { level: 0, label: "cargo", prefixLabel: "cargo", recommended: false },
        { level: 1, label: "build", prefixLabel: "cargo build", recommended: true },
        {
          level: 2,
          label: "--release",
          prefixLabel: "cargo build --release",
          recommended: false,
        },
      ]),
      confirmVariantRecommendedLevel: ref(1),
      selectConfirmVariantLevel: selectLevel,
      permissionEdit: ref(null),
      permissionDiff: ref(null),
      permissionDiffLoading: ref(false),
      selectConfirmChoice: vi.fn(),
      onContentClick: vi.fn(),
    } as unknown as PopupContext;
    const wrapper = mount(ConfirmPane, {
      global: {
        plugins: [i18n],
        provide: { [PopupCtxKey as symbol]: context },
      },
    });

    const segments = wrapper.findAll(".confirm-variant-segment");
    expect(segments.map((segment) => segment.text())).toEqual(["cargo", "build", "--release"]);
    expect(segments[0].classes()).toContain("in-prefix");
    expect(segments[1].classes()).toEqual(
      expect.arrayContaining(["in-prefix", "boundary", "committed"]),
    );
    expect(segments[2].classes()).not.toContain("in-prefix");
    expect(wrapper.find(".confirm-variant-reset").exists()).toBe(false);

    await segments[2].trigger("mouseenter");
    expect(wrapper.findAll(".confirm-variant-segment").every((segment) =>
      segment.classes().includes("in-prefix"),
    )).toBe(true);
    expect(segments[1].classes()).toContain("committed");
    expect(selectLevel).not.toHaveBeenCalled();

    await segments[2].trigger("click");
    await nextTick();
    expect(selectLevel).toHaveBeenLastCalledWith(2);
    const reset = wrapper.find(".confirm-variant-reset");
    expect(reset.text()).toBe("Reset");
    await reset.trigger("click");
    expect(selectLevel).toHaveBeenLastCalledWith(1);
  });
});
