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
              className="chip h-8 px-3.5 text-sm max-md:h-11"
            >
              {o.label}
            </button>
          );
        })}
      </div>
    </fieldset>
  );
}
