import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../api", async (importOriginal) => {
  const real = await importOriginal<typeof import("../api")>();
  return { ...real, call: vi.fn() };
});

import { ApiError, call } from "../api";
import { SECTIONS } from "../sections";
import { t } from "../strings";
import { CheatsView, finderClosesFor, focusedCheatIndex, type CheatsViewProps } from "./Cheats";
import {
  appliedText,
  cheatsPhase,
  createCheatsController,
  EMPTY_STATE,
  readList,
  readStatus,
  toggleErrorText,
  type CheatMod,
  type CheatsState,
} from "./model";

const mockCall = vi.mocked(call);

const GAME = "PPSA01650";
const mod = (over: Partial<CheatMod> = {}): CheatMod => ({
  index: 0,
  name: "Infinite HP",
  desc: "",
  button: false,
  on: false,
  ...over,
});

/** A fake helper: status says `game` runs; list answers `mods`; toggle flips in place. */
function helper(opts: { game: string | null; mods?: CheatMod[]; toggleError?: ApiError; engine?: boolean; held?: boolean }) {
  const h = { ...opts, mods: (opts.mods ?? []).map((m) => ({ ...m })), calls: [] as [string, unknown][] };
  mockCall.mockImplementation(async (route: string, body?: object) => {
    h.calls.push([route, body]);
    if (route === "/api/cheats/status")
      return {
        engine: h.engine ?? false,
        game: h.game ? { title_id: h.game, ...(h.held ? { held: true } : {}) } : null,
      } as never;
    if (route === "/api/cheats/engine") {
      h.engine = (body as { enabled: boolean }).enabled;
      return { ok: true, enabled: h.engine } as never;
    }
    if (route === "/api/cheats/list") return { mods: h.mods, truncated: false } as never;
    if (route === "/api/cheats/toggle") {
      if (h.toggleError) throw h.toggleError;
      const b = body as { index: number; on: boolean };
      const m = h.mods.find((x) => x.index === b.index)!;
      if (!m.button) m.on = b.on;
      return { ok: true, on: b.on } as never;
    }
    throw new Error(`unexpected ${route}`);
  });
  return h;
}

const view = (state: Partial<CheatsState>, over: Partial<CheatsViewProps> = {}) =>
  renderToStaticMarkup(
    <CheatsView state={{ ...EMPTY_STATE, ...state }} onToggle={() => {}} onRefresh={() => {}} {...over} />,
  );

beforeEach(() => {
  mockCall.mockReset();
});

describe("cheats section", () => {
  it("is_registered_between_files_and_settings", () => {
    expect(SECTIONS.map((s) => s.id)).toEqual(["home", "games", "saves", "files", "cheats", "system", "settings"]);
    expect(SECTIONS.find((s) => s.id === "cheats")?.labelKey).toBe("cw_cheats_title");
  });

  it("no_game_says_what_to_do", async () => {
    helper({ game: null });
    const states: CheatsState[] = [];
    const c = createCheatsController((s) => states.push(s));
    await c.loadStatus();
    expect(cheatsPhase(c.get())).toEqual({ kind: "no_game" });
    const html = view(c.get());
    expect(html).toContain(t("cw_cheats_no_game"));
    expect(html).toContain(t("cw_cheats_no_game_next"));
    expect(t("cw_cheats_no_game_next")).toBe("Start a game, then open Cheats.");
    expect(html).not.toContain("data-cheat-index");
    // No list read without a game.
    expect(mockCall.mock.calls.map((x) => x[0])).toEqual(["/api/cheats/status"]);
  });

  it("no_cheats_offers_find_and_the_pc_app", async () => {
    helper({ game: GAME, mods: [] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    expect(cheatsPhase(c.get())).toEqual({ kind: "none", title: GAME });
    const html = view(c.get(), { onFind: () => {} });
    expect(html).toContain(t("cw_cheats_none"));
    expect(t("cw_cheats_none")).toBe("No cheats for this game yet.");
    expect(html).toContain(t("cw_cheats_none_next"));
    expect(t("cw_cheats_none_next")).toBe("Find cheats for it online, or add them from the PS5Upload app on your PC.");
    expect(html).toContain(GAME);
    expect(html).toContain('data-action="cheats-find"');
    expect(html).toContain(t("cw_cheats_find"));
    expect(html).not.toContain(t("cw_cheats_find_more"));
    // Without a finder (no game known) there is no Find button.
    expect(view(c.get())).not.toContain('data-action="cheats-find"');
  });

  it("a_list_offers_find_more_and_shows_a_finished_download", async () => {
    helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    const html = view(c.get(), { onFind: () => {}, notice: "saved it" });
    expect(html).toContain(t("cw_cheats_find_more"));
    expect(html).toContain("saved it");
    // no game: no Find
    expect(view({ ...EMPTY_STATE, status: { engine: false, game: null } }, { onFind: () => {} })).not.toContain(
      'data-action="cheats-find"',
    );
  });

  it("lists_the_running_games_cheats_as_switches", async () => {
    helper({ game: GAME, mods: [mod({ index: 0, on: true }), mod({ index: 3, name: "Max money", desc: "999999" }), mod({ index: 4, name: "Heal", button: true })] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    expect(mockCall).toHaveBeenCalledWith("/api/cheats/list", { title_id: GAME });
    const html = view(c.get());
    expect(html).toContain('role="switch" aria-checked="true"');
    expect(html).toContain('data-cheat-index="3"');
    expect(html).toContain("999999");
    // A one-shot cheat is an Apply button, not a switch.
    expect(html).toMatch(/<button type="button" class="[^"]*cw-cheat"[^>]*data-cheat-index="4"/);
    expect(html).toContain(t("cw_cheats_apply"));
    // The engine is off: the header says so, and a switched-on cheat reads Saved, not On (#550).
    expect(html).toContain(t("cw_cheats_engine_off_state"));
    expect(html).toContain(`>${t("cw_cheats_saved")}<`);
    expect(html).not.toContain(`>${t("cw_cheats_on")}<`);
  });

  it("switched_on_cheats_read_on_only_while_they_are_applied", async () => {
    const h = helper({ game: GAME, mods: [mod({ index: 0, on: true })], engine: true });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    let html = view(c.get());
    expect(html).toContain(`>${t("cw_cheats_on")}<`);
    expect(html).toContain(t("cw_cheats_how"));
    // The helper paused this game (it closed right after its cheats were applied): Saved, and why.
    h.held = true;
    await c.loadStatus();
    html = view(c.get());
    expect(html).toContain(t("cw_cheats_held"));
    expect(html).toContain(`>${t("cw_cheats_saved")}<`);
  });

  it("a_switched_on_cheat_says_what_happened_in_the_game", async () => {
    // #627: the helper reports what actually happened, not only the saved switch.
    const raw = {
      mods: [
        { index: 0, name: "60 FPS", on: true, applied: "applied" },
        { index: 1, name: "B", on: true, applied: "waiting", applied_reason: "the game's code is not loaded yet" },
        { index: 2, name: "C", on: true, applied: "failed", applied_reason: "doesn't match this game version" },
        { index: 3, name: "D", on: true, applied: "changed" },
        { index: 4, name: "E", on: true, applied: "bogus" },
        { index: 5, name: "F", on: false },
      ],
    };
    const list = readList(GAME, raw);
    expect(list.mods.map((m) => m.applied)).toEqual(["applied", "waiting", "failed", "changed", undefined, undefined]);
    expect(list.mods[2].appliedReason).toBe("doesn't match this game version");
    expect(appliedText(list.mods[0])).toBe(t("cw_cheats_applied_ok"));
    expect(appliedText(list.mods[1])).toBe(t("cw_cheats_applied_waiting"));
    expect(appliedText(list.mods[2])).toBe(t("cw_cheats_applied_failed", { reason: "doesn't match this game version" }));
    expect(appliedText(list.mods[3])).toBe(t("cw_cheats_applied_changed"));
    expect(appliedText(list.mods[4])).toBeNull();
    expect(appliedText({ ...list.mods[0], on: false })).toBeNull();

    // Shown under the switch while the engine applies cheats; not while they are only saved.
    const state = { status: { engine: true, game: { title_id: GAME } }, list };
    let html = view(state);
    expect(html).toContain(t("cw_cheats_applied_waiting"));
    expect(html).toContain('data-applied="failed"');
    html = view({ ...state, status: { engine: false, game: { title_id: GAME } } });
    expect(html).not.toContain("data-applied");
  });

  it("a_waiting_cheat_is_read_again_until_it_applies", async () => {
    let applied = "waiting";
    mockCall.mockImplementation(async (route: string) => {
      if (route === "/api/cheats/status") return { engine: true, game: { title_id: GAME } } as never;
      if (route === "/api/cheats/list") return { mods: [{ index: 0, name: "A", on: true, applied }] } as never;
      throw new Error(`unexpected ${route}`);
    });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    expect(c.get().list?.mods[0].applied).toBe("waiting");
    applied = "applied";
    await c.loadStatus(); // the status poll reads the list again while something waits
    expect(c.get().list?.mods[0].applied).toBe("applied");
    const lists = () => mockCall.mock.calls.filter(([r]) => r === "/api/cheats/list").length;
    const before = lists();
    await c.loadStatus(); // nothing waits now: the poll leaves the list alone
    expect(lists()).toBe(before);
  });

  it("the_engine_switch_works_with_or_without_a_game", async () => {
    const h = helper({ game: null });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    const onEngine = (on: boolean) => void c.setEngine(on);
    // No game yet: the engine's state and its switch still show (it is remembered across restarts).
    let html = view(c.get(), { onEngine });
    expect(html).toContain(t("cw_cheats_engine_off_state"));
    expect(html).toContain('data-action="cheats-engine"');
    expect(html).toContain(t("cw_cheats_engine_turn_on"));
    await c.setEngine(true);
    expect(h.calls).toContainEqual(["/api/cheats/engine", { enabled: true }]);
    html = view(c.get(), { onEngine });
    expect(html).toContain(t("cw_cheats_engine_on_state"));
    expect(html).toContain(t("cw_cheats_engine_turn_off"));
    // With a game, the switch sits on the game card.
    h.game = GAME;
    await c.loadStatus();
    expect(view(c.get(), { onEngine })).toContain('data-action="cheats-engine"');
    // Without a handler (tests elsewhere), no switch.
    expect(view(c.get())).not.toContain('data-action="cheats-engine"');
  });

  it("toggle_calls_the_route_and_reflects_the_result", async () => {
    const h = helper({ game: GAME, mods: [mod({ index: 0 }), mod({ index: 1, name: "B" })] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    const p = c.toggle(c.get().list!.mods[1]);
    // In flight: every row is inert, the pressed one says so.
    expect(c.get().busy).toBe(1);
    const busyHtml = view(c.get());
    expect(busyHtml.match(/aria-disabled="true"/g)).toHaveLength(2);
    expect(busyHtml).toContain(t("cw_cheats_switching"));
    // A second press meanwhile is ignored.
    await c.toggle(c.get().list!.mods[0]);
    await p;
    expect(h.calls.filter((x) => x[0] === "/api/cheats/toggle")).toEqual([
      ["/api/cheats/toggle", { title_id: GAME, index: 1, on: true }],
    ]);
    // Re-read after the toggle: the helper's state is what shows.
    expect(h.calls[h.calls.length - 1][0]).toBe("/api/cheats/list");
    expect(c.get().busy).toBeNull();
    expect(c.get().list!.mods.find((m) => m.index === 1)?.on).toBe(true);
    expect(view(c.get())).not.toContain("aria-disabled");
    // And off again.
    await c.toggle(c.get().list!.mods[1]);
    expect(h.calls.filter((x) => x[0] === "/api/cheats/toggle")[1][1]).toEqual({ title_id: GAME, index: 1, on: false });
    expect(c.get().list!.mods[1].on).toBe(false);
  });

  it("a_one_shot_cheat_is_always_applied_on", async () => {
    const h = helper({ game: GAME, mods: [mod({ index: 2, button: true })] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    await c.toggle(c.get().list!.mods[0]);
    await c.toggle(c.get().list!.mods[0]);
    expect(h.calls.filter((x) => x[0] === "/api/cheats/toggle").map((x) => (x[1] as { on: boolean }).on)).toEqual([true, true]);
  });

  it("game_exited_while_open_drops_the_switches", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    expect(cheatsPhase(c.get()).kind).toBe("list");
    h.game = null;
    await c.loadStatus();
    expect(cheatsPhase(c.get())).toEqual({ kind: "exited", title: GAME });
    expect(c.get().list).toBeNull();
    const html = view(c.get());
    expect(html).toContain(t("cw_cheats_exited"));
    expect(html).not.toContain("data-cheat-index");
    expect(html).toContain('data-action="cheats-refresh"');
  });

  it("toggle_after_the_game_closed_shows_closed_not_an_error", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    h.game = null;
    h.toggleError = new ApiError(409, "game_not_running");
    await c.toggle(c.get().list!.mods[0]);
    expect(c.get().message).toBeNull();
    expect(cheatsPhase(c.get()).kind).toBe("exited");
  });

  it("toggle_right_after_the_game_exited_says_closed_at_once", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const states: CheatsState[] = [];
    const c = createCheatsController((st) => states.push(st));
    await c.loadStatus();
    h.game = null;
    h.toggleError = new ApiError(409, "game_not_running");
    let statusAsked = -1;
    const real = mockCall.getMockImplementation()!;
    mockCall.mockImplementation(async (route: string, body?: object) => {
      if (route === "/api/cheats/status" && statusAsked < 0) statusAsked = states.length;
      return real(route, body);
    });
    await c.toggle(c.get().list!.mods[0]);
    // "Closed" was on screen before the status was read again (no wait for the next poll).
    expect(statusAsked).toBeGreaterThan(0);
    expect(states.slice(0, statusAsked).some((st) => cheatsPhase(st).kind === "exited")).toBe(true);
    expect(cheatsPhase(c.get()).kind).toBe("exited");
  });

  it("a_toggle_racing_the_pc_app_says_try_again", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    h.toggleError = new ApiError(409, "game_busy");
    await c.toggle(c.get().list!.mods[0]);
    expect(c.get().message).toBe(t("cw_cheats_err_busy"));
    expect(cheatsPhase(c.get()).kind).toBe("list");
  });

  it("another_game_never_shows_the_old_games_cheats", async () => {
    const h = helper({ game: GAME, mods: [mod({ name: "Old" })] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    // The list read for the new game is slow: nothing of the old game shows meanwhile.
    h.game = "CUSA07842";
    let release!: () => void;
    mockCall.mockImplementationOnce(async () => ({ engine: false, game: { title_id: "CUSA07842" } }) as never);
    mockCall.mockImplementationOnce(
      () => new Promise((res) => (release = () => res({ mods: [mod({ name: "New" })], truncated: false } as never))),
    );
    const p = c.loadStatus();
    await Promise.resolve();
    expect(cheatsPhase(c.get()).kind).toBe("loading");
    expect(view(c.get())).not.toContain("Old");
    release();
    await p;
    expect(view(c.get())).toContain("New");
  });

  it("helper_errors_are_short_and_retryable", async () => {
    mockCall.mockRejectedValue(new ApiError(500, "oom"));
    const c = createCheatsController(() => {});
    await c.loadStatus();
    const ph = cheatsPhase(c.get());
    expect(ph.kind).toBe("error");
    const html = view(c.get());
    expect(html).toContain("oom");
    expect(html).toContain('role="alert"');
    expect(html).toContain('data-action="cheats-refresh"');
    // Offline reads as offline; a 401 is left to the reload banner.
    expect(toggleErrorText(new ApiError(0, "offline"))).toBe(t("cw_home_err_offline"));
    expect(toggleErrorText(new ApiError(401, "token"))).toBeNull();
    expect(toggleErrorText(new ApiError(409, "module_missing"))).toBe(t("cw_cheats_err_module"));
    expect(toggleErrorText(new ApiError(400, "bad_index"))).toBe(t("cw_cheats_err_changed"));
    expect(toggleErrorText(new ApiError(500, "toggle_failed"))).toContain("toggle_failed");
  });

  it("toggle_failure_keeps_the_list_and_says_why", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    h.toggleError = new ApiError(409, "module_missing");
    await c.toggle(c.get().list!.mods[0]);
    expect(c.get().message).toBe(t("cw_cheats_err_module"));
    expect(cheatsPhase(c.get()).kind).toBe("list");
    expect(c.get().list!.mods[0].on).toBe(false);
    expect(view(c.get())).toContain(t("cw_cheats_err_module"));
  });

  it("refresh_rereads_status_and_list", async () => {
    const h = helper({ game: GAME, mods: [mod()] });
    const c = createCheatsController(() => {});
    await c.loadStatus();
    h.calls.length = 0;
    h.mods[0].on = true; // the PC switched it meanwhile
    await c.refresh();
    expect(h.calls.map((x) => x[0])).toEqual(["/api/cheats/status", "/api/cheats/list"]);
    expect(c.get().list!.mods[0].on).toBe(true);
  });

  it("disposed_controller_stops_reporting", async () => {
    helper({ game: GAME, mods: [mod()] });
    const seen = vi.fn();
    const c = createCheatsController(seen);
    c.dispose();
    await c.loadStatus();
    expect(seen).not.toHaveBeenCalled();
  });

  it("replies_are_checked", () => {
    expect(readStatus(null)).toEqual({ engine: false, game: null });
    expect(readStatus({ engine: true, game: { title_id: GAME } })).toEqual({ engine: true, game: { title_id: GAME } });
    expect(readStatus({ engine: "yes", game: { title_id: 5 } })).toEqual({ engine: false, game: null });
    expect(readStatus({ engine: true, game: { title_id: GAME, held: true } })).toEqual({
      engine: true,
      game: { title_id: GAME, held: true },
    });
    expect(readStatus({ engine: true, game: { title_id: GAME, held: "yes" } })).toEqual({
      engine: true,
      game: { title_id: GAME },
    });
    const l = readList(GAME, { mods: [{ index: 1, name: "A", on: true }, { index: "2", name: "B" }, { index: 3 }, null], truncated: true });
    expect(l).toEqual({ title: GAME, mods: [{ index: 1, name: "A", desc: "", button: false, on: true }], truncated: true });
    expect(readList(GAME, "nope")).toEqual({ title: GAME, mods: [], truncated: false });
  });

  it("square_targets_the_focused_cheat_only", () => {
    const row = { closest: () => ({ getAttribute: () => "7" }) } as unknown as Element;
    const root = { contains: (el: Element) => el === row } as unknown as Element;
    expect(focusedCheatIndex(root, row)).toBe(7);
    const outside = { closest: () => ({ getAttribute: () => "7" }) } as unknown as Element;
    expect(focusedCheatIndex(root, outside)).toBeNull();
    const notACheat = { closest: () => null } as unknown as Element;
    const root2 = { contains: () => true } as unknown as Element;
    expect(focusedCheatIndex(root2, notACheat)).toBeNull();
    expect(focusedCheatIndex(null, row)).toBeNull();
  });

  it("the_finder_closes_only_when_its_game_is_confirmed_gone", () => {
    expect(finderClosesFor(GAME, GAME, 0)).toBe(false);
    expect(finderClosesFor(GAME, "CUSA00001", 0)).toBe(true);
    // one empty status read is not enough (a transient answer); two in a row are
    expect(finderClosesFor(GAME, null, 1)).toBe(false);
    expect(finderClosesFor(GAME, null, 2)).toBe(true);
  });
});
