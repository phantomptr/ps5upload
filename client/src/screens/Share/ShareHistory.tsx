// "Shared with Alex, Nov 9" / "Received from Sam, Nov 9": both ways, from the engine's small
// history file (GET /api/share), read when this shows.

import { useEffect, useState } from "react";

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
    <section className="space-y-1" data-testid="share-history">
      <h3 className="text-sm font-semibold">{tr("share_history", undefined, "History")}</h3>
      <ul className="space-y-1 text-sm">
        {items.slice(0, 20).map((h, i) => (
          <li key={`${h.at_ms}-${i}`} className="text-[var(--color-muted)] [overflow-wrap:anywhere]">
            {historyLine(tr, h)}
          </li>
        ))}
      </ul>
    </section>
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
