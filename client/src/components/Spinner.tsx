import { Loader2 } from "lucide-react";

export interface SpinnerProps {
  size?: number;
  /** Override the default muted color, e.g. "text-[var(--color-accent)]". */
  tone?: "muted" | "accent" | "inherit";
  className?: string;
}

const TONE_CLASS: Record<NonNullable<SpinnerProps["tone"]>, string> = {
  muted: "text-[var(--color-muted)]",
  accent: "text-[var(--color-accent)]",
  inherit: "",
};

/**
 * Centralized spinner so size and color are consistent across the app.
 * Decorative — the surrounding `aria-busy="true"` conveys the loading
 * state to assistive tech, so the icon itself is hidden from the a11y tree.
 *
 * Standard sizes: 12 (inline-text), 14 (default-ish), 16 (component default),
 * 20 (standalone), 32 (screen-center). Avoid odd values (11, 13, 15) —
 * they produce sub-pixel artifacts on retina displays.
 */
export function Spinner({ size = 16, tone = "muted", className = "" }: SpinnerProps) {
  return (
    <Loader2
      size={size}
      aria-hidden="true"
      className={`animate-spin ${TONE_CLASS[tone]} ${className}`}
    />
  );
}
