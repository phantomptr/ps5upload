import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/plugin-opener", () => ({
  openPath: vi.fn(),
  revealItemInDir: vi.fn(),
}));
vi.mock("./tauriEnv", () => ({ isTauriEnv: vi.fn(() => true) }));
vi.mock("../state/logs", () => ({ log: { warn: vi.fn() } }));

import { openLocalPath } from "./openLocalPath";
import { openPath, revealItemInDir } from "@tauri-apps/plugin-opener";
import { isTauriEnv } from "./tauriEnv";

describe("openLocalPath", () => {
  beforeEach(() => {
    vi.mocked(openPath).mockReset().mockResolvedValue(undefined);
    vi.mocked(revealItemInDir).mockReset().mockResolvedValue(undefined);
    vi.mocked(isTauriEnv).mockReturnValue(true);
  });

  it("opens the folder itself so the user lands among the files", async () => {
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(true);
    expect(openPath).toHaveBeenCalledWith("/Volumes/PS5GAME");
    expect(revealItemInDir).not.toHaveBeenCalled();
  });

  it("falls back to revealing when openPath is refused", async () => {
    // openPath needs a permission the default set does not grant, so the
    // fallback is the difference between working and doing nothing.
    vi.mocked(openPath).mockRejectedValue(new Error("not allowed"));
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(true);
    expect(revealItemInDir).toHaveBeenCalledWith("/Volumes/PS5GAME");
  });

  it("reports failure when both routes fail", async () => {
    vi.mocked(openPath).mockRejectedValue(new Error("no"));
    vi.mocked(revealItemInDir).mockRejectedValue(new Error("no"));
    // Must be false, not a silent true — the caller shows an error.
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(false);
  });

  it("does nothing outside Tauri, where there is no file manager", async () => {
    vi.mocked(isTauriEnv).mockReturnValue(false);
    await expect(openLocalPath("/Volumes/PS5GAME")).resolves.toBe(false);
    expect(openPath).not.toHaveBeenCalled();
  });

  it("refuses an empty path", async () => {
    await expect(openLocalPath("")).resolves.toBe(false);
    expect(openPath).not.toHaveBeenCalled();
  });
});
