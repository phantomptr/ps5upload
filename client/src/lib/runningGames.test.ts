import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("../api/ps5", () => ({ processList: vi.fn() }));

import { processList, type ProcessInfo } from "../api/ps5";
import { fetchRunningGames } from "./runningGames";

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
