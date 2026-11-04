import { useDeferredValue, useMemo, useState } from "react";
import { FileArchive, RefreshCw, Search } from "lucide-react";

import { Button, Input, Modal, Spinner } from "../../components";
import { humanizePs5Error } from "../../lib/humanizeError";
import { filterLibraryEntries } from "../../lib/libraryFilter";
import { useConnectionStore } from "../../state/connection";
import { useTr } from "../../state/lang";
import {
  entryTitleId,
  libraryForHost,
  refreshLibrary,
  useLibraryStore,
} from "../../state/library";
import { DriveRows, FpkgKstuffTip } from "../GameHub/DriveRow";

/**
 * The PS5's drives, under the grid: a game file there that no game links to (an image nothing
 * was registered from, a folder without a param.json) has no card to open, so one line says how
 * many there are and opens them as rows with every action (mount, register, move, delete…).
 * The line also carries the scan's own trouble (it failed, or it is still running the first
 * time), since the grid shows only what it found.
 */
export function UnlinkedFiles() {
  const tr = useTr();
  const host = useConnectionStore((s) => s.host?.trim() ?? "");
  const up = useConnectionStore((s) => s.payloadStatus === "up");
  const slot = useLibraryStore((s) => libraryForHost(s, up ? host : ""));
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const deferredQuery = useDeferredValue(query);
  const unlinked = useMemo(
    () =>
      (slot.entries ?? [])
        .filter((e) => !entryTitleId(e, slot))
        .sort((a, b) => a.name.localeCompare(b.name)),
    [slot],
  );
  const shown = useMemo(
    () => filterLibraryEntries(unlinked, deferredQuery),
    [unlinked, deferredQuery],
  );
  if (!up || !host) return null;

  const retry = (
    <Button
      size="sm"
      variant="ghost"
      leftIcon={<RefreshCw size={12} />}
      disabled={slot.loading}
      onClick={() => void refreshLibrary(host)}
    >
      {tr("games_retry", undefined, "Try again")}
    </Button>
  );
  return (
    <div className="mt-6 flex flex-col gap-2" data-testid="games-unlinked">
      {slot.error && (
        <p className="flex flex-wrap items-center gap-2 text-xs text-[var(--color-warn)]">
          <span>
            {tr("library_scan_error", undefined, "Couldn't scan the PS5")}:{" "}
            {humanizePs5Error(slot.error)}
          </span>
          {retry}
        </p>
      )}
      {slot.incomplete && !slot.error && (
        <p className="flex flex-wrap items-center gap-2 text-xs text-[var(--color-warn)]">
          <span>
            {tr(
              "library_scan_incomplete",
              undefined,
              "Some folders on the PS5 could not be read, so this list may be missing games.",
            )}
          </span>
          {retry}
        </p>
      )}
      {slot.entries === null && slot.loading && (
        <p className="flex items-center gap-2 text-xs text-[var(--color-muted)]">
          <Spinner size={12} />
          {tr("games_drives_scanning", undefined, "Looking for games on the PS5's drives…")}
        </p>
      )}
      {unlinked.length > 0 && (
        <div className="surface-panel flex flex-wrap items-center gap-3 !rounded-[var(--radius-card)] px-5 py-3 text-sm">
          <FileArchive size={15} className="shrink-0 text-[var(--color-muted)]" aria-hidden />
          <span className="min-w-0 flex-1">
            {tr(
              "games_unlinked_line",
              { n: unlinked.length },
              "Game files on the PS5 that no game links to yet: {n}",
            )}
          </span>
          <Button size="sm" variant="secondary" onClick={() => setOpen(true)}>
            {tr("games_unlinked_show", undefined, "Show them")}
          </Button>
        </div>
      )}
      <Modal
        open={open}
        onClose={() => setOpen(false)}
        size="xl"
        title={tr("games_unlinked_title", undefined, "Game files not linked to a game")}
      >
        <p className="mb-3 text-xs text-[var(--color-muted)]">
          {tr(
            "games_unlinked_explain",
            undefined,
            "Disk images nothing was registered from, and game folders the PS5 can't read a title ID from. Mount or register one to play it; it then shows as a game.",
          )}
        </p>
        {unlinked.some((e) => e.kind === "image") && <FpkgKstuffTip />}
        {unlinked.length > 8 && (
          <div className="mb-3">
            <Input
              block
              leftIcon={<Search size={14} />}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={tr(
                "library_search_placeholder",
                undefined,
                "Search by name, title ID, or path…",
              )}
              aria-label={tr("library_search_aria", undefined, "Filter library entries")}
            />
          </div>
        )}
        <DriveRows entries={shown} host={host} />
      </Modal>
    </div>
  );
}
