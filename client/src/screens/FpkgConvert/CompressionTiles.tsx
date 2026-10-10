// The compression choice as three tiles, each with what it costs for this game.

import { Minimize2, Scale, Zap, type LucideIcon } from "lucide-react";

import type { FpkgCompression, FpkgEstimate } from "../../api/fpkg";
import { useTr } from "../../state/lang";
import { formatBytes } from "../../lib/format";
import { prettyDuration } from "./RunCard";

const gb = formatBytes;

const TILES: { value: FpkgCompression; icon: LucideIcon; key: string; label: string }[] = [
  { value: "fast", icon: Zap, key: "fpkg.compressionFast", label: "Fast" },
  { value: "balanced", icon: Scale, key: "fpkg.compressionBalanced", label: "Balanced" },
  { value: "smallest", icon: Minimize2, key: "fpkg.compressionSmallest", label: "Smallest" },
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
      className="grid grid-cols-1 gap-2 min-[420px]:grid-cols-3"
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
            className={`border rounded-[var(--radius-card)] transition-[background-color,border-color,box-shadow] flex flex-col items-start gap-1 px-3.5 py-3 text-left text-sm disabled:opacity-60 ${
              checked
                ? "border-[color-mix(in_srgb,var(--color-accent)_45%,transparent)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)] ring-1 ring-[color-mix(in_srgb,var(--color-accent)_30%,transparent)]"
                : "border-[var(--color-border)] bg-[var(--color-surface)] hover:bg-[var(--color-surface-raised)]"
            }`}
          >
            <span className="flex w-full flex-wrap items-center gap-x-2 gap-y-1 font-medium">
              <t.icon size={15} aria-hidden className="shrink-0 text-[var(--color-accent-bright)]" />
              {tr(t.key, undefined, t.label)}
              {t.value === "balanced" && (
                <span className="rounded-full bg-[var(--color-accent-soft)] px-2 py-0.5 text-[10px] font-medium text-[var(--color-accent)]">
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
