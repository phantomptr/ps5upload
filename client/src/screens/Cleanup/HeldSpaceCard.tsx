import { useEffect, useState } from "react";
import { SearchCheck } from "lucide-react";

import { spaceCheck, type SpaceCheck } from "../../api/cleanup";
import { fetchVolumes, type Volume } from "../../api/ps5";
import { Button, Callout, Spinner } from "../../components";
import { transferAddr } from "../../lib/addr";
import { checkableDrives, heldDriveName, heldSpaceLine } from "../../lib/heldSpace";
import { useTr } from "../../state/lang";

/**
 * "Space no file uses" (#979): for each M.2 or USB drive, a check of whether the drive counts more
 * space as used than its files take, which is what a deleted file something still holds looks like.
 * Run only when asked: it counts every file on the drive.
 */
export function HeldSpaceCard({ host }: { host: string }) {
  const tr = useTr();
  const [drives, setDrives] = useState<Volume[]>([]);
  const [running, setRunning] = useState<string | null>(null);
  const [results, setResults] = useState<Record<string, SpaceCheck>>({});
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!host) return;
    let cancelled = false;
    fetchVolumes(transferAddr(host))
      .then((v) => {
        if (!cancelled) setDrives(checkableDrives(v));
      })
      .catch(() => {
        // No list, no card: Volumes says why when it is opened.
      });
    return () => {
      cancelled = true;
    };
  }, [host]);

  if (drives.length === 0) return null;

  const run = async (path: string) => {
    setRunning(path);
    setError(null);
    try {
      const r = await spaceCheck(host, path);
      setResults((prev) => ({ ...prev, [path]: r }));
    } catch (e) {
      setError(
        tr("cleanup_held_failed", { error: e instanceof Error ? e.message : String(e) }, "The check could not finish"),
      );
    } finally {
      setRunning(null);
    }
  };

  const lines = drives.flatMap((d) => (results[d.path] ? [heldSpaceLine(tr, results[d.path])] : []));
  const found = lines.some((l) => l.found);

  return (
    <Callout tone={found ? "warn" : "info"} title={tr("cleanup_held_title", undefined, "Space no file uses")}>
      <div className="space-y-2" data-testid="cleanup-held">
        <p>
          {tr(
            "cleanup_held_intro",
            undefined,
            "Checks whether an M.2 or USB drive counts more space as used than its files take. It reads names and sizes only.",
          )}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          {drives.map((d) => {
            const drive = heldDriveName(tr, d.path);
            return (
              <Button
                key={d.path}
                variant="secondary"
                size="sm"
                leftIcon={running === d.path ? <Spinner size={12} /> : <SearchCheck size={12} />}
                disabled={running !== null}
                onClick={() => void run(d.path)}
                data-testid={`cleanup-held-check-${d.path}`}
              >
                {running === d.path
                  ? tr("cleanup_held_checking", { drive }, `Checking ${drive}…`)
                  : tr("cleanup_held_check", { drive }, `Check ${drive}`)}
              </Button>
            );
          })}
        </div>
        {lines.map((l) => (
          <p key={l.text}>{l.text}</p>
        ))}
        {found ? (
          <p className="text-xs">
            {tr(
              "cleanup_held_found_help",
              undefined,
              "This is usually a file deleted while something still had it open. It comes back once that lets go, and restarting the PS5 lets go of everything. If it is still there after a restart, the PS5 has lost track of it and PS5Upload can't give it back: formatting the drive in the PS5's Settings does, after you move what you want to keep off it.",
            )}
          </p>
        ) : null}
        {error ? <p className="text-[var(--color-bad)]">{error}</p> : null}
      </div>
    </Callout>
  );
}
