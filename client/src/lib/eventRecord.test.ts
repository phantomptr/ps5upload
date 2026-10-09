import { describe, expect, it } from "vitest";
import { collapseInto, type EventRecord } from "./eventRecord";

const ev = (ts: number, code = "engine_down", src: EventRecord["src"] = "app"): EventRecord => ({
  ts,
  src,
  cat: "app",
  level: "warn",
  code,
  msg: "m",
});

describe("collapseInto", () => {
  it("folds repeats within 60 s", () => {
    let prev: EventRecord | null = null;
    const flushed: EventRecord[] = [];
    for (let i = 0; i < 5; i++) {
      const r = collapseInto(prev, ev(1000 + i * 5000));
      if (r.flushed) flushed.push(r.flushed);
      prev = r.merged;
    }
    expect(flushed).toHaveLength(0);
    expect(prev).toMatchObject({ ts: 1000, count: 5, last_ts: 21000 });
  });

  it("flushes on a different code or a gap", () => {
    const a = collapseInto(null, ev(0)).merged;
    expect(collapseInto(a, ev(1, "other")).flushed).toEqual(a);
    expect(collapseInto(a, ev(61_001)).flushed).toEqual(a);
  });

  it("never folds helper lines", () => {
    const a = collapseInto(null, ev(0, "helper_log", "helper")).merged;
    expect(collapseInto(a, ev(1, "helper_log", "helper")).flushed).toEqual(a);
  });

  it("never folds a notification or a crash, so a later error keeps its own line and time", () => {
    const warn: EventRecord = { ts: 0, src: "app", cat: "app", level: "warn", code: "notification", msg: "Slow" };
    const err: EventRecord = { ts: 1, src: "app", cat: "app", level: "error", code: "notification", msg: "Upload failed" };
    expect(collapseInto(warn, err).flushed).toEqual(warn);
    expect(collapseInto(err, { ...err, ts: 2 }).flushed).toEqual(err);
  });

  it("folds repeats that differ only in their numbers, not in their words", () => {
    const a: EventRecord = { ts: 0, src: "app", cat: "app", level: "error", code: "engine_down", msg: "engine unreachable (3 s)" };
    expect(collapseInto(a, { ...a, ts: 1, msg: "engine unreachable (17 s)" }).flushed).toBeNull();
    expect(collapseInto(a, { ...a, ts: 1, msg: "engine refused" }).flushed).toEqual(a);
    expect(collapseInto(a, { ...a, ts: 1, level: "warn" }).flushed).toEqual(a);
  });
});
