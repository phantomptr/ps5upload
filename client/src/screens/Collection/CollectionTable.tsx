import { formatDate } from "../../lib/formatDate";
import type { CollectionGame } from "../../api/collection";
import { PlatformBadge, Table } from "../../components";
import { addOnCount, formatCollectionBytes } from "../../lib/collectionView";
import { useTr } from "../../state/lang";

export function CollectionTable({
  games,
  onOpen,
}: {
  games: CollectionGame[];
  onOpen: (gameId: string) => void;
}) {
  const tr = useTr();
  return (
    <Table
      rows={games}
      rowKey={(g) => g.game_id}
      onRowClick={(g) => onOpen(g.game_id)}
      columns={[
        {
          key: "title",
          header: tr("collection.col.title", undefined, "Title"),
          cell: (g) => <span className="font-medium">{g.title}</span>,
        },
        {
          key: "id",
          header: tr("collection.col.id", undefined, "Game ID"),
          cell: (g) => <span className="font-mono text-xs">{g.game_id}</span>,
        },
        {
          key: "platform",
          header: tr("collection.col.platform", undefined, "Platform"),
          cell: (g) => <PlatformBadge platform={g.platform.toLowerCase()} />,
        },
        {
          key: "copies",
          header: tr("collection.col.copies", undefined, "Copies"),
          align: "right",
          cell: (g) => (
            <span
              className={
                g.is_duplicate ? "font-semibold text-[var(--color-warn)]" : ""
              }
            >
              {g.copies}
            </span>
          ),
        },
        {
          key: "addons",
          header: tr("collection.col.addons", undefined, "Add-ons"),
          align: "right",
          cell: (g) => addOnCount(g),
        },
        {
          key: "size",
          header: tr("collection.col.size", undefined, "Size"),
          align: "right",
          cell: (g) => (
            <span className="tabular-nums">
              {formatCollectionBytes(g.total_size_bytes)}
            </span>
          ),
        },
        {
          key: "added",
          header: tr("collection.col.added", undefined, "Added"),
          cell: (g) =>
            g.added_at ? formatDate(new Date(g.added_at), "date") : "—",
        },
      ]}
    />
  );
}
