import { describe, expect, it } from "vitest";
import { ApiError } from "../api";
import {
  appName,
  assemble,
  filterGames,
  loadInstalled,
  parseDbPage,
  parseHidden,
  parseInfo,
  parseInfoTime,
  parseProcesses,
  parseRegistered,
  placeOf,
  refusedCode,
  runningGames,
  skipTitle,
  sortGames,
  stillInstalled,
  type Game,
  type Sources,
} from "./model";

const game = (over: Partial<Game>): Game => ({
  titleId: "PPSA00001",
  name: "Game",
  origin: "pkg",
  imageBacked: false,
  source: "",
  place: "internal",
  hidden: false,
  version: null,
  sizeBytes: null,
  lastPlayed: null,
  ...over,
});

const sources = (over: Partial<Sources> = {}): Sources => ({
  appmeta: [],
  appDirs: new Map(),
  dbRows: [],
  registered: new Map(),
  hidden: new Set(),
  ...over,
});

describe("games model", () => {
  it("lists appmeta titles and app.db rows that have an app folder, like the engine", () => {
    const g = assemble(
      sources({
        appmeta: ["PPSA01650", "NPXS40001", "PSUP00001", "notatitle", "CUSA07842"],
        appDirs: new Map([
          ["PPSA02000", "/mnt/ext0/user/app"],
          ["CUSA07842", "/user/app"],
        ]),
        dbRows: [
          ["PPSA01650", "Astro Bot"],
          ["PPSA02000", "Minecraft"],
          ["PPSA03000", "Playroom (no folder)"],
          ["NPXS40002", "Settings"],
          ["CUSA07842", ""],
        ],
        registered: new Map([
          ["CUSA07842", { titleId: "CUSA07842", name: "Jak X", source: "/mnt/usb0/games/jakx", imageBacked: false }],
        ]),
        hidden: new Set(["PPSA02000"]),
      }),
    );
    expect(g.map((x) => [x.titleId, x.name, x.origin, x.place, x.hidden])).toEqual([
      ["PPSA01650", "Astro Bot", "pkg", "unknown", false],
      ["CUSA07842", "Jak X", "registered", "usb", false],
      ["PPSA02000", "Minecraft", "pkg", "ext", true],
    ]);
  });

  it("never lists the page's own tile or the console's own apps", () => {
    expect(skipTitle("PSUP00001")).toBe(true);
    expect(skipTitle("NPXS40087")).toBe(true);
    expect(skipTitle("ppsa01650")).toBe(true);
    expect(skipTitle("PPSA01650")).toBe(false);
  });

  it("places a title by its registration, else by its app folder", () => {
    expect(placeOf("registered", true, "/data/homebrew/x.ffpfsc", null)).toBe("image");
    expect(placeOf("registered", false, "/mnt/ext1/games/x", null)).toBe("ext");
    expect(placeOf("registered", false, "/data/games/x", null)).toBe("internal");
    expect(placeOf("registered", false, "/mnt/ps5upload/x", null)).toBe("folder");
    expect(placeOf("pkg", false, "", "/user/app")).toBe("internal");
    expect(placeOf("pkg", false, "", "/mnt/ext0/user/app")).toBe("ext");
    expect(placeOf("pkg", false, "", null)).toBe("unknown");
    // a homebrew installer's registration has no source: its app folder says where
    expect(placeOf("registered", false, "", "/mnt/ext1/user/app")).toBe("ext");
  });

  it("reads the helper's replies defensively", () => {
    expect(parseRegistered({ more: true, reply: { apps: [{ title_id: "A", title_name: "x", src: "/s", image_backed: true }] } })).toEqual({
      apps: new Map([["A", { titleId: "A", name: "x", source: "/s", imageBacked: true }]]),
      whole: false,
    });
    expect(parseRegistered(null).apps.size).toBe(0);
    expect(parseDbPage({ apps: [{ title_id: "A", name: "n" }], more: true, next: 500 })).toEqual({
      rows: [["A", "n"]],
      more: true,
      next: 500,
      ok: true,
    });
    expect(parseDbPage({ err: "appdb_unreadable", apps: [] }).ok).toBe(false);
    expect(parseHidden({ ok: true, hidden: ["A", 3] })).toEqual(new Set(["A"]));
    expect(parseHidden({ ok: false, error: "x" })).toBeNull();
  });

  it("reads appinfo's version, size and last-played time", () => {
    const i = parseInfo({
      ok: true,
      rows: [
        { key: "CONTENT_VERSION", val: "01.000.016" },
        { key: "#_size", val: "1073741824" },
        { key: "#_last_access_time", val: "2026-10-20 12:00:00" },
      ],
    });
    expect(i).toEqual({ version: "01.000.016", sizeBytes: 1073741824, lastPlayed: Date.UTC(2026, 9, 20, 12) });
    expect(parseInfo({ rows: [{ key: "APP_VER", val: "01.05" }] }).version).toBe("01.05");
    // #934: an updated PS4 game keeps APP_VER at the base's 01.00 and says 01.32 in VERSION.
    expect(
      parseInfo({
        ok: true,
        rows: [
          { key: "APP_VER", val: "01.00" },
          { key: "VERSION", val: "01.32" },
        ],
      }).version,
    ).toBe("01.32");
    expect(parseInfo(null)).toEqual({ version: null, sizeBytes: null, lastPlayed: null });
    expect(parseInfoTime("1760000000")).toBe(1760000000000);
    expect(parseInfoTime("1760000000000")).toBe(1760000000000);
    expect(parseInfoTime("0")).toBeNull();
    expect(parseInfoTime("garbage")).toBeNull();
  });

  it("sorts the running game first, then the last played, then by name", () => {
    const list = [
      game({ titleId: "PPSA00001", name: "Zeta", lastPlayed: 100 }),
      game({ titleId: "PPSA00002", name: "alpha" }),
      game({ titleId: "PPSA00003", name: "Beta", lastPlayed: 300 }),
      game({ titleId: "PPSA00004", name: "Gamma" }),
    ];
    expect(sortGames(list, new Set(["PPSA00004"]), "recent").map((g) => g.name)).toEqual(["Gamma", "Beta", "Zeta", "alpha"]);
    expect(sortGames(list, new Set(), "name").map((g) => g.name)).toEqual(["alpha", "Beta", "Gamma", "Zeta"]);
  });

  it("filters by name or title id, and by hidden icon", () => {
    const list = [game({ titleId: "PPSA01650", name: "Astro Bot" }), game({ titleId: "CUSA07842", name: "Jak X", hidden: true })];
    expect(filterGames(list, "astro", "all").map((g) => g.titleId)).toEqual(["PPSA01650"]);
    expect(filterGames(list, "cusa", "all").map((g) => g.titleId)).toEqual(["CUSA07842"]);
    expect(filterGames(list, "", "hidden").map((g) => g.titleId)).toEqual(["CUSA07842"]);
  });

  it("knows running games from the process list (games only, not the helper)", () => {
    const { procs, truncated } = parseProcesses({
      ok: true,
      procs: [
        { pid: 10, name: "eboot.bin", title_id: "CUSA07842", app_id: 0, kind: "app", is_self: false },
        { pid: 11, name: "ps2-emu", title_id: "CUSA07842", app_id: 99, kind: "app", is_self: false },
        { pid: 12, name: "SceShellUI", title_id: "NPXS40087", app_id: 5, kind: "system", is_self: false },
        { pid: 13, name: "ps5upload.elf", title_id: "", app_id: 0, kind: "payload", is_self: true },
        { truncated: true },
      ],
    });
    expect(truncated).toBe(true);
    expect(procs).toHaveLength(4);
    expect(runningGames(procs)).toEqual(new Map([["CUSA07842", { appId: 99, pid: 11 }]]));
  });

  it("reads an uninstall refusal's Sony code", () => {
    expect(refusedCode("uninstall_refused:0x80A30001")).toBe("0x80A30001");
    expect(refusedCode("game_running")).toBeNull();
  });

  /** A fake helper for the list routes. */
  function helperCalls(opts: { appmeta: string[]; appFolders?: Record<string, string[]>; failAppmeta?: boolean }) {
    const calls: string[] = [];
    const fn = async <T,>(route: string, body?: object): Promise<T> => {
      calls.push(route);
      const b = (body ?? {}) as { path?: string; offset?: number; title_ids?: string[] };
      if (route === "/api/fs/list") {
        if (b.path === "/user/appmeta") {
          if (opts.failAppmeta) throw new ApiError(0, "offline");
          return { entries: opts.appmeta.map((name) => ({ name, kind: "dir" })), more: false } as T;
        }
        const names = opts.appFolders?.[b.path ?? ""];
        if (!names) throw new ApiError(404, "not_found");
        return { entries: names.map((name) => ({ name, kind: "dir" })), more: false } as T;
      }
      if (route === "/api/apps/registered") return { more: false, reply: { apps: [] } } as T;
      if (route === "/api/apps/db") return { apps: [{ title_id: "PPSA01650", name: "Astro Bot" }], more: false, next: 1 } as T;
      if (route === "/api/apps/hidden") return { ok: true, hidden: [] } as T;
      if (route === "/api/apps/info")
        return {
          apps: Object.fromEntries((b.title_ids ?? []).map((id) => [id, { ok: true, rows: [{ key: "APP_VER", val: "01.00" }] }])),
        } as T;
      throw new Error(`unexpected ${route}`);
    };
    return { fn, calls };
  }

  it("loads the whole list with details", async () => {
    const h = helperCalls({ appmeta: ["PPSA01650"], appFolders: { "/user/app": ["PPSA01650"] } });
    const d = await loadInstalled(h.fn);
    expect(d.games).toHaveLength(1);
    expect(d.games[0]).toMatchObject({ titleId: "PPSA01650", name: "Astro Bot", version: "01.00", place: "internal" });
    expect(d.incomplete).toBe(false);
    expect(d.hiddenKnown).toBe(true);
  });

  it("fails the load only when /user/appmeta cannot be read", async () => {
    await expect(loadInstalled(helperCalls({ appmeta: [], failAppmeta: true }).fn)).rejects.toBeInstanceOf(ApiError);
  });

  it("checks whether an uninstalled title is still listed", async () => {
    expect(await stillInstalled("PPSA01650", helperCalls({ appmeta: ["PPSA01650"] }).fn)).toBe(true);
    // app.db still has the row, but the folders are gone: not listed (the engine's rule).
    expect(await stillInstalled("PPSA01650", helperCalls({ appmeta: [] }).fn)).toBe(false);
  });
});

describe("the running game's name", () => {
  it("reads app.db a page at a time until the title is there, once per title", async () => {
    const pages: Record<number, unknown> = {
      0: { apps: [{ title_id: "CUSA00001", name: "Old Racer" }], more: true, next: 1 },
      1: { apps: [{ title_id: "PPSA77001", name: "Astro Bot" }], more: true, next: 2 },
      2: { apps: [], more: false, next: 2 },
    };
    const calls: number[] = [];
    const fn = (async (_route: string, body?: object) => {
      const offset = (body as { offset: number }).offset;
      calls.push(offset);
      return pages[offset];
    }) as never;
    expect(await appName("PPSA77001", fn)).toBe("Astro Bot");
    expect(calls).toEqual([0, 1]);
    expect(await appName("PPSA77001", fn)).toBe("Astro Bot");
    expect(calls).toEqual([0, 1]);
    // not in app.db: every page once, then null
    expect(await appName("PPSA77002", fn)).toBeNull();
    expect(calls).toEqual([0, 1, 0, 1, 2]);
  });

  it("asks again after a failed read", async () => {
    let fail = true;
    const fn = (async () => {
      if (fail) throw new ApiError(0, "offline");
      return { apps: [{ title_id: "PPSA77003", name: "Venus" }], more: false, next: 0 };
    }) as never;
    expect(await appName("PPSA77003", fn)).toBeNull();
    fail = false;
    expect(await appName("PPSA77003", fn)).toBe("Venus");
  });
});
