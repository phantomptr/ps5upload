import { useEffect } from "react";

import { useConnectionStore } from "../state/connection";
import {
  libraryForHost,
  libraryIsStale,
  refreshLibrary,
  useLibraryStore,
} from "../state/library";
import { consoleBusy } from "./usePoll";

/**
 * Keep the connected console's drive scan (state/library) current while a screen that shows it
 * is open: the Games grid and a game page. Scans when the helper is up and the last scan is
 * missing or older than 30 s, and again when an install asks (`ps5upload:library:invalidate`).
 * Skipped while an upload to that console runs: the walk shares the helper's port with every
 * file's finalize, and can wait.
 */
export function useLibraryScan(enabled = true): void {
  const host = useConnectionStore((s) => s.host?.trim() ?? "");
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const lastRefreshedAt = useLibraryStore(
    (s) => libraryForHost(s, host).lastRefreshedAt,
  );
  const active = enabled && up && !!host;
  useEffect(() => {
    if (!active || consoleBusy(host)) return;
    if (libraryIsStale(host)) void refreshLibrary(host);
  }, [active, host, lastRefreshedAt]);
  useEffect(() => {
    if (!active) return;
    const onInvalidate = () => {
      if (!consoleBusy(host)) void refreshLibrary(host);
    };
    window.addEventListener("ps5upload:library:invalidate", onInvalidate);
    return () =>
      window.removeEventListener("ps5upload:library:invalidate", onInvalidate);
  }, [active, host]);
}
