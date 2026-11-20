import { describe, expect, it } from "vitest";
import { DOCKER_UPDATE_COMMAND, shouldShowWebUpdate, webUpdateWaiting } from "./WebUpdateToast";

describe("shouldShowWebUpdate", () => {
  const u = { current: "10.1.0", latest: "10.2.0", available: true, docker: true };

  it("shows a newer release until that version is dismissed", () => {
    expect(shouldShowWebUpdate(u, null)).toBe(true);
    expect(shouldShowWebUpdate(u, "10.2.0")).toBe(false);
    expect(shouldShowWebUpdate(u, "10.1.5")).toBe(true);
  });

  it("shows nothing without an update", () => {
    expect(shouldShowWebUpdate(null, null)).toBe(false);
    expect(shouldShowWebUpdate({ ...u, available: false }, null)).toBe(false);
  });
});

describe("webUpdateWaiting", () => {
  const u = { current: "10.1.0", latest: "10.2.0", available: true, docker: false };

  it("is the version the Settings row and dot show, gone once dismissed", () => {
    expect(webUpdateWaiting({ update: u, dismissed: null })).toBe("10.2.0");
    expect(webUpdateWaiting({ update: u, dismissed: "10.2.0" })).toBeNull();
    expect(webUpdateWaiting({ update: null, dismissed: null })).toBeNull();
  });
});

describe("DOCKER_UPDATE_COMMAND", () => {
  it("removes the replaced image, and only this app's", () => {
    const steps = DOCKER_UPDATE_COMMAND.split(" && ");
    const prune = steps[steps.length - 1];
    expect(prune).toBe(
      "docker image prune -af --filter label=org.opencontainers.image.source=https://github.com/phantomptr/ps5upload",
    );
  });
});
