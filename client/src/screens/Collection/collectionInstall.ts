import type { CollectionGame, CollectionOffer } from "../../api/collection";
import { enqueueInstall } from "../../state/consoleQueueBridge";
import { loadConsoleStates } from "../../state/collection";

/** What a queue row is called: the game, then what the package is. */
export function offerLabel(game: CollectionGame, o: CollectionOffer): string {
  if (o.category === "gp") return `${game.title} · Update ${o.version}`;
  if (o.category === "ac")
    return o.title ? `${game.title} · ${o.title}` : `${game.title} · DLC`;
  return o.version ? `${game.title} ${o.version}` : game.title;
}

/** Queues packages from the collection on `host`, streamed from this computer. The queue
 *  orders them base → update → DLC, keeps the computer and the console awake, and refuses an
 *  install that would erase an installed game. The console's state is read again once they
 *  have all finished. Returns how many were queued. */
export function installOffers(
  host: string,
  items: { game: CollectionGame; offer: CollectionOffer }[],
): number {
  const done = items.map(
    ({ game, offer }) =>
      enqueueInstall({
        host,
        request: { via: "stream", source: offer.path },
        displayName: offerLabel(game, offer),
        contentId: offer.content_id || null,
        category: offer.category,
      }).done,
  );
  if (done.length > 0) {
    void Promise.allSettled(done).then(() => loadConsoleStates(host));
  }
  return done.length;
}
