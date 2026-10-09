// The game page's decisions, kept pure: how old a console's state is, what each console's row
// offers, and the one-line summary of where the game is.

import type { CollectionLocation, CollectionOffer } from "../api/collection";
import type { ConsoleEntry, GameView } from "../api/games";
import { sendKind } from "./collectionSend";

type Tr = (key: string, vars: Record<string, string | number> | undefined, fallback: string) => string;

/** An install, update or DLC acts on what the console has: a saved state older than this is
 *  read again first. */
export const REREAD_MS = 10 * 60_000;

export function needsReread(readAtSec: number, nowMs: number): boolean {
  return nowMs - readAtSec * 1000 > REREAD_MS;
}

/** "now" for the connected console, "as of 5 min ago" for a saved read, "Not checked yet". */
export function ageLabel(readAtSec: number | null, nowMs: number, live: boolean, tr: Tr): string {
  if (live) return tr("game_age_now", undefined, "now");
  if (readAtSec === null) return tr("game_age_never", undefined, "Not checked yet");
  const s = Math.max(0, Math.floor(nowMs / 1000 - readAtSec));
  if (s < 60) return tr("game_age_just_now", undefined, "as of just now");
  if (s < 3600) return tr("game_age_min", { n: Math.floor(s / 60) }, "as of {n} min ago");
  if (s < 86400) return tr("game_age_h", { n: Math.floor(s / 3600) }, "as of {n} h ago");
  return tr("game_age_days", { n: Math.floor(s / 86400) }, "as of {n} days ago");
}

export type RowAction =
  | { kind: "play" }
  | { kind: "install"; offers: CollectionOffer[] }
  | { kind: "update"; offer: CollectionOffer }
  | { kind: "dlc"; offers: CollectionOffer[] }
  | { kind: "send" };

/** What a console's row offers: Play where it is installed on the connected console, the
 *  packages the drives could bring it (base with its update and DLC, a newer update, missing
 *  DLC), or Send when the drives hold it only as a folder, image or archive. */
export function rowActions(
  e: ConsoleEntry | undefined,
  copies: CollectionLocation[],
  connected: boolean,
): RowAction[] {
  const out: RowAction[] = [];
  const installed = !!e?.installed;
  if (installed && connected) out.push({ kind: "play" });
  if (e && !installed && e.base) {
    out.push({
      kind: "install",
      offers: [e.base, ...(e.update ? [e.update] : []), ...e.dlc_missing],
    });
    return out;
  }
  if (installed && e?.update) out.push({ kind: "update", offer: e.update });
  if (installed && e && e.dlc_missing.length > 0) out.push({ kind: "dlc", offers: e.dlc_missing });
  if (!installed && copies.some((c) => sendKind(c) && c.pkg?.complete !== false && !c.pkg?.error)) {
    out.push({ kind: "send" });
  }
  return out;
}

/** "Installed on Pro (01.004) · not on Phat · 2 copies on your drives". */
export function summaryLine(view: GameView, names: Record<string, string>, tr: Tr): string {
  const parts = view.consoles.map((c) => {
    const name = names[c.host] ?? c.host;
    if (!c.installed) return tr("game_summary_not_on", { name }, "not on {name}");
    return c.version
      ? tr("game_summary_on_v", { name, v: c.version }, "Installed on {name} ({v})")
      : tr("game_summary_on", { name }, "Installed on {name}");
  });
  const n = view.copies.length;
  if (n === 1) parts.push(tr("game_summary_copy", undefined, "1 copy on your drives"));
  else if (n > 1) parts.push(tr("game_summary_copies", { n }, "{n} copies on your drives"));
  return parts.join(" · ");
}

const TITLE_ID = /^[A-Z]{4}\d{5}$/;

/** A PlayStation title ID (`PPSA01234`, `CUSA00900`), the key consoles know a game by. */
export const hasTitleId = (id: string): boolean => TITLE_ID.test(id);

/** `UP0000-PPSA01234_00-ASTRO0000000000` → `PPSA01234`. */
export function titleIdFromContentId(contentId: string | null | undefined): string | null {
  const id = contentId?.slice(7, 16) ?? "";
  return TITLE_ID.test(id) ? id : null;
}

/** The game page's URL. */
export const gamePath = (titleId: string): string => `/games/${encodeURIComponent(titleId)}`;
