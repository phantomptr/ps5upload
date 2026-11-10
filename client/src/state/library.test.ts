import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { LibraryEntry } from "../api/ps5";
import { libraryForHost, linkRegistered, registeredForImage, useLibraryStore } from "./library";

const HOST = "10.0.0.7";
const game = { kind: "game", path: "/data/homebrew/G", name: "G" } as unknown as LibraryEntry;
const set = (entries: LibraryEntry[], incomplete?: boolean) =>
  useLibraryStore.getState().setData(HOST, entries, new Map(), [], new Map(), undefined, incomplete);
const entries = () => libraryForHost(useLibraryStore.getState(), HOST).entries;

describe("the library list", () => {
  beforeEach(() => useLibraryStore.setState({ byHost: {} }));

  it("empties when a scan that read everything finds nothing: the last game was deleted", () => {
    set([game]);
    set([]);
    expect(entries()).toEqual([]);
  });

  it("keeps what it had when part of the console could not be read", () => {
    set([game]);
    set([], true);
    expect(entries()).toEqual([game]);
  });
});

describe("linking ShadowMount+ images to their titles (#791)", () => {
  const smp = (name: string) => `/mnt/shadowmnt/${name}`;
  const title = (titleId: string, source: string) => ({ titleId, titleName: `Game ${titleId}`, source });

  it("links the image that is mounted at a title's mount point, by the mount", () => {
    const { registeredBySource } = linkRegistered(
      [title("PPSA01234", smp("Some Game"))],
      [{ path: smp("Some Game"), source_image: "/mnt/ext0/games/some-game.ffpkg" }],
    );
    expect(registeredForImage(registeredBySource, "/mnt/ext0/games/some-game.ffpkg")?.titleId).toBe("PPSA01234");
  });

  it("does not link a second copy by the title ID in its name while the game is mounted from another", () => {
    const { registeredBySource } = linkRegistered(
      [title("PPSA01234", smp("Some Game"))],
      [{ path: smp("Some Game"), source_image: "/mnt/ext0/games/PPSA01234-v1.05.ffpkg" }],
    );
    expect(registeredForImage(registeredBySource, "/mnt/ext0/games/PPSA01234-v1.05.ffpkg")?.titleId).toBe("PPSA01234");
    expect(registeredForImage(registeredBySource, "/mnt/ext0/old/PPSA01234-v1.00.ffpkg")).toBeNull();
  });

  it("falls back to the title ID in the name when the mount is not seen", () => {
    const { registeredBySource } = linkRegistered([title("PPSA01234", smp("Some Game"))], []);
    expect(registeredForImage(registeredBySource, "/mnt/ext0/games/ppsa01234.exfat")?.titleId).toBe("PPSA01234");
  });

  it("never links by title ID to a game installed from a folder", () => {
    const { registeredBySource } = linkRegistered([title("PPSA01234", "/data/homebrew/PPSA01234")], []);
    expect(registeredForImage(registeredBySource, "/mnt/ext0/PPSA01234.ffpkg")).toBeNull();
  });
});
