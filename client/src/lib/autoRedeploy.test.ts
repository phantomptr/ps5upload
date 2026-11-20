import { describe, expect, it } from "vitest";

import {
  autoRedeployDecision,
  checkShowsLiveHelper,
  LOADER_SETTLE_MS,
  liveHelperDecision,
  loaderSettled,
  MAX_LIVE_HELPER_HOLDS,
  MAX_REDEPLOYS_WITHOUT_RECOVERY,
  redeployAction,
} from "./autoRedeploy";

// The loop behind the 2026-09-14 outage: every delivered ELF replaces whatever runs at :9021, so a
// loop that keeps firing keeps killing each helper as it starts.

describe("autoRedeployDecision", () => {
  const down = { status: "down", busy: false, delivered: 0 } as const;

  it("re-arms when the console is up, and sends when it is down", () => {
    expect(autoRedeployDecision({ ...down, status: "up", delivered: 2 })).toBe("rearm");
    expect(autoRedeployDecision(down)).toBe("redeploy");
  });

  it("stops delivering after the limit without a recovery", () => {
    expect(autoRedeployDecision({ ...down, delivered: MAX_REDEPLOYS_WITHOUT_RECOVERY - 1 })).toBe("redeploy");
    expect(autoRedeployDecision({ ...down, delivered: MAX_REDEPLOYS_WITHOUT_RECOVERY })).toBe("hold");
  });

  it("never sends during a transfer, or to a helper the user stopped", () => {
    expect(autoRedeployDecision({ ...down, busy: true })).toBe("hold");
    expect(autoRedeployDecision({ ...down, stopped: true })).toBe("hold");
  });
});

describe("liveHelperDecision", () => {
  it("sends only when nothing listens, or after the holds run out", () => {
    expect(liveHelperDecision({ portsOpen: false, held: 0 })).toBe("send");
    expect(liveHelperDecision({ portsOpen: true, held: 0 })).toBe("hold");
    expect(liveHelperDecision({ portsOpen: true, held: MAX_LIVE_HELPER_HOLDS })).toBe("send");
  });
});

describe("loaderSettled", () => {
  it("waits out the jailbreak's own start-up after the loader first answers", () => {
    expect(loaderSettled({ loaderOpen: false, firstOpenAt: 1, now: 1e9 })).toBe("closed");
    expect(loaderSettled({ loaderOpen: true, firstOpenAt: undefined, now: 5 })).toBe("settle");
    expect(loaderSettled({ loaderOpen: true, firstOpenAt: 0, now: LOADER_SETTLE_MS - 1 })).toBe("settle");
    expect(loaderSettled({ loaderOpen: true, firstOpenAt: 0, now: LOADER_SETTLE_MS })).toBe("send");
  });
});

describe("redeployAction", () => {
  it("brings up once per boot when asked, else sends the helper when allowed", () => {
    expect(redeployAction({ autoBringUp: true, autoRedeploy: true, upThisBoot: false })).toBe("bring-up");
    expect(redeployAction({ autoBringUp: true, autoRedeploy: true, upThisBoot: true })).toBe("helper");
    expect(redeployAction({ autoBringUp: false, autoRedeploy: true, upThisBoot: false })).toBe("helper");
    expect(redeployAction({ autoBringUp: false, autoRedeploy: false, upThisBoot: false })).toBe("none");
    expect(redeployAction({ autoBringUp: true, autoRedeploy: false, upThisBoot: true })).toBe("none");
  });
});

describe("checkShowsLiveHelper", () => {
  it("a helper that answers, wants pairing, or is only slow is running: nothing is sent over it", () => {
    expect(checkShowsLiveHelper({ reachable: true, session: "connected" })).toBe(true);
    expect(checkShowsLiveHelper({ reachable: false, session: "needs_pairing" })).toBe(true);
    // A weak Wi-Fi link: the port took a connection and the session kept hearing from the
    // console, but node.status did not come back in time.
    expect(checkShowsLiveHelper({ reachable: false, session: "down", reason: "status_slow" })).toBe(true);
  });

  it("a console that stopped answering or refuses the helper's port has no live helper", () => {
    for (const reason of ["ava1_unreachable", "helper_not_ava1", undefined])
      expect(checkShowsLiveHelper({ reachable: false, session: "down", reason })).toBe(false);
  });
});
