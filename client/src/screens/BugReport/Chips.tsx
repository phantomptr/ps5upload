/** A single-choice row of equal-height chips (a radio group): what you were doing, how far back. */
export default function Chips<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
}) {
  return (
    <fieldset>
      <legend className="mb-1.5 block text-xs font-medium text-[var(--color-text)]">{label}</legend>
      <div className="flex flex-wrap gap-2" role="radiogroup" aria-label={label}>
        {options.map((o) => {
          const on = o.value === value;
          return (
            <button
              key={o.value}
              type="button"
              role="radio"
              aria-checked={on}
              onClick={() => onChange(o.value)}
              className={
                "inline-flex h-8 items-center rounded-full border px-3.5 text-sm transition-colors " +
                (on
                  ? "border-[var(--color-accent)] bg-[var(--color-accent-soft)] font-medium text-[var(--color-text)]"
                  : "border-[var(--color-border)] text-[var(--color-muted)] hover:border-[var(--color-border-strong)] hover:text-[var(--color-text)]")
              }
            >
              {o.label}
            </button>
          );
        })}
      </div>
    </fieldset>
  );
}
