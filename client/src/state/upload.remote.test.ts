import { beforeEach, describe, expect, it, vi } from "vitest";

const inspectFolder = vi.fn();
const remoteInspect = vi.fn();
const materialize = vi.fn();
const zipInspectStream = vi.fn();
vi.mock("../api/ps5", async (orig) => ({
  ...(await orig<typeof import("../api/ps5")>()),
  inspectFolder: (...a: unknown[]) => inspectFolder(...a),
  zipInspectStream: (...a: unknown[]) => zipInspectStream(...a),
}));
vi.mock("../api/remote", () => ({
  remoteApi: { inspectFolder: (...a: unknown[]) => remoteInspect(...a) },
}));
vi.mock("../lib/materialize", () => ({
  materializeRemote: (...a: unknown[]) => materialize(...a),
}));
vi.mock("../lib/tauriEnv", () => ({ isTauriEnv: () => true }));

import { useUploadStore } from "./upload";

const game = {
  meta_source: "param.json",
  title: "Test Game",
  title_id: "PPSA01234",
  path: "remote://nas-1/games/Test",
  total_size: 1000,
  file_count: 2,
};

describe("picking a source on a saved server", () => {
  beforeEach(() => {
    for (const f of [inspectFolder, remoteInspect, materialize, zipInspectStream]) f.mockReset();
    useUploadStore.setState({ source: null, detecting: false, detectError: null });
  });

  it("inspects a server folder through the engine, not local disk", async () => {
    remoteInspect.mockResolvedValue({ result: game, wrapped_hint: null });
    await useUploadStore.getState().pickFolder("remote://nas-1/games/Test");
    expect(inspectFolder).not.toHaveBeenCalled();
    expect(remoteInspect).toHaveBeenCalledWith("remote://nas-1/games/Test");
    expect(useUploadStore.getState().source).toMatchObject({
      kind: "game-folder",
      path: "remote://nas-1/games/Test",
    });
  });

  it("copies a server archive here first, then treats it like any archive", async () => {
    materialize.mockResolvedValue("/tmp/copy/game.zip");
    zipInspectStream.mockResolvedValue({ file_count: 1, total_uncompressed: 10, compressed_size: 5 });
    await useUploadStore.getState().pickFile("remote://nas-1/games/game.zip");
    expect(materialize).toHaveBeenCalledWith("remote://nas-1/games/game.zip");
    expect(zipInspectStream).toHaveBeenCalledWith("/tmp/copy/game.zip", expect.anything());
    expect(useUploadStore.getState().source).toMatchObject({ kind: "archive", path: "/tmp/copy/game.zip" });
  });
});
