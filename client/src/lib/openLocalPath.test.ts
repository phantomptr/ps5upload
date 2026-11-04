import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/plugin-opener", () => ({
  revealItemInDir: vi.fn(),
}));
vi.mock("./invokeLogged", () => ({ invoke: vi.fn() }));
vi.mock("./tauriEnv", () => ({ isTauriEnv: vi.fn(() => true) }));
vi.mock("../state/logs", () => ({ log: { debug: vi.fn(), warn: vi.fn() } }));

import { openLocalPath } from "./openLocalPath";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { invoke } from "./invokeLogged";
import { isTauriEnv } from "./tauriEnv";

describe("openLocalPath", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset().mockResolvedValue(undefined);
    vi.mocked(revealItemInDir).mockReset().mockResolvedValue(undefined);
    vi.mocked(isTauriEnv).mockReturnValue(true);
  });

  it("opens the folder itself so the user lands among the files", async () => {
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(true);
    expect(invoke).toHaveBeenCalledWith("open_local_folder", { path: "/Volumes/PS5GAME" });
    expect(revealItemInDir).not.toHaveBeenCalled();
  });

  it("reveals what the shell will not open as a folder", async () => {
    // A file, or a phone with no file manager to open: revealing is the difference
    // between working and doing nothing.
    vi.mocked(invoke).mockRejectedValue(new Error("not a folder"));
    await expect(openLocalPath("/Volumes/PS5GAME/game.pkg")).resolves.toBe(true);
    expect(revealItemInDir).toHaveBeenCalledWith("/Volumes/PS5GAME/game.pkg");
  });

  it("reports failure when both routes fail", async () => {
    vi.mocked(invoke).mockRejectedValue(new Error("no"));
    vi.mocked(revealItemInDir).mockRejectedValue(new Error("no"));
    // Must be false, not a silent true — the caller shows an error.
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(false);
  });

  it("does nothing outside Tauri, where there is no file manager", async () => {
    vi.mocked(isTauriEnv).mockReturnValue(false);
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("refuses an empty path", async () => {
    await expect(openLocalPath("")).resolves.toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });
});
