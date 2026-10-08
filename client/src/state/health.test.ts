import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({ healthScan: vi.fn() }));
vi.mock("../api/ps5", () => api);

import {
  healthFor,
  healthProblems,
  scanHealth,
  scanHealthIfStale,
  useHealthStore,
} from "./health";

const A = "10.0.0.5";
const report = (checks: Array<{ id: string; status: string }>) => ({
  addr: A,
  duration_ms: 1,
  checks: checks.map((c) => ({
    title: c.id,
    category: "network",
    detail: "",
    remedy: "",
    ...c,
  })),
  summary: { pass: 0, warn: 0, fail: 0, skip: 0 },
});
const at = () => healthFor(useHealthStore.getState(), A);

beforeEach(() => {
  vi.clearAllMocks();
  useHealthStore.setState({ byHost: {} });
});

describe("health, shared by Home and the Health screen", () => {
  it("keeps the last report per console with when it was taken", async () => {
    api.healthScan.mockResolvedValue(report([{ id: "a", status: "pass" }]));
    await scanHealth(A, 1000);
    expect(at()).toMatchObject({
      scanning: false,
      scannedAtMs: 1000,
      error: null,
    });
    expect(at()?.report?.checks).toHaveLength(1);
  });

  it("does not scan again while a report is fresh, and does once it is old", async () => {
    api.healthScan.mockResolvedValue(report([]));
    await scanHealthIfStale(A, 60_000, 1000);
    await scanHealthIfStale(A, 60_000, 30_000);
    expect(api.healthScan).toHaveBeenCalledTimes(1);
    await scanHealthIfStale(A, 60_000, 70_000);
    expect(api.healthScan).toHaveBeenCalledTimes(2);
  });

  it("one scan at a time per console", async () => {
    let release: (v: unknown) => void = () => {};
    api.healthScan.mockImplementation(() => new Promise((r) => (release = r)));
    const first = scanHealth(A);
    await scanHealth(A);
    expect(api.healthScan).toHaveBeenCalledTimes(1);
    release(report([]));
    await first;
  });

  it("keeps the previous report when a scan fails, and says why", async () => {
    api.healthScan.mockResolvedValueOnce(report([{ id: "a", status: "pass" }]));
    await scanHealth(A);
    api.healthScan.mockRejectedValueOnce(new Error("engine gone"));
    await scanHealth(A);
    expect(at()?.report?.checks).toHaveLength(1);
    expect(at()?.error).toContain("engine gone");
  });

  it("lists what needs attention, problems before warnings", () => {
    const r = report([
      { id: "ok", status: "pass" },
      { id: "warned", status: "warn" },
      { id: "na", status: "skip" },
      { id: "broken", status: "fail" },
    ]);
    expect(healthProblems(r as never).map((c) => c.id)).toEqual([
      "broken",
      "warned",
    ]);
    expect(healthProblems(null)).toEqual([]);
  });
});
