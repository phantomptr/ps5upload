import { useCallback, useEffect, useState } from "react";

import { gamesApi, type GameView } from "../../api/games";
import { mgmtAddr } from "../../lib/addr";
import { hasTitleId } from "../../lib/gamePage";
import { useConnectionStore } from "../../state/connection";
import { useUploadQueueStore } from "../../state/uploadQueue";

/** One game as the engine knows it: every saved console and every copy on the drives. The
 *  connected console is read again when the page opens, and the view is reloaded after a job
 *  for this game finishes. */
export function useGameView(titleId: string) {
  const host = useConnectionStore((s) => s.host?.trim() ?? "");
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const [view, setView] = useState<GameView | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setView(await gamesApi.view(titleId));
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [titleId]);

  useEffect(() => {
    setLoading(true);
    void reload();
  }, [reload]);

  // The connected console answers now: its row is current, not a saved read.
  useEffect(() => {
    if (!host || !up || !hasTitleId(titleId)) return;
    void gamesApi
      .refresh(titleId, mgmtAddr(host))
      .then(reload)
      .catch(() => {});
  }, [host, up, titleId, reload]);

  // A send or install of this game that finished changes what a console has.
  const finished = useUploadQueueStore(
    (s) =>
      s.items.filter(
        (it) =>
          it.status === "done" &&
          ((it.contentId ?? "").includes(titleId) ||
            (view?.copies.some((c) => c.absolute_path === it.sourcePath) ?? false)),
      ).length,
  );
  useEffect(() => {
    if (finished > 0) void reload();
  }, [finished, reload]);

  /** Reads this game on `consoleHost` now. Throws with the reason when it cannot be reached. */
  const refresh = useCallback(
    async (consoleHost: string) => {
      await gamesApi.refresh(titleId, mgmtAddr(consoleHost));
      await reload();
    },
    [titleId, reload],
  );

  return { view, loading, error, reload, refresh };
}
