import type { CheatMod } from "../../api/ps5";
import { useTr } from "../../state/lang";
import { cheatAppliedLine } from "./cheatApplied";

const TONE_CLASS = {
  ok: "text-emerald-400",
  wait: "text-[var(--color-muted)]",
  warn: "text-amber-400",
} as const;

/** The small status line under a cheat's switch (#627). Renders nothing when there is nothing to
 *  say. */
export function CheatAppliedLine({
  mod,
  className = "",
}: {
  mod: Pick<CheatMod, "on" | "type" | "applied" | "applied_reason">;
  className?: string;
}) {
  const tr = useTr();
  const line = cheatAppliedLine(mod, tr);
  if (!line) return null;
  return (
    <p
      className={`text-[11px] leading-snug ${TONE_CLASS[line.tone]} ${className}`}
      data-applied={mod.applied}
    >
      {line.text}
    </p>
  );
}
