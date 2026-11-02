/**
 * The Cheats section's state, kept out of React so tests drive it directly.
 *
 * The helper's routes (payload/src/webui_api.c) adapt the cheat methods the PC
 * engine calls (engine/crates/ps5upload-core/src/cheats.rs):
 *   /api/cheats/status            → {engine, game: {title_id} | null}   (CHEATS_STATUS)
 *   /api/cheats/list   {title_id} → {mods: [{index,name,desc,button,on,applied?,applied_reason?}], truncated} (CHEATS_GET)
 *   /api/cheats/toggle {title_id, index, on} → {ok, on}                  (CHEATS_TOGGLE)
 *   /api/cheats/engine {enabled}  → {ok, enabled}                       (CHEATS_ENGINE_SET)
 * A toggle needs that game running (the helper patches the live process); a
 * switch-on also turns the helper's cheat engine on. The helper remembers the
 * engine switch across restarts (#550).
 */
import { ApiError, call, isOffline } from "../api";
import { t } from "../strings";

export interface CheatsStatus {
  /** The helper's cheat engine: applies switched-on cheats when their game starts. */
  engine: boolean;
  /** `held`: the helper paused this game's cheats (it closed right after they were applied). */
  game: { title_id: string; held?: boolean } | null;
}

export interface CheatMod {
  /** The helper's flat index across the game's cheat files (what a toggle names). */
  index: number;
  name: string;
  desc: string;
  /** A one-shot cheat: applied each time, it has no on/off state. */
  button: boolean;
  on: boolean;
  /** An XML game patch made for another game version than the installed one. */
  otherVersion?: boolean;
  /** What happened to this switched-on cheat in the running game (#627); absent when unknown. */
  applied?: CheatApplied;
  /** Why, for waiting / failed / changed (the helper's own words). */
  appliedReason?: string;
}

export type CheatApplied = "applied" | "waiting" | "failed" | "changed";
const APPLIED: readonly string[] = ["applied", "waiting", "failed", "changed"];

/** The line under a switched-on cheat: applied, waiting for the game, failed (and why), or
 *  changed by the game. null when there is nothing to say (off, or an older helper). */
export function appliedText(mod: CheatMod): string | null {
  if (!mod.on || mod.button || !mod.applied) return null;
  switch (mod.applied) {
    case "applied":
      return t("cw_cheats_applied_ok");
    case "waiting":
      return t("cw_cheats_applied_waiting");
    case "failed":
      return t("cw_cheats_applied_failed", { reason: mod.appliedReason || "?" });
    case "changed":
      return t("cw_cheats_applied_changed");
  }
}

export interface CheatList {
  title: string;
  mods: CheatMod[];
  /** The helper's reply was cut: not every cheat is listed. */
  truncated: boolean;
}

export interface CheatsState {
  status: CheatsStatus | null;
  /** The last status read failed (null once one succeeds). */
  statusError: string | null;
  /** The last game seen running while the section was open: tells "closed" from "never ran". */
  seen: string | null;
  /** The running game's cheats; null while not read (or no game runs). */
  list: CheatList | null;
  listError: string | null;
  /** The index of the cheat being switched, or null. */
  busy: number | null;
  /** The last toggle's failure, one translated line. */
  message: string | null;
  /** The engine switch is being set. */
  engineBusy: boolean;
}

export type CheatsPhase =
  | { kind: "loading" }
  | { kind: "error"; text: string }
  | { kind: "no_game" }
  | { kind: "exited"; title: string }
  | { kind: "none"; title: string }
  | { kind: "list"; title: string; list: CheatList };

export const EMPTY_STATE: CheatsState = {
  status: null,
  statusError: null,
  seen: null,
  list: null,
  listError: null,
  busy: null,
  message: null,
  engineBusy: false,
};

/** What the section shows for `s`. Never a stale list: only the running game's own cheats. */
export function cheatsPhase(s: CheatsState): CheatsPhase {
  if (!s.status) return s.statusError ? { kind: "error", text: s.statusError } : { kind: "loading" };
  const game = s.status.game;
  if (!game) return s.seen ? { kind: "exited", title: s.seen } : { kind: "no_game" };
  const title = game.title_id;
  if (s.list && s.list.title === title) {
    return s.list.mods.length ? { kind: "list", title, list: s.list } : { kind: "none", title };
  }
  if (s.listError) return { kind: "error", text: s.listError };
  return { kind: "loading" };
}

/** A failed read as one line; null for a 401 (the reload banner says it). */
export function loadErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    if (err.status === 401) return null;
    if (isOffline(err)) return t("cw_home_err_offline");
    return t("cw_cheats_err_load", { code: err.code });
  }
  return t("cw_cheats_err_load", { code: err instanceof Error ? err.message : String(err) });
}

/** A failed toggle as one line; null when the page says it some other way (game gone, 401). */
export function toggleErrorText(err: unknown): string | null {
  if (err instanceof ApiError) {
    if (err.status === 401 || err.code === "game_not_running") return null;
    if (isOffline(err)) return t("cw_home_err_offline");
    if (err.code === "module_missing") return t("cw_cheats_err_module");
    if (err.code === "game_busy") return t("cw_cheats_err_busy");
    if (err.code === "wrong_version") return t("cw_cheats_err_version");
    if (err.code === "bad_index" || err.code === "no_cheats") return t("cw_cheats_err_changed");
    return t("cw_cheats_err_toggle", { code: err.code });
  }
  return t("cw_cheats_err_toggle", { code: err instanceof Error ? err.message : String(err) });
}

const str = (v: unknown): string => (typeof v === "string" ? v : "");

/** The status reply, checked (a helper that answered nonsense reads as "no game"). */
export function readStatus(r: unknown): CheatsStatus {
  const o = (r ?? {}) as { engine?: unknown; game?: unknown };
  const g = o.game as { title_id?: unknown; held?: unknown } | null | undefined;
  const title = g && typeof g.title_id === "string" && g.title_id ? g.title_id : null;
  const game = title ? (g?.held === true ? { title_id: title, held: true } : { title_id: title }) : null;
  return { engine: o.engine === true, game };
}

/** The list reply, checked: entries without an index or a name are dropped. */
export function readList(title: string, r: unknown): CheatList {
  const o = (r ?? {}) as { mods?: unknown; truncated?: unknown };
  const mods: CheatMod[] = [];
  if (Array.isArray(o.mods)) {
    for (const m of o.mods as Record<string, unknown>[]) {
      if (!m || typeof m.index !== "number" || !Number.isInteger(m.index) || !str(m.name)) continue;
      mods.push({
        index: m.index,
        name: str(m.name),
        desc: str(m.desc),
        button: m.button === true,
        on: m.on === true,
        ...(m.other_version === true ? { otherVersion: true } : {}),
        ...(typeof m.applied === "string" && APPLIED.includes(m.applied)
          ? { applied: m.applied as CheatApplied, ...(str(m.applied_reason) ? { appliedReason: str(m.applied_reason) } : {}) }
          : {}),
      });
    }
  }
  return { title, mods, truncated: o.truncated === true };
}

export interface CheatsController {
  /** Read the status; a new game's cheats are read with it. */
  loadStatus(): Promise<void>;
  /** Read the running game's cheats again. */
  loadList(): Promise<void>;
  /** Switch `mod` (a one-shot cheat is applied); ignored while another switch runs. */
  toggle(mod: CheatMod): Promise<void>;
  /** Turn the helper's cheat engine on or off (it remembers it across restarts). */
  setEngine(on: boolean): Promise<void>;
  /** Create / coming back: the status and the cheats again. */
  refresh(): Promise<void>;
  /** Stop reporting (the section closed). */
  dispose(): void;
  get(): CheatsState;
}

export function createCheatsController(onChange: (s: CheatsState) => void): CheatsController {
  let s: CheatsState = EMPTY_STATE;
  let live = true;
  /** Every list read gets a number; only the newest one's answer is kept. */
  let listSeq = 0;

  const set = (patch: Partial<CheatsState>) => {
    if (!live) return;
    s = { ...s, ...patch };
    onChange(s);
  };
  const title = () => s.status?.game?.title_id ?? null;

  const loadList = async () => {
    const tid = title();
    if (!tid) return;
    const seq = ++listSeq;
    try {
      const r = await call<unknown>("/api/cheats/list", { title_id: tid });
      // A switch running now re-reads when it ends: this older answer would flash its old state.
      if (seq !== listSeq || title() !== tid || s.busy !== null) return;
      set({ list: readList(tid, r), listError: null });
    } catch (e) {
      if (seq !== listSeq || title() !== tid) return;
      // A list already shown stays (a failed refresh); the first read's failure is the page's error.
      set({ listError: s.list?.title === tid ? null : (loadErrorText(e) ?? s.listError) });
    }
  };

  const loadStatus = async () => {
    let next: CheatsStatus;
    try {
      next = readStatus(await call<unknown>("/api/cheats/status"));
    } catch (e) {
      set({ statusError: loadErrorText(e) ?? s.statusError });
      return;
    }
    const before = title();
    const now = next.game?.title_id ?? null;
    if (now === before) {
      set({ status: next, statusError: null });
      // A cheat waiting for the game to load: read the list again to show when it applies.
      if (now && s.list?.title === now && s.list.mods.some((m) => m.applied === "waiting")) await loadList();
      return;
    }
    // Another game, or none: the old switches go at once (never toggle a game that is not running).
    listSeq++;
    set({ status: next, statusError: null, list: null, listError: null, message: null, seen: now ?? s.seen });
    if (now) await loadList();
  };

  const toggle = async (mod: CheatMod) => {
    const tid = title();
    if (s.busy !== null || !tid || s.list?.title !== tid) return;
    const on = mod.button ? true : !mod.on;
    set({ busy: mod.index, message: null });
    let gone = false;
    try {
      const r = await call<{ on?: unknown }>("/api/cheats/toggle", { title_id: tid, index: mod.index, on });
      // Show the switch's new state at once; the re-read below confirms it.
      if (!mod.button && s.list?.title === tid) {
        const done = r && typeof r.on === "boolean" ? r.on : on;
        // A switch that went through applied it (or took it off): its status starts over.
        const next = (m: CheatMod): CheatMod => {
          const { applied: _a, appliedReason: _r, ...rest } = m;
          return done ? { ...rest, on: true, applied: "applied" } : { ...rest, on: false };
        };
        set({ list: { ...s.list, mods: s.list.mods.map((m) => (m.index === mod.index ? next(m) : m)) } });
      }
    } catch (e) {
      gone = e instanceof ApiError && e.code === "game_not_running";
      set({ message: toggleErrorText(e) });
    }
    if (gone && s.status && title() === tid) {
      // Said at once (the "closed" screen), not after the next status read: the helper dropped its
      // running-game cache with this answer, so the read below confirms it (or brings the game back).
      listSeq++;
      set({ busy: null, status: { ...s.status, game: null }, seen: tid, list: null, listError: null });
      await loadStatus();
      return;
    }
    set({ busy: null });
    if (gone) await loadStatus();
    else await loadList();
  };

  const setEngine = async (on: boolean) => {
    if (s.engineBusy) return;
    set({ engineBusy: true, message: null });
    try {
      await call("/api/cheats/engine", { enabled: on });
    } catch (e) {
      set({ message: toggleErrorText(e) });
    }
    set({ engineBusy: false });
    await loadStatus();
  };

  return {
    loadStatus,
    loadList,
    toggle,
    setEngine,
    refresh: async () => {
      const before = title();
      await loadStatus();
      // loadStatus read the list already when the game changed.
      if (before && title() === before) await loadList();
    },
    dispose: () => {
      live = false;
    },
    get: () => s,
  };
}
