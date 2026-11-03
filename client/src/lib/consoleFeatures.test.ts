import { describe, expect, it, vi } from "vitest";

const exists = new Set<string>();
vi.mock("../api/ps5", () => ({
  fsPathExists: vi.fn(async (_addr: string, p: string) => exists.has(p)),
  cheatsStatus: vi.fn(async () => ({
    enabled: true,
    patches_last: 2,
    patches_total: 5,
    game_running: true,
    game_title_id: "PPSA01234",
    game_pid: 77,
    held: false,
  })),
}));
vi.mock("./rpBlockers", () => ({
  readConsoleText: vi.fn(async () => "enabled=1\nmode=apply\nhold_s=0\n"),
}));
vi.mock("../api/consoleShares", () => ({
  listConsoleShares: vi.fn(async () => [
    { id: "a", name: "NAS", host: "192.168.1.5", port: 445, share: "Games", user: "bob", domain: "HOME", start_path: "/ps5", has_password: true },
  ]),
}));

import { collectConsoleFeatures, hushSummary, type Step } from "./consoleFeatures";

const step: Step = async (_name, fn) => {
  try {
    return await fn();
  } catch {
    return null;
  }
};

describe("console add-ons for a bug report", () => {
  it("Hush: installed, hold mode, and whether its patch is in place", async () => {
    exists.clear();
    exists.add("/data/hush");
    exists.add("/data/hush/state.txt");
    expect(await hushSummary("ps5:9114")).toEqual({
      installed: true,
      enabled: true,
      mode: "apply",
      hold_s: 0,
      state_txt_present: true,
    });
  });

  it("states and names only: no share user, domain, folder or password", async () => {
    exists.clear();
    const f = await collectConsoleFeatures("192.168.1.10", "192.168.1.10:9114", "192.168.1.10:9113", step);
    expect(f.hush?.installed).toBe(false);
    expect(f.cheats).toMatchObject({ enabled: true, game_running: true, patches_total: 5, held: false });
    expect(f.console_shares).toEqual([{ host: "192.168.1.5", share: "Games", has_user: true, has_password: true }]);
    const text = JSON.stringify(f);
    for (const s of ["bob", "HOME", "/ps5", "game_pid"]) expect(text).not.toContain(s);
  });
});
