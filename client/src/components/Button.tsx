import { Spinner } from "./Spinner";

export type ButtonVariant = "primary" | "secondary" | "ghost" | "danger";

export interface ButtonProps
  extends Omit<
    React.ButtonHTMLAttributes<HTMLButtonElement>,
    "className" | "children"
  > {
  variant?: ButtonVariant;
  size?: "sm" | "md" | "lg";
  loading?: boolean;
  leftIcon?: React.ReactNode;
  rightIcon?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}

/**
 * Shared button primitive. Four variants cover every in-app use:
 *
 *   primary   — the main CTA on a screen; filled accent color.
 *   secondary — neutral action (Refresh, Choose file); bordered.
 *   ghost     — subtle tertiary (Dismiss, icon-only); border-less.
 *   danger    — destructive (Delete, Uninstall); red border.
 *
 * Three sizes:
 *   sm — header actions and inline row actions (default).
 *   md — primary CTAs and dialog confirms.
 *   lg — hero / full-width CTAs (FirstRun, empty-state actions).
 *
 * `loading` swaps the left icon for a spinner and disables the button.
 * `leftIcon` / `rightIcon` give consistent icon spacing without the
 *  caller having to remember to wrap in a flex container.
 *
 * Tailwind classes are composed here so every button in the app
 * ends up with the same hover / disabled / focus treatment — no
 * more remembering `hover:bg-[var(--color-surface-3)]` at each
 * call site.
 */
export function Button({
  variant = "secondary",
  size = "sm",
  loading = false,
  leftIcon,
  rightIcon,
  children,
  disabled,
  type = "button",
  className = "",
  ...rest
}: ButtonProps) {
  // Keyboard focus comes from the global :focus-visible ring in index.css
  // (v3) so buttons match every other interactive element — no per-component
  // ring classes needed.
  // `whitespace-nowrap`: a button is a single-line control. Without it, a
  // button squeezed in a tight flex row (min-width:0 globally) wraps its
  // label vertically, one character per line. If a label is genuinely too
  // long, that's a layout decision for the call site (truncate / give room),
  // never a reason to stack letters.
  const base =
    "inline-flex items-center justify-center gap-1.5 whitespace-nowrap rounded-[var(--radius-control)] font-semibold " +
    "transition-[background-color,border-color,color,transform,box-shadow] active:translate-y-px " +
    "disabled:cursor-not-allowed disabled:opacity-50";

  // `max-md:min-h-11` enforces the 44px touch floor (mobile-design §4.1)
  // below the md breakpoint — the same breakpoint the rest of the app
  // uses to mean "mobile". Without it `md` buttons measured 43px and
  // `sm` buttons 33px on a 448px-wide phone: an audit of every route
  // found this single primitive accounted for most sub-44px targets in
  // the whole app. Desktop density is untouched.
  // min-w matters too: icon-only buttons measured 43px WIDE (and 50 tall),
  // so a height-only floor would have left them failing on the other axis.
  const touchFloor = "max-md:min-h-11 max-md:min-w-11";

  const sizing =
    size === "lg"
      ? "px-5 py-2.5 text-sm"
      : size === "md"
        ? "px-4 py-2 text-sm"
        : "px-3 py-1.5 text-xs";

  const spinnerSize = size === "lg" ? 16 : size === "md" ? 14 : 12;

  const variants: Record<ButtonVariant, string> = {
    primary:
      "border border-transparent bg-[var(--color-accent)] text-[var(--color-accent-contrast)] shadow-sm hover:brightness-110",
    secondary:
      "border border-[var(--color-border)] bg-[var(--color-surface-raised)] text-[var(--color-text)] shadow-sm hover:border-[var(--color-border-strong)] hover:bg-[var(--color-surface-3)]",
    ghost:
      "border border-transparent text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]",
    danger:
      "border border-[var(--color-bad)] bg-[var(--color-surface-raised)] text-[var(--color-bad)] hover:bg-[var(--color-bad-soft)]",
  };

  return (
    <button
      type={type}
      disabled={disabled || loading}
      className={`${base} ${touchFloor} ${sizing} ${variants[variant]} ${className}`}
      {...rest}
    >
      {loading ? (
        <Spinner size={spinnerSize} tone="inherit" />
      ) : (
        leftIcon
      )}
      {/* `truncate` is a safety net, not a layout choice. `whitespace-nowrap`
          above means a button squeezed below its label's width overflows its
          own box — and any ancestor with `overflow-hidden` (every card in the
          app, for its rounded corners) then slices the button through the
          middle of a glyph. Ellipsizing is the graceful version of that, and
          it costs nothing when there is room. Call sites that must never
          truncate should give the button room; see the action row in
          InstalledApps for how. */}
      {children && <span className="truncate">{children}</span>}
      {!loading && rightIcon}
    </button>
  );
}
