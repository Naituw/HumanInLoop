import { describe, expect, it } from "vitest";
import {
  canComposerDock,
  cmdEnterQuestionIndex,
  composerHomeVisibleRatio,
  isComposerHomeFullyVisible,
  projectedDockedComposerHomeHeight,
  resolveActionQuestionIndex,
  resolveComposerDocked,
  shouldApplyScrollSpy,
  shouldDeactivateOffscreenComposer,
  shouldRevealQuestionBeforeCmdEnter,
  type ComposerDockGeometry,
} from "./composerDock";

function geometry(overrides: Partial<ComposerDockGeometry> = {}): ComposerDockGeometry {
  return {
    homeTop: 300,
    homeBottom: 380,
    dockedHomeHeight: 80,
    viewportTop: 100,
    viewportBottom: 500,
    viewportBottomAfterUndock: 500,
    ...overrides,
  };
}

describe("canComposerDock", () => {
  it("requires an upward scroll even after manual activation", () => {
    expect(canComposerDock(true, true, false, false)).toBe(false);
    expect(canComposerDock(true, true, false, true)).toBe(true);
  });

  it("requires an upward scroll after an automatically focused owner was fully seen", () => {
    expect(canComposerDock(true, false, false, false)).toBe(false);
    expect(canComposerDock(true, false, true, false)).toBe(false);
    expect(canComposerDock(true, false, true, true)).toBe(true);
  });

  it("does not start docking after the inline owner has blurred", () => {
    expect(canComposerDock(false, true, true, true)).toBe(false);
  });
});

describe("docked composer Cmd+Enter", () => {
  it("uses the focused editor instead of the scroll-spy question", () => {
    expect(cmdEnterQuestionIndex(0, 2)).toBe(2);
    expect(cmdEnterQuestionIndex(0, null)).toBe(0);
  });

  it("does not reveal an offscreen card when its focused editor is docked", () => {
    expect(shouldRevealQuestionBeforeCmdEnter(2, 2, 2, true)).toBe(false);
    expect(shouldRevealQuestionBeforeCmdEnter(2, null, 2, true)).toBe(true);
    expect(shouldRevealQuestionBeforeCmdEnter(2, 2, null, true)).toBe(true);
  });
});

describe("multi-question action target", () => {
  it("keeps actions on the focused editor while the viewport moves", () => {
    expect(resolveActionQuestionIndex(0, 1)).toBe(1);
  });

  it("hands actions back to the viewport after the editor blurs", () => {
    expect(resolveActionQuestionIndex(0, null)).toBe(0);
  });
});

describe("scroll-spy scheduling", () => {
  it("ignores composer-only measurements such as modifier key activation", () => {
    expect(shouldApplyScrollSpy(false, true, 1_000, 700)).toBe(false);
  });

  it("accepts real scrolling after the navigation lock expires", () => {
    expect(shouldApplyScrollSpy(true, true, 1_000, 700)).toBe(true);
    expect(shouldApplyScrollSpy(true, true, 600, 700)).toBe(false);
  });

  it("never applies the vertical scroll-spy in sequential mode", () => {
    expect(shouldApplyScrollSpy(true, false, 1_000, 0)).toBe(false);
  });
});

describe("offscreen composer deactivation", () => {
  it("deactivates a focused editor after its card fully leaves the viewport", () => {
    expect(shouldDeactivateOffscreenComposer(1, null, true)).toBe(true);
  });

  it("keeps a docked editor active even when its card is offscreen", () => {
    expect(shouldDeactivateOffscreenComposer(1, 1, true)).toBe(false);
  });

  it("keeps partially visible and already blurred editors unchanged", () => {
    expect(shouldDeactivateOffscreenComposer(1, null, false)).toBe(false);
    expect(shouldDeactivateOffscreenComposer(null, null, true)).toBe(false);
  });
});

describe("resolveComposerDocked", () => {
  it("does not dock an owner that has never been visible inline", () => {
    expect(
      resolveComposerDocked(false, false, geometry({ homeTop: 520, homeBottom: 600 }))
    ).toBe(false);
  });

  it("docks when the input home crosses the viewport bottom", () => {
    expect(
      resolveComposerDocked(false, true, geometry({ homeTop: 450, homeBottom: 510 }))
    ).toBe(true);
  });

  it("does not dock when the input leaves through the viewport top", () => {
    expect(
      resolveComposerDocked(false, true, geometry({ homeTop: 60, homeBottom: 140 }))
    ).toBe(false);
    expect(
      resolveComposerDocked(true, true, geometry({ homeTop: 60, homeBottom: 140 }))
    ).toBe(false);
  });

  it("uses a return gap so the boundary cannot oscillate", () => {
    expect(
      resolveComposerDocked(true, true, geometry({ homeTop: 411, homeBottom: 496 }))
    ).toBe(true);
    expect(
      resolveComposerDocked(true, true, geometry({ homeTop: 410, homeBottom: 495 }))
    ).toBe(false);
  });

  it("bases return on the input home rather than a tall attachment area", () => {
    const inputHome = geometry({ homeTop: 350, homeBottom: 470 });
    expect(resolveComposerDocked(true, true, inputHome)).toBe(false);
  });

  it("returns as soon as the viewport released by the dock can contain the home", () => {
    expect(
      resolveComposerDocked(
        true,
        true,
        geometry({
          homeTop: 490,
          homeBottom: 570,
          viewportBottom: 500,
          viewportBottomAfterUndock: 650,
        })
      )
    ).toBe(false);
  });

  it("lets a tall inline editor clip down to its docked height before docking", () => {
    expect(
      resolveComposerDocked(
        false,
        true,
        geometry({
          homeTop: 350,
          homeBottom: 590,
          dockedHomeHeight: 120,
        })
      )
    ).toBe(false);
    expect(
      resolveComposerDocked(
        false,
        true,
        geometry({
          homeTop: 381,
          homeBottom: 621,
          dockedHomeHeight: 120,
        })
      )
    ).toBe(true);
  });

  it("returns a tall editor when its dock-sized portion fits without expanding abruptly", () => {
    expect(
      resolveComposerDocked(
        true,
        true,
        geometry({
          homeTop: 380,
          homeBottom: 620,
          dockedHomeHeight: 120,
        })
      )
    ).toBe(true);
    expect(
      resolveComposerDocked(
        true,
        true,
        geometry({
          homeTop: 370,
          homeBottom: 610,
          dockedHomeHeight: 120,
        })
      )
    ).toBe(false);
  });
});

describe("projectedDockedComposerHomeHeight", () => {
  it("keeps a short textarea unchanged", () => {
    expect(projectedDockedComposerHomeHeight(80, 80)).toBe(80);
  });

  it("caps only the textarea portion of a tall composer home", () => {
    expect(projectedDockedComposerHomeHeight(274, 240)).toBe(154);
  });
});

describe("composerHomeVisibleRatio", () => {
  it("measures partial visibility within the content viewport", () => {
    expect(composerHomeVisibleRatio(geometry())).toBe(1);
    expect(
      composerHomeVisibleRatio(geometry({ homeTop: 460, homeBottom: 540 }))
    ).toBe(0.5);
    expect(
      composerHomeVisibleRatio(geometry({ homeTop: 500, homeBottom: 580 }))
    ).toBe(0);
  });
});

describe("isComposerHomeFullyVisible", () => {
  it("rejects a home clipped at either edge", () => {
    expect(isComposerHomeFullyVisible(geometry())).toBe(true);
    expect(
      isComposerHomeFullyVisible(geometry({ homeTop: 80, homeBottom: 180 }))
    ).toBe(false);
    expect(
      isComposerHomeFullyVisible(geometry({ homeTop: 460, homeBottom: 540 }))
    ).toBe(false);
  });
});
