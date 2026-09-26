export interface IconButtonProps
  extends Omit<
    React.ButtonHTMLAttributes<HTMLButtonElement>,
    "className" | "children" | "aria-label"
  > {
  "aria-label": string;
  variant?: "ghost" | "secondary" | "danger";
  size?: "sm" | "md";
  active?: boolean;
  children: React.ReactNode;
  className?: string;
}

/**
 * Square icon-only button. Always requires `aria-label` — an icon button
 * without an accessible name is invisible to screen readers.
 *
 *   sm = 36px hit area (inline rows, headers)
 *   md = 44px hit area (default, meets WCAG 2.5.5 target size)
 *
 * `active` renders a "pressed" visual (e.g. a pinned filter button) but
 * does NOT use `aria-pressed` — if the control is genuinely a toggle,
 * use Toggle or a role="checkbox" element instead.
 */
export function IconButton({
  "aria-label": ariaLabel,
  variant = "ghost",
  size = "md",
  active = false,
  children,
  disabled,
  type = "button",
  className = "",
  ...rest
}: IconButtonProps) {
  if (import.meta.env.DEV && !ariaLabel) {
    console.warn(
      "[IconButton] rendered without aria-label — icon buttons must have an accessible name.",
    );
  }

  // `md` is already 44px. `sm` is 36px, which is fine for a mouse but
  // under the touch floor (mobile-design §4.1) — grow it below md, the
  // breakpoint the rest of the app uses to mean "mobile".
  const sizing =
    size === "md" ? "h-11 w-11" : "h-9 w-9 max-md:h-11 max-md:w-11";

  const variants: Record<NonNullable<IconButtonProps["variant"]>, string> = {
    ghost:
      "border border-transparent text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
    secondary:
      "border border-[var(--color-border)] bg-[var(--color-surface)] text-[var(--color-text)] hover:bg-[var(--color-surface-3)]",
    danger:
      "border border-[var(--color-bad)] bg-[var(--color-surface)] text-[var(--color-bad)] hover:bg-[var(--color-bad-soft)]",
  };

  const activeCls = active
    ? "bg-[var(--color-surface-3)] text-[var(--color-text)]"
    : "";

  return (
    <button
      type={type}
      aria-label={ariaLabel}
      disabled={disabled}
      className={[
        "inline-flex shrink-0 items-center justify-center rounded-md",
        "transition-colors disabled:cursor-not-allowed disabled:opacity-50",
        sizing,
        variants[variant],
        activeCls,
        className,
      ]
        .filter(Boolean)
        .join(" ")}
      {...rest}
    >
      {children}
    </button>
  );
}
