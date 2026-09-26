import type { LucideIcon } from "lucide-react";

export type BadgeTone =
  | "neutral"
  | "accent"
  | "good"
  | "warn"
  | "bad"
  | "ps4"
  | "ps5";

export interface BadgeProps {
  children: React.ReactNode;
  tone?: BadgeTone;
  size?: "sm" | "md";
  variant?: "soft" | "solid" | "outline";
  dot?: boolean;
  icon?: LucideIcon;
  className?: string;
  title?: string;
}

/**
 * Compact status pill. Replaces the inline `<span className="badge ...">`
 * snippets scattered across screens with a single typed component.
 *
 * Tones map to semantic colors:
 *   neutral — default, muted background
 *   accent  — brand accent
 *   good    — success / online / installed
 *   warn    — caution / partial
 *   bad     — error / offline / failed
 *   ps4/ps5 — platform tags
 *
 * Variants:
 *   soft    — tinted background (default; readable on cards)
 *   solid   — filled (use sparingly; high contrast)
 *   outline — border only (low emphasis, dense rows)
 */
export function Badge({
  children,
  tone = "neutral",
  size = "sm",
  variant = "soft",
  dot = false,
  icon: Icon,
  className = "",
  title,
}: BadgeProps) {
  const sizeCls =
    size === "md"
      ? "px-2 py-0.5 text-xs"
      : "px-1.5 py-0.5 text-[0.625rem] leading-tight";

  const toneColor = {
    neutral: "var(--color-muted)",
    accent: "var(--color-accent)",
    good: "var(--color-good)",
    warn: "var(--color-warn)",
    bad: "var(--color-bad)",
    ps4: "var(--color-ps4, #003791)",
    ps5: "var(--color-ps5, #0070cc)",
  }[tone];

  const variantCls = {
    soft: "",
    solid: "",
    outline: "",
  }[variant];

  // Compute background/foreground per variant+tone.
  const style: React.CSSProperties = { color: toneColor };
  if (variant === "soft") {
    style.color = toneColor;
    style.background = `color-mix(in srgb, ${toneColor} 14%, transparent)`;
  } else if (variant === "solid") {
    style.color = "var(--color-accent-contrast)";
    style.background = toneColor;
  } else {
    // outline
    style.border = `1px solid ${toneColor}`;
    style.background = "transparent";
  }

  return (
    <span
      title={title}
      className={[
        "inline-flex items-center gap-1 rounded-full font-medium whitespace-nowrap",
        sizeCls,
        variantCls,
        className,
      ]
        .filter(Boolean)
        .join(" ")}
      style={style}
    >
      {dot && (
        <span
          aria-hidden="true"
          className="inline-block h-1.5 w-1.5 rounded-full"
          style={{ background: variant === "solid" ? "currentColor" : toneColor }}
        />
      )}
      {Icon && <Icon size={size === "md" ? 12 : 10} aria-hidden="true" />}
      {children}
    </span>
  );
}
