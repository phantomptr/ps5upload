import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import {
  Bell,
  X,
  CheckCircle2,
  AlertTriangle,
  XCircle,
  Info,
  ArrowRight,
  Trash2,
} from "lucide-react";
import {
  useNotificationsStore,
  type Notification,
  type NotificationLevel,
} from "../state/notifications";
import { useTr } from "../state/lang";

/**
 * Notification inbox — sidebar bell + slide-out panel.
 *
 * The bell is a single button that lives in the sidebar footer. It
 * shows an unread badge sourced from the store. Clicking it opens
 * a side panel that lists every notification newest-first; opening
 * the panel marks all entries as read.
 *
 * Why it lives next to the theme toggle: both are persistent
 * affordances that aren't tied to any one screen, and putting the
 * bell anywhere else (header, status bar) would steal pixels from
 * surfaces that already work.
 */
export default function NotificationInbox() {
  const tr = useTr();
  const navigate = useNavigate();
  const entries = useNotificationsStore((s) => s.entries);
  const markAllRead = useNotificationsStore((s) => s.markAllRead);
  const remove = useNotificationsStore((s) => s.remove);
  const clear = useNotificationsStore((s) => s.clear);
  const [open, setOpen] = useState(false);
  // "Unread only" filter — useful when the inbox accumulates over
  // a long session and the user wants to see what's actually new.
  // Resets each time the panel reopens so a stale filter doesn't
  // hide entries from a fresh session.
  const [unreadOnly, setUnreadOnly] = useState(false);
  // Snapshot of which entry ids were unread when the panel opened.
  // Stored in state (not ref) because the toggle button conditionally
  // renders based on this set's size — refs can't be read during
  // render per react-hooks/refs.
  const [unreadIdsAtOpen, setUnreadIdsAtOpen] = useState<Set<string>>(
    () => new Set(),
  );
  const wrapperRef = useRef<HTMLDivElement>(null);

  const unread = entries.filter((e) => !e.read).length;
  const visibleEntries = unreadOnly
    ? entries.filter((e) => unreadIdsAtOpen.has(e.id))
    : entries;

  useEffect(() => {
    if (!open) return;
    function onClick(e: MouseEvent) {
      if (
        wrapperRef.current &&
        !wrapperRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", onClick, true);
    return () => document.removeEventListener("mousedown", onClick, true);
  }, [open]);

  // Snapshot which entries were unread at the open transition — the
  // "unread only" filter uses this so the user's view doesn't shrink
  // to nothing 200 ms in when markAllRead fires. Reads `entries` via
  // `useNotificationsStore.getState()` (instead of the closed-over
  // value from `entries`) so the snapshot reflects state at the exact
  // open instant even if a re-render is in flight.
  useEffect(() => {
    if (!open) return;
    const fresh = useNotificationsStore
      .getState()
      .entries.filter((e) => !e.read)
      .map((e) => e.id);
    setUnreadIdsAtOpen(new Set(fresh));
    setUnreadOnly(false);
  }, [open]);

  // Mark-as-read while the panel is open. Re-runs whenever `unread`
  // changes — so notifications that arrive AFTER the panel was opened
  // get marked read too (previous version closed over the initial
  // `unread` count and missed those, leaving them perpetually unread).
  useEffect(() => {
    if (!open || unread === 0) return;
    const id = window.setTimeout(() => markAllRead(), 200);
    return () => window.clearTimeout(id);
  }, [open, unread, markAllRead]);

  return (
    <div ref={wrapperRef} className="relative">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-label={tr("notifications_open", undefined, "Open notifications")}
        className="relative rounded-md p-1.5 text-[var(--color-muted)] transition-colors hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)]"
      >
        <Bell size={14} />
        {unread > 0 && (
          <span
            className="absolute -right-0.5 -top-0.5 inline-flex h-3.5 min-w-3.5 items-center justify-center rounded-full bg-[var(--color-bad)] px-1 text-[9px] font-semibold tabular-nums text-white"
            title={tr(
              unread === 1 ? "notifications_unread_one" : "notifications_unread_many",
              { count: unread },
              `${unread} unread notification${unread === 1 ? "" : "s"}`,
            )}
          >
            {unread > 9 ? "9+" : unread}
          </span>
        )}
      </button>

      {open && (
        <div
          // Anchor by the LEFT edge of the bell, not the right. The
          // bell lives in the sidebar footer (left side of window),
          // so `right-0` made the 320px panel grow leftward past
          // the window's left edge and clip off-screen. `left-0`
          // grows rightward into the main content area. The
          // max-w-[calc(100vw-2rem)] keeps small windows safe.
          className="absolute bottom-full left-0 mb-1 w-80 max-w-[calc(100vw-2rem)] rounded-md border border-[var(--color-border)] bg-[var(--color-surface)] shadow-lg"
          style={{ zIndex: 60 }}
        >
          <header className="flex items-center justify-between border-b border-[var(--color-border)] px-3 py-2">
            <h3 className="text-xs font-semibold">
              {tr("notifications_title", undefined, "Notifications")}
            </h3>
            <div className="flex items-center gap-1">
              {unreadIdsAtOpen.size > 0 && (
                <button
                  type="button"
                  onClick={() => setUnreadOnly((v) => !v)}
                  className={`rounded px-1.5 py-0.5 text-[10px] ${
                    unreadOnly
                      ? "bg-[var(--color-accent-soft)] text-[var(--color-accent)]"
                      : "text-[var(--color-muted)] hover:bg-[var(--color-surface-2)] hover:text-[var(--color-text)]"
                  }`}
                  title={tr(
                    "notifications_unread_only_tooltip",
                    undefined,
                    "Show only entries that were unread when this panel opened",
                  )}
                >
                  {tr("notifications_unread_only", undefined, "Unread")}
                </button>
              )}
              {entries.length > 0 && (
                <button
                  type="button"
                  onClick={clear}
                  className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-2)] hover:text-[var(--color-text)]"
                  title={tr("notifications_clear", undefined, "Clear all")}
                >
                  <Trash2 size={12} />
                </button>
              )}
              <button
                type="button"
                onClick={() => setOpen(false)}
                className="rounded p-1 text-[var(--color-muted)] hover:bg-[var(--color-surface-2)] hover:text-[var(--color-text)]"
              >
                <X size={12} />
              </button>
            </div>
          </header>
          <div className="max-h-96 overflow-y-auto">
            {visibleEntries.length === 0 ? (
              <div className="px-3 py-6 text-center text-xs text-[var(--color-muted)]">
                {unreadOnly
                  ? tr(
                      "notifications_no_unread",
                      undefined,
                      "No unread entries. Untoggle 'Unread' to see history.",
                    )
                  : tr(
                      "notifications_empty",
                      undefined,
                      "No notifications yet. Successful uploads, install errors, and update offers will appear here.",
                    )}
              </div>
            ) : (
              <ul>
                {visibleEntries.map((e) => (
                  <NotificationRow
                    key={e.id}
                    entry={e}
                    onJump={(link) => {
                      navigate(link);
                      setOpen(false);
                    }}
                    onRemove={() => remove(e.id)}
                  />
                ))}
              </ul>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function NotificationRow({
  entry,
  onJump,
  onRemove,
}: {
  entry: Notification;
  onJump: (link: string) => void;
  onRemove: () => void;
}) {
  const tr = useTr();
  const ts = new Date(entry.ts);
  return (
    <li className="group border-b border-[var(--color-border)] px-3 py-2 text-xs last:border-0">
      <div className="flex items-start gap-2">
        <LevelIcon level={entry.level} />
        <div className="min-w-0 flex-1">
          <div className="font-medium">{entry.title}</div>
          {entry.body && (
            <div className="mt-0.5 break-words text-[var(--color-muted)]">
              {entry.body}
            </div>
          )}
          <div className="mt-1 flex items-center gap-2 text-[10px] text-[var(--color-muted)]">
            <span>{ts.toLocaleString()}</span>
            {entry.link && (
              <button
                type="button"
                onClick={() => onJump(entry.link!)}
                className="inline-flex items-center gap-0.5 text-[var(--color-accent)] hover:underline"
              >
                {tr("notifications_jump", undefined, "open")}
                <ArrowRight size={9} />
              </button>
            )}
          </div>
        </div>
        <button
          type="button"
          onClick={onRemove}
          className="opacity-0 transition-opacity group-hover:opacity-100"
          aria-label={tr("notifications_remove", undefined, "Dismiss")}
        >
          <X size={11} className="text-[var(--color-muted)]" />
        </button>
      </div>
    </li>
  );
}

function LevelIcon({ level }: { level: NotificationLevel }) {
  if (level === "success")
    return <CheckCircle2 size={12} className="mt-0.5 shrink-0 text-[var(--color-good)]" />;
  if (level === "warning")
    return <AlertTriangle size={12} className="mt-0.5 shrink-0 text-[var(--color-warn)]" />;
  if (level === "error")
    return <XCircle size={12} className="mt-0.5 shrink-0 text-[var(--color-bad)]" />;
  return <Info size={12} className="mt-0.5 shrink-0 text-[var(--color-muted)]" />;
}
