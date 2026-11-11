// A package row past its upload is an install, not an upload: nothing may re-run it, reset it or
// count it as finished work by itself.
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import {
  canResumeUnattended,
  isInstallWork,
  mayDeleteSourceAfterUpload,
  retryFailedRow,
  retryPatch,
  shouldRestAfterRun,
  type QueueItem,
} from "./uploadQueue";

const row = (over: Partial<QueueItem>): QueueItem =>
  ({
    id: "a",
    sourceKind: "pkg",
    sourcePath: "/g/p.pkg",
    displayName: "P",
    resolvedDest: "/data/pkg/P.pkg",
    addr: "10.0.0.2:9113",
    status: "failed",
    error: "Another install is already running on this PS5.",
    errorReason: null,
    errorDetail: null,
    completedAt: 100,
    ...over,
  }) as QueueItem;

describe("isInstallWork", () => {
  it("is an install item, or a package past its upload", () => {
    expect(isInstallWork(row({ sourceKind: "install" }))).toBe(true);
    expect(isInstallWork(row({ uploadDone: true }))).toBe(true);
    expect(isInstallWork(row({ uploadDone: false }))).toBe(false);
    expect(isInstallWork(row({ sourceKind: "folder", uploadDone: true }))).toBe(false);
  });
});

describe("canResumeUnattended", () => {
  it("resumes a failed upload whose failure reads as transport", () => {
    expect(canResumeUnattended(row({ error: "connection reset" }))).toBe(true);
  });
  it("never re-runs a package that reached its install, whatever its failure says", () => {
    expect(canResumeUnattended(row({ uploadDone: true, error: "connection reset" }))).toBe(false);
    // Saved by an older build, which never set uploadDone before the failure was saved.
    expect(canResumeUnattended(row({ installPhase: "error", error: "connection reset" }))).toBe(false);
    expect(canResumeUnattended(row({ sourceKind: "install", error: "connection reset" }))).toBe(false);
  });
  it("leaves fatal failures and other states alone", () => {
    expect(canResumeUnattended(row({ errorReason: "ava1_no_space" }))).toBe(false);
    expect(canResumeUnattended(row({ status: "pending" }))).toBe(false);
  });
});

describe("retryPatch", () => {
  it("installs a package that failed at its install from the copy on the console", () => {
    const p = retryPatch(row({ uploadDone: true, installPhase: "error", installJobId: "j1" }));
    expect(p.status).toBe("pending");
    expect(p.sourceKind).toBe("install");
    expect(p.install).toEqual({ via: "library", path: "/data/pkg/P.pkg" });
    expect(p.uploadDone).toBe(false);
    expect(p.installPhase).toBeNull();
    expect(p.installJobId).toBeNull();
  });
  it("sends again a package that never finished sending, or was not to be installed", () => {
    expect(retryPatch(row({ uploadDone: false })).sourceKind).toBeUndefined();
    expect(retryPatch(row({ uploadDone: true, installAfterUpload: false })).sourceKind).toBeUndefined();
    expect(retryPatch(row({ sourceKind: "folder", uploadDone: true })).sourceKind).toBeUndefined();
  });
});

describe("shouldRestAfterRun", () => {
  const started = 1000;
  const up = (over: Partial<QueueItem>) =>
    row({ sourceKind: "folder", status: "done", completedAt: 2000, ...over });
  it("rests after a run that sent something and all went well", () => {
    expect(shouldRestAfterRun([up({})], "10.0.0.2", started, false)).toBe(true);
  });
  it("is not fooled by a finished row from an earlier run", () => {
    const old = up({ completedAt: 500 });
    expect(shouldRestAfterRun([old], "10.0.0.2", started, false)).toBe(false);
    expect(
      shouldRestAfterRun([old, up({ id: "b", status: "failed" })], "10.0.0.2", started, false),
    ).toBe(false);
  });
  it("never rests over an install, on this run or still going", () => {
    expect(
      shouldRestAfterRun([up({}), up({ id: "b", sourceKind: "install" })], "10.0.0.2", started, false),
    ).toBe(false);
    expect(
      shouldRestAfterRun([up({}), up({ id: "b", sourceKind: "pkg" })], "10.0.0.2", started, false),
    ).toBe(false);
    expect(shouldRestAfterRun([up({})], "10.0.0.2", started, true)).toBe(false);
    expect(
      shouldRestAfterRun(
        [up({}), up({ id: "b", status: "running", completedAt: null })],
        "10.0.0.2",
        started,
        false,
      ),
    ).toBe(false);
  });
  it("does not look at another console", () => {
    expect(shouldRestAfterRun([up({ addr: "10.0.0.3:9113" })], "10.0.0.2", started, false)).toBe(false);
  });
});

describe("Retry failed", () => {
  it("installs a package that failed at its install from the copy on the console, like Retry", () => {
    const failed = row({ status: "failed", uploadDone: true, installPhase: "error", error: "refused" });
    const next = retryFailedRow(failed);
    expect(next.status).toBe("pending");
    expect(next.sourceKind).toBe("install");
    expect(next.install).toEqual({ via: "library", path: "/data/pkg/P.pkg" });
    // Not an upload any more: the run does not reset it to send the bytes again.
    expect(next.uploadDone).toBe(false);
    expect(next).toEqual({ ...failed, ...retryPatch(failed) });
  });
  it("sends again a package that never got there, and clears a failed install row", () => {
    expect(retryFailedRow(row({ status: "failed", uploadDone: false })).sourceKind).toBe("pkg");
    const install = retryFailedRow(
      row({
        status: "failed",
        sourceKind: "install",
        installPhase: "error",
        installPct: 40,
        installNote: "x",
        fallbackToUpload: true,
      }),
    );
    expect(install.status).toBe("pending");
    expect(install.installPhase).toBeNull();
    expect(install.installPct).toBeNull();
    expect(install.installNote).toBeNull();
    expect(install.fallbackToUpload).toBe(false);
  });
});

describe("rest after upload with a console-to-console copy", () => {
  const started = 1000;
  const up = (over: Partial<QueueItem>) =>
    row({ sourceKind: "folder", status: "done", completedAt: 2000, ...over });
  const copy = (over: Partial<QueueItem>) =>
    row({ id: "c", sourceKind: "folder", addr: "10.0.0.3:9113", fromConsole: "10.0.0.2", ...over });
  it("never rests a console another console's queue copies from, running or still to come", () => {
    expect(
      shouldRestAfterRun([up({}), copy({ status: "running", completedAt: null })], "10.0.0.2", started, false),
    ).toBe(false);
    expect(
      shouldRestAfterRun([up({}), copy({ status: "pending", completedAt: null })], "10.0.0.2", started, false),
    ).toBe(false);
    expect(
      shouldRestAfterRun(
        [up({}), copy({ status: "running", completedAt: null, fromConsole: "10.0.0.2:9113" })],
        "10.0.0.2",
        started,
        false,
      ),
    ).toBe(false);
  });
  it("rests once that copy is over, or when it reads from some other console", () => {
    expect(shouldRestAfterRun([up({}), copy({ status: "done" })], "10.0.0.2", started, false)).toBe(true);
    expect(shouldRestAfterRun([up({}), copy({ status: "failed" })], "10.0.0.2", started, false)).toBe(true);
    expect(
      shouldRestAfterRun(
        [up({}), copy({ status: "running", completedAt: null, fromConsole: "10.0.0.9" })],
        "10.0.0.2",
        started,
        false,
      ),
    ).toBe(true);
  });
});

describe("delete source after upload", () => {
  it("only after an upload the console confirmed saved", () => {
    expect(mayDeleteSourceAfterUpload({ deleteSourceAfterUpload: true }, undefined)).toBe(true);
    expect(mayDeleteSourceAfterUpload({ deleteSourceAfterUpload: true }, { unsettled: false })).toBe(true);
    // "Not confirmed saved: send it again if the console loses power" needs the source.
    expect(mayDeleteSourceAfterUpload({ deleteSourceAfterUpload: true }, { unsettled: true })).toBe(false);
    expect(mayDeleteSourceAfterUpload({ deleteSourceAfterUpload: false }, undefined)).toBe(false);
    expect(mayDeleteSourceAfterUpload({}, undefined)).toBe(false);
  });
});
