import { beforeEach, describe, expect, it } from "vitest";
import { transferScreenBusy } from "../lib/ps5Transfers";
import {
  autoRedeployDecision,
  liveHelperDecision,
  LOADER_SETTLE_MS,
  loaderSettled,
  MAX_LIVE_HELPER_HOLDS,
  MAX_REDEPLOYS_WITHOUT_RECOVERY,
  redeployAction,
  redeployBrakeResets,
  REDEPLOY_GAVE_UP_RETRY_MS,
} from "../lib/autoRedeploy";
import { useTransferStore } from "../state/transfer";
import { useUploadQueueStore } from "../state/uploadQueue";

/**
 * Regression guard for the auto-redeploy loop.
 *
 * A user's bug bundle showed a 20 GB upload that could never finish: the
 * transfer saturated the console at ~150 MB/s, the mgmt-port health poll
 * started timing out, the helper was declared "down", auto-redeploy pushed a
 * fresh payload, the new instance took over and the old one shut down — which
 * killed the upload mid-flight. It then restarted and did it again. 33
 * redeploys, 23 helper shutdowns, 132 GB re-sent for a 20 GB archive.
 *
 * `AppShell`'s redeploy tick fires when `payloadStatus === "down" &&
 * !transferScreenBusy(host)`. These tests pin the predicate that gates it,
 * per-console, so a live upload can never be redeployed over again.
 */
describe("auto-redeploy must not fire over a live transfer", () => {
  const HOST = "192.168.88.2";
  const OTHER = "192.168.88.9";

  beforeEach(() => {
    useTransferStore.setState({ phasesByHost: {} });
    useUploadQueueStore.setState({ items: [], running: false });
  });

  it("reports busy while a one-shot upload to that console is running", () => {
    expect(transferScreenBusy(HOST)).toBe(false);
    useTransferStore.setState({
      phasesByHost: {
        [HOST]: {
          kind: "running",
          jobId: "j1",
          startedAtMs: 1,
          bytesSent: 1,
          totalBytes: 2,
          bytesPerSec: 1,
          files: [],
          fileCount: 0,
          filesCompleted: 0,
          skippedFiles: 0,
          skippedBytes: 0,
          filesFinalized: 0,
          filesFinalizingTotal: 0,
          bytesFinalized: 0,
        },
      },
    });
    expect(transferScreenBusy(HOST)).toBe(true);
  });

  it("counts the 'starting' phase too — the window before the job id lands", () => {
    // The upload is already committed at this point; a redeploy here is just
    // as destructive as one mid-stream.
    useTransferStore.setState({ phasesByHost: { [HOST]: { kind: "starting" } } });
    expect(transferScreenBusy(HOST)).toBe(true);
  });

  it("is scoped per console — a busy console A must not block console B", () => {
    useTransferStore.setState({ phasesByHost: { [HOST]: { kind: "starting" } } });
    expect(transferScreenBusy(HOST)).toBe(true);
    // B is idle: a genuinely dead helper on B must still be redeployable.
    expect(transferScreenBusy(OTHER)).toBe(false);
  });

  it("is not busy once the transfer reaches a terminal phase", () => {
    useTransferStore.setState({
      phasesByHost: { [HOST]: { kind: "failed", error: "boom" } },
    });
    expect(transferScreenBusy(HOST)).toBe(false);
  });
});

/**
 * Regression guard for the 2026-09-14 console kill.
 *
 * An engine outage during a dev restart made every probe fail. The poller
 * read each failure as a console miss, flipped both consoles to "down", and
 * — because the engine is the only thing that can probe them — nothing could
 * ever flip them back. The redeploy loop then pushed a fresh ELF at :9021
 * every ~32 s for six minutes: 10 helpers into each console, each one killing
 * the helper the previous one had just started. Both consoles stopped
 * answering and had to be rebooted.
 *
 * Two fixes, both pinned here: a console whose state we could not learn is
 * never "down" (the poller now leaves it untouched), and a console that takes
 * helpers without recovering gets three then silence.
 */
describe("auto-redeploy must not run away on a healthy console", () => {
  const delivered = (n: number) => ({ status: "down" as const, busy: false, delivered: n });

  it("never fires on a console whose state we could not learn", () => {
    // The engine didn't answer, so the poller has no verdict — an engine
    // outage must never be spent as an ELF at the console.
    expect(
      autoRedeployDecision({ status: undefined, busy: false, delivered: 0 }),
    ).toBe("hold");
    expect(
      autoRedeployDecision({ status: "unknown", busy: false, delivered: 0 }),
    ).toBe("hold");
  });

  it("fires exactly once for a console that really is down", () => {
    expect(autoRedeployDecision(delivered(0))).toBe("redeploy");
    expect(autoRedeployDecision(delivered(1))).toBe("redeploy");
  });

  it("stops after MAX_REDEPLOYS_WITHOUT_RECOVERY delivered helpers", () => {
    expect(autoRedeployDecision(delivered(MAX_REDEPLOYS_WITHOUT_RECOVERY - 1))).toBe(
      "redeploy",
    );
    expect(autoRedeployDecision(delivered(MAX_REDEPLOYS_WITHOUT_RECOVERY))).toBe(
      "hold",
    );
    // Well past the brake (the 09-14 loop reached 10) it must still hold.
    expect(autoRedeployDecision(delivered(10))).toBe("hold");
  });

  it("past the brake, sends one more only a quarter hour after the last, never sooner", () => {
    const past = (ago: number | null) => ({ ...delivered(MAX_REDEPLOYS_WITHOUT_RECOVERY), lastDeliveredAgoMs: ago });
    // Before 12.6.15 it held for good, until someone pressed Send.
    expect(autoRedeployDecision(past(REDEPLOY_GAVE_UP_RETRY_MS - 1))).toBe("hold");
    expect(autoRedeployDecision(past(null))).toBe("hold");
    expect(autoRedeployDecision(past(REDEPLOY_GAVE_UP_RETRY_MS))).toBe("redeploy");
    expect(autoRedeployDecision({ ...past(REDEPLOY_GAVE_UP_RETRY_MS), delivered: 10 })).toBe("redeploy");
    // The retry keeps every other guard.
    expect(autoRedeployDecision({ ...past(REDEPLOY_GAVE_UP_RETRY_MS), busy: true })).toBe("hold");
    expect(autoRedeployDecision({ ...past(REDEPLOY_GAVE_UP_RETRY_MS), stopped: true })).toBe("hold");
    expect(autoRedeployDecision({ ...past(REDEPLOY_GAVE_UP_RETRY_MS), power: "rest" })).toBe("hold");
    expect(autoRedeployDecision({ ...past(REDEPLOY_GAVE_UP_RETRY_MS), status: "unknown" })).toBe("hold");
  });

  it("starts the brake over after rest mode or the console being off, not on one dropped probe", () => {
    expect(redeployBrakeResets({ power: "rest", unreachableTicks: 0 })).toBe(true);
    expect(redeployBrakeResets({ power: "unreachable", unreachableTicks: 2 })).toBe(true);
    expect(redeployBrakeResets({ power: "unreachable", unreachableTicks: 1 })).toBe(false);
    expect(redeployBrakeResets({ power: "awake", unreachableTicks: 0 })).toBe(false);
  });

  it("re-arms on any up verdict, so the next standby still recovers", () => {
    expect(
      autoRedeployDecision({ status: "up", busy: false, delivered: 10 }),
    ).toBe("rearm");
  });

  it("still refuses to redeploy over a live transfer, even when down", () => {
    expect(autoRedeployDecision({ status: "down", busy: true, delivered: 0 })).toBe(
      "hold",
    );
  });
});

/**
 * A FW 12.70 report: STATUS failed while :9113 still accepted connections,
 * and each helper that came up reported "prior_instance": "replaced". A
 * redeploy over a helper that still owns our ports replaces a live process
 * and drops the connection it was meant to restore.
 */
describe("a helper whose ports still accept connections", () => {
  it("is left alone", () => {
    expect(liveHelperDecision({ portsOpen: true, held: 0 })).toBe("hold");
    expect(
      liveHelperDecision({ portsOpen: true, held: MAX_LIVE_HELPER_HOLDS - 1 }),
    ).toBe("hold");
  });

  it("is replaced once it has stayed unresponsive long enough to be wedged", () => {
    expect(
      liveHelperDecision({ portsOpen: true, held: MAX_LIVE_HELPER_HOLDS }),
    ).toBe("send");
  });

  it("does not delay a send when nothing of ours is listening", () => {
    expect(liveHelperDecision({ portsOpen: false, held: 0 })).toBe("send");
  });
});

/** An FW 11.40 console crashed three times out of three when the app was running while it
 *  booted: the loader answers before kstuff is loaded, and the loop pushed the helper into
 *  that window. The unattended push now waits for the loader to have been up a while. */
describe("loaderSettled", () => {
  const now = 1_000_000;
  it("does not send while the loader port is closed", () => {
    expect(loaderSettled({ loaderOpen: false, firstOpenAt: undefined, now })).toBe("closed");
  });
  it("holds the first sighting of an open loader", () => {
    expect(loaderSettled({ loaderOpen: true, firstOpenAt: undefined, now })).toBe("settle");
  });
  it("holds until the loader has been open for the settle period", () => {
    expect(
      loaderSettled({ loaderOpen: true, firstOpenAt: now - LOADER_SETTLE_MS + 1, now }),
    ).toBe("settle");
    expect(loaderSettled({ loaderOpen: true, firstOpenAt: now - LOADER_SETTLE_MS, now })).toBe(
      "send",
    );
  });
});

/** #479: "Bring up automatically" runs the whole Quick bring-up once per console boot; after
 *  that a lost helper is only re-sent (and only when the reconnect setting is on). */
describe("redeployAction", () => {
  it("brings a console up once per boot when the setting is on", () => {
    expect(redeployAction({ autoBringUp: true, autoRedeploy: true, upThisBoot: false })).toBe(
      "bring-up",
    );
    expect(redeployAction({ autoBringUp: true, autoRedeploy: false, upThisBoot: false })).toBe(
      "bring-up",
    );
  });
  it("only re-sends the helper once it has been up this boot", () => {
    expect(redeployAction({ autoBringUp: true, autoRedeploy: true, upThisBoot: true })).toBe(
      "helper",
    );
    expect(redeployAction({ autoBringUp: true, autoRedeploy: false, upThisBoot: true })).toBe(
      "none",
    );
  });
  it("keeps the old helper-only behaviour with the setting off (the default)", () => {
    expect(redeployAction({ autoBringUp: false, autoRedeploy: true, upThisBoot: false })).toBe(
      "helper",
    );
    expect(redeployAction({ autoBringUp: false, autoRedeploy: false, upThisBoot: false })).toBe(
      "none",
    );
  });
});
