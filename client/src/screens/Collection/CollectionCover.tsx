import { useCallback, useEffect, useState } from "react";

import {
  cachedCollectionCover,
  collection,
  collectionCoverDataUrl,
  type CollectionGame,
} from "../../api/collection";
import { useImageRetry } from "../../lib/useImageRetry";
import { useCollectionStore } from "../../state/collection";

/** A game's cover: the one read out of the game, then the online one, then its initials. A
 *  cover that fails to load moves to the next, never to a broken image. The one read out of
 *  the game also comes over the IPC when the desktop window's own load of it is refused (the
 *  engine's cross-site guard turns WebKit's request for it away). */
export function CollectionCover({
  game,
  className = "",
}: {
  game: Pick<CollectionGame, "game_id" | "title" | "local_cover" | "cover_url">;
  className?: string;
}) {
  const scannedAt = useCollectionStore((s) => s.library?.generated_at);
  const local = game.local_cover ? collection.coverUrl(game.game_id, scannedAt) : null;
  const sources = [local, game.cover_url ?? null].filter((s): s is string => !!s);
  const [at, setAt] = useState(0);
  // A cover that failed once (asked for before the scan had saved it) is tried again when the
  // game's covers change, instead of staying on the initials for good.
  const key = sources.join("|");
  useEffect(() => setAt(0), [key]);
  const candidate = sources[at] ?? null;
  const onLocal = !!local && at === 0;
  const { src, onError, failed } = useImageRetry(candidate, {
    cached: onLocal ? cachedCollectionCover(game.game_id) : undefined,
    fallbackLoader: () =>
      onLocal ? collectionCoverDataUrl(game.game_id) : Promise.resolve(null),
  });
  const advance = useCallback(() => setAt((i) => i + 1), []);
  useEffect(() => {
    if (failed) advance();
  }, [failed, advance]);
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
      onError={onError}
      className={`aspect-square w-full bg-[var(--color-surface-3)] object-cover ${className}`}
    />
  );
}
