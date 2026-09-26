import { useId } from "react";

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectProps
  extends Omit<
    React.SelectHTMLAttributes<HTMLSelectElement>,
    "className"
  > {
  label?: string;
  error?: string;
  hint?: string;
  options?: SelectOption[];
  block?: boolean;
  className?: string;
}

/**
 * Labelled native `<select>`. Native selects give us the best a11y and
 * the best mobile UX (the OS-native picker) for free.
 *
 * Styled via the `.select` class (sibling to `.input` in index.css).
 * Same label/error/hint/id pattern as Input.
 */
export function Select({
  label,
  error,
  hint,
  options,
  block = true,
  className = "",
  id,
  children,
  ...rest
}: SelectProps) {
  const autoId = useId();
  const selectId = id ?? autoId;
  const hintId = `${selectId}-hint`;
  const errorId = `${selectId}-error`;

  const describedBy = [error ? errorId : null, hint ? hintId : null]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <div className={block ? "w-full" : ""}>
      {label && (
        <label
          htmlFor={selectId}
          className="mb-1 block text-xs font-medium text-[var(--color-text)]"
        >
          {label}
        </label>
      )}
      <select
        id={selectId}
        aria-invalid={error ? "true" : undefined}
        aria-describedby={describedBy}
        className={["select", error ? "border-[var(--color-bad)]" : "", className]
          .filter(Boolean)
          .join(" ")}
        {...rest}
      >
        {/* Allow caller-provided <option> children OR the `options` prop. */}
        {children ??
          (options ?? []).map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
      </select>
      {error ? (
        <p id={errorId} className="mt-1 text-xs text-[var(--color-bad)]">
          {error}
        </p>
      ) : hint ? (
        <p id={hintId} className="mt-1 text-xs text-[var(--color-muted)]">
          {hint}
        </p>
      ) : null}
    </div>
  );
}
