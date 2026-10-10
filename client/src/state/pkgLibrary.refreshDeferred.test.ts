import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));
vi.mock("../api/ps5", () => ({
  fsListDir: vi.fn(async () => []),
  pkgMetadataConsole: vi.fn(async () => null),
}));

import { fsListDir } from "../api/ps5";
import { pkgLibraryStore } from "./pkgLibrary";

describe("pkg library refresh during an install", () => {
  it("is held and flagged instead of silently dropped", async () => {
    const store = pkgLibraryStore("10.0.0.7");
    store.setState({ installing: true });
    await store.getState().refresh("10.0.0.7");
    expect(fsListDir).not.toHaveBeenCalled();
    expect(store.getState().refreshDeferred).toBe(true);
  });

  it("is not flagged when nothing is installing", async () => {
    const store = pkgLibraryStore("10.0.0.8");
    await store.getState().refresh("10.0.0.8");
    expect(store.getState().refreshDeferred).toBe(false);
  });
});
