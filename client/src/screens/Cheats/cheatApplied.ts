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
  m: Pick<CheatMod, "on" | "type" | "applied" | "applied_reason">,
  tr: Tr,
): { tone: CheatAppliedTone; text: string } | null {
  if (!m.on || m.type === "button") return null;
  switch (m.applied) {
    case "applied":
      return {
        tone: "ok",
        text: tr("cheats_applied_ok", undefined, "Applied ✓"),
      };
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

/** A cheat switched by hand that went through: on is applied now, off has no status. */
export function afterToggle<
  M extends Pick<CheatMod, "on" | "applied" | "applied_reason">,
>(m: M, on: boolean): M {
  const { applied: _a, applied_reason: _r, ...rest } = m;
  return (
    on ? { ...rest, on: true, applied: "applied" } : { ...rest, on: false }
  ) as M;
}

/** Some switched-on cheat is waiting for the game: worth reading the list again soon. */
export function anyWaiting(mods: Pick<CheatMod, "on" | "applied">[]): boolean {
  return mods.some((m) => m.on && m.applied === "waiting");
}
