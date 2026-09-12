import { describe, expect, it } from "vitest";
import {
  cheatFilterOptions,
  filterCheatEntries,
  isSupportedCheatFormat,
  isUsableGameTitle,
  namesFromRepoEntries,
  resolveCheatName,
  NO_CHEAT_FILTERS,
} from "./cheatBrowse";
import type { CheatRepoEntry } from "../api/ps5";

const e = (
  filename: string,
  game_title: string,
  format: string,
  title_id = "",
  game_version = "",
): CheatRepoEntry => ({ filename, game_title, format, repo_id: "etahen", title_id, game_version });

/** Rows taken verbatim from the published etaHEN / GoldHEN indexes. */
const real: CheatRepoEntry[] = [
  e("CUSA15438_01.00.json", "Zombieland: Double Tap - Road Trip", "json", "CUSA15438", "01.00"),
  e("CUSA09193_01.05_2.json", "RESIDENT EVIL 3", "json", "CUSA09193", "01.05"),
  e("CUSA34394_05.01.json", "God of War Ragnarok", "json", "CUSA34394", "05.01"),
  e("CUSA14936_01.03.json", "EP4800-CUSA14936_00-0000111122223334", "json", "CUSA14936", "01.03"),
  e("CUSA00556_01.11.shn", "The Last of Us Remastered", "shn", "CUSA00556", "01.11"),
  e("readme.md", "", "mc4"),
];

describe("isUsableGameTitle", () => {
  it("rejects a content id masquerading as a name", () => {
    // Real index rows carry these. Showing one is worse than showing the
    // title id, because it looks like data rather than a gap.
    expect(isUsableGameTitle("EP4800-CUSA14936_00-0000111122223334")).toBe(false);
    expect(isUsableGameTitle("CUSA15438")).toBe(false);
    expect(isUsableGameTitle("  ")).toBe(false);
    expect(isUsableGameTitle(undefined)).toBe(false);
  });

  it("accepts a real name", () => {
    expect(isUsableGameTitle("God of War Ragnarok")).toBe(true);
    expect(isUsableGameTitle("[PROTOTYPE®2]")).toBe(true);
  });
});

describe("resolveCheatName", () => {
  const installed = new Map([["CUSA00556", "The Last of Us™ Remastered"]]);
  const repo = namesFromRepoEntries(real);

  it("prefers the cheat file's own name", () => {
    expect(resolveCheatName("CUSA00556", {
      fromCheatFile: "TLOU (cheat author name)", installed, fromRepoIndex: repo,
    })).toBe("TLOU (cheat author name)");
  });

  it("falls back to the installed game's name", () => {
    expect(resolveCheatName("CUSA00556", { installed, fromRepoIndex: repo }))
      .toBe("The Last of Us™ Remastered");
  });

  it("uses the repo index for a game that is NOT installed", () => {
    // The reported bug: a downloaded cheat for a game you do not have shows
    // a bare title id, because the only name source was the installed list.
    expect(resolveCheatName("CUSA34394", { installed, fromRepoIndex: repo }))
      .toBe("God of War Ragnarok");
  });

  it("falls through to the title id rather than showing junk", () => {
    expect(resolveCheatName("CUSA14936", { installed, fromRepoIndex: repo }))
      .toBe("CUSA14936");
    expect(resolveCheatName("CUSA99999", {})).toBe("CUSA99999");
  });
});

describe("cheatFilterOptions", () => {
  it("offers only what is actually present, newest version first", () => {
    const o = cheatFilterOptions(real);
    expect(o.formats).toEqual(["json", "mc4", "shn"]);
    expect(o.versions[0]).toBe("05.01");
    expect(o.versions).toContain("01.00");
    // A filename that breaks the convention contributes no version.
    expect(o.versions).not.toContain("");
  });
});

describe("filterCheatEntries", () => {
  const installedIds = new Set(["CUSA00556", "CUSA34394"]);

  it("returns everything when nothing is selected", () => {
    expect(filterCheatEntries(real, NO_CHEAT_FILTERS, installedIds)).toHaveLength(real.length);
  });

  it("filters by cheat format", () => {
    const got = filterCheatEntries(real, { ...NO_CHEAT_FILTERS, format: "shn" }, installedIds);
    expect(got.map((x) => x.filename)).toEqual(["CUSA00556_01.11.shn"]);
  });

  it("filters by game version", () => {
    const got = filterCheatEntries(real, { ...NO_CHEAT_FILTERS, version: "01.05" }, installedIds);
    expect(got.map((x) => x.filename)).toEqual(["CUSA09193_01.05_2.json"]);
  });

  it("can narrow to games on this console", () => {
    const got = filterCheatEntries(
      real, { ...NO_CHEAT_FILTERS, installedOnly: true }, installedIds,
    );
    expect(got.map((x) => x.title_id)).toEqual(["CUSA34394", "CUSA00556"]);
  });

  it("combines filters", () => {
    const got = filterCheatEntries(
      real, { format: "json", version: "05.01", installedOnly: true }, installedIds,
    );
    expect(got.map((x) => x.filename)).toEqual(["CUSA34394_05.01.json"]);
  });

  it("drops an unparseable row only from the installed filter", () => {
    // It has no title id, so it cannot be matched against the console — but
    // it must still be reachable when that filter is off.
    const all = filterCheatEntries(real, NO_CHEAT_FILTERS, installedIds);
    expect(all.some((x) => x.filename === "readme.md")).toBe(true);
    const narrowed = filterCheatEntries(
      real, { ...NO_CHEAT_FILTERS, installedOnly: true }, installedIds,
    );
    expect(narrowed.some((x) => x.filename === "readme.md")).toBe(false);
  });
});

describe("isSupportedCheatFormat", () => {
  it("accepts what the payload can parse", () => {
    expect(isSupportedCheatFormat("json")).toBe(true);
    expect(isSupportedCheatFormat("shn")).toBe(true);
    expect(isSupportedCheatFormat("SHN")).toBe(true);
  });

  it("rejects mc4, which installs and then shows nothing", () => {
    // The payload has no AES decryption for MC4, so a downloaded .mc4 lands on
    // disk and the title then reports "no cheats found".
    expect(isSupportedCheatFormat("mc4")).toBe(false);
    expect(isSupportedCheatFormat("")).toBe(false);
    expect(isSupportedCheatFormat(undefined)).toBe(false);
  });
});
