import { useCallback, useEffect, useRef, useState } from "react";
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
import { useDocumentVisible } from "../../lib/visibility";
import { useStaleHostGuard } from "../../lib/staleHostGuard";
import { transferAddr } from "../../lib/addr";
import { notifList, notifClear, type Notification } from "../../api/ps5";
import { humanizePs5Error } from "../../lib/humanizeError";

function formatTs(ts: number): string {
  if (!ts) return "—";
  return new Date(ts * 1000).toLocaleString();
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
  const visible = useDocumentVisible();
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

  useEffect(() => {
    void refresh();
    if (!visible) return;
    const id = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(id);
  }, [refresh, visible]);

  const unreadCount = items.filter((n) => !n.read).length;

  return (
    <div className="mx-auto max-w-4xl space-y-4 p-4">
      <PageHeader
        icon={Bell}
        title={tr("ps5notif_title", undefined, "PS5 Notifications")}
        description={tr(
          "ps5notif_description",
          undefined,
          "On-PS5 system notifications. Auto-refreshes every 5 seconds.",
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
        {error && <ErrorCard title={error} />}

        {unreadCount > 0 && (
          <div className="rounded-lg border border-[var(--color-warn)] bg-[var(--color-warn-soft)] px-4 py-2 text-sm text-[var(--color-warn)]">
            <span className="flex items-center gap-2">
              <Mail size={14} />
              {tr(
                "notifications_unread",
                { count: unreadCount },
                `${unreadCount} unread`,
              )}
            </span>
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
              "ps5notif_empty_hint",
              undefined,
              "System notifications from the PS5 will appear here",
            )}
          />
        ) : (
          <div className="space-y-2">
            {items.map((n) => (
              <div
                key={n.seq}
                className={`rounded-md border px-3 py-2.5 ${
                  n.read
                    ? "border-[var(--color-border)] bg-[var(--color-surface-2)]"
                    : "border-[var(--color-accent)]/40 bg-[var(--color-accent-soft)]"
                }`}
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
                      <span className="font-mono tabular-nums">#{n.seq}</span>
                      <span>{formatTs(n.ts)}</span>
                      <span className={`font-medium ${levelColor(n.level)}`}>
                        {n.level}
                      </span>
                      {n.read ? (
                        <CheckCheck size={12} className="opacity-50" />
                      ) : (
                        <MailOpen size={12} />
                      )}
                    </div>
                    <div className="mt-1 text-sm text-[var(--color-text)]">
                      {n.msg}
                    </div>
                  </div>
                </div>
              </div>
            ))}
          </div>
        )}
      </ConnectionGate>
    </div>
  );
}
