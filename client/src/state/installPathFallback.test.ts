import { beforeEach, describe, expect, it } from "vitest";
import { installPathFallbackOptions, useInstallPathFallbackStore } from "./installPathFallback";

describe("install path fallback switch", () => {
  beforeEach(() => useInstallPathFallbackStore.setState({ enabled: false }));

  it("adds nothing to an install while it is off (the default)", () => {
    expect(installPathFallbackOptions()).toEqual({});
  });

  it("asks the engine to allow the last resort once it is on", () => {
    useInstallPathFallbackStore.getState().setEnabled(true);
    expect(installPathFallbackOptions()).toEqual({ console_path_fallback: true });
  });
});
