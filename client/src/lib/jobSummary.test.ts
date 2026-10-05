import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine.test" }));

import { fetchJobSummaries, fetchJobSummary, whyEntry, type JobSummary } from "./jobSummary";

function stub(routes: Record<string, { status?: number; body: unknown }>) {
  const calls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      calls.push(url);
      const r = routes[url.replace("http://engine.test", "")];
      if (!r) return { ok: false, status: 404, json: async () => ({}) };
      return { ok: (r.status ?? 200) < 400, status: r.status ?? 200, json: async () => r.body };
    }),
  );
  return calls;
}

const summary = {
  job_id: "abc",
  result: "done",
  why: { dominant: "receiver_bound", pct: 70.6, text: "x" },
} as unknown as JobSummary;

describe("job summaries", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("reads one summary by job id", async () => {
    const calls = stub({ "/api/jobs/abc/summary": { body: summary } });
    expect((await fetchJobSummary("abc"))?.job_id).toBe("abc");
    expect(calls).toEqual(["http://engine.test/api/jobs/abc/summary"]);
  });

  it("is null, not an error, when there is no summary or the engine is gone", async () => {
    stub({});
    expect(await fetchJobSummary("nope")).toBeNull();
    vi.stubGlobal("fetch", vi.fn(async () => Promise.reject(new Error("down"))));
    expect(await fetchJobSummary("x")).toBeNull();
    expect(await fetchJobSummaries()).toEqual([]);
  });

  it("lists the newest summaries with the limit", async () => {
    const calls = stub({ "/api/jobs/summaries?limit=20": { body: { summaries: [summary] } } });
    expect(await fetchJobSummaries(20)).toHaveLength(1);
    expect(calls[0]).toBe("http://engine.test/api/jobs/summaries?limit=20");
  });

  it("maps every dominant share to its own catalog key", () => {
    const keys = (["receiver_bound", "source_starved", "credit_starved", "network", "unmeasured"] as const).map(
      (d) => whyEntry({ ...summary, why: { dominant: d, pct: 40, text: "" } }).key,
    );
    expect(new Set(keys).size).toBe(5);
    expect(whyEntry(summary).pct).toBe(71);
    expect(whyEntry({ ...summary, why: undefined } as unknown as JobSummary).key).toBe("job_why_unmeasured");
  });
});
