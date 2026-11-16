import { beforeEach, describe, expect, it, vi } from "vitest";

// The package header read: one part, or a split package's part.
let parts = 1;
const invoke = vi.fn(async (cmd: string) => {
  if (cmd !== "pkg_metadata_split") throw new Error(`unexpected ${cmd}`);
  return {
    head: { content_id: "UP0000-PPSA01234_00-GAME000000000000", title: "Game", category: "gd", platform: "PS5" },
    parts: Array.from({ length: parts }, () => ({})),
    total_size: 10,
  };
});
vi.mock("../lib/invokeLogged", () => ({ invoke: (cmd: string) => invoke(cmd) }));

import { PKG_CHOICE_KEY } from "../lib/pkgUploadChoice";
import { useUploadStore } from "./upload";
import { inspectSource, setBatchInspector, useUploadBatch } from "./uploadBatch";

// vitest's node env has no `window`; safeStorage reads `window.localStorage`. An in-memory stub,
// set after the imports so the modules load as they do in node.
const stored = new globalThis.Map<string, string>();
const localStorage = {
  getItem: (k: string) => (stored.has(k) ? (stored.get(k) as string) : null),
  setItem: (k: string, v: string) => void stored.set(k, String(v)),
  removeItem: (k: string) => void stored.delete(k),
};
(globalThis as { window?: unknown }).window = { localStorage };

const flush = () => new Promise((r) => setTimeout(r, 0));

describe("a .pkg picked on Upload", () => {
  beforeEach(() => {
    parts = 1;
    invoke.mockClear();
    localStorage.removeItem(PKG_CHOICE_KEY);
    useUploadStore.setState({ source: null, detecting: false, detectError: null, pkgChoice: "install" });
  });

  it("installs by default: a pkg source with its header", async () => {
    await useUploadStore.getState().pickFile("/d/Game.pkg");
    const s = useUploadStore.getState();
    expect(s.source?.kind).toBe("pkg");
    expect(s.source?.pkgInfo?.title).toBe("Game");
    expect(s.detectError).toBeNull();
  });

  it("copied, is a plain file source and its header is not read", async () => {
    useUploadStore.setState({ pkgChoice: "copy" });
    await useUploadStore.getState().pickFile("/d/Game.pkg");
    expect(useUploadStore.getState().source?.kind).toBe("file");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("refuses a split package's part for install, and copies it", async () => {
    parts = 3;
    await useUploadStore.getState().pickFile("/d/Game_0.pkg");
    expect(useUploadStore.getState().detectError).toMatch(/split package/);
    await useUploadStore.getState().setPkgChoice("copy");
    const s = useUploadStore.getState();
    expect(s.source).toMatchObject({ kind: "file", path: "/d/Game_0.pkg" });
    expect(s.detectError).toBeNull();
  });

  it("remembers the choice and picks the package again that way", async () => {
    await useUploadStore.getState().pickFile("/d/Game.pkg");
    await useUploadStore.getState().setPkgChoice("copy");
    expect(localStorage.getItem(PKG_CHOICE_KEY)).toBe("copy");
    expect(useUploadStore.getState().source?.kind).toBe("file");
    await useUploadStore.getState().setPkgChoice("install");
    expect(localStorage.getItem(PKG_CHOICE_KEY)).toBe("install");
    expect(useUploadStore.getState().source?.kind).toBe("pkg");
  });

  it("leaves other files alone", async () => {
    useUploadStore.setState({ pkgChoice: "copy" });
    await useUploadStore.getState().pickFile("/d/notes.txt");
    expect(useUploadStore.getState().source?.kind).toBe("file");
    await useUploadStore.getState().setPkgChoice("install");
    expect(useUploadStore.getState().source?.kind).toBe("file");
  });
});

describe("a .pkg in the review list", () => {
  beforeEach(() => {
    parts = 1;
    invoke.mockClear();
    localStorage.removeItem(PKG_CHOICE_KEY);
    useUploadStore.setState({ pkgChoice: "install" });
    setBatchInspector(inspectSource);
    useUploadBatch.getState().clear();
  });

  it("inspectSource routes by the choice, and lets a split part through only when copying", async () => {
    expect((await inspectSource("/d/Game.pkg", false, null)).kind).toBe("pkg");
    expect((await inspectSource("/d/Game.pkg", false, null, "copy")).kind).toBe("file");
    parts = 2;
    await expect(inspectSource("/d/Game_0.pkg", false, null, "install")).rejects.toThrow(/split package/);
    expect((await inspectSource("/d/Game_0.pkg", false, null, "copy")).kind).toBe("file");
  });

  it("new rows take the remembered choice; other rows have none", async () => {
    localStorage.setItem(PKG_CHOICE_KEY, "copy");
    useUploadBatch.getState().add([
      { path: "/d/A.pkg", isDir: false },
      { path: "/d/b.txt", isDir: false },
    ]);
    await flush();
    const [a, b] = useUploadBatch.getState().rows;
    expect(a).toMatchObject({ pkgChoice: "copy", status: "ready" });
    expect(a.source?.kind).toBe("file");
    expect(b.pkgChoice).toBeNull();
  });

  it("a row's choice is remembered, re-reads the row, and brings back a refused split part", async () => {
    parts = 2;
    useUploadBatch.getState().add([{ path: "/d/Game_0.pkg", isDir: false }]);
    await flush();
    let row = useUploadBatch.getState().rows[0];
    expect(row).toMatchObject({ pkgChoice: "install", status: "error", include: false });
    useUploadBatch.getState().setPkgChoice(row.id, "copy");
    await flush();
    row = useUploadBatch.getState().rows[0];
    expect(row).toMatchObject({ pkgChoice: "copy", status: "ready", include: true, error: null });
    expect(row.source?.kind).toBe("file");
    expect(localStorage.getItem(PKG_CHOICE_KEY)).toBe("copy");
    expect(useUploadStore.getState().pkgChoice).toBe("copy");
  });
});
