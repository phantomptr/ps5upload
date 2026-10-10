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
 * Every variant is a row of chips (active = white filled pill, inactive =
 * hairline outline); the variant only sets spacing:
 *   underline — page-level tabs (Logs, Payloads); a little more room
 *   pills     — Game Hub style
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
    underline: "flex flex-wrap items-center gap-2",
    pills: "flex flex-wrap items-center gap-1.5",
    segmented: "inline-flex flex-wrap items-center gap-1.5",
  };

  const tabCls = (disabled: boolean): string => {
    const base = "chip gap-1.5 whitespace-nowrap";
    const sizing =
      size === "md" ? "min-h-9 px-4 text-sm" : "min-h-8 px-3 text-xs";
    if (disabled) return `${base} ${sizing} opacity-50 cursor-not-allowed`;
    return `${base} ${sizing}`;
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
            className={tabCls(!!tab.disabled)}
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
