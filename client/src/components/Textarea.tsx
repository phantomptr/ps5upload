import { useId } from "react";

export interface TextareaProps
  extends Omit<
    React.TextareaHTMLAttributes<HTMLTextAreaElement>,
    "className"
  > {
  label?: string;
  error?: string;
  hint?: string;
  block?: boolean;
  className?: string;
}

/**
 * Labelled `<textarea>`. Same label/error/hint pattern as Input.
 * Uses the `.input` class so it matches Input borders and focus styles.
 */
export function Textarea({
  label,
  error,
  hint,
  block = true,
  className = "",
  id,
  ...rest
}: TextareaProps) {
  const autoId = useId();
  const textareaId = id ?? autoId;
  const hintId = `${textareaId}-hint`;
  const errorId = `${textareaId}-error`;

  const describedBy = [error ? errorId : null, hint ? hintId : null]
    .filter(Boolean)
    .join(" ") || undefined;

  return (
    <div className={block ? "w-full" : ""}>
      {label && (
        <label
          htmlFor={textareaId}
          className="mb-1 block text-xs font-medium text-[var(--color-text)]"
        >
          {label}
        </label>
      )}
      <textarea
        id={textareaId}
        aria-invalid={error ? "true" : undefined}
        aria-describedby={describedBy}
        className={["input resize-y", error ? "border-[var(--color-bad)]" : "", className]
          .filter(Boolean)
          .join(" ")}
        {...rest}
      />
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
