import { useId } from "react";
import type { LucideIcon } from "lucide-react";
import { haptic } from "../lib/haptics";

export interface Segment {
  value: string;
  label: string;
  icon?: LucideIcon;
}

export interface SegmentedControlProps {
  segments: Segment[];
  value: string;
  onChange: (value: string) => void;
  ariaLabel?: string;
  className?: string;
}

/**
 * Pill-shaped multi-option toggle. Implements the WAI-ARIA Radiogroup
 * pattern:
 *
 *   - role="radiogroup" on the container
 *   - role="radio" + aria-checked on each segment
 *   - Arrow keys move between segments (NOT Tab — Tab leaves the group)
 *   - Only the checked segment is in the tab order (tabindex=0; others =-1)
 *
 * Use for view-mode toggles (File Browser list/grid, Activity feed
 * kind, etc).
 */
export function SegmentedControl({
  segments,
  value,
  onChange,
  ariaLabel,
  className = "",
}: SegmentedControlProps) {
  const groupId = useId();

  const currentIndex = Math.max(
    0,
    segments.findIndex((s) => s.value === value),
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    let next = -1;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      next = (currentIndex + 1) % segments.length;
    } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      next = (currentIndex - 1 + segments.length) % segments.length;
    } else if (e.key === "Home") {
      next = 0;
    } else if (e.key === "End") {
      next = segments.length - 1;
    }
    if (next >= 0) {
      e.preventDefault();
      haptic("selection");
      onChange(segments[next].value);
      // Move focus to the newly-checked radio.
      const btn = document.getElementById(`${groupId}-${next}`);
      btn?.focus();
    }
  };

  return (
    <div
      role="radiogroup"
      aria-label={ariaLabel}
      className={[
        "inline-flex items-center gap-0.5 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-0.5",
        className,
      ].join(" ")}
    >
      {segments.map((seg, i) => {
        const checked = seg.value === value;
        const Icon = seg.icon;
        return (
          <button
            key={seg.value}
            id={`${groupId}-${i}`}
            type="button"
            role="radio"
            aria-checked={checked}
            tabIndex={checked ? 0 : -1}
            onClick={() => {
              haptic("selection");
              onChange(seg.value);
            }}
            onKeyDown={handleKeyDown}
            className={[
              // max-md:min-h-11 — segments measured 27px on a phone, well
              // under the 44px touch floor (mobile-design §4.1). Desktop
              // density is unchanged.
              "inline-flex items-center gap-1.5 rounded-md px-2.5 py-1 text-xs font-medium transition-colors max-md:min-h-11 max-md:px-4",
              checked
                ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
                : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
            ].join(" ")}
          >
            {Icon && <Icon size={12} aria-hidden="true" />}
            {seg.label}
          </button>
        );
      })}
    </div>
  );
}
