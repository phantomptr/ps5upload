import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

import type { LibraryEntry } from "../api/ps5";
import { libraryForHost, useLibraryStore } from "./library";

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
