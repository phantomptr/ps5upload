import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import { setBatchInspector, useUploadBatch } from "./uploadBatch";

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("the Upload batch", () => {
  let running = 0;
  let peak = 0;
  let release: (() => void)[] = [];
  beforeEach(() => {
    running = 0;
    peak = 0;
    release = [];
    useUploadBatch.getState().clear();
    setBatchInspector(async (path, isDir, password) => {
      running++;
      peak = Math.max(peak, running);
      await new Promise<void>((r) => release.push(r));
      running--;
      if (path.endsWith(".rar") && password !== "pw") throw new Error("rar_password_required");
      return { kind: isDir ? "game-folder" : "file", path, meta: null, wrappedHint: null, zipInfo: null };
    });
  });

  it("inspects at most four at a time and ignores a path added twice", async () => {
    useUploadBatch.getState().add(
      Array.from({ length: 6 }, (_, i) => ({ path: `/g/${i}`, isDir: true })).concat([{ path: "/g/0", isDir: true }]),
    );
    expect(useUploadBatch.getState().rows).toHaveLength(6);
    await flush();
    expect(peak).toBe(4);
    while (release.length) {
      release.shift()!();
      await flush();
    }
    expect(useUploadBatch.getState().rows.every((r) => r.status === "ready")).toBe(true);
  });

  it("asks an encrypted archive for its password, then inspects it again", async () => {
    useUploadBatch.getState().add([{ path: "/dl/g.rar", isDir: false }]);
    await flush();
    release.shift()!();
    await flush();
    const row = useUploadBatch.getState().rows[0];
    expect(row.status).toBe("needs-password");
    useUploadBatch.getState().setPassword(row.id, "pw");
    await flush();
    release.shift()!();
    await flush();
    expect(useUploadBatch.getState().rows[0]).toMatchObject({ status: "ready", password: "pw" });
  });

  it("keeps an inspect failure on its row, and a removed row's late result is dropped", async () => {
    setBatchInspector(async () => {
      throw new Error("unreadable");
    });
    useUploadBatch.getState().add([{ path: "/x", isDir: false }]);
    await flush();
    expect(useUploadBatch.getState().rows[0]).toMatchObject({ status: "error", error: "unreadable" });
    useUploadBatch.getState().remove(useUploadBatch.getState().rows[0].id);
    expect(useUploadBatch.getState().rows).toHaveLength(0);
  });

  it("gives each archive the default its listing says, and asks when it can't tell (#645)", async () => {
    const zipInfo = (names: string[] | null) => ({
      file_count: 0, total_uncompressed: 0, compressed_size: 0, title: null, title_id: null,
      content_id: null, application_category_type: null, game_root: null,
      entry_names: names, entry_names_complete: true,
    });
    setBatchInspector(async (path) => ({
      kind: "archive",
      path,
      meta: null,
      wrappedHint: null,
      zipInfo: path.includes("y2jb")
        ? zipInfo(["y2jb/payload.js", "y2jb/main.elf"])
        : path.includes("game")
          ? zipInfo(["PPSA01234/sce_sys/icon0.png", "PPSA01234/eboot.bin"])
          : null,
    }));
    useUploadBatch.getState().add([
      { path: "/dl/y2jb.zip", isDir: false },
      { path: "/dl/game.7z", isDir: false },
      { path: "/dl/unknown.zip", isDir: false },
    ]);
    await flush();
    const choice = () => useUploadBatch.getState().rows.map((r) => r.archiveChoice);
    expect(choice()).toEqual(["as-is", "unpack", null]);
    // The user's answer stays, also through a later re-inspection.
    const unknown = useUploadBatch.getState().rows[2];
    useUploadBatch.getState().setArchiveChoice(unknown.id, "as-is");
    useUploadBatch.getState().setPassword(unknown.id, "pw");
    await flush();
    expect(choice()).toEqual(["as-is", "unpack", "as-is"]);
  });
});
