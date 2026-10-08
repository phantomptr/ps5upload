import { useEffect, useState } from "react";

import { collection, type CollectionGame } from "../../api/collection";
import { useCollectionStore } from "../../state/collection";

/** A game's cover: the one read out of the game, then the online one, then its initials. A
 *  cover that fails to load moves to the next, never to a broken image. */
export function CollectionCover({
  game,
  className = "",
}: {
  game: CollectionGame;
  className?: string;
}) {
  const scannedAt = useCollectionStore((s) => s.library?.generated_at);
  const sources = [
    game.local_cover ? collection.coverUrl(game.game_id, scannedAt) : null,
    game.cover_url ?? null,
  ].filter((s): s is string => !!s);
  const [at, setAt] = useState(0);
  // A cover that failed once (asked for before the scan had saved it) is tried again when the
  // game's covers change, instead of staying on the initials for good.
  const key = sources.join("|");
  useEffect(() => setAt(0), [key]);
  const src = sources[at];
  if (!src) {
    const initials = game.title
      .split(/\s+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((w) => w[0]?.toUpperCase() ?? "")
      .join("");
    return (
      <div
        className={`grid aspect-square w-full place-items-center bg-[var(--color-surface-3)] text-2xl font-semibold text-[var(--color-muted)] ${className}`}
        aria-hidden
      >
        {initials || "?"}
      </div>
    );
  }
  return (
    <img
      src={src}
      alt=""
      loading="lazy"
      onError={() => setAt((i) => i + 1)}
      className={`aspect-square w-full bg-[var(--color-surface-3)] object-cover ${className}`}
    />
  );
}
