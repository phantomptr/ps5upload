import { beforeEach, describe, expect, it } from "vitest";

import { runningAfterClose, runningOn, useRunningAppsStore } from "./runningApps";

const store = () => useRunningAppsStore.getState();

describe("running apps store", () => {
  beforeEach(() => {
    useRunningAppsStore.setState({ titleIds: new Set(), host: null, updatedAtMs: 0 });
  });

  it("keys every writer by the bare host", () => {
    store().setRunning(["PPSA01341"], "192.0.2.100:9120");
    expect(store().host).toBe("192.0.2.100");
    store().setRunning(["PPSA01341"], "192.0.2.100");
    expect(store().host).toBe("192.0.2.100");
  });

  it("keeps the same set when only the address shape differs", () => {
    store().setRunning(["PPSA01341", "CUSA07842"], "192.0.2.100");
    const first = store().titleIds;
    store().setRunning(["CUSA07842", "PPSA01341"], "192.0.2.100:9120");
    expect(store().titleIds).toBe(first);
  });

  it("reallocates when the members change", () => {
    store().setRunning(["PPSA01341"], "192.0.2.100");
    const first = store().titleIds;
    store().setRunning(["CUSA07842"], "192.0.2.100");
    expect(store().titleIds).not.toBe(first);
    expect([...store().titleIds]).toEqual(["CUSA07842"]);
  });

  it("runningOn only answers for the console the set was read on", () => {
    store().setRunning(["PPSA01341"], "192.0.2.100:9120");
    expect(runningOn(store(), "192.0.2.100").has("PPSA01341")).toBe(true);
    expect(runningOn(store(), "192.0.2.100:9120").has("PPSA01341")).toBe(true);
    expect(runningOn(store(), "192.0.2.99").size).toBe(0);
    expect(runningOn(store(), null).size).toBe(0);
  });

  it("clearForHostChange normalises the host too", () => {
    store().setRunning(["PPSA01341"], "192.0.2.100");
    store().clearForHostChange("192.0.2.99:9120");
    expect(store().host).toBe("192.0.2.99");
    expect(store().titleIds.size).toBe(0);
  });
});

describe("runningAfterClose", () => {
  it("drops the closed title from that console's set", () => {
    const s = { titleIds: new Set(["A", "B"]), host: "10.0.0.2" };
    expect(runningAfterClose(s, "10.0.0.2:9200", "A")).toEqual(["B"]);
  });

  it("never republishes another console's running titles under this one", () => {
    // The store holds console B's set; the game was closed on console A.
    const s = { titleIds: new Set(["B1", "B2"]), host: "10.0.0.3" };
    expect(runningAfterClose(s, "10.0.0.2", "A1")).toEqual([]);
  });
});
