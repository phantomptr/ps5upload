import { useCallback, useRef, useState } from "react";
import {
  Bell,
  RefreshCw,
  CheckCheck,
  Mail,
  MailOpen,
  Trash2,
} from "lucide-react";
import {
  PageHeader,
  Button,
  ErrorCard,
  ConnectionGate,
  EmptyState,
  Spinner,
} from "../../components";
import { useTr } from "../../state/lang";
import { useConnectionStore } from "../../state/connection";
import { usePoll } from "../../lib/usePoll";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import { notifList, notifClear, type Notification } from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";
import { hostOf } from "../../lib/addr";
import { isNotifRead, usePs5NotifRead } from "../../state/ps5NotifRead";
import { formatDate } from "../../lib/formatDate";

function formatTs(ts: number): string {
  if (!ts) return "—";
  return formatDate(ts * 1000);
}

function levelColor(level: string): string {
  const l = level.toLowerCase();
  if (l === "error" || l === "critical") return "text-[var(--color-bad)]";
  if (l === "warning" || l === "warn") return "text-[var(--color-warn)]";
  if (l === "info") return "text-[var(--color-accent)]";
  return "text-[var(--color-muted)]";
}

const POLL_MS = 5_000;

export default function NotificationsScreen() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host);
  const payloadStatus = useConnectionStore((s) => s.payloadStatus);
  const addr = host ? transferAddr(host) : "";
  const guard = useStaleHostGuard();

  const [items, setItems] = useState<Notification[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [clearing, setClearing] = useState(false);
  const sinceSeqRef = useRef(0);
  const busyRef = useRef(false);

  const refresh = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    if (busyRef.current) return;
    busyRef.current = true;
    setLoading(true);
    setError(null);
    const probe = guard.capture();
    try {
      const list = await notifList(sinceSeqRef.current, addr);
      if (probe.isStale()) return;
      if (list.notifications.length > 0) {
        setItems((prev) => [...list.notifications, ...prev]);
        const maxSeq = list.notifications.reduce(
          (m, n) => Math.max(m, n.seq),
          sinceSeqRef.current,
        );
        sinceSeqRef.current = maxSeq;
      }
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      busyRef.current = false;
      setLoading(false);
    }
  }, [addr, payloadStatus, guard]);

  const handleClear = useCallback(async () => {
    if (!addr || payloadStatus !== "up") return;
    setClearing(true);
    setError(null);
    const probe = guard.capture();
    try {
      await notifClear(addr);
      if (probe.isStale()) return;
      // The payload keeps its sequence counter running rather than
      // rewinding it, so sinceSeqRef stays valid and the next poll
      // returns only genuinely new notifications.
      setItems([]);
    } catch (e) {
      if (probe.isStale()) return;
      setError(humanizePs5Error(String(e)));
    } finally {
      setClearing(false);
    }
  }, [addr, payloadStatus, guard]);

  // Paused while hidden or while a transfer to this console runs.
  usePoll(refresh, POLL_MS, { host, enabled: !!addr && payloadStatus === "up" });

  // The payload has no read state, so it is kept here, per console.
  const readState = usePs5NotifRead((s) => (host ? s.byHost[hostOf(host)] : undefined));
  const markAll = usePs5NotifRead((s) => s.markAll);
  const setRead = usePs5NotifRead((s) => s.setRead);
  const isRead = (n: Notification) => isNotifRead(readState, n.seq);
  const unreadCount = items.filter((n) => !isRead(n)).length;
  const maxSeq = items.reduce((m, n) => Math.max(m, n.seq), 0);

  return (
    <div className="app-page space-y-4">
      <PageHeader
        icon={Bell}
        title={tr("ps5notif_title", undefined, "PS5 Notifications")}
        description={tr(
          "ps5notif_description_v3",
          undefined,
          "Messages ps5upload has put on your TV (Remote Play, cheats), read back from its helper on the console. Not the PS5's own notification panel. Refreshes every 5 seconds.",
        )}
        count={items.length}
        right={
          <div className="flex items-center gap-2">
            <Button
              variant="ghost"
              size="sm"
              leftIcon={<Trash2 size={14} />}
              onClick={handleClear}
              disabled={
                clearing ||
                items.length === 0 ||
                payloadStatus !== "up" ||
                !addr
              }
            >
              {clearing
                ? tr("ps5notif_clearing", undefined, "Clearing\u2026")
                : tr("ps5notif_clear", undefined, "Clear all")}
            </Button>
            <Button
              variant="ghost"
              size="sm"
              onClick={refresh}
              disabled={loading || payloadStatus !== "up" || !addr}
            >
              {loading ? (
                <Spinner size={14} tone="inherit" />
              ) : (
                <RefreshCw size={14} />
              )}
            </Button>
          </div>
        }
      />

      <ConnectionGate>
        {error && <ErrorCard title={error} onRetry={() => void refresh()} />}

        {unreadCount > 0 && (
          <div className="flex flex-wrap items-center justify-between gap-2 rounded-[var(--radius-card)] border border-[var(--glass-edge)] bg-[var(--color-surface-raised)] px-4 py-2.5 text-sm shadow-[var(--edge-highlight)]">
            <span className="flex items-center gap-2.5 font-medium">
              <span className="icon-disc !h-7 !w-7" aria-hidden>
                <Mail size={13} />
              </span>
              {tr(
                "notifications_unread",
                { count: unreadCount },
                `${unreadCount} unread`,
              )}
            </span>
            <Button
              variant="secondary"
              size="sm"
              leftIcon={<CheckCheck size={14} />}
              onClick={() => host && markAll(host, maxSeq)}
            >
              {tr("notifications_mark_all_read", undefined, "Mark all as read")}
            </Button>
          </div>
        )}

        {items.length === 0 && !loading ? (
          <EmptyState
            icon={Bell}
            title={tr(
              "ps5notif_empty",
              undefined,
              "No notifications",
            )}
            message={tr(
              "ps5notif_empty_hint_v2",
              undefined,
              "When ps5upload puts a message on your TV, such as a Remote Play PIN or a cheat being switched on, it is listed here so you can read it again. This is not the PS5's own notification panel; the console does not let us read that.",
            )}
          />
        ) : (
          <div className="space-y-2">
            {items.map((n) => {
              const read = isRead(n);
              return (
                <div
                  key={n.seq}
                  className={`rounded-[var(--radius-card)] border px-4 py-3 transition-colors ${
                    read
                      ? "border-[var(--color-border)] bg-[var(--color-surface)]"
                      : "border-[var(--glass-edge)] bg-[var(--color-surface-raised)] shadow-[var(--edge-highlight),var(--shadow-1)]"
                  }`}
                >
                  <div className="flex items-start justify-between gap-3">
                    <span
                      aria-hidden
                      className={`mt-1.5 h-2 w-2 shrink-0 rounded-full ${
                        read ? "bg-transparent" : "bg-[var(--color-accent-bright)]"
                      }`}
                    />
                    <div className="min-w-0 flex-1">
                      <div
                        className={`text-sm ${
                          read
                            ? "text-[var(--color-muted)]"
                            : "font-medium text-[var(--color-text)]"
                        }`}
                      >
                        {n.msg}
                      </div>
                      <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-[var(--color-muted)]">
                        <span className={`font-medium ${levelColor(n.level)}`}>
                          {n.level}
                        </span>
                        <span>{formatTs(n.ts)}</span>
                        <span className="font-mono tabular-nums opacity-70">#{n.seq}</span>
                      </div>
                    </div>
                    <button
                      type="button"
                      onClick={() => host && setRead(host, n.seq, !read)}
                      className="flex shrink-0 items-center gap-1 rounded-full px-2.5 py-1 text-xs text-[var(--color-muted)] hover:bg-[var(--color-surface-3)] hover:text-[var(--color-text)] max-md:min-h-11"
                    >
                      {read ? <Mail size={12} /> : <MailOpen size={12} />}
                      {read
                        ? tr("notifications_mark_unread", undefined, "Mark unread")
                        : tr("notifications_mark_read", undefined, "Mark read")}
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}
