import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const set = vi.fn();
vi.mock("../api/saveBackup", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../api/saveBackup")>()),
  saveBackupApi: { set: (...a: unknown[]) => set(...a), view: vi.fn() },
}));

import { backupHosts, clampKeep, legacySettings, numberBoxLeft, startSaveBackupSync } from "./autoSaveBackup";
import { useRosterStore } from "./roster";
import { stampLabel } from "../screens/GameHub/SaveBackupsList";

describe("automatic save backup (engine-run)", () => {
  it("hands over an older version's settings only when a folder was set", () => {
    expect(legacySettings(JSON.stringify({ enabled: true, dir: "/Users/me/saves", keep: 99 }))).toEqual({
      enabled: true,
      dir: "/Users/me/saves",
      keep: 50,
    });
    expect(legacySettings(JSON.stringify({ enabled: true, dir: " " }))).toBeNull();
    expect(legacySettings(null)).toBeNull();
    expect(legacySettings("not json")).toBeNull();
  });

  it("gives the engine every console once, by bare address", () => {
    expect(backupHosts([{ host: "192.168.1.9" }, { host: "192.168.1.9:9021" }, { host: "10.0.0.2" }, { host: "" }])).toEqual([
      "10.0.0.2",
      "192.168.1.9",
    ]);
  });

  it("keeps the version count between 1 and 50", () => {
    expect(clampKeep(0)).toBe(1);
    expect(clampKeep(80)).toBe(50);
    expect(clampKeep("x")).toBe(5);
  });

  it("an emptied Versions to keep box saves nothing and goes back, where it saved 1", () => {
    // Selected to retype, deleted, then a click elsewhere.
    expect(numberBoxLeft("", 5, clampKeep)).toEqual({ save: null, show: "5" });
    expect(numberBoxLeft("  ", 5, clampKeep)).toEqual({ save: null, show: "5" });
    expect(numberBoxLeft("abc", 5, clampKeep)).toEqual({ save: null, show: "5" });
    // A real choice is saved; the same number is not sent again.
    expect(numberBoxLeft("3", 5, clampKeep)).toEqual({ save: 3, show: "3" });
    expect(numberBoxLeft("5", 5, clampKeep)).toEqual({ save: null, show: "5" });
    // Out of range: the nearest allowed, and the box says so (999 stayed on screen at 50).
    expect(numberBoxLeft("999", 50, clampKeep)).toEqual({ save: null, show: "50" });
    expect(numberBoxLeft("999", 5, clampKeep)).toEqual({ save: 50, show: "50" });
    expect(numberBoxLeft("0", 5, clampKeep)).toEqual({ save: 1, show: "1" });
  });

  it("labels a backup's time in UTC", () => {
    expect(stampLabel("2026-10-24_101530")).toBe("2026-10-24 10:15 UTC");
    expect(stampLabel("odd")).toBe("odd");
  });
});

describe("giving the engine the console list", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    set.mockReset();
    useRosterStore.setState({ profiles: [{ host: "10.0.0.2" }] as never });
  });
  afterEach(() => vi.useRealTimers());

  it("tries again by itself when the engine was not up, then stops", async () => {
    set.mockRejectedValueOnce(new Error("engine not up")).mockRejectedValueOnce(new Error("engine not up")).mockResolvedValue({});
    const stop = startSaveBackupSync();
    await vi.advanceTimersByTimeAsync(0);
    expect(set).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(set).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(20_000);
    expect(set).toHaveBeenCalledTimes(3);
    expect(set).toHaveBeenLastCalledWith({ add_hosts: ["10.0.0.2"], remove_hosts: [] });
    await vi.advanceTimersByTimeAsync(600_000);
    expect(set).toHaveBeenCalledTimes(3);
    stop();
  });

  it("stops trying when stopped", async () => {
    set.mockRejectedValue(new Error("engine not up"));
    const stop = startSaveBackupSync();
    await vi.advanceTimersByTimeAsync(0);
    stop();
    const calls = set.mock.calls.length;
    await vi.advanceTimersByTimeAsync(600_000);
    expect(set).toHaveBeenCalledTimes(calls);
  });
});
