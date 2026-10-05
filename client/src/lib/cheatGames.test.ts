import { describe, expect, it } from "vitest";

import type { CheatRepoEntry, CheatTitle, InstalledTitle } from "../api/ps5";
import {
  applyCheatFilters,
  buildCheatGames,
  EMPTY_CHEAT_FILTERS,
  cheatSections,
  compareVersions,
  filterCheatGames,
  hasVersionMatch,
  rankCheatFiles,
  sameVersion,
} from "./cheatGames";

const inst = (titleId: string, titleName: string, system = false) =>
  ({ titleId, titleName, system, origin: "registered", imageBacked: false, source: "" }) as InstalledTitle;
const entry = (filename: string, game_version: string, format = "mc4"): CheatRepoEntry => ({
  filename,
  game_title: "",
  format,
  repo_id: "henmix",
  title_id: filename.slice(0, 9),
  game_version,
});
const dl = (title_id: string, version = "", running = false): CheatTitle => ({
  title_id,
  name: title_id,
  version,
  running,
});

describe("versions", () => {
  it("compares by number, not text", () => {
    expect(sameVersion("01.000.016", "1.0.16")).toBe(true);
    expect(sameVersion("01.000.016", "01.000.021")).toBe(false);
    expect(compareVersions("01.10", "01.05")).toBeGreaterThan(0);
    expect(sameVersion("", "01.00")).toBe(false);
    expect(sameVersion(null, null)).toBe(false);
  });
});

describe("buildCheatGames", () => {
  const index = [
    entry("PPSA23226_01.000.010.json", "01.000.010", "json"),
    entry("PPSA23226_01.000.016_a.mc4", "01.000.016"),
    entry("PPSA23226_01.000.016_x.cht", "01.000.016", "cht"),
    entry("PPSA19534_01.000.016_b.mc4", "01.000.016"),
  ];

  it("lists installed games with what the collection has for each", () => {
    const games = buildCheatGames({
      installed: [inst("PPSA23226", "Black Myth: Wukong"), inst("PPSA01650", "YouTube"), inst("NPXS40000", "Sys", true)],
      downloaded: [],
      index,
    });
    expect(games.map((g) => g.titleId).sort()).toEqual(["PPSA01650", "PPSA23226"]);
    const wukong = games.find((g) => g.titleId === "PPSA23226")!;
    // an unreadable format is not offered
    expect(wukong.available.map((e) => e.format)).toEqual(["json", "mc4"]);
  });

  it("keeps a downloaded cheat for a game that is not installed", () => {
    const games = buildCheatGames({
      installed: [],
      downloaded: [dl("CUSA09193", "01.05")],
      index: [],
      repoNames: new Map([["CUSA09193", "RESIDENT EVIL 3"]]),
    });
    expect(games[0]).toMatchObject({ name: "RESIDENT EVIL 3", installed: false, downloaded: true, downloadedVersion: "01.05" });
  });

  it("groups: playing, ready, available, none", () => {
    const games = buildCheatGames({
      installed: [
        inst("PPSA23226", "Black Myth"),
        inst("PPSA19534", "Battlefield 6"),
        inst("PPSA01650", "YouTube"),
        inst("PPSA26344", "Ghost"),
      ],
      downloaded: [dl("PPSA19534", "01.000.016")],
      index,
      runningTitleId: "ppsa26344",
    });
    const sections = cheatSections(games);
    expect(sections.map((s) => [s.key, s.games.map((g) => g.titleId)])).toEqual([
      ["playing", ["PPSA26344"]],
      ["ready", ["PPSA19534"]],
      ["available", ["PPSA23226"]],
      ["none", ["PPSA01650"]],
    ]);
  });

  it("searches by name or id", () => {
    const games = buildCheatGames({ installed: [inst("PPSA23226", "Black Myth"), inst("PPSA19534", "Battlefield")], downloaded: [], index });
    expect(filterCheatGames(games, "myth").map((g) => g.titleId)).toEqual(["PPSA23226"]);
    expect(filterCheatGames(games, "19534").map((g) => g.titleId)).toEqual(["PPSA19534"]);
  });
});

describe("rankCheatFiles", () => {
  const files = [
    entry("PPSA23226_01.000.010.json", "01.000.010", "json"),
    entry("PPSA23226_01.000.021_c.mc4", "01.000.021"),
    entry("PPSA23226_01.000.016_a.mc4", "01.000.016"),
  ];
  it("puts the installed version first, then newest", () => {
    expect(rankCheatFiles(files, "01.000.016").map((e) => e.game_version)).toEqual([
      "01.000.016",
      "01.000.021",
      "01.000.010",
    ]);
    expect(hasVersionMatch(files, "01.000.016")).toBe(true);
    expect(hasVersionMatch(files, "01.000.099")).toBe(false);
  });
  it("is newest first when the installed version is unknown", () => {
    expect(rankCheatFiles(files, null)[0].game_version).toBe("01.000.021");
  });
});

describe("names, formats and filters (R16)", () => {
  const games = () =>
    buildCheatGames({
      installed: [inst("CUSA00001", "Alpha"), inst("CUSA00003", "Gamma")],
      downloaded: [
        { title_id: "CUSA00001", name: "Alpha", version: "01.00", formats: ["mc4"], enabled: 2, running: false },
        { title_id: "CUSA00002", name: "Killzone Shadow Fall", version: "", formats: ["json", "shn"], enabled: 0, running: true },
      ],
      index: [entry("CUSA00003_01.00.shn", "01.00", "shn")],
      runningTitleId: "CUSA00002",
    });

  it("names a game that is not installed from its cheat file, falling back to the id", () => {
    const g = games().find((x) => x.titleId === "CUSA00002")!;
    expect(g.name).toBe("Killzone Shadow Fall");
    const bare = buildCheatGames({
      installed: [],
      downloaded: [{ title_id: "CUSA00009", name: "CUSA00009", running: false }],
      index: [],
    });
    expect(bare[0].name).toBe("CUSA00009");
  });

  it("carries the on-console formats and the enabled count", () => {
    const g = games().find((x) => x.titleId === "CUSA00001")!;
    expect(g.downloadedFormats).toEqual(["mc4"]);
    expect(g.enabledCount).toBe(2);
  });

  const ids = (f: Partial<typeof EMPTY_CHEAT_FILTERS>) =>
    applyCheatFilters(games(), { ...EMPTY_CHEAT_FILTERS, ...f })
      .map((g) => g.titleId)
      .sort();

  it("filters by format on the console or in the collection", () => {
    expect(ids({ format: "mc4" })).toEqual(["CUSA00001"]);
    expect(ids({ format: "shn" })).toEqual(["CUSA00002", "CUSA00003"]);
  });

  it("filters by state", () => {
    expect(ids({ state: "switchedOn" })).toEqual(["CUSA00001"]);
    expect(ids({ state: "playing" })).toEqual(["CUSA00002"]);
    expect(ids({ state: "toDownload" })).toEqual(["CUSA00003"]);
    expect(ids({ state: "ready" })).toEqual(["CUSA00001", "CUSA00002"]);
  });

  it("filters by scope and searches by name, combined", () => {
    expect(ids({ scope: "installed" })).toEqual(["CUSA00001", "CUSA00003"]);
    expect(ids({ query: "kill" })).toEqual(["CUSA00002"]);
    expect(ids({ query: "a", format: "mc4" })).toEqual(["CUSA00001"]);
    expect(ids({})).toHaveLength(3);
  });
});
