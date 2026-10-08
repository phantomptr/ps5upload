import { useState } from "react";

import { collection, type CollectionGame } from "../../api/collection";

/** A game's cover: the one read out of the game, then the online one, then its initials. A
 *  cover that fails to load moves to the next, never to a broken image. */
export function CollectionCover({
  game,
  className = "",
}: {
  game: CollectionGame;
  className?: string;
}) {
  const sources = [
    game.local_cover ? collection.coverUrl(game.game_id) : null,
    game.cover_url ?? null,
  ].filter((s): s is string => !!s);
  const [at, setAt] = useState(0);
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
