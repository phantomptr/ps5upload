// "Shared with Alex, Nov 9" / "Received from Sam, Nov 9": both ways, from the engine's small
// history file (GET /api/share), read when this shows. Folded away under "Past shares".

import { useEffect, useState } from "react";
import { ArrowDownLeft, ArrowUpRight, ChevronRight } from "lucide-react";

import { shareList, type ShareHistoryEntry } from "../../api/share";
import { formatDate } from "../../lib/formatDate";
import { useTr } from "../../state/lang";

export function ShareHistory({ initial }: { initial?: ShareHistoryEntry[] }) {
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
  if (!items || items.length === 0) return null;
  return (
    <details className="group" data-testid="share-history">
      <summary className="flex min-h-9 cursor-pointer list-none items-center gap-1.5 text-xs font-medium text-[var(--color-muted)] hover:text-[var(--color-text)] [&::-webkit-details-marker]:hidden">
        <ChevronRight size={14} aria-hidden className="transition-transform group-open:rotate-90" />
        {tr("share_history_n", { n: items.length }, "Past shares ({n})")}
      </summary>
      <ul className="mt-1 divide-y divide-[var(--color-border)] rounded-[var(--radius-card)] border border-[var(--color-border)] text-sm">
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
    </details>
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
