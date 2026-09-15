import { beforeEach, describe, expect, it } from "vitest";
import { transferScreenBusy } from "../lib/ps5Transfers";
import {
  autoRedeployDecision,
  MAX_REDEPLOYS_WITHOUT_RECOVERY,
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
