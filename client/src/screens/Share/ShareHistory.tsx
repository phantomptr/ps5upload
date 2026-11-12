// "Shared with Alex, Nov 9" / "Received from Sam, Nov 9": both ways, from the engine's small
// history file (GET /api/share), read when this shows. Folded away under "Past shares".

import { useEffect, useState } from "react";
import { ArrowDownLeft, ArrowUpRight, ChevronRight, History } from "lucide-react";

import { shareList, type ShareHistoryEntry } from "../../api/share";
import { Card } from "../../components";
import { formatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";

/** `fold` (a game's Share panel): folded away, hidden when empty. `card` (the Share screen): a card
 *  that is always there, saying so when nothing was shared yet. */
export function ShareHistory({ initial, variant = "fold" }: { initial?: ShareHistoryEntry[]; variant?: "fold" | "card" }) {
  const tr = useTr();
  const [items, setItems] = useState<ShareHistoryEntry[] | null>(initial ?? null);
  useEffect(() => {
    if (initial) return;
    let live = true;
    shareList()
      .then((l) => live && setItems(l.history))
      .catch(() => live && setItems([]));
    return () => {
      live = false;
    };
  }, [initial]);
  if (variant === "card") {
    return (
      <Card
        title={
          items?.length
            ? tr("share_history_n", { n: items.length }, "Past shares ({n})")
            : tr("share_history_title", undefined, "Past shares")
        }
        icon={History}
      >
        {!items ? null : items.length === 0 ? (
          <p className="text-sm text-[var(--color-muted)]" data-testid="share-history-empty">
            {tr("share_history_empty", undefined, "Nothing shared or received yet.")}
          </p>
        ) : (
          <HistoryList items={items} className="-mx-2 max-h-80 overflow-y-auto" />
        )}
      </Card>
    );
  }
  if (!items || items.length === 0) return null;
  return (
    <details className="group" data-testid="share-history">
      <summary className="flex min-h-9 cursor-pointer list-none items-center gap-1.5 text-xs font-medium text-[var(--color-muted)] hover:text-[var(--color-text)] [&::-webkit-details-marker]:hidden">
        <ChevronRight size={14} aria-hidden className="transition-transform group-open:rotate-90" />
        {tr("share_history_n", { n: items.length }, "Past shares ({n})")}
      </summary>
      <HistoryList items={items} className="mt-1 rounded-[var(--radius-card)] border border-[var(--color-border)]" />
    </details>
  );
}

function HistoryList({ items, className }: { items: ShareHistoryEntry[]; className: string }) {
  const tr = useTr();
  return (
    <ul className={`divide-y divide-[var(--color-border)] text-sm ${className}`} data-testid="share-history-list">
      {items.slice(0, 20).map((h, i) => {
        const Icon = h.dir === "sent" ? ArrowUpRight : ArrowDownLeft;
        return (
          <li key={`${h.at_ms}-${i}`} className="flex items-center gap-2 px-3 py-2 text-[var(--color-muted)]">
            <Icon size={14} aria-hidden className="shrink-0" />
            <span className="min-w-0 [overflow-wrap:anywhere]">{historyLine(tr, h)}</span>
          </li>
        );
      })}
    </ul>
  );
}

type Tr = ReturnType<typeof useTr>;

/** One history entry in words. */
export function historyLine(tr: Tr, h: ShareHistoryEntry): string {
  const date = formatDate(h.at_ms, "date");
  const base =
    h.dir === "sent"
      ? tr("share_history_sent", { name: h.name, date }, "Shared with {name}, {date}")
      : tr("share_history_received", { name: h.name, date }, "Received from {name}, {date}");
  if (h.state === "done") return base;
  const why =
    h.state === "declined"
      ? tr("share_history_declined", undefined, "declined")
      : h.state === "cancelled"
        ? tr("share_history_cancelled", undefined, "cancelled")
        : tr("share_history_unfinished", undefined, "not finished");
  return `${base} (${why})`;
}
