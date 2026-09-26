import { useId } from "react";

export interface InputProps
  extends Omit<
    React.InputHTMLAttributes<HTMLInputElement>,
    "className"
  > {
  label?: string;
  labelHint?: string;
  error?: string;
  hint?: string;
  leftIcon?: React.ReactNode;
  rightSlot?: React.ReactNode;
  block?: boolean;
  className?: string;
}

/**
 * Labelled text input. Renders a `<label>` + `<input>` pair with the
 * correct `htmlFor`/`id` association, optional hint text below, and
 * `aria-invalid` + `aria-describedby` wiring when `error` is set.
 *
 * `leftIcon` / `rightSlot` render inside the input box (the input gets
 * extra left/right padding to make room). Use `rightSlot` for inline
 * actions like a "Test" button.
 */
export function Input({
  label,
  labelHint,
  error,
  hint,
  leftIcon,
  rightSlot,
  block = true,
  className = "",
  id,
  ...rest
}: InputProps) {
  const autoId = useId();
  const inputId = id ?? autoId;
  const hintId = `${inputId}-hint`;
  const errorId = `${inputId}-error`;

  const describedBy = [error ? errorId : null, hint ? hintId : null]
    .filter(Boolean)
    .join(" ") || undefined;

  const wrapperCls = block ? "w-full" : "";

  return (
    <div className={wrapperCls}>
      {(label || labelHint) && (
        <div className="mb-1 flex items-baseline justify-between gap-2">
          {label && (
            <label
              htmlFor={inputId}
              className="text-xs font-medium text-[var(--color-text)]"
            >
              {label}
            </label>
          )}
          {labelHint && (
            <span className="text-xs text-[var(--color-muted)]">
              {labelHint}
            </span>
          )}
        </div>
      )}
      <div className="relative flex items-center">
        {leftIcon && (
          <span className="pointer-events-none absolute left-2.5 flex items-center text-[var(--color-muted)]">
            {leftIcon}
          </span>
        )}
        <input
          id={inputId}
          aria-invalid={error ? "true" : undefined}
          aria-describedby={describedBy}
          className={[
            "input",
            // `!` matters: `.input` is defined as UNLAYERED css in
            // index.css and sets the `padding` shorthand, which beats
            // Tailwind's layered padding utilities. Without the
            // important modifier the icon renders on top of the text —
            // measured at 11.25px of left padding against an icon
            // occupying 29-47px. Applies to every leftIcon/rightSlot
            // call site, not just one screen.
            leftIcon ? "pl-9!" : "",
            rightSlot ? "pr-12!" : "",
            error ? "border-[var(--color-bad)]" : "",
            className,
          ]
            .filter(Boolean)
            .join(" ")}
          {...rest}
        />
        {rightSlot && (
          <span className="absolute right-1.5 flex items-center">
            {rightSlot}
          </span>
        )}
      </div>
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
