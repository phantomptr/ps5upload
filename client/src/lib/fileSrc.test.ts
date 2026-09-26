import { describe, expect, it, vi, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  convertFileSrc: vi.fn((p: string) => `asset://${p}`),
}));
vi.mock("./tauriEnv", () => ({ isTauriEnv: vi.fn() }));

import { localFileSrc } from "./fileSrc";
import { isTauriEnv } from "./tauriEnv";
import { convertFileSrc } from "@tauri-apps/api/core";

describe("localFileSrc", () => {
  beforeEach(() => vi.mocked(convertFileSrc).mockClear());

  it("converts a path inside Tauri", () => {
    vi.mocked(isTauriEnv).mockReturnValue(true);
    expect(localFileSrc("/tmp/icon0.png")).toBe("asset:///tmp/icon0.png");
  });

  it("returns null in the browser instead of throwing", () => {
    // The #271 regression: convertFileSrc reads a Tauri global that does
    // not exist in the web UI, so calling it there threw and took the
    // upload screen down as soon as a folder was picked.
    vi.mocked(isTauriEnv).mockReturnValue(false);
    expect(localFileSrc("/tmp/icon0.png")).toBeNull();
    expect(convertFileSrc).not.toHaveBeenCalled();
  });

  it("returns null for an absent path without calling into Tauri", () => {
    vi.mocked(isTauriEnv).mockReturnValue(true);
    expect(localFileSrc(null)).toBeNull();
    expect(localFileSrc(undefined)).toBeNull();
    expect(localFileSrc("")).toBeNull();
    expect(convertFileSrc).not.toHaveBeenCalled();
  });
});
