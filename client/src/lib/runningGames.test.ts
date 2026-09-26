import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("../api/ps5", () => ({ processList: vi.fn() }));

import { processList, type ProcessInfo } from "../api/ps5";
import { fetchRunningGames, sortRunningFirst } from "./runningGames";

const mockedList = vi.mocked(processList);

function proc(p: Partial<ProcessInfo>): ProcessInfo {
  return {
    pid: 0,
    name: "",
    comm: "",
    title_id: "",
    app_id: 0,
    memory_mib: 0,
    threads: 1,
    kind: "app",
    ...p,
  };
}

describe("fetchRunningGames", () => {
  beforeEach(() => mockedList.mockReset());

  it("collapses a title's many processes into one running entry", async () => {
    mockedList.mockResolvedValue({
      truncated: false,
      processes: [
        proc({ pid: 100, title_id: "CUSA00900", app_id: 0, comm: "eboot.bin" }),
        proc({ pid: 101, title_id: "CUSA00900", app_id: 42, comm: "GnmCompositor" }),
        proc({ pid: 102, title_id: "CUSA00900", app_id: 0, comm: "AudioOut" }),
      ],
    });
    const m = await fetchRunningGames("ip:9114");
    expect(m.size).toBe(1);
    // Prefers the process that carries a real app id (for a clean appKill).
    expect(m.get("CUSA00900")).toEqual({
      titleId: "CUSA00900",
      appId: 42,
      pid: 101,
    });
  });

  it("ignores the helper itself, payload, and system processes", async () => {
    mockedList.mockResolvedValue({
      truncated: false,
      processes: [
        proc({ pid: 1, title_id: "CUSA00001", kind: "app", is_self: true }),
        proc({ pid: 2, title_id: "", kind: "payload", comm: "ps5upload" }),
        proc({ pid: 3, title_id: "NPXS40000", kind: "system" }),
        proc({ pid: 4, title_id: "PPSA01234", kind: "app", app_id: 7 }),
      ],
    });
    const m = await fetchRunningGames("ip:9114");
    expect([...m.keys()]).toEqual(["PPSA01234"]);
  });

  it("returns empty when nothing game-like is running", async () => {
    mockedList.mockResolvedValue({
      truncated: false,
      processes: [proc({ pid: 2, kind: "payload", comm: "ps5upload" })],
    });
    const m = await fetchRunningGames("ip:9114");
    expect(m.size).toBe(0);
  });

  it("falls back to the pid when a title has no app id", async () => {
    mockedList.mockResolvedValue({
      truncated: false,
      processes: [proc({ pid: 55, title_id: "CUSA07842", app_id: 0 })],
    });
    const m = await fetchRunningGames("ip:9114");
    expect(m.get("CUSA07842")).toEqual({
      titleId: "CUSA07842",
      appId: 0,
      pid: 55,
    });
  });
});

const t = (titleId: string) => ({ titleId });
const ids = (rows: readonly { titleId: string }[]) =>
  rows.map((r) => r.titleId);

describe("sortRunningFirst", () => {
  it("moves a running title to the front", () => {
    const rows = [t("A"), t("B"), t("C")];
    expect(ids(sortRunningFirst(rows, new Set(["C"])))).toEqual([
      "C",
      "A",
      "B",
    ]);
  });

  it("preserves the order of everything else", () => {
    // The caller has usually already sorted by play time. A stable sort is
    // what lets this be applied on top instead of replacing that ordering.
    const rows = [t("A"), t("B"), t("C"), t("D")];
    expect(ids(sortRunningFirst(rows, new Set(["C"])))).toEqual([
      "C",
      "A",
      "B",
      "D",
    ]);
  });

  it("preserves the relative order of several running titles", () => {
    const rows = [t("A"), t("B"), t("C"), t("D")];
    expect(ids(sortRunningFirst(rows, new Set(["D", "B"])))).toEqual([
      "B",
      "D",
      "A",
      "C",
    ]);
  });

  it("returns the input untouched when nothing is running", () => {
    // The common case by far — it should not allocate a copy.
    const rows = [t("A"), t("B")];
    expect(sortRunningFirst(rows, new Set())).toBe(rows);
  });

  it("ignores running titles that are not in the list", () => {
    // The process list can name a title the installed list has not caught up
    // with yet (or a system app we never show).
    const rows = [t("A"), t("B")];
    expect(ids(sortRunningFirst(rows, new Set(["ZZ"])))).toEqual(["A", "B"]);
  });

  it("accepts the Map the Games screen actually holds", () => {
    // InstalledApps keeps title -> RunningGame so it has a kill handle; the
    // Library store keeps a bare Set. Both must work or one call site
    // silently no-ops.
    const rows = [t("A"), t("B")];
    const running = new Map([["B", { titleId: "B", appId: 1, pid: 2 }]]);
    expect(ids(sortRunningFirst(rows, running))).toEqual(["B", "A"]);
  });

  it("handles an empty list", () => {
    expect(sortRunningFirst([], new Set(["A"]))).toEqual([]);
  });
});
