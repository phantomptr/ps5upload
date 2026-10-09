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
});
