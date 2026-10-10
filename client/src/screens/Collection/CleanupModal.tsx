import { useEffect, useRef, useState } from "react";
import { Search, Square, Trash2 } from "lucide-react";

import {
  collection,
  type JunkStatus,
  type MacSettings,
} from "../../api/collection";
import { Button, Modal, Spinner, Toggle } from "../../components";
import { formatCollectionBytes } from "../../lib/collectionView";
import { setCollectionSweep, useCollectionStore } from "../../state/collection";
import { engineIsOnThisDevice } from "../../state/engine";
import { useTr } from "../../state/lang";

/** A path shown from its collection folder ("games/PS4/._x.pkg"), the full one on hover. */
function shortPath(path: string, roots: string[]): string {
  const root = roots.find((r) => path.startsWith(`${r}/`));
  if (!root) return path;
  const base = root.split("/").filter(Boolean).pop() ?? "";
  return `${base}/${path.slice(root.length + 1)}`;
}

/** Finder's leftovers in the games folders, and the Mac settings that stop them coming back. */
export function CleanupModal({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const tr = useTr();
  const settings = useCollectionStore((s) => s.settings);
  const [junk, setJunk] = useState<JunkStatus | null>(null);
  const [cleaned, setCleaned] = useState<{
    removed: number;
    freed: number;
    failed: string[];
  } | null>(null);
  const [mac, setMac] = useState<MacSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const poll = async () => {
    try {
      const st = await collection.junk();
      setJunk(st);
      if (st.running) timer.current = setTimeout(() => void poll(), 700);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  useEffect(() => {
    if (!open) return;
    setCleaned(null);
    setError(null);
    void poll();
    // The Mac options run tools on the engine's computer: offered only when that is this one.
    if (engineIsOnThisDevice()) {
      collection
        .macos()
        .then(setMac)
        .catch(() => setMac(null));
    }
    return () => {
      if (timer.current) clearTimeout(timer.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const scan = async () => {
    setCleaned(null);
    setError(null);
    try {
      setJunk(await collection.junkScan());
      void poll();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const clean = async () => {
    if (!junk?.token) return;
    setBusy(true);
    try {
      setCleaned(await collection.junkClean(junk.token));
      setJunk(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const macAction = async (fn: () => Promise<MacSettings>) => {
    setBusy(true);
    setError(null);
    try {
      setMac(await fn());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const finished = junk && !junk.running && junk.finished_ms > 0;
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title={tr("collection.junk_title", undefined, "Clean up junk files")}
      bodyClassName="p-4 sm:p-5"
    >
      <p className="text-sm text-[var(--color-muted)]">
        {tr(
          "collection.junk_body",
          undefined,
          "A Mac leaves hidden files beside what it copies: a “._” file for every file on an exFAT drive (each one taking a whole cluster), plus .DS_Store, .localized and Icon files. No console reads them. A “._” file is removed only when it really is one of these; nothing else is touched.",
        )}
      </p>
      {error && <p className="mt-3 text-sm text-[var(--color-bad)]">{error}</p>}
      <div className="mt-4 flex flex-wrap items-center gap-2">
        {junk?.running ? (
          <>
            <Spinner size={14} />
            <span className="text-sm text-[var(--color-muted)]">
              {tr(
                "collection.junk_scanning",
                { n: junk.checked.toLocaleString() },
                "Looking… {n} items checked",
              )}
            </span>
            <Button
              size="sm"
              variant="secondary"
              leftIcon={<Square size={11} />}
              onClick={() => void collection.junkCancel().then(setJunk)}
            >
              {tr("collection.stop_scan", undefined, "Stop scan")}
            </Button>
          </>
        ) : (
          <Button
            size="sm"
            variant="secondary"
            leftIcon={<Search size={12} />}
            onClick={() => void scan()}
          >
            {tr("collection.junk_scan", undefined, "Look for junk files")}
          </Button>
        )}
      </div>
      {finished && junk.cancelled && (
        <p className="mt-2 text-sm text-[var(--color-muted)]">
          {tr(
            "collection.junk_cancelled",
            undefined,
            "Stopped; nothing was removed.",
          )}
        </p>
      )}
      {finished && !junk.cancelled && junk.found === 0 && (
        <p className="mt-2 text-sm text-[var(--color-good)]">
          {tr("collection.junk_none", undefined, "No junk files found.")}
        </p>
      )}
      {finished && !junk.cancelled && junk.found > 0 && (
        <div className="mt-3">
          <div className="text-sm text-[var(--color-text)]">
            {tr(
              "collection.junk_found",
              {
                n: junk.found.toLocaleString(),
                size: formatCollectionBytes(junk.allocated),
              },
              "{n} files, {size} on disk",
            )}
          </div>
          <ul className="mt-2 max-h-48 overflow-y-auto rounded-2xl border border-[var(--glass-edge)] bg-[var(--color-surface)] p-2 font-mono text-[0.6875rem] text-[var(--color-muted)]">
            {junk.sample.map((f) => (
              <li key={f.path} className="break-all" title={f.path}>
                {shortPath(f.path, settings?.roots ?? [])}
              </li>
            ))}
            {junk.found > junk.sample.length && (
              <li>
                {tr(
                  "collection.junk_more",
                  { n: (junk.found - junk.sample.length).toLocaleString() },
                  "… and {n} more",
                )}
              </li>
            )}
          </ul>
          <div className="mt-3 flex justify-end">
            <Button
              size="sm"
              variant="danger"
              loading={busy}
              leftIcon={<Trash2 size={12} />}
              onClick={() => void clean()}
            >
              {tr(
                "collection.junk_remove",
                { n: junk.found.toLocaleString() },
                "Remove {n} files",
              )}
            </Button>
          </div>
        </div>
      )}
      {cleaned && (
        <p className="mt-3 text-sm text-[var(--color-good)]">
          {tr(
            "collection.junk_removed",
            {
              n: cleaned.removed.toLocaleString(),
              size: formatCollectionBytes(cleaned.freed),
            },
            "Removed {n} files, {size} freed.",
          )}
          {cleaned.failed.length > 0 &&
            ` ${tr(
              "collection.junk_failed",
              { n: cleaned.failed.length },
              "{n} could not be removed.",
            )}`}
        </p>
      )}

      {settings && (
        <div className="mt-5 border-t border-[var(--color-border)] pt-4">
          <Toggle
            checked={settings.sweep_sidecars}
            onChange={(on) => void setCollectionSweep(on)}
            label={tr(
              "collection.junk_sweep",
              undefined,
              "Remove new “._” files after each scan",
            )}
            hint={tr(
              "collection.junk_sweep_hint",
              undefined,
              "Only the Mac's “._” files, each checked before it goes. Other junk is left for this window.",
            )}
          />
        </div>
      )}

      {mac?.available && (
        <div className="mt-5 border-t border-[var(--color-border)] pt-4">
          <h3 className="mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--color-muted)]">
            {tr("collection.mac_title", undefined, "This Mac")}
          </h3>
          <Toggle
            checked={mac.finder_network_off && mac.finder_usb_off}
            disabled={busy}
            onChange={(on) => void macAction(() => collection.macosFinder(on))}
            label={tr(
              "collection.mac_finder",
              undefined,
              "Stop Finder writing .DS_Store on network shares and USB drives",
            )}
            hint={tr(
              "collection.mac_finder_hint",
              undefined,
              "Finder's own setting for your Mac user. It applies once Finder restarts (log out and in).",
            )}
          />
          {mac.volumes.map((v) => (
            <div
              key={v.volume}
              className="mt-3 flex flex-wrap items-center gap-2 text-sm"
            >
              <span className="text-[var(--color-text)]">
                {v.indexing === "on"
                  ? tr(
                      "collection.mac_spotlight_on",
                      { volume: v.volume },
                      "Spotlight indexes {volume}",
                    )
                  : v.indexing === "off"
                    ? tr(
                        "collection.mac_spotlight_off",
                        { volume: v.volume },
                        "Spotlight is off for {volume}",
                      )
                    : v.volume}
              </span>
              {v.indexing === "on" && (
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busy}
                  onClick={() =>
                    void macAction(() =>
                      collection.macosSpotlight(v.volume, "off"),
                    )
                  }
                >
                  {tr(
                    "collection.mac_spotlight_disable",
                    undefined,
                    "Turn Spotlight off for this drive",
                  )}
                </Button>
              )}
              {v.indexing !== "on" && v.has_index && (
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={busy}
                  onClick={() =>
                    void macAction(() =>
                      collection.macosSpotlight(v.volume, "remove_index"),
                    )
                  }
                >
                  {tr(
                    "collection.mac_spotlight_remove",
                    undefined,
                    "Remove the old index",
                  )}
                </Button>
              )}
            </div>
          ))}
          {mac.volumes.length > 0 && (
            <p className="mt-1 text-xs text-[var(--color-muted)]">
              {tr(
                "collection.mac_spotlight_hint",
                undefined,
                "macOS asks for your password. Spotlight on a games drive only spends time and space indexing packages.",
              )}
            </p>
          )}
        </div>
      )}
    </Modal>
  );
}
