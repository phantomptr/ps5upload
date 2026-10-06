// Which fixes to offer when the console cannot reach this computer on Windows (F2.1).
// The engine reads the facts (adapter, network category, firewall rule); this decides what the
// card offers. Pure, so both rules are tested without a Windows machine.
import type { NetDiag } from "../api/ps5";

export interface NetworkFixOffers {
  /** Open Settings so the person can make this network Private (only a Public one). */
  makePrivate: boolean;
  /** A profile to add an inbound ps5upload rule for, after confirmation; null = none. */
  allowProfile: "public" | "private" | null;
}

export function networkFixOffers(diag: NetDiag | null | undefined): NetworkFixOffers {
  const none: NetworkFixOffers = { makePrivate: false, allowProfile: null };
  if (!diag) return none;
  // Windows Firewall is off for this profile, or a rule already allows ps5upload: the cause is
  // somewhere else, and a fix that cannot help is not offered.
  const windowsBlocks = diag.firewall_enabled !== false && diag.allowed_by_rule !== true;
  if (!windowsBlocks) return none;
  if (diag.category === "public") return { makePrivate: true, allowProfile: "public" };
  if (diag.category === "private") return { makePrivate: false, allowProfile: "private" };
  return none;
}

/** True when an install error says the console could not connect to this computer to stream
 *  (the engine's reach failure, or Sony's never-fetched network codes). */
export function isStreamUnreachableError(raw: string | null | undefined): boolean {
  return (
    !!raw &&
    /cannot connect to this computer|never reached this computer|0x80431064|0x80431068|0x8041013d/i.test(raw)
  );
}

/** The offers when the engine could not read Windows' network state (no diagnosis): a direct
 *  cable or Internet Connection Sharing link is almost always a Public network, so allowing
 *  ps5upload on Public networks is the fix that helps. Only on the Windows desktop app, where
 *  the engine runs on this computer. */
export function fallbackNetworkFixOffers(windowsDesktop: boolean): NetworkFixOffers {
  return windowsDesktop
    ? { makePrivate: false, allowProfile: "public" }
    : { makePrivate: false, allowProfile: null };
}
