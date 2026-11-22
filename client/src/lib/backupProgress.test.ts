import { describe, expect, it } from "vitest";

import type {
  BackupRun,
  BackupRunItem,
  BackupRunsView,
  BackupRunSummary,
} from "../api/saveBackup";
import {
  itemOutcome,
  overall,
  pressOnce,
  retryable,
  rows,
  shouldPoll,
  SpeedMeter,
  stoppedNote,
  toRow,
} from "./backupProgress";

function item(
  id: string,
  dests: BackupRunItem["dests"],
  over: Partial<BackupRunItem> = {},
): BackupRunItem {
  return {
    id,
    host: "10.0.0.2",
    kind: "ps5",
    title_id: "PPSA01234",
    user_id: 0x10000001,
    path: `/user/home/10000001/savedata_prospero/${id}`,
    size: 100,
    mtime: 1,
    step: "waiting",
    error: null,
    tries: 0,
    dests,
    ...over,
  };
}

const d = (
  step: BackupRunItem["step"],
  extra: Partial<NonNullable<BackupRunItem["dests"]["share"]>> = {},
) => ({
  step,
  error: null,
  file: null,
  tries: 1,
  ...extra,
});

function run(items: BackupRunItem[], over: Partial<BackupRun> = {}): BackupRun {
  return {
    id: "share-1",
    which: "share",
    trigger: "manual",
    started_at: 1_000_000,
    updated_at: 1_000_000,
    ended_at: 0,
    state: "running",
    reason_code: null,
    reason: null,
    items,
    unreached: {},
    ...over,
  };
}

function summary(over: Partial<BackupRunSummary> = {}): BackupRunSummary {
  return {
    id: "share-1",
    trigger: "manual",
    started_at: 1_000_000,
    ended_at: 0,
    state: "running",
    reason_code: null,
    reason: null,
    total: 66,
    verified: 7,
    failed: 0,
    skipped: 0,
    left: 59,
    read_failed: 0,
    bytes_total: 66_000,
    bytes_done: 7_000,
    first_error: null,
    places: {},
    ...over,
  };
}

describe("backup progress rows", () => {
  it("tells each save's state from its places", () => {
    expect(
      itemOutcome(item("a", { share: d("verified"), drive: d("verified") })),
    ).toBe("verified");
    expect(
      itemOutcome(item("b", { share: d("verified"), drive: d("skipped") })),
    ).toBe("verified");
    expect(
      itemOutcome(item("c", { share: d("skipped"), drive: d("skipped") })),
    ).toBe("skipped");
    expect(
      itemOutcome(item("e", { share: d("failed"), drive: d("verified") })),
    ).toBe("failed");
    expect(
      itemOutcome(
        item("f", { share: d("waiting") }, { step: "failed", error: "x" }),
      ),
    ).toBe("failed");
    expect(
      itemOutcome(item("g", { share: d("verified"), drive: d("waiting") })),
    ).toBe("left");
  });

  it("shows the busy place with how far its copy is, and the reason of a failure", () => {
    const copying = toRow(
      item(
        "a",
        { share: d("copying", { pct: 42 }), drive: d("waiting") },
        { step: "copying" },
      ),
      ["share", "drive"],
    );
    expect(copying.step).toBe("copying");
    expect(copying.pct).toBe(42);
    expect(copying.places.map((p) => p.place)).toEqual(["share", "drive"]);
    const reading = toRow(
      item("b", { share: d("waiting") }, { step: "reading" }),
      ["share", "drive"],
    );
    expect(reading.step).toBe("reading");
    expect(reading.places).toHaveLength(1);
    const failed = toRow(
      item("c", {
        share: d("failed", { error: "connection reset" }),
        drive: d("verified"),
      }),
      ["share", "drive"],
    );
    expect(failed.step).toBe("failed");
    expect(failed.error).toBe("connection reset");
    const unread = toRow(
      item(
        "e",
        { share: d("waiting") },
        { step: "failed", error: "not a save" },
      ),
      ["share"],
    );
    expect(unread.error).toBe("not a save");
    expect(toRow(item("f", { share: d("waiting") }), ["share"]).step).toBe(
      "waiting",
    );
  });

  it("lists busy saves first, then failed, waiting and done; filters; Retry takes the failed", () => {
    const r = run(
      [
        item("done", { share: d("verified") }),
        item("wait", { share: d("waiting") }),
        item("bad", { share: d("failed", { error: "x" }) }),
        item("now", { share: d("checking") }, { step: "checking" }),
      ],
      { state: "done", ended_at: 2_000_000 },
    );
    expect(rows(r, "share").map((x) => x.id)).toEqual([
      "now",
      "bad",
      "wait",
      "done",
    ]);
    expect(rows(r, "share", "failed").map((x) => x.id)).toEqual(["bad"]);
    expect(rows(r, "share", "verified").map((x) => x.id)).toEqual(["done"]);
    expect(retryable(r)).toEqual(["bad"]);
    // No Retry while it runs.
    expect(retryable({ ...r, state: "running" })).toEqual([]);
    expect(rows(null, "share")).toEqual([]);
  });
});

describe("backup progress overall", () => {
  it("counts saves and bytes, and guesses time left from the speed seen", () => {
    const view: BackupRunsView = {
      run: run([]),
      summary: summary(),
      history: [],
      now: 1_000_000 + 60_000,
    };
    const meter = new SpeedMeter();
    expect(meter.sample("share-1", 7_000, 1_060_000)).toBeNull();
    // Not enough time yet.
    expect(meter.sample("share-1", 8_000, 1_061_000)).toBeNull();
    const speed = meter.sample("share-1", 17_000, 1_070_000);
    expect(speed).toBe(1_000);
    const o = overall(
      {
        ...view,
        summary: summary({ bytes_done: 17_000, verified: 17, left: 49 }),
      },
      speed,
    )!;
    expect(o.done).toBe(17);
    expect(o.total).toBe(66);
    expect(o.elapsedMs).toBe(60_000);
    expect(o.leftMs).toBe(49_000_000 / 1_000);
    expect(o.running).toBe(true);
    // A new run starts its own measure (a continued one begins with saves done).
    expect(meter.sample("share-2", 40_000, 2_000_000)).toBeNull();
  });

  it("an ended run has no speed, and its time is up to its end", () => {
    const view: BackupRunsView = {
      run: run([], { state: "done", ended_at: 1_090_000 }),
      summary: summary({
        state: "done",
        verified: 66,
        left: 0,
        bytes_done: 66_000,
      }),
      history: [],
      now: 9_999_999,
    };
    const o = overall(view, 500)!;
    expect(o.speed).toBeNull();
    expect(o.leftMs).toBeNull();
    expect(o.elapsedMs).toBe(90_000);
    expect(
      overall({ run: null, summary: null, history: [], now: 0 }, null),
    ).toBeNull();
  });
});

describe("backup card note", () => {
  it("says how far a run that stopped got, and whether there is anything to continue", () => {
    const stopped: BackupRunsView = {
      run: run([], {
        state: "stopped",
        reason_code: "console_gone",
        reason: "The PS5 isn't answering",
        ended_at: 3_000,
      }),
      summary: summary({ state: "stopped" }),
      history: [],
      now: 4_000,
    };
    expect(stoppedNote(stopped)).toEqual({
      done: 7,
      total: 66,
      code: "console_gone",
      reason: "The PS5 isn't answering",
      at: 3_000,
      canContinue: true,
    });
    // An interrupted run (the engine stopped) reads the same way, at its last change.
    const interrupted = stoppedNote({
      ...stopped,
      run: run([], {
        state: "interrupted",
        reason_code: "interrupted",
        ended_at: 0,
        updated_at: 2_500,
      }),
    });
    expect(interrupted?.code).toBe("interrupted");
    expect(interrupted?.at).toBe(2_500);
    // Cancelled with nothing left: said, but no Continue.
    const cancelled = stoppedNote({
      ...stopped,
      run: run([], {
        state: "cancelled",
        reason_code: "cancelled",
        ended_at: 3_000,
      }),
      summary: summary({ state: "cancelled", left: 0 }),
    });
    expect(cancelled?.canContinue).toBe(false);
    // A run that went through, or one running: nothing to say.
    expect(
      stoppedNote({ ...stopped, run: run([], { state: "done" }) }),
    ).toBeNull();
    expect(
      stoppedNote({ ...stopped, run: run([], { state: "running" }) }),
    ).toBeNull();
    expect(stoppedNote(null)).toBeNull();
  });

  it("polls only while a run runs and the page is shown", () => {
    const view: BackupRunsView = {
      run: run([]),
      summary: summary(),
      history: [],
      now: 0,
    };
    expect(shouldPoll(view, true)).toBe(true);
    expect(shouldPoll(view, false)).toBe(false);
    expect(shouldPoll({ ...view, run: run([], { state: "done" }) }, true)).toBe(
      false,
    );
    expect(shouldPoll(null, true)).toBe(false);
  });
});

describe("Continue and Retry pressed twice (#895)", () => {
  it("sends once until the first press has settled, then again", async () => {
    let calls = 0;
    let finish: () => void = () => {};
    const guard = { current: false };
    const again = () =>
      pressOnce(guard, async () => {
        calls++;
        await new Promise<void>((r) => (finish = r));
      });
    const first = again();
    void again();
    void again();
    expect(calls).toBe(1);
    finish();
    await first;
    const third = again();
    expect(calls).toBe(2);
    finish();
    await third;
  });

  it("is free again after a press that failed", async () => {
    let calls = 0;
    const guard = { current: false };
    const again = () =>
      pressOnce(guard, async () => {
        calls++;
        throw new Error("HTTP 409");
      });
    await expect(again()).rejects.toThrow("HTTP 409");
    await expect(again()).rejects.toThrow("HTTP 409");
    expect(calls).toBe(2);
  });
});
