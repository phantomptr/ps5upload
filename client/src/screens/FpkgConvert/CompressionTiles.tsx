// The compression choice as three tiles, each with what it costs for this game.

import type { FpkgCompression, FpkgEstimate } from "../../api/fpkg";
import { useTr } from "../../state/lang";
import { prettyDuration } from "./RunCard";

/** Decimal gigabytes, as the PS5's own storage screen counts. */
function gb(bytes: number): string {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${(bytes / 1e6).toFixed(0)} MB`;
}

const TILES: { value: FpkgCompression; icon: string; key: string; label: string }[] = [
  { value: "fast", icon: "⚡", key: "fpkg.compressionFast", label: "Fast" },
  { value: "balanced", icon: "⚖️", key: "fpkg.compressionBalanced", label: "Balanced" },
  { value: "smallest", icon: "🗜️", key: "fpkg.compressionSmallest", label: "Smallest" },
];

export interface CompressionTilesProps {
  value: FpkgCompression;
  onChange: (value: FpkgCompression) => void;
  /** This game's estimates; "pending" while they are worked out; null/undefined when there is
   *  no game yet or the estimate failed (shown as a dash, never as a spinner that never ends). */
  estimates: Record<FpkgCompression, FpkgEstimate> | "pending" | null | undefined;
  disabled?: boolean;
}

export function CompressionTiles({ value, onChange, estimates, disabled }: CompressionTilesProps) {
  const tr = useTr();
  return (
    <div
      role="radiogroup"
      aria-label={tr("fpkg.compression", undefined, "Compression")}
      className="grid grid-cols-1 gap-2 sm:grid-cols-3"
    >
      {TILES.map((t) => {
        const checked = t.value === value;
        const e = estimates && estimates !== "pending" ? estimates[t.value] : undefined;
        return (
          <button
            key={t.value}
            type="button"
            role="radio"
            aria-checked={checked ? "true" : "false"}
            disabled={disabled}
            onClick={() => onChange(t.value)}
            className={`flex flex-col items-start gap-0.5 rounded-lg border p-3 text-left text-sm transition-colors disabled:opacity-60 ${
              checked
                ? "border-[var(--color-accent)] bg-[var(--color-accent-soft,transparent)]"
                : "border-[var(--color-border)] hover:border-[var(--color-accent)]"
            }`}
          >
            <span className="font-medium">
              <span aria-hidden>{t.icon}</span> {tr(t.key, undefined, t.label)}
              {t.value === "balanced" && (
                <span className="ml-1.5 text-xs font-normal text-[var(--color-muted)]">
                  {tr("fpkg.recommended", undefined, "Recommended")}
                </span>
              )}
            </span>
            <span className="text-xs text-[var(--color-muted)]">
              {e
                ? `~${gb(e.bytes)} · ~${prettyDuration(e.seconds * 1000)}`
                : estimates === "pending"
                  ? tr("fpkg.estimating", undefined, "Estimating…")
                  : "—"}
            </span>
          </button>
        );
      })}
    </div>
  );
}
