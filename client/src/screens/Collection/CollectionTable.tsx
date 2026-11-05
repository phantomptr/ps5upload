import { formatDate } from "../../lib/formatDate";
import type { CollectionGame, GameConsoleState } from "../../api/collection";
import { PlatformBadge, Table } from "../../components";
import { addOnCount, formatCollectionBytes } from "../../lib/collectionView";
import { firmwareShortfall } from "../../lib/collectionInsights";
import { formatPlayTime } from "../../state/playTime";
import { useTr } from "../../state/lang";
import { GameQuickActions } from "./CollectionCard";
import { PlayControl, type PlayState } from "./PlayControl";

export function CollectionTable({
  games,
  onOpen,
  host,
  consoleFw,
  consoleStates,
  cheatIds,
  playSec,
  selecting,
  selected,
  onToggleSelect,
  playOf,
  onPlay,
  onStop,
}: {
  games: CollectionGame[];
  onOpen: (gameId: string) => void;
  host?: string;
  consoleFw?: string | null;
  consoleStates?: Map<string, GameConsoleState>;
  cheatIds?: Set<string> | null;
  playSec?: (gameId: string) => number | undefined;
  selecting?: boolean;
  selected?: Set<string>;
  onToggleSelect?: (gameId: string) => void;
  /** Play / Close game for a game installed on the selected console that can start there. */
  playOf?: (gameId: string) => PlayState | undefined;
  onPlay?: (gameId: string) => void;
  onStop?: (gameId: string) => void;
}) {
  const tr = useTr();
  // Play first, so a phone (where the table scrolls sideways) sees it without scrolling. Its
  // own clicks and keys stay in it rather than opening the game.
  const play =
    !selecting && playOf && onPlay && onStop
      ? [
          {
            key: "play",
            header: "",
            cell: (g: CollectionGame) => {
              const st = playOf(g.game_id);
              return st ? (
                <PlayControl
                  variant="row"
                  state={st}
                  title={g.title}
                  onPlay={() => onPlay(g.game_id)}
                  onStop={() => onStop(g.game_id)}
                />
              ) : null;
            },
          },
        ]
      : [];
  const pick = selecting
    ? [
        {
          key: "pick",
          header: "",
          cell: (g: CollectionGame) => (
            <input
              type="checkbox"
              aria-label={g.title}
              checked={!!selected?.has(g.game_id)}
              onChange={() => onToggleSelect?.(g.game_id)}
              onClick={(e) => e.stopPropagation()}
            />
          ),
        },
      ]
    : [];
  return (
    <Table
      rows={games}
      rowKey={(g) => g.game_id}
      onRowClick={(g) =>
        selecting ? onToggleSelect?.(g.game_id) : onOpen(g.game_id)
      }
      columns={[
        ...pick,
        ...play,
        {
          key: "title",
          header: tr("collection.col.title", undefined, "Title"),
          cell: (g) => {
            const need = firmwareShortfall(g, consoleFw ?? null);
            return (
              <span className="font-medium">
                {g.title}
                {need && (
                  <span className="ms-2 text-xs font-normal text-[var(--color-warn)]">
                    {tr("collection.fw_badge", { need }, "Needs FW {need}")}
                  </span>
                )}
              </span>
            );
          },
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
          key: "played",
          header: tr("collection.col.played", undefined, "Played"),
          align: "right",
          cell: (g) => (
            <span className="tabular-nums">
              {formatPlayTime(playSec?.(g.game_id))}
            </span>
          ),
        },
        {
          key: "added",
          header: tr("collection.col.added", undefined, "Added"),
          cell: (g) =>
            g.added_at ? formatDate(new Date(g.added_at), "date") : "—",
        },
        {
          key: "actions",
          header: "",
          align: "right",
          // The row opens the game on click; the menu's own clicks and keys stay in the menu.
          cell: (g) =>
            selecting ? null : (
              <span
                className="inline-flex"
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => e.stopPropagation()}
              >
                <GameQuickActions
                  game={g}
                  onOpen={onOpen}
                  host={host}
                  consoleFw={consoleFw}
                  consoleState={consoleStates?.get(g.game_id)}
                  hasCheats={!!cheatIds?.has(g.game_id.toUpperCase())}
                />
              </span>
            ),
        },
      ]}
    />
  );
}
