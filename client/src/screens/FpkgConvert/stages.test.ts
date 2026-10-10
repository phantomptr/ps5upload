import { describe, expect, it } from "vitest";

import type { Task } from "../../state/tasks";
import { overallProgress, stageRows } from "./stages";

const base = {
  mode: "convert-install" as const,
  source: "/g",
  host: "h",
  startedMs: 0,
  stageStartedMs: 0,
  jobId: "j",
  installTaskId: null,
  taskId: null,
  copiedSource: null,
  extractedSource: null,
  packagePath: null,
  titleId: null,
};

const task = (current: number, total: number) =>
  ({ progress: { current, total, unit: "bytes" } }) as unknown as Task;

describe("stageRows", () => {
  it("follows the pipeline's own stage and bytes for an upload install (no stream task)", () => {
    const sending = stageRows(
      { phase: "running", ...base, mode: "install", stage: "send", stageDone: 40, stageTotal: 100, stageMs: {} },
      null,
    );
    expect(sending[0]).toMatchObject({ stage: "send", state: "active", done: 40, total: 100 });
    const installing = stageRows(
      { phase: "running", ...base, mode: "install", stage: "install", stageDone: 0, stageTotal: 0, stageMs: { send: 9 } },
      null,
    );
    expect(installing.map((r) => [r.stage, r.state])).toEqual([
      ["send", "done"],
      ["install", "active"],
    ]);
  });

  it("marks earlier stages done and the current one active", () => {
    const rows = stageRows(
      { phase: "running", ...base, stage: "write", stageDone: 5, stageTotal: 10, stageMs: { check: 1, plan: 2, compress: 3 } },
      null,
    );
    expect(rows.map((r) => [r.stage, r.state])).toEqual([
      ["check", "done"],
      ["plan", "done"],
      ["compress", "done"],
      ["write", "active"],
      ["verify", "pending"],
      ["send", "pending"],
      ["install", "pending"],
    ]);
    expect(rows[3]).toMatchObject({ done: 5, total: 10 });
    expect(rows[2]).toMatchObject({ ms: 3 });
  });

  it("splits the install into send then install by the task's bytes", () => {
    const p = { phase: "running" as const, ...base, stage: "send" as const, stageDone: 0, stageTotal: 0, stageMs: {} };
    expect(stageRows(p, task(5, 10)).find((r) => r.state === "active")).toMatchObject({ stage: "send", done: 5, total: 10 });
    expect(stageRows(p, task(10, 10)).find((r) => r.state === "active")?.stage).toBe("install");
  });

  it("shows the failed stage and nothing after it as done", () => {
    const rows = stageRows(
      { phase: "failed", mode: "convert", source: "/g", host: null, stage: "verify", message: "bad", packagePath: null, stageMs: {}, titleId: null },
      null,
    );
    expect(rows.map((r) => r.state)).toEqual(["done", "done", "done", "done", "failed"]);
  });

  it("has no install rows for Convert only, and only install rows for a retry", () => {
    const convertOnly = stageRows(
      { phase: "failed", mode: "convert", source: "/g", host: null, stage: "write", message: "", packagePath: null, stageMs: {}, titleId: null },
      null,
    );
    expect(convertOnly.map((r) => r.stage)).not.toContain("send");
    const retry = stageRows(
      { phase: "running", ...base, mode: "install", stage: "send", stageDone: 0, stageTotal: 0, stageMs: {} },
      null,
    );
    expect(retry.map((r) => r.stage)).toEqual(["send", "install"]);
  });

  it("is all done once the run finished", () => {
    const rows = stageRows(
      { phase: "done", mode: "convert-install", source: "/g", host: "h", packagePath: "/p", packageBytes: 1, convertMs: 1, installMs: 1, stageMs: {}, deleted: false, titleId: null },
      null,
    );
    expect(rows.every((r) => r.state === "done")).toBe(true);
  });

  it("weights progress by stage size", () => {
    const rows = stageRows(
      { phase: "running", ...base, mode: "convert", stage: "compress", stageDone: 50, stageTotal: 100, stageMs: {} },
      null,
    );
    const p = overallProgress(rows);
    // check + plan (4 of 85) + half of compress (27.5 of 85) ≈ 0.37
    expect(p).toBeGreaterThan(0.3);
    expect(p).toBeLessThan(0.45);
    expect(overallProgress(stageRows({ phase: "idle" }, null))).toBe(0);
  });

  it("reads a server folder in place, and copies then unpacks a server archive", () => {
    const folder = stageRows(
      { phase: "running", ...base, source: "remote://nas-1/g", mode: "convert", stage: "check", stageDone: 0, stageTotal: 0, stageMs: {} },
      null,
    );
    expect(folder.map((r) => r.stage)).toEqual(["check", "plan", "compress", "write", "verify"]);
    const rows = stageRows(
      { phase: "running", ...base, source: "remote://nas-1/g.zip", mode: "convert", stage: "copy", stageDone: 5, stageTotal: 10, stageMs: {} },
      null,
    );
    expect(rows.map((r) => r.stage)).toEqual(["copy", "extract", "check", "plan", "compress", "write", "verify"]);
    expect(rows[0]).toMatchObject({ state: "active", done: 5, total: 10 });
  });

  it("lists the copy for compressing a server image", () => {
    const rows = stageRows(
      { phase: "running", ...base, source: "remote://nas-1/g.exfat", mode: "ffpfsc", stage: "copy", stageDone: 0, stageTotal: 0, stageMs: {} },
      null,
    );
    expect(rows[0]).toMatchObject({ stage: "copy", state: "active" });
  });

  it("shows the pack row only for an image build that packs LZ4 assets", () => {
    const packing = stageRows(
      { phase: "running", ...base, mode: "image", stage: "pack", stageDone: 1, stageTotal: 4, stageMs: {} },
      null,
    );
    expect(packing.map((r) => [r.stage, r.state])).toEqual([
      ["check", "done"],
      ["pack", "active"],
      ["plan", "pending"],
      ["compress", "pending"],
      ["write", "pending"],
      ["verify", "pending"],
    ]);
    // Past the pack stage it stays listed (and done), into the result.
    const writing = stageRows(
      { phase: "running", ...base, mode: "image", stage: "write", stageDone: 0, stageTotal: 0, stageMs: { pack: 5 } },
      null,
    );
    expect(writing[1]).toMatchObject({ stage: "pack", state: "done", ms: 5 });
    const plain = stageRows(
      { phase: "running", ...base, mode: "image", stage: "write", stageDone: 0, stageTotal: 0, stageMs: {} },
      null,
    );
    expect(plain.map((r) => r.stage)).not.toContain("pack");
  });
});
