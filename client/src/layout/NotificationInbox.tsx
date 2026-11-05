import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router";
import { ArrowRight, Bell, Tv, X } from "lucide-react";

import { useNotificationsStore } from "../state/notifications";
import { useTr } from "../state/lang";
import { useFitOnScreen } from "../components/useFitOnScreen";
import { NotificationItem, useSeenWhileOpen } from "./notificationList";
import { useBackDismiss } from "../lib/systemBack";

/**
 * Which Tailwind anchor class the panel gets for a given alignment.
 *
 * Exported so the one thing that actually broke on Android is assertable
 * without rendering an open panel: `left-0` for a bell at the RIGHT edge of
 * the window grew a 320px panel off the edge of the display. The width clamp
 * on the panel caps its size but never moves its anchor, so only this can fix
 * it. (Below md the panel is a full-width sheet instead; see PANEL_SHEET.)
 */
export function panelAnchorClass(align: "left" | "right"): string {
  return align === "right" ? "right-0" : "left-0";
}

/** Which way the panel opens from the bell: up from a footer, down from a
 *  top bar. */
export function panelPlacementClass(placement: "above" | "below"): string {
  return placement === "below" ? "top-full mt-2" : "bottom-full mb-1";
}

/** Below md (a phone) the panel is not hung from the bell at all: it is a
 *  sheet fixed under the top bar, 8px from each side, within the safe area,
 *  and never taller than the screen above the tab bar (its list scrolls).
 *  Wherever the bell sits, nothing can be cut off. */
export const PANEL_SHEET =
  "max-md:fixed max-md:inset-x-2 max-md:top-[calc(var(--safe-top)+3.75rem)] max-md:bottom-auto max-md:m-0 max-md:w-auto max-md:max-w-none max-md:max-h-[calc(100vh-var(--safe-top)-var(--safe-bottom)-10rem)]";

/** How many of the latest notifications the bell shows; the rest are on the
 *  Notifications screen. */
export const PEEK_COUNT = 8;

/** The unread ids among the entries the bell shows (the newest PEEK_COUNT). */
export function peekUnreadIds(entries: readonly { id: string; read: boolean }[]): string[] {
  return entries
    .slice(0, PEEK_COUNT)
    .filter((e) => !e.read)
    .map((e) => e.id);
}

/**
 * The bell: a peek at this app's latest notifications.
 *
 * It lists the same notifications as the "This app" tab of the Notifications
 * screen (the newest few), and "See all" opens that tab. Opening it marks the ones
 * it shows read (not older unread ones it does not show), as showing that tab does (see layout/notificationList.tsx). The badge
 * counts this app's unread notifications only: the messages put on the PS5's
 * screen are read from the console, which the bell does not poll; a link at
 * the bottom goes to them.
 */
export default function NotificationInbox({
  align = "left",
  placement = "above",
  variant = "plain",
}: {
  /** Which edge of the bell the panel is anchored to on desktop — which way
   *  it grows. Pick the side away from the nearest window edge. */
  align?: "left" | "right";
  /** Opens up (footers, the default) or down (a top bar). */
  placement?: "above" | "below";
  /** "plain" is the quiet bell of the phone top bar; "orb" the white round
   *  button of the desktop top bar. */
  variant?: "plain" | "orb";
} = {}) {
  const tr = useTr();
  const navigate = useNavigate();
  const entries = useNotificationsStore((s) => s.entries);
  const markRead = useNotificationsStore((s) => s.markRead);
  const remove = useNotificationsStore((s) => s.remove);
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const fit = useFitOnScreen(panelRef, open);

  const unread = entries.filter((e) => !e.read).length;
  const peek = entries.slice(0, PEEK_COUNT);
  // Only what the bell shows is marked read: unread entries past the newest
  // PEEK_COUNT stay unread (and keep the badge) until the Notifications screen
  // shows them.
  const seen = useSeenWhileOpen(open, peekUnreadIds(entries), markRead);
  useBackDismiss(open, () => setOpen(false));

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (wrapperRef.current && !wrapperRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setOpen(false);
        triggerRef.current?.focus();
      }
    }
    document.addEventListener("mousedown", onClick, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onClick, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const go = (to: string) => {
    setOpen(false);
    navigate(to);
  };
  const unreadLabel = tr("notifications_unread_count", { count: unread }, "Unread notifications: {count}");

  return (
    <div ref={wrapperRef} className="relative">
      <button
        ref={triggerRef}
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-label={tr("notifications_open", undefined, "Open notifications")}
        aria-expanded={open}
        aria-haspopup="dialog"
        // Under md the hit area grows to the 44px touch floor.
        className={
          variant === "orb"
            ? "icon-orb-button relative"
            : "relative flex items-center justify-center rounded-full p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] max-md:h-11 max-md:w-11"
        }
      >
        <Bell size={18} />
        {unread > 0 && (
          <span
            data-testid="notifications-badge"
            className="absolute -end-0.5 -top-0.5 inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-[var(--color-bad)] px-1 text-[10px] font-semibold tabular-nums text-[var(--color-accent-contrast)]"
            title={unreadLabel}
          >
            {unread > 9 ? "9+" : unread}
          </span>
        )}
      </button>

      {open && (
        <div
          ref={panelRef}
          role="dialog"
          aria-label={tr("notifications_title", undefined, "Notifications")}
          data-testid="notifications-panel"
          className={`anim-rise glass-float elev-2 absolute flex w-[22rem] max-w-[calc(100vw-1rem)] flex-col overflow-hidden rounded-[var(--radius-card)] ${panelPlacementClass(placement)} ${panelAnchorClass(align)} ${PANEL_SHEET}`}
          style={{ zIndex: 60, ...fit }}
        >
          <header className="flex shrink-0 items-center justify-between gap-2 border-b border-[var(--color-border)] px-3 py-2">
            <h3 className="min-w-0 truncate text-sm font-semibold">
              {tr("notifications_title", undefined, "Notifications")}
              <span className="ms-1.5 text-xs font-normal text-[var(--color-muted)]">
                {tr("notif_tab_app", undefined, "This app")}
              </span>
            </h3>
            <button
              type="button"
              onClick={() => {
                setOpen(false);
                triggerRef.current?.focus();
              }}
              aria-label={tr("close", undefined, "Close")}
              title={tr("close", undefined, "Close")}
              className="rounded-full p-1.5 text-[var(--color-muted)] hover:bg-[var(--color-surface-2)] hover:text-[var(--color-text)] max-md:p-2.5"
            >
              <X size={14} />
            </button>
          </header>
          <div className="min-h-0 flex-1 overflow-y-auto overscroll-contain md:max-h-96">
            {peek.length === 0 ? (
              <div className="px-4 py-6 text-center text-xs text-[var(--color-muted)]">
                {tr(
                  "notifications_empty",
                  undefined,
                  "No notifications yet. Successful uploads, install errors, and update offers will appear here.",
                )}
              </div>
            ) : (
              <ul>
                {peek.map((e) => (
                  <NotificationItem
                    key={e.id}
                    compact
                    level={e.level}
                    title={e.title}
                    body={e.body}
                    ts={e.ts}
                    isNew={seen.has(e.id)}
                    link={e.link}
                    onJump={go}
                    onRemove={() => remove(e.id)}
                  />
                ))}
              </ul>
            )}
          </div>
          <footer className="flex shrink-0 flex-wrap items-center justify-between gap-x-3 gap-y-1 border-t border-[var(--color-border)] px-3 py-2 text-xs">
            <button
              type="button"
              onClick={() => go("/notifications")}
              className="inline-flex min-h-8 items-center gap-1 font-medium text-[var(--color-accent)] hover:underline max-md:min-h-11"
            >
              {entries.length > PEEK_COUNT
                ? tr("notif_see_all_count", { count: entries.length }, `See all ${entries.length}`)
                : tr("notif_see_all", undefined, "See all")}
              <ArrowRight size={12} className="rtl:rotate-180" />
            </button>
            <button
              type="button"
              onClick={() => go("/notifications?tab=ps5")}
              className="inline-flex min-h-8 items-center gap-1 text-[var(--color-muted)] hover:text-[var(--color-text)] max-md:min-h-11"
            >
              <Tv size={12} aria-hidden />
              {tr("notif_tab_ps5", undefined, "On the PS5")}
            </button>
          </footer>
        </div>
      )}
    </div>
  );
}
