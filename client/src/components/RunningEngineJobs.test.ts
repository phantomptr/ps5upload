import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { cancelOrphanJob } from "./RunningEngineJobs";

describe("cancelOrphanJob", () => {
  it("resolves null once the engine took the cancel", async () => {
    const cancel = vi.fn(async () => {});
    await expect(cancelOrphanJob("j1", cancel)).resolves.toBeNull();
    expect(cancel).toHaveBeenCalledWith("j1");
  });

  it("returns the error when the cancel failed, so the banner stays", async () => {
    const cancel = vi.fn(async () => {
      throw new Error("engine unreachable");
    });
    await expect(cancelOrphanJob("j1", cancel)).resolves.toBe("engine unreachable");
  });
});
