import { beforeEach, describe, expect, it, vi } from "vitest";

// Every RPC fails the way a dying helper's do: this is the report that
// motivated the black box (FW 12.70, helper up for 7.6 s, report generated in
// the same second — "read frame header: Connection reset by peer" on all of it).
vi.mock("./invokeLogged", () => ({
  invoke: vi.fn(() =>
    Promise.reject(new Error("read frame header: Connection reset by peer")),
  ),
}));

import { useConnectionStore } from "../state/connection";
import {
  blackBoxFor,
  clearBlackBoxes,
  recordBlackBox,
  resetBlackBoxCooldowns,
  shouldCaptureBlackBox,
  BLACK_BOX_COOLDOWN_MS,
} from "./payloadBlackBox";
import { buildPs5Snapshot, capturePayloadBlackBox } from "./ps5Snapshot";

const HOST = "192.168.1.50";
const LAST_WORDS = [
  { name: "stderr_old.log", text: "[payload2] mgmt accept: Bad file descriptor\n" },
  { name: "crash.log", text: "FATAL sig=11 frame=0x42\n" },
];

describe("payload black box in bug reports", () => {
  beforeEach(() => {
    clearBlackBoxes();
    resetBlackBoxCooldowns();
  });

  /* The common crash-report case: by the time someone files it, the helper is
   * down. That used to ship no payload logs at all. */
  it("a report filed while the helper is down carries the kept logs", async () => {
    recordBlackBox(HOST, LAST_WORDS, new Date("2026-09-22T15:51:35Z"));
    useConnectionStore.setState({ host: HOST, payloadStatus: "down" });

    const { snapshot, payload_logs } = await buildPs5Snapshot({ redact: true });

    expect(payload_logs.map((f) => f.name)).toEqual(["stderr_old.log", "crash.log"]);
    expect(snapshot.payload_logs_source).toBe("cached");
    expect(snapshot.payload_logs_captured_at).toBe("2026-09-22T15:51:35.000Z");
    expect(snapshot.payload_log_files).toEqual(["stderr_old.log", "crash.log"]);
  });

  /* The exact race in the FW 12.70 report: status still said "up" (it is
   * debounced), but every read was reset. The kept copy must stand in. */
  it("a report that races the helper's death falls back to the kept logs", async () => {
    recordBlackBox(HOST, LAST_WORDS);
    useConnectionStore.setState({ host: HOST, payloadStatus: "up" });

    const { snapshot, payload_logs } = await buildPs5Snapshot({ redact: true });

    expect(payload_logs).toHaveLength(2);
    expect(snapshot.payload_logs_source).toBe("cached");
  });

  /* With nothing kept, say so rather than implying the logs were empty. */
  it("says 'none' when there is neither a live read nor a kept copy", async () => {
    useConnectionStore.setState({ host: HOST, payloadStatus: "down" });
    const { snapshot, payload_logs } = await buildPs5Snapshot({ redact: true });
    expect(payload_logs).toEqual([]);
    expect(snapshot.payload_logs_source).toBe("none");
    expect(snapshot.payload_logs_captured_at).toBeNull();
  });

  /* A capture attempted against a helper that has just died reads nothing.
   * That must not erase the copy explaining the PREVIOUS death — which is the
   * one being asked about. */
  it("a failed capture never erases an earlier good one", async () => {
    recordBlackBox(HOST, LAST_WORDS);
    await capturePayloadBlackBox(HOST);
    expect(blackBoxFor(HOST)?.files).toEqual(LAST_WORDS);
  });

  /* Kept per console: two consoles must never trade evidence. */
  it("keeps logs per console", () => {
    recordBlackBox(HOST, LAST_WORDS);
    expect(blackBoxFor("192.168.1.51")).toBeNull();
  });
});

describe("black box capture cooldown", () => {
  beforeEach(() => resetBlackBoxCooldowns());

  /* A helper flapping every few seconds must not be read on every edge, but
   * the gap has to stay short: each new instance has a new predecessor. */
  it("allows one capture per console per cooldown window", () => {
    expect(shouldCaptureBlackBox(HOST, 1_000)).toBe(true);
    expect(shouldCaptureBlackBox(HOST, 1_000 + BLACK_BOX_COOLDOWN_MS - 1)).toBe(false);
    expect(shouldCaptureBlackBox(HOST, 1_000 + BLACK_BOX_COOLDOWN_MS)).toBe(true);
  });

  it("does not let one console's cooldown block another's", () => {
    expect(shouldCaptureBlackBox(HOST, 1_000)).toBe(true);
    expect(shouldCaptureBlackBox("192.168.1.51", 1_001)).toBe(true);
  });
});
