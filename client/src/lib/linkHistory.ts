/**
 * How a console's connection has been holding up, as the engine records it from the app's own
 * status checks (`GET /api/ps5/link-history`, engine `link_drops.rs`): drops, recoveries, a
 * helper that started over, USB drives appearing, the console's link (cable or Wi-Fi) once known,
 * and a hint when the connection keeps dropping.
 *
 * Read from the engine on this computer only: asking it sends nothing to the console.
 */

import { getEngineUrl } from "../state/engine";
import { hostOf } from "./addr";

export type LinkHintKind = "usb_wifi" | "keeps_dropping";

export interface LinkHint {
  kind: LinkHintKind;
  /** Drops in this episode. */
  drops: number;
  /** The episode's first drop (Unix ms): a dismissal is keyed on it, so a new episode shows again. */
  since_ms: number;
  last_ms: number;
  drive_at_ms?: number;
}

export interface LinkHistory {
  events: Record<string, unknown>[];
  link: { kind: "wired" | "wifi" | "vpn" | "virtual" | "unknown"; iface?: string; at_ms: number } | null;
  hint: LinkHint | null;
  up: boolean | null;
}

/** The console's record. `readLink` asks its helper for the link when it is not known yet (one
 *  call to the console; the bug report wants it). Throws when the engine does not answer, or
 *  answers 404 (an engine older than this record). */
export async function fetchLinkHistory(
  host: string,
  opts: { readLink?: boolean; timeoutMs?: number } = {},
): Promise<LinkHistory> {
  const q = new URLSearchParams({ addr: hostOf(host) || host });
  if (opts.readLink) q.set("read_link", "1");
  const res = await fetch(`${getEngineUrl()}/api/ps5/link-history?${q.toString()}`, {
    signal: AbortSignal.timeout(opts.timeoutMs ?? 3_000),
  });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as LinkHistory;
}

/** The hint to show for a console now, or null: none from the engine, the console asleep (its
 *  helper is gone by design, not dropping), or the person dismissed this episode. */
export function hintToShow(
  history: LinkHistory | null | undefined,
  opts: { dismissedSince?: number; resting?: boolean },
): LinkHint | null {
  const h = history?.hint;
  if (!h || opts.resting) return null;
  if (h.kind !== "usb_wifi" && h.kind !== "keeps_dropping") return null;
  if (opts.dismissedSince !== undefined && opts.dismissedSince === h.since_ms) return null;
  return h;
}
