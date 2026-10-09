import { describe, expect, it, vi } from "vitest";
vi.mock("../state/engine", () => ({ getEngineUrl: () => "http://engine" }));
vi.mock("./appJournal", () => ({ readAppEvents: async () => [], appJournalDropped: () => 0 }));
import { fetchTimeline, filterCats, formatLine, mergeEvents, recentProblems } from "./reportTimeline";
import type { EventRecord } from "./eventRecord";

const e = (ts: number, src: EventRecord["src"], code: string, level: EventRecord["level"] = "info"): EventRecord => ({
  ts,
  src,
  cat: "connection",
  level,
  code,
  msg: code,
});

describe("mergeEvents", () => {
  it("sorts by time and keeps each source's own order on ties", () => {
    const m = mergeEvents([e(2, "engine", "b"), e(2, "engine", "c")], [e(1, "app", "a"), e(2, "app", "d")]);
    expect(m.map((x) => x.code)).toEqual(["a", "b", "c", "d"]);
  });
});

describe("recentProblems", () => {
  it("returns the newest warnings and errors first", () => {
    const r = recentProblems([e(1, "engine", "x", "warn"), e(2, "engine", "y"), e(3, "helper", "z", "error")]);
    expect(r.map((x) => x.code)).toEqual(["z", "x"]);
  });
  it("caps the list", () => {
    const many = Array.from({ length: 30 }, (_, i) => e(i, "engine", `c${i}`, "error"));
    expect(recentProblems(many, 10)).toHaveLength(10);
    expect(recentProblems(many, 10)[0].code).toBe("c29");
  });
});

describe("filterCats", () => {
  it("keeps only the chosen categories", () => {
    const install = { ...e(1, "engine", "i"), cat: "install" as const };
    expect(filterCats([e(0, "engine", "c"), install], new Set(["install"]))).toEqual([install]);
  });
});

describe("formatLine", () => {
  it("shows time, source, console, level, code and a folded count", () => {
    const line = formatLine(
      { ...e(0, "engine", "reconnecting", "warn"), count: 37, last_ts: 360_000, console: "10.0.0.9" },
      "UTC",
    );
    expect(line).toBe("1970-01-01 00:00:00 [engine] 10.0.0.9  connection WARN reconnecting: reconnecting (×37 until 00:06:00)");
  });
  it("leaves out what an event does not have", () => {
    expect(formatLine({ ts: 0, src: "app", cat: "app", level: "info", msg: "started" }, "UTC")).toBe(
      "1970-01-01 00:00:00 [app] app INFO started",
    );
  });
});

describe("fetchTimeline", () => {
  it("does not cut the engine's events at this machine's clock", async () => {
    // A Docker host whose clock runs ahead would otherwise lose its newest events: the ones
    // right before the user pressed Report.
    const ahead = Date.now() + 120_000;
    const f = vi.fn(async (_url: string) => new Response(JSON.stringify({ events: [{ ts: ahead, src: "engine", cat: "connection", level: "error", msg: "lost" }], dropped: 0 })));
    vi.stubGlobal("fetch", f);
    const t = await fetchTimeline(0, Date.now());
    expect(String(f.mock.calls[0][0])).not.toContain("until=");
    expect(t.events).toHaveLength(1);
    vi.unstubAllGlobals();
  });
});
