// ps5upload:// links for a bug report: what the app did with the links it was handed (the
// shell's own count and the scheme's registration come from `open_links_status`). Only the kind
// of each link is kept, never the link: it carries an invite or a phone controller's key.

import { readAppLink } from "./appLinks";

export interface AppLinkDiag {
  /** Links the page took from the shell since it started. */
  taken: number;
  /** Of those, links that led somewhere (a well-formed invite or controller link). */
  routed: number;
  /** "join", "pad", "receive" or "unknown" for the last one taken. */
  last_kind: "join" | "pad" | "receive" | "unknown" | null;
  last_at_ms: number | null;
}

let state: AppLinkDiag = { taken: 0, routed: 0, last_kind: null, last_at_ms: null };

/** Note the links the page took (components/OpenLinks). */
export function noteAppLinks(links: readonly string[], now = Date.now()): void {
  for (const l of links) {
    const kind = readAppLink(l)?.kind ?? "unknown";
    state = {
      taken: state.taken + 1,
      routed: state.routed + (kind === "unknown" ? 0 : 1),
      last_kind: kind,
      last_at_ms: now,
    };
  }
}

export function appLinkDiag(): AppLinkDiag {
  return { ...state };
}

/** Tests only. */
export function resetAppLinkDiag(): void {
  state = { taken: 0, routed: 0, last_kind: null, last_at_ms: null };
}
