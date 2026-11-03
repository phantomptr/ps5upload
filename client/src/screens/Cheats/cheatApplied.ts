import type { CheatMod } from "../../api/ps5";

type Tr = (
  key: string,
  vars?: Record<string, string | number>,
  fallback?: string,
) => string;

export type CheatAppliedTone = "ok" | "wait" | "warn";

/** The line under a switched-on cheat (#627): what actually happened to it in the running
 *  game, not only the saved switch. null when there is nothing to say: the cheat is off, the
 *  game is not running, or the helper is older than 10.7.2. */
export function cheatAppliedLine(
  m: Pick<
    CheatMod,
    | "on"
    | "type"
    | "applied"
    | "applied_reason"
    | "applied_after_ms"
    | "applied_late"
  >,
  tr: Tr,
): { tone: CheatAppliedTone; text: string } | null {
  if (!m.on || m.type === "button") return null;
  switch (m.applied) {
    case "applied": {
      if (!m.applied_late)
        return {
          tone: "ok",
          text: tr("cheats_applied_ok", undefined, "Applied ✓"),
        };
      // Applied, but maybe too late for a patch the game reads only at start (#627).
      const seconds = lateSeconds(m.applied_after_ms);
      if (seconds === null)
        return {
          tone: "warn",
          text: tr(
            "cheats_applied_late_unseen",
            undefined,
            "Applied after the game had already started — if it has no effect, restart the game with it on.",
          ),
        };
      return {
        tone: "warn",
        text: tr(
          "cheats_applied_late",
          { seconds },
          `Applied ${seconds} s after the game started — if it has no effect, restart the game with it on.`,
        ),
      };
    }
    case "waiting":
      return {
        tone: "wait",
        text: tr(
          "cheats_applied_waiting",
          undefined,
          "Waiting for the game to load…",
        ),
      };
    case "failed": {
      const reason = m.applied_reason || "?";
      return {
        tone: "warn",
        text: tr(
          "cheats_applied_failed",
          { reason },
          `Couldn't apply: ${reason}`,
        ),
      };
    }
    case "changed":
      return {
        tone: "warn",
        text: tr(
          "cheats_applied_changed",
          undefined,
          "The game changed this; not touched",
        ),
      };
    default:
      return null;
  }
}

/** Whole seconds after the game started (at least 1), or null when the start was not seen. */
export function lateSeconds(ms: number | undefined): number | null {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return null;
  return Math.max(1, Math.round(ms / 1000));
}

/** A cheat switched by hand that went through: on is applied now, off has no status. */
export function afterToggle<
  M extends Pick<
    CheatMod,
    "on" | "applied" | "applied_reason" | "applied_after_ms" | "applied_late"
  >,
>(m: M, on: boolean): M {
  const {
    applied: _a,
    applied_reason: _r,
    applied_after_ms: _t,
    applied_late: _l,
    ...rest
  } = m;
  return (
    on ? { ...rest, on: true, applied: "applied" } : { ...rest, on: false }
  ) as M;
}

/** Some switched-on cheat is waiting for the game: worth reading the list again soon. */
export function anyWaiting(mods: Pick<CheatMod, "on" | "applied">[]): boolean {
  return mods.some((m) => m.on && m.applied === "waiting");
}
