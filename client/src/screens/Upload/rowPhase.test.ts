import { describe, expect, it } from "vitest";
import { rowPhase } from "./QueuePanel";

const base = { status: "running", totalBytes: 100, bytesSent: 100 } as const;

describe("rowPhase", () => {
  it("says installing once the queue has started the install", () => {
    // Seen on Android: a 15 MB patch sat on "Finalizing on PS5 — committing
    // the file index… don't close the app" while it was actually installing.
    expect(rowPhase({ ...base, installPhase: "installing" })).toBe("installing");
  });

  it("says finalizing while the upload commits (no install yet)", () => {
    expect(rowPhase({ ...base, installPhase: undefined })).toBe("finalizing");
  });

  it("is neither while bytes are still flowing or the row is idle", () => {
    expect(rowPhase({ ...base, bytesSent: 50, installPhase: undefined })).toBeNull();
    expect(rowPhase({ ...base, status: "done", installPhase: "done" })).toBeNull();
  });
});
