import { useEffect, useState } from "react";

import { fetchTitleInfo, type TitleInfo } from "./titleDetails";

/**
 * Resolve a title id to its display name + cover art (best-effort).
 *
 * Wraps {@link fetchTitleInfo} — which is cache-first and only scrapes the
 * upstream on a miss — in a hook with AbortController cleanup, so screens can
 * show "Saros (PPSA07631)" instead of a bare id and fall back to a CDN cover
 * for games the console has no local art for. Returns null until resolved (or
 * permanently, for ids with no upstream); callers should fall back to the raw
 * id in that case.
 */
export function useTitleInfo(titleId: string | null | undefined): TitleInfo | null {
  const [info, setInfo] = useState<TitleInfo | null>(null);
  useEffect(() => {
    setInfo(null);
    if (!titleId) return;
    const controller = new AbortController();
    void fetchTitleInfo(titleId, controller.signal)
      .then((result) => {
        if (!controller.signal.aborted) setInfo(result);
      })
      .catch(() => {
        // Best-effort: a failed lookup just leaves the bare id showing.
      });
    return () => controller.abort();
  }, [titleId]);
  return info;
}
