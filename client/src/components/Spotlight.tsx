import { type ReactNode, useEffect, useId, useRef } from "react";
import { X } from "lucide-react";
import type { LucideIcon } from "lucide-react";

import { useTr } from "../state/lang";
import { useScrollLock } from "../lib/useScrollLock";
import { haptic } from "../lib/haptics";

/**
 * v5 Spotlight primitive (§22.21).
 *
 * Games-tab hero overlay. Full-screen on mobile, large panel on
 * desktop. Blurred backdrop from `game.iconUrl`. Used both as the
 * game-detail Spotlight (tap a game tile) and as the mobile Command
 * Palette pattern (§17.5).
 *
 * Accessibility:
 *   - role="dialog", aria-modal="true".
 *   - Escape closes; focus trapped; focus restored to trigger.
 *   - Title is labelled via aria-labelledby.
 *   - Actions are buttons with aria-disabled when `disabled`.
 *   - `disabledReason` becomes the button's title attribute.
 */

export interface SpotlightAction {
  label: string;
  icon?: LucideIcon;
  onClick: () => void;
  primary?: boolean;
  disabled?: boolean;
  disabledReason?: string;
}

export interface SpotlightProps {
  game: { title: string; iconUrl?: string };
  actions: SpotlightAction[];
  onClose: () => void;
  /** Optional extra content below the title (description, metadata). */
  children?: ReactNode;
}

export function Spotlight({ game, actions, onClose, children }: SpotlightProps) {
  const tr = useTr();
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement | null>(null);
  const previousFocusRef = useRef<HTMLElement | null>(null);

  useScrollLock(true);

  useEffect(() => {
    if (typeof document !== "undefined") {
      const active = document.activeElement;
      previousFocusRef.current = active instanceof HTMLElement ? active : null;
    }
    queueMicrotask(() => {
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = panel.querySelector<HTMLElement>(
        'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
      );
      (focusable ?? panel).focus({ preventScroll: true });
    });

    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      // Focus trap: Tab / Shift+Tab cycles within the panel.
      if (e.key === "Tab" && panelRef.current) {
        const focusables = panelRef.current.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
        );
        if (focusables.length === 0) return;
        const first = focusables[0];
        const last = focusables[focusables.length - 1];
        if (e.shiftKey && document.activeElement === first) {
          e.preventDefault();
          last.focus();
        } else if (!e.shiftKey && document.activeElement === last) {
          e.preventDefault();
          first.focus();
        }
      }
    };
    window.addEventListener("keydown", handler);
    return () => {
      window.removeEventListener("keydown", handler);
      const prev = previousFocusRef.current;
      previousFocusRef.current = null;
      if (prev && typeof document !== "undefined") {
        queueMicrotask(() => prev.focus({ preventScroll: true }));
      }
    };
  }, [onClose]);

  const primary = actions.find((a) => a.primary && !a.disabled);
  const secondary = actions.filter((a) => !a.primary);

  return (
    <div
      className="anim-scrim fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-0 sm:p-4 md:p-6"
      onClick={onClose}
    >
      {/* Blurred backdrop from game icon */}
      {game.iconUrl && (
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 scale-110 bg-cover bg-center opacity-30 blur-2xl"
          style={{ backgroundImage: `url(${game.iconUrl})` }}
        />
      )}

      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
        className="anim-pop elev-3 relative flex max-h-[100dvh] w-full flex-col overflow-hidden bg-[var(--color-surface-2)] sm:max-h-[90dvh] sm:rounded-xl md:max-w-2xl"
      >
        {/* Header with game icon + title + close */}
        <header className="flex items-center gap-3 border-b border-[var(--color-border)] p-4">
          {game.iconUrl && (
            <img
              src={game.iconUrl}
              alt=""
              className="h-12 w-12 shrink-0 rounded-lg object-cover"
            />
          )}
          <h2
            id={titleId}
            className="min-w-0 flex-1 truncate text-lg font-semibold"
          >
            {game.title}
          </h2>
          <button
            type="button"
            onClick={onClose}
            aria-label={tr("close", undefined, "Close")}
            title={tr("close", undefined, "Close")}
            className="shrink-0 rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
          >
            <X size={20} />
          </button>
        </header>

        {/* Body */}
        {children && (
          <div className="min-h-0 flex-1 overflow-y-auto p-4">
            {children}
          </div>
        )}

        {/* Action bar */}
        {actions.length > 0 && (
          <footer className="flex flex-col gap-2 border-t border-[var(--color-border)] p-4 sm:flex-row sm:items-center sm:justify-end">
            {primary && (
              <button
                type="button"
                disabled={primary.disabled}
                title={primary.disabled ? primary.disabledReason : undefined}
                onClick={() => {
                  if (primary.disabled) return;
                  haptic("confirm");
                  primary.onClick();
                }}
                className="flex items-center justify-center gap-2 rounded-md bg-[var(--color-accent)] px-4 py-2.5 text-sm font-medium text-[var(--color-accent-contrast)] transition-opacity hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
              >
                {primary.icon && <primary.icon size={16} />}
                {primary.label}
              </button>
            )}
            {secondary.map((action, i) => {
              const Icon = action.icon;
              return (
                <button
                  key={`${action.label}-${i}`}
                  type="button"
                  disabled={action.disabled}
                  title={action.disabled ? action.disabledReason : undefined}
                  onClick={() => {
                    if (action.disabled) return;
                    haptic("tap");
                    action.onClick();
                  }}
                  className="flex items-center justify-center gap-2 rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] px-4 py-2.5 text-sm font-medium text-[var(--color-text)] transition-colors hover:bg-[var(--color-surface-3)] disabled:cursor-not-allowed disabled:opacity-50"
                >
                  {Icon && <Icon size={16} />}
                  {action.label}
                </button>
              );
            })}
          </footer>
        )}
      </div>
    </div>
  );
}
