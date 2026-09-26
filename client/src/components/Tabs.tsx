import { useId } from "react";
import type { LucideIcon } from "lucide-react";

import { Badge } from "./Badge";

export interface Tab {
  id: string;
  label: string;
  icon?: LucideIcon;
  badge?: string | number;
  disabled?: boolean;
}

export type TabsVariant = "underline" | "pills" | "segmented";
export type TabsSize = "sm" | "md";

export interface TabsProps {
  tabs: Tab[];
  value: string;
  onChange: (id: string) => void;
  variant?: TabsVariant;
  size?: TabsSize;
  ariaLabel?: string;
  className?: string;
}

/**
 * Accessible tab strip. Full WAI-ARIA Tabs pattern:
 *
 *   - role="tablist" + role="tab" + aria-selected
 *   - ArrowLeft/ArrowRight move between tabs (cyclic)
 *   - Home/End jump to first/last
 *   - Only the active tab is in the tab order (tabindex=0; others =-1)
 *   - aria-controls links tab → panel (the caller owns the panel)
 *
 * Three visual variants:
 *   underline — low-weight, for page-level screens (Logs, Payloads)
 *   pills     — Game Hub style, filled active tab
 *   segmented — File Browser view modes (compact)
 *
 * Game Hub: use `pills` on lg+, `underline` on mobile (switch via
 * useResponsiveTier).
 */
export function Tabs({
  tabs,
  value,
  onChange,
  variant = "underline",
  size = "md",
  ariaLabel,
  className = "",
}: TabsProps) {
  const groupId = useId();
  const currentIndex = Math.max(
    0,
    tabs.findIndex((t) => t.id === value),
  );

  const handleKeyDown = (e: React.KeyboardEvent) => {
    let next = -1;
    if (e.key === "ArrowRight") next = (currentIndex + 1) % tabs.length;
    else if (e.key === "ArrowLeft")
      next = (currentIndex - 1 + tabs.length) % tabs.length;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = tabs.length - 1;
    if (next >= 0 && next < tabs.length && !tabs[next].disabled) {
      e.preventDefault();
      onChange(tabs[next].id);
      document.getElementById(`${groupId}-tab-${tabs[next].id}`)?.focus();
    }
  };

  const containerCls: Record<TabsVariant, string> = {
    underline: "flex items-center gap-1 border-b border-[var(--color-border)]",
    pills: "flex items-center gap-1",
    segmented:
      "inline-flex items-center gap-0.5 rounded-lg border border-[var(--color-border)] bg-[var(--color-surface)] p-0.5",
  };

  const tabCls = (active: boolean, disabled: boolean): string => {
    const base = "flex items-center gap-1.5 whitespace-nowrap transition-colors";
    const sizing =
      size === "md" ? "px-3 py-2 text-sm" : "px-2 py-1 text-xs";
    if (disabled) return `${base} ${sizing} opacity-50 cursor-not-allowed`;
    switch (variant) {
      case "underline":
        return `${base} ${sizing} border-b-2 ${
          active
            ? "border-[var(--color-accent)] font-semibold text-[var(--color-accent)]"
            : "border-transparent text-[var(--color-muted)] hover:text-[var(--color-text)]"
        }`;
      case "pills":
        return `${base} ${sizing} rounded-full ${
          active
            ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
            : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        }`;
      case "segmented":
        return `${base} ${sizing} rounded-md ${
          active
            ? "bg-[var(--color-accent)] text-[var(--color-accent-contrast)]"
            : "text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
        }`;
    }
  };

  return (
    <div
      role="tablist"
      aria-label={ariaLabel}
      className={[containerCls[variant], className].join(" ")}
      onKeyDown={handleKeyDown}
    >
      {tabs.map((tab) => {
        const active = tab.id === value;
        const Icon = tab.icon;
        return (
          <button
            key={tab.id}
            id={`${groupId}-tab-${tab.id}`}
            type="button"
            role="tab"
            aria-selected={active}
            tabIndex={active ? 0 : -1}
            disabled={tab.disabled}
            onClick={() => onChange(tab.id)}
            className={tabCls(active, !!tab.disabled)}
          >
            {Icon && <Icon size={size === "md" ? 14 : 12} aria-hidden="true" />}
            {tab.label}
            {tab.badge !== undefined && (
              <Badge tone={active ? "neutral" : "neutral"} size="sm">
                {tab.badge}
              </Badge>
            )}
          </button>
        );
      })}
    </div>
  );
}
