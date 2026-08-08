import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it } from "vitest";
import { i18n } from "../../i18n";
import type { AgentRecord } from "../../lib/types";
import Sidebar from "./Sidebar.vue";

const record = (overrides: Partial<AgentRecord>): AgentRecord => ({
  seq: 5,
  kind: "codex",
  sessionId: "parent-session",
  title: "Same native title",
  cwd: "/tmp/project",
  startedAt: 1_700_000_000,
  lastActivity: 1_700_000_010,
  state: "working",
  ...overrides,
});

describe("Agent console Sidebar", () => {
  beforeEach(() => {
    i18n.global.locale.value = "zh";
    localStorage.clear();
  });

  it("distinguishes a forked session with its direct parent sequence", () => {
    const parent = record({});
    const child = record({
      seq: 6,
      sessionId: "child-session",
      forkedFromSessionId: parent.sessionId,
      state: "idle",
    });
    const wrapper = mount(Sidebar, {
      props: {
        groups: [
          {
            key: "/tmp/project",
            label: "project",
            path: "/tmp/project",
            items: [parent, child],
          },
        ],
        recent: [],
        selectedId: child.sessionId,
        newTaskSupported: true,
        todoCounts: {},
        nowMs: 1_700_000_020_000,
      },
      global: { plugins: [i18n] },
    });

    const rows = wrapper.findAll(".sess");
    expect(rows).toHaveLength(2);
    expect(rows[0].find(".fork-mini").exists()).toBe(false);
    expect(rows[1].find(".fork-mini").text()).toBe("从 #5 分叉");
  });
});
