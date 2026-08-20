import { describe, expect, it } from "vitest";
import { mountLoads, tabLoads } from "./tabData";

describe("settings tab loads", () => {
  it("starts local integration status on any settings mount, without five-agent readiness", () => {
    expect(mountLoads("general", true)).toEqual([
      { type: "integration-status" },
      { type: "general" },
      { type: "about" },
    ]);
    expect(mountLoads("channel", true)).toEqual([{ type: "integration-status" }]);
    expect(mountLoads("general", true).some((load) => load.type === "agent-tasks")).toBe(
      false,
    );
    expect(mountLoads("general", true).some((load) => load.type === "pi-version")).toBe(
      false,
    );
  });

  it("probes only Pi version when the integration tab is shown", () => {
    expect(tabLoads("integration", true)).toEqual([
      { type: "integration-status" },
      { type: "pi-version" },
    ]);
  });

  it("loads five-agent readiness only on the advanced tab", () => {
    expect(tabLoads("advanced", true)).toEqual([{ type: "agent-tasks", force: false }]);
    expect(tabLoads("advanced", false)).toEqual([]);
    expect(mountLoads("advanced", true)).toEqual([
      { type: "integration-status" },
      { type: "agent-tasks", force: false },
    ]);
  });
});
