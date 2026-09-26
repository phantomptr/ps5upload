import { useId } from "react";
import { haptic } from "../lib/haptics";

export interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: React.ReactNode;
  /** Inline content only — rendered inside a `<p>` under the label. */
  hint?: React.ReactNode;
  disabled?: boolean;
  className?: string;
}

/**
 * Accessible switch. Uses `<button role="switch">` semantics — switches
 * are distinct from checkboxes (they represent binary state changes,
 * not selections in a form field).
 *
 *   - 52×28px visual track + thumb (the inner span)
 *   - 44px hit area on touch (WCAG 2.5.5) — the outer button grows to
 *     44px below the md breakpoint. The track and the hit area are
 *     deliberately different elements; merging them silently caps the
 *     target at 28px.
 *   - Space and Enter toggle (native button behavior)
 *   - Fires haptic("selection") on change for touch users
 */
export function Toggle({
  checked,
  onChange,
  label,
  hint,
  disabled = false,
  className = "",
}: ToggleProps) {
  const autoId = useId();
  const labelId = `${autoId}-label`;
  const hintId = `${autoId}-hint`;
  const describedBy = hint ? hintId : undefined;

  const handleClick = () => {
    if (disabled) return;
    haptic("selection");
    onChange(!checked);
  };

  return (
    <div className={["flex items-center gap-3", className].filter(Boolean).join(" ")}>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-labelledby={labelId}
        aria-describedby={describedBy}
        disabled={disabled}
        onClick={handleClick}
        // The BUTTON is the hit area; the inner span is the visible
        // track. They were the same element before, which capped the hit
        // area at the track's 28px height — under the 44px floor this
        // component's own docs claim. Splitting them lets the target grow
        // on touch while the switch keeps its size.
        className={[
          "relative inline-flex shrink-0 items-center justify-center",
          "disabled:cursor-not-allowed disabled:opacity-50",
          "max-md:h-11",
        ].join(" ")}
      >
        <span
          aria-hidden="true"
          className={[
            "relative flex h-7 items-center rounded-full",
            "transition-colors duration-150",
            checked ? "bg-[var(--color-accent)]" : "bg-[var(--color-surface-3)]",
          ].join(" ")}
          style={{ width: "3.25rem" }}
        >
          <span
            aria-hidden="true"
            className={[
              "inline-block h-5 w-5 transform rounded-full bg-white shadow transition-transform duration-150",
              checked ? "translate-x-6" : "translate-x-1",
            ].join(" ")}
          />
        </span>
      </button>
      <div>
        <span id={labelId} className="text-sm text-[var(--color-text)]">
          {label}
        </span>
        {hint && (
          <p id={hintId} className="mt-0.5 text-xs text-[var(--color-muted)]">
            {hint}
          </p>
        )}
      </div>
    </div>
  );
}
