import { describe, expect, it } from "vitest";

import { filterInstalledTitles } from "./installedFilter";
import type { InstalledTitle } from "../api/ps5";

function title(over: Partial<InstalledTitle>): InstalledTitle {
  return {
    titleId: "CUSA00000",
    titleName: "Untitled",
    origin: "registered",
    imageBacked: false,
    source: "",
    system: false,
    ...over,
  } as InstalledTitle;
}

const BLOODBORNE = title({
  titleId: "CUSA00900",
  titleName: "Bloodborne",
  source: "/mnt/ext1/games/Bloodborne",
});
const DEAD_SPACE = title({
  titleId: "PPSA01342",
  titleName: "Dead Space",
  source: "/mnt/usb0/DeadSpace.exfat",
});
const ALL = [BLOODBORNE, DEAD_SPACE];

describe("filterInstalledTitles", () => {
  it("matches on the game's name, case-insensitively", () => {
    expect(filterInstalledTitles(ALL, "bloodborne")).toEqual([BLOODBORNE]);
    expect(filterInstalledTitles(ALL, "BLOOD")).toEqual([BLOODBORNE]);
  });

  it("matches on the game code", () => {
    expect(filterInstalledTitles(ALL, "PPSA01342")).toEqual([DEAD_SPACE]);
  });

  it("matches a game code typed in lower case", () => {
    expect(filterInstalledTitles(ALL, "ppsa01342")).toEqual([DEAD_SPACE]);
  });

  it("matches a game code typed with a dash", () => {
    // People copy codes out of filenames and forum posts, where they are
    // punctuated every which way. Requiring the exact spelling makes the
    // code search useless precisely when you are pasting one in.
    expect(filterInstalledTitles(ALL, "PPSA-01342")).toEqual([DEAD_SPACE]);
  });

  it("matches a game code typed with a space", () => {
    expect(filterInstalledTitles(ALL, "ppsa 01342")).toEqual([DEAD_SPACE]);
  });

  it("matches on the source path", () => {
    expect(filterInstalledTitles(ALL, "usb0")).toEqual([DEAD_SPACE]);
  });

  it("requires every word to match, not just one", () => {
    expect(filterInstalledTitles(ALL, "dead space")).toEqual([DEAD_SPACE]);
    // "bloodborne" and "space" share no single title.
    expect(filterInstalledTitles(ALL, "bloodborne space")).toEqual([]);
  });

  it("ignores runs of whitespace around and between words", () => {
    expect(filterInstalledTitles(ALL, "  dead   space  ")).toEqual([DEAD_SPACE]);
  });

  it("returns the very same array when the query is empty", () => {
    // Referential identity, so the memoised group derivations downstream
    // can skip re-filtering entirely when nobody is searching.
    expect(filterInstalledTitles(ALL, "")).toBe(ALL);
    expect(filterInstalledTitles(ALL, "   ")).toBe(ALL);
  });

  it("returns nothing when nothing matches", () => {
    expect(filterInstalledTitles(ALL, "elden ring")).toEqual([]);
  });

  it("does not match everything when the query is only punctuation", () => {
    // Stripping punctuation to compare codes leaves an empty needle, and
    // every string contains the empty string — so a stray "-" would show
    // the whole library and look like the search was ignored.
    expect(filterInstalledTitles(ALL, "---")).toEqual([]);
  });

  it("does not mutate or reorder the input", () => {
    const input = [...ALL];
    filterInstalledTitles(input, "a");
    expect(input).toEqual(ALL);
  });
});
