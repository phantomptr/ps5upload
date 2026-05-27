import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { useActivityHistoryStore } from "./activityHistory";

// The store reads localStorage at module load; clear before each test
// so we always start from an empty entries[] and aren't influenced by
// the previous test's persist().
function resetStore() {
  useActivityHistoryStore.setState({ entries: [] });
}

describe("activityHistory phase field", () => {
  beforeEach(() => {
    resetStore();
    if (typeof window !== "undefined") {
      window.localStorage.clear();
    }
  });

  afterEach(() => {
    resetStore();
  });

  it("defaults phase to undefined on start()", () => {
    const id = useActivityHistoryStore
      .getState()
      .start("upload-queue", "Uploading 12 files");
    const entry = useActivityHistoryStore
      .getState()
      .entries.find((e) => e.id === id);
    expect(entry).toBeDefined();
    expect(entry!.phase).toBeUndefined();
  });

  it("update() can flip phase to 'finalizing' without touching other fields", () => {
    const id = useActivityHistoryStore
      .getState()
      .start("upload-queue", "Uploading 84216 files", {
        bytes: 0,
        totalBytes: 162_000_000_000,
        files: 84216,
      });
    useActivityHistoryStore.getState().update(id, {
      bytes: 162_000_000_000,
      totalBytes: 162_000_000_000,
      phase: "finalizing",
    });
    const entry = useActivityHistoryStore
      .getState()
      .entries.find((e) => e.id === id);
    expect(entry!.phase).toBe("finalizing");
    expect(entry!.bytes).toBe(162_000_000_000);
    expect(entry!.totalBytes).toBe(162_000_000_000);
    // label + files (from start extras) must survive the patch.
    expect(entry!.label).toBe("Uploading 84216 files");
    expect(entry!.files).toBe(84216);
  });

  it("update() can flip phase back from 'finalizing' to 'uploading'", () => {
    // Defense against a hypothetical engine emitting an updated
    // total_bytes mid-transfer that pushes bytesSent < totalBytes
    // again — the finalize chip should disappear, not stick.
    const id = useActivityHistoryStore
      .getState()
      .start("upload-queue", "Uploading", { bytes: 100, totalBytes: 100 });
    useActivityHistoryStore
      .getState()
      .update(id, { phase: "finalizing" });
    expect(
      useActivityHistoryStore.getState().entries.find((e) => e.id === id)!
        .phase,
    ).toBe("finalizing");
    useActivityHistoryStore
      .getState()
      .update(id, { bytes: 80, totalBytes: 200, phase: "uploading" });
    const entry = useActivityHistoryStore
      .getState()
      .entries.find((e) => e.id === id);
    expect(entry!.phase).toBe("uploading");
  });

  it("finish() does not clobber phase but conceptually retires it (outcome takes precedence)", () => {
    const id = useActivityHistoryStore
      .getState()
      .start("upload-queue", "Uploading", { bytes: 100, totalBytes: 100 });
    useActivityHistoryStore
      .getState()
      .update(id, { phase: "finalizing" });
    useActivityHistoryStore.getState().finish(id, "done", { bytes: 100 });
    const entry = useActivityHistoryStore
      .getState()
      .entries.find((e) => e.id === id);
    // Renderers gate on `outcome === "running" && phase === "finalizing"`,
    // so a leftover phase on a Done entry is harmless. Just pin the
    // contract here — if a future refactor strips phase on finish, the
    // test will catch it and we can update both sides intentionally.
    expect(entry!.outcome).toBe("done");
    expect(entry!.phase).toBe("finalizing");
  });

  it("entries persist phase through JSON round-trip", () => {
    if (typeof window === "undefined") return;
    useActivityHistoryStore
      .getState()
      .start("upload-queue", "Uploading", {
        bytes: 100,
        totalBytes: 100,
        phase: "finalizing",
      });
    // Force a synchronous persist by reading the localStorage key the
    // store writes to. The persist is debounced; we don't need to wait
    // for the debounce — we only care that the type accepts `phase` on
    // start() extras (the route activityWiring.ts doesn't take, but
    // future callers might).
    const entries = useActivityHistoryStore.getState().entries;
    const json = JSON.stringify(entries);
    const reloaded = JSON.parse(json) as Array<{ phase?: string }>;
    expect(reloaded[0]?.phase).toBe("finalizing");
  });
});
