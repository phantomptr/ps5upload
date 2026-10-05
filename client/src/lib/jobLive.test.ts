import { describe, expect, it } from "vitest";

import { bottleneckCause, jobLiveFromSnapshot } from "./jobLive";

describe("job live notes", () => {
  it("maps every word the engine uses for a bottleneck", () => {
    expect(bottleneckCause("network")).toBe("network");
    expect(bottleneckCause("source")).toBe("source");
    expect(bottleneckCause("console drive")).toBe("disk");
    expect(bottleneckCause("console workers")).toBe("workers");
    expect(bottleneckCause("console memory")).toBe("memory");
    expect(bottleneckCause("none")).toBeNull();
    expect(bottleneckCause("")).toBeNull();
    expect(bottleneckCause(undefined)).toBeNull();
    expect(bottleneckCause("something new")).toBeNull();
  });

  it("carries nothing for a snapshot without the new fields", () => {
    expect(jobLiveFromSnapshot({})).toBeUndefined();
    expect(jobLiveFromSnapshot(undefined)).toBeUndefined();
    expect(jobLiveFromSnapshot({ phase: "sending", settling: false })).toBeUndefined();
  });

  it("reads the skipping phase with its byte counts", () => {
    expect(
      jobLiveFromSnapshot({
        phase: "skipping",
        skip_done_bytes: 5,
        skip_total_bytes: 20,
      }),
    ).toEqual({
      skipping: true,
      skipDoneBytes: 5,
      skipTotalBytes: 20,
      bottleneck: null,
      settling: false,
    });
  });

  it("reads the bottleneck live, or from the finished job's commit ack", () => {
    expect(jobLiveFromSnapshot({ bottleneck: "console drive" })?.bottleneck).toBe("disk");
    expect(
      jobLiveFromSnapshot({ commit_ack: { bottleneck: "source" } })?.bottleneck,
    ).toBe("source");
  });

  it("settling is true only when the engine says so", () => {
    expect(jobLiveFromSnapshot({ settling: true })?.settling).toBe(true);
    expect(jobLiveFromSnapshot({ settling: undefined })).toBeUndefined();
  });

  it("carries the settle counts only while settling, never below zero or above the total", () => {
    expect(
      jobLiveFromSnapshot({ settling: true, settle_files_left: 30, settle_files_total: 100 }),
    ).toMatchObject({ settling: true, settleLeft: 30, settleTotal: 100 });
    expect(
      jobLiveFromSnapshot({ settling: true, settle_files_left: 30, settle_files_total: 10 }),
    ).toMatchObject({ settleLeft: 30, settleTotal: 30 });
    expect(jobLiveFromSnapshot({ settling: true })).not.toHaveProperty("settleLeft");
    expect(jobLiveFromSnapshot({ settle_files_left: 3 })).toBeUndefined();
  });

  it("carries the console's settle warning from a finished job's commit_ack, and only then", () => {
    const warned = jobLiveFromSnapshot({
      commit_ack: { warning: "files are still being made durable on the console" },
    });
    expect(warned?.unsettled).toBe(true);
    expect(jobLiveFromSnapshot({ commit_ack: { bottleneck: "network" } })?.unsettled).toBeFalsy();
    expect(jobLiveFromSnapshot({ commit_ack: { warning: "" } })).toBeUndefined();
    expect(jobLiveFromSnapshot({ commit_ack: { warning: null } })).toBeUndefined();
  });
});
