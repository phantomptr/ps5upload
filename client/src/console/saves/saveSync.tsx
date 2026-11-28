// Save sync on this PS5, read only. Save sync runs in the PS5Upload app (or a Docker engine), which
// writes where each game kept in step stands into this console's mesh document "save-sync"
// (engine save_sync_console.rs); the helper's /api/save-sync reads it back. Each engine keeps its
// own entry, with only this console's side: its user, the other console, and codes this page says
// in its own words. Changing anything stays the app's.
import { ArrowLeftRight } from "lucide-react";
import { ApiError, call } from "../api";
import { t } from "../strings";
import { formatDateTime } from "../time";

export interface SyncGame {
  title_id: string;
  /** This console's user (8 hex digits, its home folder). */
  user: string;
  /** The other console, by its name in the app. */
  other: string;
  state: "in_step" | "waiting" | "conflict" | "cant_sync";
  reason: string | null;
  /** The state is about this console (it is the one waited for, or the one with no save yet). */
  here: boolean;
  copied_ms: number | null;
  copied_here: boolean | null;
}

/** One engine's entry: the device it runs on, when it wrote, and its games. */
export interface SyncEntry {
  device: string;
  at: number;
  games: SyncGame[];
}

/** An entry this old says its engine may have stopped. */
export const STALE_MS = 2 * 3_600_000;

const STATES = new Set(["in_step", "waiting", "conflict", "cant_sync"]);

/** The entries in what /api/save-sync answered; anything odd is left out (never a crash). */
export function parseSaveSync(raw: unknown): SyncEntry[] {
  const doc = (raw as { doc?: unknown } | null)?.doc;
  const engines = (doc as { engines?: unknown } | null)?.engines;
  if (!engines || typeof engines !== "object") return [];
  const out: SyncEntry[] = [];
  for (const e of Object.values(engines as Record<string, unknown>)) {
    if (!e || typeof e !== "object") continue;
    const o = e as Record<string, unknown>;
    const games = (Array.isArray(o.games) ? o.games : []).filter(
      (g): g is SyncGame =>
        !!g &&
        typeof g === "object" &&
        typeof (g as SyncGame).title_id === "string" &&
        typeof (g as SyncGame).user === "string" &&
        STATES.has((g as SyncGame).state),
    );
    out.push({
      device: typeof o.name === "string" ? o.name : "",
      at: typeof o.at_ms === "number" ? o.at_ms : 0,
      games,
    });
  }
  return out.sort((x, y) => y.at - x.at);
}

/** Reads it; null when the helper has no /api/save-sync (older) or the read failed. */
export async function loadSaveSync(): Promise<SyncEntry[] | null> {
  try {
    return parseSaveSync(await call<unknown>("/api/save-sync", {}));
  } catch (e) {
    if (e instanceof ApiError) return null;
    throw e;
  }
}

/** The games of `user`, each with the entry it came from. */
export function linesFor(entries: readonly SyncEntry[], user: string | null): { game: SyncGame; entry: SyncEntry }[] {
  if (!user) return [];
  const u = user.toLowerCase();
  const out: { game: SyncGame; entry: SyncEntry }[] = [];
  for (const entry of entries) for (const game of entry.games) if (game.user.toLowerCase() === u) out.push({ game, entry });
  return out;
}

export function badgeText(g: SyncGame): string {
  switch (g.state) {
    case "in_step":
      return t("cw_sync_in_step");
    case "conflict":
      return t("cw_sync_choose");
    case "cant_sync":
      return t("cw_sync_cant_badge");
    default:
      return t("cw_sync_waiting");
  }
}

/** What the game waits for, or why it can't sync, in plain words; null for none. */
export function reasonText(g: SyncGame): string | null {
  const where = g.other;
  if (g.state === "conflict") return t("cw_sync_conflict");
  if (g.state === "in_step") {
    if (!g.copied_ms) return null;
    return g.copied_here
      ? t("cw_sync_last_here", { when: formatDateTime(g.copied_ms) })
      : t("cw_sync_last_there", { console: where, when: formatDateTime(g.copied_ms) });
  }
  switch (g.reason) {
    case "offline":
      return t("cw_sync_wait_offline", { console: where });
    case "settling":
      return t("cw_sync_wait_settling", { console: where });
    case "running":
      return g.here ? t("cw_sync_wait_running_here") : t("cw_sync_wait_running_there", { console: where });
    case "queued":
      return t("cw_sync_wait_queued");
    case "error":
      return t("cw_sync_wait_error");
    case "no_save":
      return g.here ? t("cw_sync_no_save_here") : t("cw_sync_no_save_there", { console: where });
    case "ps4":
      return t("cw_sync_ps4");
    case "failed":
      return t("cw_sync_failed");
    case "refused":
      return t("cw_sync_refused");
    default:
      return null;
  }
}

/** The Save sync card for the user shown; nothing when no game of theirs is kept in step. */
export function SaveSyncCard({
  entries,
  user,
  names,
  now,
}: {
  entries: readonly SyncEntry[] | null;
  user: string | null;
  names: ReadonlyMap<string, string>;
  /** When the entries were read (ms): an entry this much older than STALE_MS is said to be. */
  now: number;
}) {
  const lines = linesFor(entries ?? [], user);
  if (!lines.length) return null;
  const by = (entries ?? []).filter((e) => lines.some((l) => l.entry === e));
  return (
    <section className="cw-card cw-sync" aria-label={t("cw_sync_title")} data-save-sync>
      <div className="cw-sync-head">
        <ArrowLeftRight aria-hidden />
        <span className="cw-game-title">{t("cw_sync_title")}</span>
      </div>
      <p className="cw-dim">{t("cw_sync_hint")}</p>
      <ul className="cw-sync-list">
        {lines.map(({ game: g }) => {
          const why = reasonText(g);
          return (
            <li key={`${g.title_id}|${g.other}`} className="cw-sync-item" data-sync-title={g.title_id}>
              <div>
                <span className="cw-sync-name">{names.get(g.title_id) || g.title_id}</span>
                <span className={`cw-tag${g.state === "in_step" ? " is-on" : ""}`}>{badgeText(g)}</span>
              </div>
              <div className="cw-dim">{t("cw_sync_with", { console: g.other })}</div>
              {why ? <div className="cw-dim">{why}</div> : null}
            </li>
          );
        })}
      </ul>
      {by.map((e) => (
        <p key={`${e.device}|${e.at}`} className="cw-dim cw-sync-foot">
          {now - e.at >= STALE_MS
            ? t("cw_sync_stale", { when: formatDateTime(e.at), device: e.device })
            : t("cw_sync_updated", { when: formatDateTime(e.at), device: e.device })}
        </p>
      ))}
    </section>
  );
}
