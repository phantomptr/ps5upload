import { useCallback, useEffect, useRef, useState } from "react";

import { gamesApi, type ConsoleEntry, type GameView } from "../../api/games";
import { hostOf, mgmtAddr } from "../../lib/addr";
import { hasTitleId } from "../../lib/gamePage";
import { useConnectionStore } from "../../state/connection";
import { useRosterStore } from "../../state/roster";
import { useUploadQueueStore } from "../../state/uploadQueue";
import { loadTitled, resultFor, type TitledResult } from "./gameViewState";

/** One game as the engine knows it: every saved console and every copy on the drives. The
 *  connected console is read again when the page opens, and a console is read again after a
 *  job for this game finishes on it. */
export function useGameView(titleId: string) {
  const host = useConnectionStore((s) => s.host?.trim() ?? "");
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const rosterHosts = useRosterStore((s) => s.profiles.map((p) => hostOf(p.host)).join(","));
  // Tagged with the title it answers, so a page that moved to another game
  // never shows the previous one (see gameViewState).
  const [held, setHeld] = useState<TitledResult<GameView> | null>(null);
  const onScreen = useRef(titleId);
  useEffect(() => {
    onScreen.current = titleId;
  }, [titleId]);

  const reload = useCallback(
    () =>
      loadTitled(titleId, gamesApi.view, () => onScreen.current, (r) =>
        // A failed re-read keeps what this page already showed, next to the error.
        setHeld((prev) =>
          r.error && prev?.titleId === r.titleId ? { ...r, value: prev.value } : r,
        ),
      ),
    [titleId],
  );

  useEffect(() => {
    void reload();
  }, [reload]);

  const mine = resultFor(held, titleId);
  const view = mine?.value ?? null;
  const loading = mine === null;
  const error = mine?.error ?? null;

  // The engine forgets consoles the app no longer has (a sync the roster missed while the
  // engine was away happens here).
  useEffect(() => {
    void gamesApi.keep(rosterHosts ? rosterHosts.split(",").filter(Boolean) : []).catch(() => {});
  }, [rosterHosts]);

  // The connected console answers now: its row is current, not a saved read.
  useEffect(() => {
    if (!host || !up || !hasTitleId(titleId)) return;
    void gamesApi
      .refresh(titleId, mgmtAddr(host))
      .then(reload)
      .catch(() => {});
  }, [host, up, titleId, reload]);

  /** Reads this game on `consoleHost` now. Throws with the reason when it cannot be reached. */
  const refresh = useCallback(
    async (consoleHost: string): Promise<ConsoleEntry> => {
      const entry = await gamesApi.refresh(titleId, mgmtAddr(consoleHost));
      await reload();
      return entry;
    },
    [titleId, reload],
  );

  // A send or install of this game that finished changes what that console has: read it again.
  const copyPaths = view?.copies.map((c) => c.absolute_path).join("\n") ?? "";
  const finished = useUploadQueueStore((s) =>
    s.items
      .filter(
        (it) =>
          it.status === "done" &&
          ((it.contentId ?? "").includes(titleId) || copyPaths.split("\n").includes(it.sourcePath)),
      )
      .map((it) => `${it.id}@${hostOf(it.addr)}`)
      .join(","),
  );
  const seen = useRef<Set<string> | null>(null);
  const seenFor = useRef(titleId);
  useEffect(() => {
    // Another game's page: its own finished jobs set the new baseline.
    if (seenFor.current !== titleId) {
      seenFor.current = titleId;
      seen.current = null;
    }
    const ids = finished ? finished.split(",") : [];
    // Jobs that were already done when the page opened changed nothing new.
    if (seen.current === null) {
      seen.current = new Set(ids);
      return;
    }
    const fresh = ids.filter((id) => !seen.current!.has(id));
    if (fresh.length === 0) return;
    for (const id of fresh) seen.current.add(id);
    const hosts = [...new Set(fresh.map((id) => id.split("@")[1]).filter(Boolean))];
    void Promise.all(
      hosts.map((h) => (hasTitleId(titleId) ? gamesApi.refresh(titleId, mgmtAddr(h)).catch(() => null) : null)),
    ).then(reload);
  }, [finished, titleId, reload]);

  return { view, loading, error, reload, refresh };
}
