import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The automatic install restarts the app, which ends everything the engine carries for it.
// A stream registers no task, so "idle" has to look at it directly.

import { useGoLiveStore } from "./goLive";
import { useRemotePlayStore } from "./remotePlay";
import { appIdle } from "./update";

const join = vi.hoisted(() => ({ active: false }));
vi.mock("./joinSession", async (orig) => ({
  ...(await orig<typeof import("./joinSession")>()),
  activeJoin: () => (join.active ? {} : null),
}));

describe("appIdle", () => {
  beforeEach(() => {
    join.active = false;
    useRemotePlayStore.setState({ stream: null });
    useGoLiveStore.setState({ status: null } as never);
  });
  afterEach(() => {
    useRemotePlayStore.setState({ stream: null });
    useGoLiveStore.setState({ status: null } as never);
  });

  it("is idle with nothing going on", async () => {
    await expect(appIdle()).resolves.toBe(true);
  });

  it("is not idle during Remote Play (a host's Play Together included)", async () => {
    useRemotePlayStore.setState({ stream: {} as never });
    await expect(appIdle()).resolves.toBe(false);
  });

  it("is not idle while in a friend's game", async () => {
    join.active = true;
    await expect(appIdle()).resolves.toBe(false);
  });

  it("is not idle during Go live, and is once it stopped", async () => {
    useGoLiveStore.setState({ status: { live: { phase: "live" } } } as never);
    await expect(appIdle()).resolves.toBe(false);
    useGoLiveStore.setState({ status: { live: { phase: "ended" } } } as never);
    await expect(appIdle()).resolves.toBe(true);
  });
});
