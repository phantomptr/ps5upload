import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ApiError } from "../api";
import { SECTIONS } from "../sections";
import { _setStringsLoaderForTests, chooseLanguage, t } from "../strings";
import { closeGame, GamesView, launchErrorText, uninstallErrorText, type GamesViewProps } from "./Games";
import type { Game, RunningGame } from "./model";

const game = (over: Partial<Game> = {}): Game => ({
  titleId: "PPSA01650",
  name: "Astro Bot",
  origin: "pkg",
  imageBacked: false,
  source: "",
  place: "internal",
  hidden: false,
  version: "01.000.016",
  sizeBytes: 2 * 1024 ** 3,
  lastPlayed: null,
  ...over,
});

const base = (over: Partial<GamesViewProps> = {}): GamesViewProps => ({
  data: { games: [game(), game({ titleId: "CUSA07842", name: "Jak X", origin: "registered", source: "/mnt/usb0/jakx", place: "usb" })], incomplete: false, hiddenKnown: true },
  loadError: null,
  query: "",
  sort: "recent",
  show: "all",
  running: new Map(),
  foreground: null,
  launching: null,
  removing: new Set(),
  message: null,
  openId: null,
  ask: null,
  busy: false,
  onQuery: vi.fn(),
  onSort: vi.fn(),
  onShow: vi.fn(),
  onRefresh: vi.fn(),
  onOpen: vi.fn(),
  onCloseSheet: vi.fn(),
  onLaunch: vi.fn(),
  onToggleHidden: vi.fn(),
  onAsk: vi.fn(),
  onConfirm: vi.fn(),
  onDismiss: vi.fn(),
  ...over,
});

const html = (over: Partial<GamesViewProps> = {}) => renderToStaticMarkup(<GamesView {...base(over)} />);
const run = (appId = 7, pid = 120): RunningGame => ({ appId, pid });

describe("Games section", () => {
  it("writes Last played in the page's language, not the browser's", () => {
    const ms = Date.UTC(2026, 2, 7, 12);
    const open = { openId: "PPSA01650", data: { games: [game({ lastPlayed: ms })], incomplete: false, hiddenKnown: true } };
    _setStringsLoaderForTests(() => Promise.resolve({}));
    try {
      chooseLanguage("ja");
      const ja = new Date(ms).toLocaleDateString("ja");
      expect(html(open)).toContain(ja);
      chooseLanguage("de");
      const de = new Date(ms).toLocaleDateString("de");
      expect(de).not.toBe(ja);
      expect(html(open)).toContain(de);
    } finally {
      chooseLanguage("");
      _setStringsLoaderForTests(null);
    }
  });

  it("is in the rail after Home, before Saves and Files", () => {
    expect(SECTIONS.map((s) => s.id).slice(0, 4)).toEqual(["home", "games", "saves", "files"]);
    expect(SECTIONS.find((s) => s.id === "games")?.labelKey).toBe("cw_games_title");
  });

  it("shows every game as a card, the running one first and tagged", () => {
    const out = html({ running: new Map([["CUSA07842", run()]]), foreground: "CUSA07842" });
    expect(out.indexOf("Jak X")).toBeLessThan(out.indexOf("Astro Bot"));
    expect(out).toContain(t("cw_games_on_screen"));
    expect(out).toContain('data-title-id="PPSA01650"');
  });

  it("says why the list is empty", () => {
    expect(html({ data: null })).toContain(t("cw_games_loading"));
    expect(html({ data: null, loadError: "x" })).toContain(t("cw_games_err_load_next"));
    expect(html({ query: "zelda" })).toContain(t("cw_games_no_match", { query: "zelda" }));
    expect(html({ show: "hidden" })).toContain(t("cw_games_none_hidden"));
  });

  it("opens a sheet with the game's details and actions", () => {
    const out = html({ openId: "CUSA07842" });
    expect(out).toContain("CUSA07842");
    expect(out).toContain("/mnt/usb0/jakx");
    expect(out).toContain(t("cw_home_vol_usb"));
    expect(out).toContain('data-game-action="launch"');
    expect(out).toContain('data-game-action="hide"');
    expect(out).toContain('data-game-action="uninstall"');
    expect(out).not.toContain('data-game-action="close"');
  });

  it("offers Close and no Uninstall while that game runs, and no Launch while another runs", () => {
    const mine = html({ openId: "PPSA01650", running: new Map([["PPSA01650", run()]]) });
    expect(mine).toContain('data-game-action="close"');
    expect(mine).toContain(t("cw_games_resume"));
    expect(mine).toMatch(/data-game-action="uninstall"[^>]*disabled/);
    const other = html({ openId: "PPSA01650", running: new Map([["CUSA07842", run()]]) });
    expect(other).toMatch(/data-game-action="launch"[^>]*disabled/);
    expect(other).toContain(t("cw_games_note_other_running"));
  });

  it("hides Hide/Show when the console could not say which icons are hidden", () => {
    const out = html({
      openId: "PPSA01650",
      data: { games: [game({ hidden: null })], incomplete: false, hiddenKnown: false },
    });
    expect(out).not.toContain('data-game-action="hide"');
    expect(out).not.toContain('data-action="games-show"');
  });

  it("asks before an uninstall, in the words of where the game came from", () => {
    // The sheet stays mounted under its confirm: ○ returns to the button that asked.
    const asked = html({ openId: "PPSA01650", ask: { kind: "uninstall", game: game() } });
    expect(asked).toContain('data-game-action="uninstall"');
    expect(asked.indexOf('data-game-action="uninstall"')).toBeLessThan(asked.indexOf('role="alertdialog"'));
    expect((asked.match(/data-nav-modal/g) ?? []).length).toBe(2);
    const pkg = html({ ask: { kind: "uninstall", game: game() } });
    expect(pkg).toContain(t("cw_games_confirm_uninstall_pkg"));
    const reg = html({ ask: { kind: "uninstall", game: game({ origin: "registered" }) } });
    expect(reg).toContain(t("cw_games_confirm_uninstall_registered"));
  });

  it("explains launch and uninstall refusals", () => {
    expect(launchErrorText(new ApiError(409, "other_game_running"))).toBe(t("cw_games_err_other_running"));
    expect(launchErrorText(new ApiError(500, "launch_sony_error_0x80940033"))).toBe(t("cw_games_err_launch_refused"));
    expect(uninstallErrorText(new ApiError(500, "uninstall_refused:0x80A30001"))).toBe(
      t("cw_games_err_uninstall_refused", { code: "0x80A30001" }),
    );
    expect(uninstallErrorText(new ApiError(409, "game_running"))).toBe(t("cw_games_err_running"));
    expect(launchErrorText(new ApiError(401, "token"))).toBeNull();
  });
});

describe("closeGame (the PC app's Close: app kill, then SIGKILL)", () => {
  const deps = (opts: { closeFails?: boolean; stays?: number; killErr?: ApiError }) => {
    const calls: [string, unknown][] = [];
    let reads = 0;
    return {
      calls,
      d: {
        call: (async (route: string, body?: object) => {
          calls.push([route, body]);
          if (route === "/api/app/close" && opts.closeFails) throw new ApiError(500, "kill_failed");
          if (route === "/api/proc/kill" && opts.killErr) throw opts.killErr;
          return {};
        }) as never,
        running: async () => {
          reads++;
          return reads <= (opts.stays ?? 0) ? new Map([["PPSA01650", run(7, 120)]]) : new Map();
        },
        wait: async () => {},
      },
    };
  };

  it("stops at the app kill when the game goes", async () => {
    const h = deps({});
    await closeGame("PPSA01650", run(), h.d);
    expect(h.calls.map((c) => c[0])).toEqual(["/api/app/close"]);
  });

  it("falls back to killing the process when the app kill is refused", async () => {
    const h = deps({ closeFails: true, stays: 5 });
    await closeGame("PPSA01650", run(), h.d);
    expect(h.calls).toEqual([
      ["/api/app/close", { app_id: 7 }],
      ["/api/proc/kill", { pid: 120 }],
    ]);
  });

  it("falls back when the game is still there after the app kill", async () => {
    const h = deps({ stays: 99 });
    await closeGame("PPSA01650", run(), h.d);
    expect(h.calls.map((c) => c[0])).toEqual(["/api/app/close", "/api/proc/kill"]);
  });

  it("counts a game gone before the kill as closed, and reports any other kill failure", async () => {
    await expect(closeGame("PPSA01650", run(), deps({ closeFails: true, stays: 5, killErr: new ApiError(404, "not_running") }).d)).resolves.toBeUndefined();
    await expect(
      closeGame("PPSA01650", run(), deps({ closeFails: true, stays: 5, killErr: new ApiError(409, "Operation not permitted") }).d),
    ).rejects.toBeInstanceOf(ApiError);
  });
});
