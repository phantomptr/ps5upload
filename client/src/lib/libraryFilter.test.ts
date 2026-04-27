import { describe, expect, it } from "vitest";

import type { LibraryEntry } from "../api/ps5";
import { filterLibraryEntries } from "./libraryFilter";

const make = (over: Partial<LibraryEntry> = {}): LibraryEntry => ({
  kind: "game",
  name: "Sample",
  path: "/data/etaHEN/games/Sample",
  volume: "/data",
  scope: "etaHEN/games",
  size: 0,
  ...over,
});

const ENTRIES: LibraryEntry[] = [
  make({
    name: "Dead Space",
    path: "/mnt/ext1/etaHEN/games/Dead Space",
    volume: "/mnt/ext1",
    titleId: "PPSA03845",
  }),
  make({
    name: "Astro's Playroom",
    path: "/data/homebrew/Astro",
    volume: "/data",
    scope: "homebrew",
    titleId: "PPSA01325",
  }),
  make({
    kind: "image",
    name: "horizon-zero-dawn.exfat",
    path: "/mnt/ext1/horizon-zero-dawn.exfat",
    volume: "/mnt/ext1",
    titleId: null,
    imageFormat: "exfat",
    scope: "exfat",
  }),
];

describe("filterLibraryEntries", () => {
  it("returns the input array reference when query is empty", () => {
    expect(filterLibraryEntries(ENTRIES, "")).toBe(ENTRIES);
    expect(filterLibraryEntries(ENTRIES, "   ")).toBe(ENTRIES);
  });

  it("matches by name, case-insensitive", () => {
    const r = filterLibraryEntries(ENTRIES, "dead");
    expect(r).toHaveLength(1);
    expect(r[0].name).toBe("Dead Space");
  });

  it("matches by titleId", () => {
    const r = filterLibraryEntries(ENTRIES, "PPSA03845");
    expect(r).toHaveLength(1);
    expect(r[0].name).toBe("Dead Space");
  });

  it("matches by titleId case-insensitive", () => {
    const r = filterLibraryEntries(ENTRIES, "ppsa03845");
    expect(r).toHaveLength(1);
  });

  it("matches by path fragment (volume name)", () => {
    const r = filterLibraryEntries(ENTRIES, "ext1");
    expect(r.map((e) => e.name).sort()).toEqual([
      "Dead Space",
      "horizon-zero-dawn.exfat",
    ]);
  });

  it("matches by scope", () => {
    const r = filterLibraryEntries(ENTRIES, "homebrew");
    expect(r).toHaveLength(1);
    expect(r[0].name).toBe("Astro's Playroom");
  });

  it("AND-matches multi-word queries against combined fields", () => {
    // "dead" matches name; "ext1" matches path. Only Dead Space has both.
    const r = filterLibraryEntries(ENTRIES, "dead ext1");
    expect(r).toHaveLength(1);
    expect(r[0].name).toBe("Dead Space");
  });

  it("returns empty when no entry contains every token", () => {
    const r = filterLibraryEntries(ENTRIES, "dead astro");
    expect(r).toEqual([]);
  });

  it("collapses runs of whitespace in the query", () => {
    const r = filterLibraryEntries(ENTRIES, "dead    space");
    expect(r).toHaveLength(1);
  });

  it("matches images alongside games", () => {
    const r = filterLibraryEntries(ENTRIES, "horizon");
    expect(r).toHaveLength(1);
    expect(r[0].kind).toBe("image");
  });

  it("handles a missing titleId gracefully", () => {
    // The disk-image row has titleId=null; query for any non-existent
    // title should not throw and should not match it spuriously.
    const r = filterLibraryEntries(ENTRIES, "PPSA00000");
    expect(r).toEqual([]);
  });
});
