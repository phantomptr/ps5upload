import { Copy as CopyIcon } from "lucide-react";

import type { CollectionGame, GameConsoleState } from "../../api/collection";
import { Badge, PlatformBadge } from "../../components";
import { addOnCount, formatCollectionBytes } from "../../lib/collectionView";
import { useTr } from "../../state/lang";
import { CollectionCover } from "./CollectionCover";

/** "1 copy · 2 add-ons": copies are full copies of the game, add-ons its updates and DLC. */
export function useCopiesLabel() {
  const tr = useTr();
  return (g: CollectionGame) => {
    const copies =
      g.copies === 1
        ? tr("collection.copies_one", undefined, "1 copy")
        : tr("collection.copies_n", { n: g.copies }, "{n} copies");
    const addOns = addOnCount(g);
    if (addOns === 0) return copies;
    const extra =
      addOns === 1
        ? tr("collection.addons_one", undefined, "1 add-on")
        : tr("collection.addons_n", { n: addOns }, "{n} add-ons");
    return `${copies} · ${extra}`;
  };
}

/** What the selected console has of this game, as a short badge; nothing when it lacks it. */
export function ConsoleBadge({ state }: { state?: GameConsoleState }) {
  const tr = useTr();
  if (!state?.installed) return null;
  if (state.update) {
    return (
      <Badge
        tone="accent"
        size="sm"
        title={tr(
          "collection.badge_update_hint",
          { have: state.installed_version ?? "?", v: state.update.version },
          "The console has {have}; the collection has update {v}.",
        )}
      >
        {tr(
          "collection.badge_update",
          { v: state.update.version },
          "Update {v}",
        )}
      </Badge>
    );
  }
  return (
    <Badge tone="good" size="sm">
      {state.installed_version
        ? tr(
            "collection.badge_installed_v",
            { v: state.installed_version },
            "Installed · {v}",
          )
        : tr("collection.badge_installed", undefined, "Installed")}
      {state.dlc_missing.length > 0
        ? ` · +${state.dlc_missing.length} DLC`
        : ""}
    </Badge>
  );
}

export function CollectionCard({
  game,
  onOpen,
  consoleState,
}: {
  game: CollectionGame;
  onOpen: () => void;
  consoleState?: GameConsoleState;
}) {
  const tr = useTr();
  const copiesLabel = useCopiesLabel();
  return (
    <button
      type="button"
      onClick={onOpen}
      className="group flex flex-col overflow-hidden rounded-xl border border-[var(--color-border)] bg-[var(--color-surface-2)] text-left transition-colors hover:border-[var(--color-accent)]"
      data-testid="collection-card"
    >
      <div className="relative">
        <CollectionCover game={game} />
        <div className="absolute left-2 top-2 drop-shadow">
          <PlatformBadge platform={game.platform.toLowerCase()} />
        </div>
        <div className="absolute bottom-2 left-2 drop-shadow">
          <ConsoleBadge state={consoleState} />
        </div>
        {game.is_duplicate && (
          <span className="absolute right-2 top-2 drop-shadow">
            <Badge
              tone="warn"
              size="sm"
              icon={CopyIcon}
              title={tr(
                "collection.duplicate_hint",
                undefined,
                "More than one full copy of this game is on disk.",
              )}
            >
              {tr("collection.duplicate", undefined, "Duplicate")}
            </Badge>
          </span>
        )}
      </div>
      <div className="flex flex-1 flex-col gap-1 p-2.5 sm:p-3">
        <div
          className="line-clamp-2 min-h-[2.5rem] text-sm font-semibold"
          title={game.title}
        >
          {game.title}
        </div>
        <div className="truncate font-mono text-xs text-[var(--color-muted)]">
          {game.game_id}
        </div>
        <div className="mt-auto flex items-center justify-between gap-2 pt-1 text-xs text-[var(--color-muted)]">
          <span className="truncate">{copiesLabel(game)}</span>
          <span className="shrink-0 tabular-nums">
            {formatCollectionBytes(game.total_size_bytes)}
          </span>
        </div>
      </div>
    </button>
  );
}
