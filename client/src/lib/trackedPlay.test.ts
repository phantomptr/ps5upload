import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { playFor, trackedPlayMap } from "./trackedPlay";

const entry = (title_id: string, total_seconds: number, last_seen_ts: number, last_launch_ts = 0) => ({
  title_id,
  launches: 1,
  total_seconds,
  last_launch_ts,
  last_seen_ts,
  session_active: false,
});

const local = {
  byHost: { "192.168.1.100": { PPSA00001: 50 } },
  lastSeenByHost: { "192.168.1.100": { PPSA00001: 1_000 } },
};

describe("tracked play time", () => {
  it("maps the helper's activity table by title, seconds to ms", () => {
    const m = trackedPlayMap([entry("PPSA00001", 3600, 1_700_000_000), entry("CUSA00002", 60, 0, 1_600_000_000)]);
    expect(m.get("PPSA00001")).toEqual({ seconds: 3600, lastSeenMs: 1_700_000_000_000 });
    // No last-seen yet: the launch time stands in.
    expect(m.get("CUSA00002")?.lastSeenMs).toBe(1_600_000_000_000);
  });

  it("reads the helper's numbers when it answered, even over the app's own count", () => {
    const tracked = trackedPlayMap([entry("PPSA00001", 3600, 1_700_000_000)]);
    expect(playFor(tracked, local, "192.168.1.100", "PPSA00001").seconds).toBe(3600);
    // A title the helper never saw is unplayed, not the app's count.
    expect(playFor(tracked, local, "192.168.1.100", "PPSA00009").seconds).toBeUndefined();
  });

  it("falls back to the app's own count when the helper has no tracker", () => {
    expect(playFor(null, local, "192.168.1.100", "PPSA00001")).toEqual({ seconds: 50, lastSeenMs: 1_000 });
  });
});
