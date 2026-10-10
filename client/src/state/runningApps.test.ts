import { beforeEach, describe, expect, it } from "vitest";

import { runningOn, useRunningAppsStore } from "./runningApps";

const store = () => useRunningAppsStore.getState();

describe("running apps store", () => {
  beforeEach(() => {
    useRunningAppsStore.setState({ titleIds: new Set(), host: null, updatedAtMs: 0 });
  });

  it("keys every writer by the bare host", () => {
    store().setRunning(["PPSA01341"], "192.168.86.100:9120");
    expect(store().host).toBe("192.168.86.100");
    store().setRunning(["PPSA01341"], "192.168.86.100");
    expect(store().host).toBe("192.168.86.100");
  });

  it("keeps the same set when only the address shape differs", () => {
    store().setRunning(["PPSA01341", "CUSA07842"], "192.168.86.100");
    const first = store().titleIds;
    store().setRunning(["CUSA07842", "PPSA01341"], "192.168.86.100:9120");
    expect(store().titleIds).toBe(first);
  });

  it("reallocates when the members change", () => {
    store().setRunning(["PPSA01341"], "192.168.86.100");
    const first = store().titleIds;
    store().setRunning(["CUSA07842"], "192.168.86.100");
    expect(store().titleIds).not.toBe(first);
    expect([...store().titleIds]).toEqual(["CUSA07842"]);
  });

  it("runningOn only answers for the console the set was read on", () => {
    store().setRunning(["PPSA01341"], "192.168.86.100:9120");
    expect(runningOn(store(), "192.168.86.100").has("PPSA01341")).toBe(true);
    expect(runningOn(store(), "192.168.86.100:9120").has("PPSA01341")).toBe(true);
    expect(runningOn(store(), "192.168.86.99").size).toBe(0);
    expect(runningOn(store(), null).size).toBe(0);
  });

  it("clearForHostChange normalises the host too", () => {
    store().setRunning(["PPSA01341"], "192.168.86.100");
    store().clearForHostChange("192.168.86.99:9120");
    expect(store().host).toBe("192.168.86.99");
    expect(store().titleIds.size).toBe(0);
  });
});
