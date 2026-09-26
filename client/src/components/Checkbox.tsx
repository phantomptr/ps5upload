import { useId } from "react";

export interface CheckboxProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: React.ReactNode;
  hint?: string;
  error?: string;
  disabled?: boolean;
  indeterminate?: boolean;
  className?: string;
}

/**
 * Tri-state checkbox for "select all" patterns. `indeterminate` is set
 * on the DOM node via a ref (React can't set it declaratively).
 *
 * Renders a native `<input type="checkbox">` + `<label>` pair with the
 * correct association; accent-color check styling lives in index.css.
 */
export function Checkbox({
  checked,
  onChange,
  label,
  hint,
  error,
  disabled = false,
  indeterminate = false,
  className = "",
}: CheckboxProps) {
  const autoId = useId();
  const cbId = autoId;
  const hintId = `${cbId}-hint`;
  const errorId = `${cbId}-error`;

  const describedBy = [error ? errorId : null, hint ? hintId : null]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <div className={className}>
      {/* The row — not the 20px box — is the touch target. A checkbox
          drawn at 44px looks wrong, so instead the row gets the 44px
          minimum height and the <label htmlFor> makes the whole width of
          it toggle the box (mobile-design §4.1). */}
      <div className="flex items-start gap-2 max-md:min-h-11 max-md:items-center">
        <input
          ref={(el) => {
            if (el) el.indeterminate = indeterminate;
          }}
          id={cbId}
          type="checkbox"
          className="checkbox mt-0.5 h-5 w-5 max-md:mt-0"
          checked={checked}
          disabled={disabled}
          aria-invalid={error ? "true" : undefined}
          aria-describedby={describedBy}
          onChange={(e) => onChange(e.currentTarget.checked)}
        />
        <div className="max-md:flex max-md:min-h-11 max-md:flex-col max-md:justify-center">
          {/* The label carries the min-height, not the row. Only the
              label toggles the box (htmlFor), so padding the row would
              add dead space that looks tappable but isn't. */}
          <label
            htmlFor={cbId}
            className="text-sm text-[var(--color-text)] max-md:flex max-md:min-h-11 max-md:items-center"
          >
            {label}
          </label>
          {hint && (
            <p id={hintId} className="mt-0.5 text-xs text-[var(--color-muted)]">
              {hint}
            </p>
          )}
          {error && (
            <p id={errorId} className="mt-0.5 text-xs text-[var(--color-bad)]">
              {error}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
