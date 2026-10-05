import { describe, expect, it } from "vitest";

import { parseRunningJobs, unclaimedJobs } from "./engineJobs";

describe("engine jobs", () => {
  it("keeps only running jobs and reads their progress", () => {
    const jobs = parseRunningJobs([
      { job_id: "a", status: "running", job: { bytes_sent: 5, total_bytes: 10, files_finalized: 2, files_finalizing_total: 4, started_at_ms: 99 } },
      { job_id: "b", status: "done", job: {} },
      { job_id: "c", status: "failed", job: {} },
      { status: "running", job: {} },
      null,
    ]);
    expect(jobs).toEqual([
      { jobId: "a", bytesSent: 5, totalBytes: 10, filesFinalized: 2, filesFinalizingTotal: 4, startedAtMs: 99 },
    ]);
  });

  it("is empty for anything that is not a list", () => {
    expect(parseRunningJobs({})).toEqual([]);
    expect(parseRunningJobs(undefined)).toEqual([]);
  });

  it("drops what this tab already watches", () => {
    const jobs = parseRunningJobs([
      { job_id: "a", status: "running", job: {} },
      { job_id: "b", status: "running", job: {} },
    ]);
    expect(unclaimedJobs(jobs, new Set(["a"])).map((j) => j.jobId)).toEqual(["b"]);
  });
});
