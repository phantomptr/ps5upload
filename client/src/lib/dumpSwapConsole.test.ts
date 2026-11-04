import { beforeEach, describe, expect, it, vi } from "vitest";

const api = vi.hoisted(() => ({
  fsPathPresent: vi.fn(),
  fsMkdir: vi.fn(),
  fsDelete: vi.fn(),
  fsListDir: vi.fn(),
  fsReadPreview: vi.fn(),
  fsWriteText: vi.fn(),
  fsMove: vi.fn(),
  appUnregister: vi.fn(),
}));
vi.mock("../api/ps5", async (orig) => ({
  isPathMissingError: (await orig<typeof import("../api/ps5")>()).isPathMissingError,
  ...api,
}));

import { consoleSwapDeps } from "./dumpSwapConsole";

// The swap decides about moving and deleting a game from these answers: "the console could not
// answer" has to stay apart from "not there" all the way down.
describe("consoleSwapDeps", () => {
  const busy = new Error("mgmt read timed out");
  let deps: ReturnType<typeof consoleSwapDeps>;
  beforeEach(() => {
    for (const f of Object.values(api)) f.mockReset();
    deps = consoleSwapDeps("10.0.0.5");
  });

  it("rejects from exists, readText, remove and mkdir when the console cannot answer", async () => {
    api.fsPathPresent.mockRejectedValue(busy);
    await expect(deps.exists("/a")).rejects.toBe(busy);
    await expect(deps.readText("/a")).rejects.toBe(busy);
    await expect(deps.remove("/a")).rejects.toBe(busy);
    await expect(deps.mkdir("/a")).rejects.toBe(busy);
    await expect(deps.isRegistered("PPSA00001")).rejects.toBe(busy);
    expect(api.fsDelete).not.toHaveBeenCalled();
    expect(api.fsMkdir).not.toHaveBeenCalled();
  });

  it("acts on a real answer", async () => {
    api.fsPathPresent.mockResolvedValue(false);
    await expect(deps.readText("/a")).resolves.toBeNull();
    await deps.remove("/a");
    expect(api.fsDelete).not.toHaveBeenCalled();
    await deps.mkdir("/a");
    expect(api.fsMkdir).toHaveBeenCalledTimes(1);

    api.fsPathPresent.mockResolvedValue(true);
    await deps.remove("/a");
    expect(api.fsDelete).toHaveBeenCalledTimes(1);
    api.fsReadPreview.mockResolvedValue({ base64: btoa("hello") });
    await expect(deps.readText("/a")).resolves.toBe("hello");
  });

  it("lists a missing folder as empty, and rejects any other failure", async () => {
    api.fsListDir.mockRejectedValueOnce(new Error("fs_list_dir_opendir_errno_2"));
    await expect(deps.list("/a")).resolves.toEqual([]);
    api.fsListDir.mockRejectedValueOnce(busy);
    await expect(deps.list("/a")).rejects.toBe(busy);
  });

  it("fails a refused write", async () => {
    api.fsWriteText.mockResolvedValue({ ok: false, err: "fs_write_denied" });
    await expect(deps.writeText("/a", "x")).rejects.toThrow("fs_write_denied");
  });
});
