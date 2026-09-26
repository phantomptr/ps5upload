import { useId } from "react";
import { haptic } from "../lib/haptics";

export interface RadioOption {
  value: string;
  label: React.ReactNode;
  hint?: string;
  disabled?: boolean;
}

export interface RadioGroupProps {
  name: string;
  value: string;
  onChange: (value: string) => void;
  options: RadioOption[];
  label?: string;
  orientation?: "vertical" | "horizontal";
  className?: string;
}

/**
 * Accessible radio group. Renders a `<fieldset><legend>` for the group
 * label, then native `<input type="radio">` per option. Native radios
 * give us arrow-key navigation, tab behavior, and screen-reader support
 * for free.
 *
 * Fires haptic("selection") on change.
 */
export function RadioGroup({
  name,
  value,
  onChange,
  options,
  label,
  orientation = "vertical",
  className = "",
}: RadioGroupProps) {
  const groupId = useId();

  const handleChange = (v: string) => {
    haptic("selection");
    onChange(v);
  };

  const containerCls =
    orientation === "horizontal"
      ? "flex flex-wrap gap-x-4 gap-y-2"
      : "flex flex-col gap-2";

  return (
    <fieldset className={className}>
      {label && (
        <legend className="mb-1 text-xs font-medium text-[var(--color-text)]">
          {label}
        </legend>
      )}
      <div className={containerCls}>
        {options.map((opt) => {
          const radioId = `${groupId}-${opt.value}`;
          const hintId = opt.hint ? `${radioId}-hint` : undefined;
          const checked = value === opt.value;
          return (
            <div key={opt.value} className="flex items-start gap-2">
              <input
                type="radio"
                id={radioId}
                name={name}
                value={opt.value}
                checked={checked}
                disabled={opt.disabled}
                aria-describedby={hintId}
                onChange={() => handleChange(opt.value)}
                className="radio mt-0.5 h-5 w-5"
              />
              <div>
                <label
                  htmlFor={radioId}
                  className="text-sm text-[var(--color-text)]"
                >
                  {opt.label}
                </label>
                {opt.hint && (
                  <p
                    id={hintId}
                    className="mt-0.5 text-xs text-[var(--color-muted)]"
                  >
                    {opt.hint}
                  </p>
                )}
              </div>
            </div>
          );
        })}
      </div>
    </fieldset>
  );
}
